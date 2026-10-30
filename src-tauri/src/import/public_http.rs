//! Bounded anonymous page/catalog transport for public metadata adapters.
use super::{http::Cancellation, model::Platform};
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};
use tokio::sync::{Mutex, Semaphore};

const MAX_BYTES: usize = 16 * 1024 * 1024;

fn validate(platform: Platform, raw: &str) -> Result<url::Url, String> {
    let invalid = || "Public metadata URL is not supported.".to_string();
    let url = url::Url::parse(raw).map_err(|_| invalid())?;
    let host = url.host_str().ok_or_else(invalid)?;
    let allowed = match platform {
        Platform::Spotify => matches!(
            host,
            "open.spotify.com"
                | "api.spotify.com"
                | "api-partner.spotify.com"
                | "spclient.wg.spotify.com"
        ),
        Platform::Soundcloud => {
            matches!(
                host,
                "soundcloud.com"
                    | "www.soundcloud.com"
                    | "api-v2.soundcloud.com"
                    | "api.soundcloud.com"
            ) || host.ends_with(".sndcdn.com")
        }
        Platform::AppleMusic => matches!(
            host,
            "music.apple.com"
                | "amp-api.music.apple.com"
                | "amp-api-edge.music.apple.com"
                | "amp-api.apps.apple.com"
                | "amp-api-edge.apps.apple.com"
        ),
        _ => false,
    };
    if !allowed
        || url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url)
}

struct Transport {
    client: reqwest::Client,
    gate: Semaphore,
    next: [Mutex<tokio::time::Instant>; 3],
}

fn transport() -> Result<&'static Transport, String> {
    static HTTP: OnceLock<Result<Transport, String>> = OnceLock::new();
    HTTP.get_or_init(|| {
        Ok(Transport {
            client: reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(8))
                .timeout(Duration::from_secs(20))
                .user_agent("Mozilla/5.0 (compatible; Capsule/0.3; public metadata importer)")
                .build()
                .map_err(|_| "Could not initialize public metadata access.".to_string())?,
            gate: Semaphore::new(2),
            next: std::array::from_fn(|_| Mutex::new(tokio::time::Instant::now())),
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

pub async fn get_text(
    platform: Platform,
    raw: &str,
    authorization: Option<&str>,
    cancel: &Cancellation,
) -> Result<String, String> {
    bounded(
        platform,
        raw,
        authorization,
        "text/html,application/xhtml+xml,application/javascript;q=0.9,*/*;q=0.8",
        cancel,
    )
    .await
}

pub async fn get_json(
    platform: Platform,
    raw: &str,
    authorization: Option<&str>,
    cancel: &Cancellation,
) -> Result<Value, String> {
    let raw = bounded(
        platform,
        raw,
        authorization,
        "application/json",
        cancel,
    )
    .await?;
    serde_json::from_str(&raw).map_err(|_| "The provider returned invalid public metadata.".into())
}

async fn bounded(
    platform: Platform,
    raw: &str,
    authorization: Option<&str>,
    accept: &str,
    cancel: &Cancellation,
) -> Result<String, String> {
    let initial = validate(platform, raw)?;
    cancel.check()?;
    let http = transport()?;
    let request = async {
        let _permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("Import cancelled.".into()),
            permit = http.gate.acquire() => permit.map_err(|_| "Public metadata access is unavailable.")?,
        };
        let index = match platform {
            Platform::Spotify => 0,
            Platform::Soundcloud => 1,
            Platform::AppleMusic => 2,
            _ => unreachable!(),
        };
        let mut current = initial.clone();
        let mut retries = 0;
        let mut redirects = 0;
        loop {
            cancel.check()?;
            let deadline = {
                let mut next = http.next[index].lock().await;
                let deadline = (*next).max(tokio::time::Instant::now());
                *next = deadline + Duration::from_millis(200);
                deadline
            };
            tokio::time::sleep_until(deadline).await;
            let mut request = http.client.get(current.clone());
            let origin = match platform {
                Platform::Spotify => "https://open.spotify.com",
                Platform::Soundcloud => "https://soundcloud.com",
                Platform::AppleMusic => "https://music.apple.com",
                _ => unreachable!(),
            };
            request = request
                .header(reqwest::header::ACCEPT, accept)
                .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.8")
                .header(reqwest::header::ORIGIN, origin);
            // Never forward even an anonymous token to a different redirect host.
            if current.host_str() == initial.host_str() {
                if let Some(value) = authorization {
                    request = request.header(reqwest::header::AUTHORIZATION, value);
                }
            }
            let mut response = request
                .send()
                .await
                .map_err(|_| "Could not reach the public metadata provider.".to_string())?;
            let status = response.status();
            if status.is_redirection() {
                if redirects >= 3 {
                    return Err("Public metadata redirected too many times.".into());
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or("Invalid public metadata redirect.")?;
                let next = current
                    .join(location)
                    .map_err(|_| "Invalid public metadata redirect.")?;
                current = validate(platform, next.as_str())?;
                redirects += 1;
                continue;
            }
            if status.as_u16() == 429 && retries < 1 {
                let seconds = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(2);
                if seconds <= 10 {
                    retries += 1;
                    tokio::time::sleep(Duration::from_secs(seconds.max(1))).await;
                    continue;
                }
            }
            if !status.is_success() {
                return Err(format!(
                    "Public {} metadata request failed (HTTP {}).",
                    platform.as_str(),
                    status.as_u16()
                ));
            }
            if response
                .content_length()
                .is_some_and(|size| size > MAX_BYTES as u64)
            {
                return Err("Public metadata response is too large.".into());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "Public metadata transfer failed.".to_string())?
            {
                if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
                    return Err("Public metadata response is too large.".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            cancel.check()?;
            return String::from_utf8(bytes)
                .map_err(|_| "Public metadata is not valid text.".into());
        }
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Import cancelled.".into()),
        result = tokio::time::timeout(Duration::from_secs(30), request) => result.map_err(|_| "Public metadata request timed out.".to_string())?,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permits_only_provider_https_pages_and_catalogs() {
        for (provider, url) in [
            (
                Platform::Spotify,
                "https://open.spotify.com/embed/playlist/a",
            ),
            (
                Platform::Spotify,
                "https://spclient.wg.spotify.com/playlist/v2/playlist/a",
            ),
            (
                Platform::Soundcloud,
                "https://api-v2.soundcloud.com/tracks?ids=1,2",
            ),
            (
                Platform::Soundcloud,
                "https://a-v2.sndcdn.com/assets/app.js",
            ),
            (
                Platform::AppleMusic,
                "https://music.apple.com/us/playlist/a/pl.test",
            ),
            (
                Platform::AppleMusic,
                "https://amp-api.music.apple.com/v1/catalog/us/playlists/pl.test",
            ),
        ] {
            assert!(validate(provider, url).is_ok(), "{url}");
        }
    }
    #[test]
    fn rejects_cross_provider_redirects_and_credential_urls() {
        for raw in [
            "http://open.spotify.com/playlist/a",
            "https://open.spotify.com.evil.test/a",
            "https://user:secret@open.spotify.com/a",
            "https://open.spotify.com:444/a",
            "https://127.0.0.1/a",
            "https://soundcloud.com/a",
            "https://open.spotify.com/a#secret",
        ] {
            assert!(validate(Platform::Spotify, raw).is_err(), "{raw}");
        }
        assert!(validate(Platform::Local, "https://open.spotify.com/a").is_err());
    }
}
