//! Bounded, cancellable access to official metadata APIs. Never returns raw HTTP
//! errors: those may contain a YouTube API key in the request URL.
use super::model::Platform;
use serde_json::Value;
use std::{future::Future, pin::Pin, time::Duration};
use tokio::sync::{watch, Mutex, Semaphore};

#[derive(Clone)]
pub struct Cancellation(watch::Sender<bool>);
impl Default for Cancellation {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl Cancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        loop {
            if *receiver.borrow_and_update() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err("Import cancelled.".into())
        } else {
            Ok(())
        }
    }
}

pub(crate) type JsonFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;
pub(crate) trait JsonTransport: Send + Sync {
    fn get<'a>(
        &'a self,
        platform: Platform,
        path: &'a str,
        authorization: Option<&'a str>,
        cancel: &'a Cancellation,
    ) -> JsonFuture<'a>;
}

pub struct ApiClient {
    client: reqwest::Client,
    gate: Semaphore,
    next_request: [Mutex<tokio::time::Instant>; 4],
}
impl ApiClient {
    pub fn shared() -> Result<&'static Self, String> {
        static CLIENT: std::sync::OnceLock<Result<ApiClient, String>> = std::sync::OnceLock::new();
        CLIENT.get_or_init(Self::new).as_ref().map_err(Clone::clone)
    }
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            gate: Semaphore::new(2),
            next_request: std::array::from_fn(|_| Mutex::new(tokio::time::Instant::now())),
            client: reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(25))
                .connect_timeout(Duration::from_secs(10))
                .build()
                .map_err(|_| "Could not initialize metadata HTTP client.")?,
        })
    }
    async fn ready(&self, platform: Platform, cancel: &Cancellation) -> Result<(), String> {
        let index = provider_index(platform)?;
        loop {
            cancel.check()?;
            let mut next = self.next_request[index].lock().await;
            let now = tokio::time::Instant::now();
            if *next <= now {
                *next = now + Duration::from_millis(250);
                return Ok(());
            }
            let deadline = *next;
            drop(next);
            tokio::select! { _ = cancel.cancelled() => return Err("Import cancelled.".into()), _ = tokio::time::sleep_until(deadline) => {} }
        }
    }
    async fn cooldown(&self, platform: Platform, seconds: u64) -> Result<(), String> {
        let mut next = self.next_request[provider_index(platform)?].lock().await;
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(seconds))
            .ok_or("Provider returned an unsupported rate-limit delay.")?;
        *next = (*next).max(deadline);
        Ok(())
    }
}
fn retry_seconds(header: Option<&str>, attempt: u32) -> u64 {
    header
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1 << attempt.min(10))
        .max(1)
}
fn provider_index(platform: Platform) -> Result<usize, String> {
    match platform {
        Platform::Spotify => Ok(0),
        Platform::AppleMusic => Ok(1),
        Platform::Youtube => Ok(2),
        Platform::Soundcloud => Ok(3),
        _ => Err("Unsupported metadata API.".into()),
    }
}
fn api_host(platform: Platform) -> Result<&'static str, String> {
    match platform {
        Platform::Spotify => Ok("api.spotify.com"),
        Platform::AppleMusic => Ok("api.music.apple.com"),
        Platform::Youtube => Ok("www.googleapis.com"),
        Platform::Soundcloud => Ok("api.soundcloud.com"),
        _ => Err("Unsupported metadata API.".into()),
    }
}
pub(crate) fn api_url(platform: Platform, path: &str) -> Result<url::Url, String> {
    let host = api_host(platform)?;
    let bad = || "Unsafe metadata API URL.".to_string();
    let path_only = path.split('?').next().unwrap_or_default();
    if path_only.contains('\\')
        || path_only.to_ascii_lowercase().contains("%2f")
        || path_only.to_ascii_lowercase().contains("%2e")
        || path_only.contains("/../")
    {
        return Err(bad());
    }
    let url = if path.starts_with('/') && !path.starts_with("//") {
        url::Url::parse(&format!("https://{host}{path}"))
    } else {
        url::Url::parse(path)
    }
    .map_err(|_| bad())?;
    if url.scheme() != "https"
        || url.host_str() != Some(host)
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(bad());
    }
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    let safe_id = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-:".contains(&c))
            && value != "."
            && value != ".."
    };
    let valid = match platform {
        Platform::Spotify => {
            parts.first() == Some(&"v1")
                && parts.get(2).is_some_and(|v| safe_id(v))
                && match parts.get(1) {
                    Some(&"tracks") => parts.len() == 3,
                    Some(&"albums") => {
                        parts.len() == 3 || (parts.len() == 4 && parts[3] == "tracks")
                    }
                    Some(&"playlists") => {
                        parts.len() == 3
                            || (parts.len() == 4 && matches!(parts[3], "tracks" | "items"))
                    }
                    _ => false,
                }
        }
        Platform::AppleMusic => {
            parts.first() == Some(&"v1")
                && parts.get(1) == Some(&"catalog")
                && parts
                    .get(2)
                    .is_some_and(|s| s.len() == 2 && s.bytes().all(|c| c.is_ascii_alphabetic()))
                && parts.get(4).is_some_and(|v| safe_id(v))
                && matches!(parts.get(3), Some(&"playlists" | &"albums" | &"songs"))
                && (parts.len() == 5
                    || (parts.len() == 6 && parts[5] == "tracks" && parts[3] != "songs"))
        }
        Platform::Youtube => {
            parts.len() == 3
                && parts[..2] == ["youtube", "v3"]
                && matches!(
                    parts[2],
                    "playlists" | "playlistItems" | "videos" | "search" | "channels"
                )
        }
        Platform::Soundcloud => {
            parts.as_slice() == ["resolve"]
                || (parts.get(1).is_some_and(|v| safe_id(v))
                    && match parts[0] {
                        "tracks" => parts.len() == 2,
                        "playlists" => {
                            parts.len() == 2 || (parts.len() == 3 && parts[2] == "tracks")
                        }
                        "users" => {
                            parts.len() == 2
                                || (parts.len() == 3 && parts[2] == "tracks")
                                || (parts.len() == 4 && parts[2..] == ["likes", "tracks"])
                        }
                        _ => false,
                    })
        }
        _ => false,
    };
    if !valid {
        return Err(bad());
    }
    Ok(url)
}

/// A pagination response can continue only the resource it was requested for.
pub(crate) fn next_page(platform: Platform, current: &str, next: &str) -> Result<String, String> {
    let a = api_url(platform, current)?;
    let b = api_url(platform, next)?;
    if a.path() != b.path() {
        return Err("Provider returned an unsafe pagination link.".into());
    }
    Ok(b.to_string())
}

impl JsonTransport for ApiClient {
    fn get<'a>(
        &'a self,
        platform: Platform,
        path: &'a str,
        authorization: Option<&'a str>,
        cancel: &'a Cancellation,
    ) -> JsonFuture<'a> {
        Box::pin(async move {
            let url = api_url(platform, path)?;
            cancel.check()?;
            let _permit = tokio::select! {
                _ = cancel.cancelled() => return Err("Import cancelled.".into()),
                value = self.gate.acquire() => value.map_err(|_| "Metadata HTTP service stopped.")?,
            };
            for attempt in 0..=3u32 {
                self.ready(platform, cancel).await?;
                let mut request = self
                    .client
                    .get(url.clone())
                    .header(reqwest::header::ACCEPT, "application/json");
                if let Some(auth) = authorization {
                    request = request.header(reqwest::header::AUTHORIZATION, auth);
                }
                let response = tokio::select! {
                    _ = cancel.cancelled() => return Err("Import cancelled.".into()),
                    value = request.send() => value.map_err(|_| "Metadata API request failed or timed out.")?,
                };
                let status = response.status();
                // /resolve may return a redirect to the API resource. Surface
                // only a validated resource location; never follow redirects.
                if status.is_redirection()
                    && platform == Platform::Soundcloud
                    && url.path() == "/resolve"
                {
                    let location = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .ok_or("SoundCloud returned no resource location.")?;
                    let resolved = api_url(platform, location)?;
                    let p: Vec<_> = resolved.path().trim_matches('/').split('/').collect();
                    if p.len() != 2 || !matches!(p[0], "tracks" | "playlists" | "users") {
                        return Err("SoundCloud returned an unsafe resource location.".into());
                    }
                    return Ok(
                        serde_json::json!({"capsule_resolved_resource":resolved.to_string()}),
                    );
                }
                if status.as_u16() == 429 || status.is_server_error() {
                    // Waiting remains cancellable even for long provider cooldowns.
                    let seconds = retry_seconds(
                        response
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok()),
                        attempt,
                    );
                    self.cooldown(platform, seconds).await?;
                    if attempt < 3 {
                        continue;
                    }
                }
                if !status.is_success() {
                    return Err(match status.as_u16() {
                        401 => "Provider credentials are missing, invalid, or expired.",
                        403 => "Provider denied access. Check application access, account requirements, and collection permissions.",
                        404 => "Provider collection or track is missing or private.",
                        429 => "Provider rate limit reached. Try again later.",
                        _ => "Metadata API returned an unsuccessful response.",
                    }.into());
                }
                const LIMIT: usize = 8 * 1024 * 1024;
                if response.content_length().is_some_and(|n| n > LIMIT as u64) {
                    return Err("Metadata response exceeded the size limit.".into());
                }
                let mut response = response;
                let mut body = Vec::new();
                loop {
                    let chunk = tokio::select! { _ = cancel.cancelled() => return Err("Import cancelled.".into()), value = response.chunk() => value.map_err(|_| "Could not read metadata API response.")? };
                    let Some(chunk) = chunk else {
                        break;
                    };
                    if body.len().saturating_add(chunk.len()) > LIMIT {
                        return Err("Metadata response exceeded the size limit.".into());
                    }
                    body.extend_from_slice(&chunk);
                }
                return serde_json::from_slice(&body)
                    .map_err(|_| "Provider returned invalid JSON metadata.".into());
            }
            Err("Metadata API retry limit reached.".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_after_is_never_shortened_below_the_provider_delay() {
        assert_eq!(retry_seconds(Some("120"), 0), 120);
        assert_eq!(retry_seconds(None, 2), 4);
        assert_eq!(retry_seconds(Some("invalid"), 1), 2);
    }
    #[test]
    fn rejects_api_redirect_targets_and_cross_resource_pagination() {
        assert!(api_url(Platform::Spotify, "https://evil.example/v1/tracks/x").is_err());
        assert!(api_url(Platform::Spotify, "http://api.spotify.com/v1/tracks/x").is_err());
        assert!(api_url(Platform::Spotify, "https://api.spotify.com:444/v1/tracks/x").is_err());
        assert!(api_url(Platform::Spotify, "/v1/playlists/x/items?offset=50").is_ok());
        assert!(next_page(
            Platform::Spotify,
            "/v1/playlists/x/items",
            "https://api.spotify.com/v1/playlists/other/items"
        )
        .is_err());
        assert!(next_page(
            Platform::AppleMusic,
            "/v1/catalog/us/playlists/pl.x/tracks",
            "/v1/catalog/us/playlists/pl.x/tracks?offset=100"
        )
        .is_ok());
        assert!(api_url(
            Platform::Soundcloud,
            "/resolve?url=https%3A%2F%2Fsoundcloud.com%2Fartist%2Ftrack"
        )
        .is_ok());
        assert!(api_url(Platform::Soundcloud, "/tracks/123/streams").is_err());
        assert!(api_url(Platform::Soundcloud, "/users/123/followings").is_err());
    }
    #[tokio::test]
    async fn cancellation_is_sticky_and_wakes_all_waiters() {
        let cancel = Cancellation::default();
        let second = cancel.clone();
        cancel.cancel();
        tokio::time::timeout(Duration::from_millis(50), second.cancelled())
            .await
            .unwrap();
        assert!(second.check().is_err());
    }
    #[tokio::test]
    async fn request_waiting_for_shared_capacity_can_be_cancelled() {
        let client = ApiClient::new().unwrap();
        let _capacity = client.gate.acquire_many(2).await.unwrap();
        let cancel = Cancellation::default();
        cancel.cancel();
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            client.get(Platform::Spotify, "/v1/tracks/x", None, &cancel),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
    }
}
