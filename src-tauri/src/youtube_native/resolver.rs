//! One in-process client; short-lived URLs never enter the database or logs.
use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use rustypipe::client::RustyPipe;
use std::{
    collections::HashMap,
    fmt,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

const CACHE_LIMIT: usize = 16;
type Fetch = Arc<dyn Fn(String) -> BoxFuture<'static, Result<Resolved, String>> + Send + Sync>;
type Resolution = Shared<BoxFuture<'static, Result<Resolved, String>>>;

#[derive(Clone)]
pub struct Resolved {
    pub url: String,
    pub itag: u32,
    pub duration_ms: u64,
    pub audio_info: crate::audio::AudioInfo,
    expires: Instant,
}
impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("itag", &self.itag)
            .field("duration_ms", &self.duration_ms)
            .finish_non_exhaustive()
    }
}
struct Entry {
    serial: u64,
    result: Resolution,
    task: tokio::task::AbortHandle,
}
pub struct Resolver {
    fetch: Fetch,
    cache: Mutex<HashMap<String, Entry>>,
    serial: AtomicU64,
    slots: Arc<Semaphore>,
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub fn valid_media_url(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && url
                .host_str()
                .is_some_and(|host| host.ends_with(".googlevideo.com"))
    })
}

fn public_player(data: &serde_json::Value, id: &str) -> Result<Resolved, String> {
    if data["playabilityStatus"]["status"].as_str() != Some("OK") {
        return Err("YouTube could not provide public audio for this recording. It may be unavailable or require sign-in.".into());
    }
    if data["videoDetails"]["videoId"].as_str() != Some(id) {
        return Err("YouTube returned a different recording.".into());
    }
    if data["videoDetails"]["isLive"].as_bool() == Some(true)
        || !data["playabilityStatus"]["liveStreamability"].is_null()
    {
        return Err("Live streams are not supported by native playback.".into());
    }
    let formats = data["streamingData"]["adaptiveFormats"]
        .as_array()
        .ok_or("No compatible native audio stream is available for this recording.")?;
    let audio = formats
        .iter()
        .filter(|format| {
            format["mimeType"]
                .as_str()
                .is_some_and(|mime| mime.starts_with("audio/mp4;") && mime.contains("mp4a.40.2"))
                && format["drmTrackType"].is_null()
                && (format["drmFamilies"].is_null()
                    || format["drmFamilies"]
                        .as_array()
                        .is_some_and(|systems| systems.is_empty()))
                && format["url"].as_str().is_some_and(valid_media_url)
        })
        .max_by_key(|format| format["bitrate"].as_u64().unwrap_or(0))
        .ok_or("No compatible native audio stream is available for this recording.")?;
    let url = audio["url"].as_str().expect("validated audio URL");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let expires = url::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "expire")
                .and_then(|(_, value)| value.parse::<u64>().ok())
        })
        .map(|expires| expires.saturating_sub(now))
        .or_else(|| {
            data["streamingData"]["expiresInSeconds"]
                .as_str()?
                .parse()
                .ok()
        })
        .unwrap_or(60)
        .saturating_sub(30)
        .min(300);
    let duration_ms = audio["approxDurationMs"]
        .as_str()
        .and_then(|v| v.parse().ok())
        .or_else(|| {
            data["videoDetails"]["lengthSeconds"]
                .as_str()?
                .parse::<u64>()
                .ok()?
                .checked_mul(1000)
        })
        .unwrap_or(0);
    Ok(Resolved {
        url: url.to_string(),
        itag: audio["itag"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("YouTube returned an invalid audio format.")?,
        duration_ms,
        audio_info: crate::audio::AudioInfo {
            codec: Some("AAC".into()),
            container: Some("MP4/M4A".into()),
            bitrate_kbps: audio["averageBitrate"].as_u64().or_else(|| audio["bitrate"].as_u64())
                .and_then(|v| u32::try_from((v + 500) / 1000).ok()).filter(|v| (1..=100_000).contains(v)),
            sample_rate_hz: audio["audioSampleRate"].as_str().and_then(|v| v.parse().ok())
                .or_else(|| audio["audioSampleRate"].as_u64().and_then(|v| u32::try_from(v).ok()))
                .filter(|v| (1..=768_000).contains(v)),
            channels: audio["audioChannels"].as_u64().and_then(|v| u16::try_from(v).ok())
                .filter(|v| (1..=64).contains(v)),
            ..Default::default()
        },
        expires: Instant::now() + Duration::from_secs(expires),
    })
}

impl Resolver {
    pub async fn resolve_recovering(&self, id: &str) -> Result<Resolved, String> {
        for attempt in 0..3 {
            match self.resolve(id).await {
                Err(message)
                    if attempt < 2
                        && matches!(
                            message.as_str(),
                            "YouTube audio resolution could not connect."
                                | "YouTube audio resolution was interrupted."
                                | "Could not initialize the public YouTube session."
                        ) =>
                {
                    self.invalidate(id);
                    tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
                }
                result => return result,
            }
        }
        unreachable!("bounded resolution returns its final result")
    }
    fn new(fetch: Fetch) -> Self {
        Self {
            fetch,
            cache: Mutex::new(HashMap::new()),
            serial: AtomicU64::new(0),
            slots: Arc::new(Semaphore::new(2)),
        }
    }
    pub fn public(storage: &Path) -> Result<Self, String> {
        // No browser process, auto-discovered helper, account cookie or raw error reports.
        let client = RustyPipe::builder()
            .storage_dir(storage)
            .no_botguard()
            .no_reporter()
            .timeout(Duration::from_secs(10))
            .n_http_retries(1)
            .build()
            .map_err(|_| "Could not initialize native YouTube playback.".to_string())?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Could not initialize native YouTube playback.".to_string())?;
        Ok(Self::new(Arc::new(move |id| {
            let client = client.clone();
            let http = http.clone();
            async move {
                // RustyPipe 0.11's iOS URLs can serve the first chunk then return 403.
                // Use the public VisionOS context with the same anonymous visitor.
                // Context reference: yt-dlp/extractor/youtube/_base.py (VISIONOS 1.02).
                let visitor =
                    client.query().get_visitor_data(false).await.map_err(|_| {
                        "Could not initialize the public YouTube session.".to_string()
                    })?;
                let mut response = http
                    .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
                    .header("X-Youtube-Client-Name", "101")
                    .header("X-Youtube-Client-Version", "1.02")
                    .json(&serde_json::json!({
                        "context":{"client":{
                            "clientName":"VISIONOS", "clientVersion":"1.02",
                            "deviceMake":"Apple", "deviceModel":"RealityDevice17,1",
                            "osName":"visionOS", "osVersion":"26.5.23O471",
                            "hl":"en", "gl":"US", "visitorData":visitor
                        }},
                        "videoId":id, "contentCheckOk":true, "racyCheckOk":true
                    }))
                    .send()
                    .await
                    .map_err(|_| "YouTube audio resolution could not connect.".to_string())?;
                if !response.status().is_success() {
                    return Err("YouTube could not provide public audio for this recording.".into());
                }
                let mut body = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| "YouTube audio resolution was interrupted.".to_string())?
                {
                    if body.len() + chunk.len() > 2 * 1024 * 1024 {
                        return Err("YouTube returned an oversized player response.".into());
                    }
                    body.extend_from_slice(&chunk);
                }
                let data = serde_json::from_slice(&body)
                    .map_err(|_| "YouTube returned an unreadable player response.".to_string())?;
                public_player(&data, &id)
            }
            .boxed()
        })))
    }
    pub async fn resolve(&self, id: &str) -> Result<Resolved, String> {
        if !valid_id(id) {
            return Err("Choose a valid YouTube recording first.".into());
        }
        let (serial, pending) = {
            let mut cache = self.cache.lock().expect("YouTube URL cache");
            cache.retain(|_, entry| match entry.result.peek() {
                Some(Ok(stream)) => stream.expires > Instant::now(),
                Some(Err(_)) => false,
                None => true,
            });
            if let Some(entry) = cache.get(id) {
                (entry.serial, entry.result.clone())
            } else {
                if cache.len() >= CACHE_LIMIT {
                    let oldest = cache
                        .iter()
                        .filter(|(_, entry)| entry.result.peek().is_some())
                        .min_by_key(|(_, entry)| entry.serial)
                        .map(|(id, _)| id.clone());
                    if let Some(oldest) = oldest {
                        cache.remove(&oldest);
                    } else {
                        return Err("YouTube playback is busy. Try this recording again.".into());
                    }
                }
                let serial = self.serial.fetch_add(1, Ordering::Relaxed);
                let fetch = self.fetch.clone();
                let slots = self.slots.clone();
                let owned_id = id.to_string();
                // An aborted playback waiter cannot strand a shared request/semaphore permit.
                let job = tokio::spawn(async move {
                    let _slot = slots
                        .acquire_owned()
                        .await
                        .map_err(|_| "Native playback stopped.".to_string())?;
                    tokio::time::timeout(Duration::from_secs(25), fetch(owned_id))
                        .await
                        .map_err(|_| "YouTube audio resolution timed out.".to_string())?
                });
                let task = job.abort_handle();
                let pending = async move {
                    job.await
                        .map_err(|_| "YouTube audio resolution failed.".to_string())?
                }
                .boxed()
                .shared();
                cache.insert(
                    id.into(),
                    Entry {
                        serial,
                        result: pending.clone(),
                        task,
                    },
                );
                (serial, pending)
            }
        };
        let result = pending.await.and_then(|stream| {
            if stream.expires <= Instant::now() {
                Err("YouTube returned an expired stream. Retry this recording.".into())
            } else if !valid_media_url(&stream.url) {
                Err("YouTube returned an invalid media address.".into())
            } else {
                Ok(stream)
            }
        });
        if result.is_err() {
            let mut cache = self.cache.lock().expect("YouTube URL cache");
            if cache.get(id).is_some_and(|entry| entry.serial == serial) {
                cache.remove(id);
            }
        }
        result
    }

    pub fn invalidate(&self, id: &str) {
        if let Some(entry) = self.cache.lock().expect("YouTube URL cache").remove(id) {
            entry.task.abort();
        }
    }
    pub fn prioritize(&self, id: &str) {
        self.cache
            .lock()
            .expect("YouTube URL cache")
            .retain(|cached_id, entry| {
                let keep = cached_id == id || entry.result.peek().is_some();
                if !keep {
                    entry.task.abort();
                }
                keep
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{future::BoxFuture, FutureExt};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    fn resolved(lifetime: Duration) -> Resolved {
        Resolved {
            url: "https://rr1---sn-test.googlevideo.com/videoplayback?expire=2000000000".into(),
            itag: 140,
            duration_ms: 30_000,
            audio_info: Default::default(),
            expires: Instant::now() + lifetime,
        }
    }
    fn backend(calls: Arc<AtomicUsize>) -> Fetch {
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                tokio::task::yield_now().await;
                Ok(resolved(Duration::from_secs(300)))
            }
            .boxed()
        })
    }

    #[tokio::test]
    async fn concurrent_requests_and_replays_share_one_resolution() {
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = Resolver::new(backend(calls.clone()));
        let (first, second) = tokio::join!(
            resolver.resolve("M7lc1UVf-VE"),
            resolver.resolve("M7lc1UVf-VE")
        );
        assert_eq!(first.unwrap().itag, second.unwrap().itag);
        resolver.resolve("M7lc1UVf-VE").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn expired_and_failed_resolutions_are_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let fetch: Fetch = Arc::new(move |_| {
            let call = seen.fetch_add(1, Ordering::SeqCst);
            async move {
                match call {
                    0 => Err("unavailable".into()),
                    1 => Ok(resolved(Duration::ZERO)),
                    _ => Ok(resolved(Duration::from_secs(300))),
                }
            }
            .boxed()
        });
        let resolver = Resolver::new(fetch);
        assert!(resolver.resolve("M7lc1UVf-VE").await.is_err());
        assert!(resolver.resolve("M7lc1UVf-VE").await.is_err());
        assert!(resolver.resolve("M7lc1UVf-VE").await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn rejects_bad_ids_before_the_backend_and_does_not_log_signed_urls() {
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = Resolver::new(backend(calls.clone()));
        for id in [
            "",
            "../password",
            "https://youtu.be/M7lc1UVf-VE",
            "M7lc1UVf-V!",
        ] {
            assert!(resolver.resolve(id).await.is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!format!("{:?}", resolved(Duration::from_secs(1))).contains("googlevideo"));
    }

    #[test]
    fn media_urls_must_be_https_googlevideo_hosts_without_credentials() {
        assert!(valid_media_url(
            "https://rr1---sn-test.googlevideo.com/videoplayback?x=1"
        ));
        for url in [
            "http://rr1.googlevideo.com/",
            "https://googlevideo.com.evil.test/",
            "https://127.0.0.1/",
            "https://user:secret@rr1.googlevideo.com/",
            "file:///tmp/audio",
        ] {
            assert!(!valid_media_url(url), "{url}");
        }
    }

    #[test]
    fn public_player_selects_supported_aac_and_rejects_unusable_responses() {
        let format = serde_json::json!({
            "itag":140, "mimeType":"audio/mp4; codecs=\"mp4a.40.2\"", "bitrate":129000,
            "url":"https://rr1.googlevideo.com/videoplayback?expire=2000000000",
            "approxDurationMs":"138410"
        });
        let mut data = serde_json::json!({"playabilityStatus":{"status":"OK"},
            "videoDetails":{"videoId":"k13FUGmRRHc","lengthSeconds":"139"},
            "streamingData":{"expiresInSeconds":"21600","adaptiveFormats":[format.clone()]}});
        assert_eq!(
            public_player(&data, "k13FUGmRRHc").unwrap().duration_ms,
            138410
        );
        let mut unsupported = format.clone();
        unsupported["mimeType"] = "audio/mp4; codecs=\"ac-3\"".into();
        unsupported["bitrate"] = 384000.into();
        data["streamingData"]["adaptiveFormats"]
            .as_array_mut()
            .unwrap()
            .push(unsupported);
        assert_eq!(public_player(&data, "k13FUGmRRHc").unwrap().itag, 140);
        assert!(public_player(&data, "ub7N1zW1z-4").is_err());
        data["playabilityStatus"]["liveStreamability"] =
            serde_json::json!({"liveStreamabilityRenderer":{}});
        assert!(public_player(&data, "k13FUGmRRHc").is_err());
        data["playabilityStatus"]["liveStreamability"] = serde_json::Value::Null;
        data["videoDetails"]["isLiveContent"] = true.into();
        assert!(
            public_player(&data, "k13FUGmRRHc").is_ok(),
            "archived live recordings remain playable"
        );
        data["streamingData"]["adaptiveFormats"][0]["drmFamilies"] =
            serde_json::json!(["WIDEVINE"]);
        assert!(public_player(&data, "k13FUGmRRHc").is_err());
        data["playabilityStatus"]["status"] = "LOGIN_REQUIRED".into();
        assert!(public_player(&data, "k13FUGmRRHc").is_err());
    }

    #[tokio::test]
    async fn transient_resolution_failure_recovers() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let resolver = Resolver::new(Arc::new(move |_| {
            let attempt = count.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt == 0 {
                    Err("YouTube audio resolution could not connect.".into())
                } else {
                    Ok(resolved(Duration::from_secs(300)))
                }
            }
            .boxed()
        }));
        assert!(resolver.resolve_recovering("M7lc1UVf-VE").await.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn resolution_network_retries_stop_after_three_attempts() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let resolver = Resolver::new(Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            async { Err("YouTube audio resolution was interrupted.".into()) }.boxed()
        }));
        assert!(resolver.resolve_recovering("M7lc1UVf-VE").await.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn unavailable_recording_is_not_retried() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let resolver = Resolver::new(Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            async { Err("YouTube could not provide public audio for this recording.".into()) }
                .boxed()
        }));
        assert!(resolver.resolve_recovering("M7lc1UVf-VE").await.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn limits_network_work_and_cached_entries() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let fetch: Fetch = {
            let active = active.clone();
            let peak = peak.clone();
            Arc::new(move |_| {
                let active = active.clone();
                let peak = peak.clone();
                async move {
                    let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(resolved(Duration::from_secs(300)))
                }
                .boxed() as BoxFuture<'static, Result<Resolved, String>>
            })
        };
        let resolver = Resolver::new(fetch);
        futures_util::future::join_all((0..8).map(|i| {
            let id = format!("test{i:07}");
            let resolver = &resolver;
            async move {
                resolver.resolve(&id).await.unwrap();
            }
        }))
        .await;
        assert!(peak.load(Ordering::SeqCst) <= 2);
        for i in 0..40 {
            resolver.resolve(&format!("test{i:07}")).await.unwrap();
        }
        assert!(resolver.cache.lock().unwrap().len() <= CACHE_LIMIT);
    }

    #[tokio::test]
    async fn selecting_another_track_cancels_stale_network_work() {
        let started = Arc::new(tokio::sync::Notify::new());
        let fetch: Fetch = {
            let started = started.clone();
            Arc::new(move |id| {
                let started = started.clone();
                async move {
                    if id == "M7lc1UVf-VE" {
                        started.notify_one();
                        std::future::pending::<()>().await;
                    }
                    Ok(resolved(Duration::from_secs(300)))
                }
                .boxed()
            })
        };
        let resolver = Arc::new(Resolver::new(fetch));
        let first = {
            let resolver = resolver.clone();
            tokio::spawn(async move { resolver.resolve("M7lc1UVf-VE").await })
        };
        started.notified().await;
        resolver.prioritize("aqz-KE-bpKQ");
        assert!(tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(resolver.resolve("aqz-KE-bpKQ").await.is_ok());
    }
}
