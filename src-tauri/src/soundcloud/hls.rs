//! Bounded, unencrypted VOD MP3/fMP4 HLS adapter for the native byte stream.
use crate::stream::{Cancel, Shared};
use std::{future::Future, io::Write, path::Path, sync::OnceLock, time::Duration};

const MAX_MANIFEST: usize = 1024 * 1024;
const MAX_SEGMENTS: usize = 4096;
const MAX_SEGMENT: u64 = 16 * 1024 * 1024;
const MAX_AUDIO: u64 = 512 * 1024 * 1024;

/// SoundCloud CDN manifests use /playlist/ URLs, sometimes without .m3u8.
pub fn is_manifest_url(raw: &str) -> bool {
    super::validate_audio_url(raw).is_ok_and(|url| {
        url.path().ends_with(".m3u8")
            || url.path().contains("/playlist/")
            || url.host_str().is_some_and(|host| host.contains("-hls-"))
    })
}

fn parse_manifest(base: &str, raw: &str) -> Result<Vec<url::Url>, String> {
    let base = super::validate_audio_url(base)?;
    if raw.len() > MAX_MANIFEST {
        return Err("SoundCloud HLS manifest is too large.".into());
    }
    let mut lines = raw.trim_start_matches('\u{feff}').lines().map(str::trim);
    if lines.next() != Some("#EXTM3U") {
        return Err("SoundCloud returned an invalid HLS manifest.".into());
    }
    let mut segments = Vec::new();
    let mut duration = false;
    let mut ended = false;
    let mut has_init = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line == "#EXT-X-ENDLIST" {
            ended = true;
            continue;
        }
        if ended {
            return Err("Unexpected data after SoundCloud HLS end marker.".into());
        }
        if let Some(key) = line.strip_prefix("#EXT-X-KEY:") {
            if key != "METHOD=NONE" {
                return Err("Encrypted SoundCloud HLS audio is unsupported.".into());
            }
        } else if let Some(map) = line.strip_prefix("#EXT-X-MAP:") {
            if has_init || !segments.is_empty() || map.contains("BYTERANGE") {
                return Err("Unsupported SoundCloud HLS initialization layout.".into());
            }
            let uri = map
                .strip_prefix("URI=\"")
                .and_then(|v| v.strip_suffix('"'))
                .ok_or("Invalid SoundCloud HLS initialization URL.")?;
            let init = base
                .join(uri)
                .map_err(|_| "Invalid SoundCloud HLS initialization URL.")?;
            super::validate_audio_url(init.as_str())?;
            segments.push(init);
            has_init = true;
        } else if line.starts_with("#EXT-X-STREAM-INF:")
            || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
            || line.starts_with("#EXT-X-SESSION-KEY:")
            || line.starts_with("#EXT-X-BYTERANGE:")
            || line.starts_with("#EXT-X-GAP")
        {
            return Err("Unsupported SoundCloud HLS stream layout.".into());
        } else if let Some(info) = line.strip_prefix("#EXTINF:") {
            let seconds = info
                .split(',')
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0 && *v <= 600.0);
            if seconds.is_none() || duration {
                return Err("Invalid SoundCloud HLS segment duration.".into());
            }
            duration = true;
        } else if !line.starts_with('#') {
            if !duration || segments.len() >= MAX_SEGMENTS {
                return Err("SoundCloud HLS segment list is invalid or too large.".into());
            }
            let url = base
                .join(line)
                .map_err(|_| "Invalid SoundCloud HLS segment URL.")?;
            super::validate_audio_url(url.as_str())?;
            if url.path().ends_with(".m3u8")
                || url.path().contains("/playlist/")
                || [".ts", ".aac", ".mp4"]
                    .iter()
                    .any(|extension| url.path().ends_with(extension))
            {
                return Err("Only native MP3 or fragmented MP4 HLS segments are supported.".into());
            }
            if url.path().ends_with(".m4s") && !has_init {
                return Err("SoundCloud fragmented MP4 audio is missing initialization.".into());
            }
            segments.push(url);
            duration = false;
        }
    }
    if !ended || duration || segments.len() <= usize::from(has_init) {
        return Err("SoundCloud HLS must be a complete VOD playlist.".into());
    }
    Ok(segments)
}

fn http_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .https_only(true)
                .connect_timeout(Duration::from_secs(8))
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() < 3
                        && super::validate_audio_url(attempt.url().as_str()).is_ok()
                    {
                        attempt.follow()
                    } else {
                        attempt.stop()
                    }
                }))
                .build()
                .map_err(|_| "Could not initialize SoundCloud HLS transport.".into())
        })
        .as_ref()
        .map_err(Clone::clone)
}

async fn cancellable<T>(
    cancel: &Cancel,
    request: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let cancellation = async {
        loop {
            if cancel.stopped() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::select! {
        biased;
        _ = cancellation => Err("SoundCloud playback superseded.".into()),
        result = request => result,
    }
}

async fn response(
    client: &reqwest::Client,
    url: &url::Url,
    cancel: &Cancel,
) -> Result<reqwest::Response, String> {
    cancellable(cancel, async {
        let response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|_| "Could not reach SoundCloud audio CDN.")?;
        if !response.status().is_success() {
            return Err(format!(
                "SoundCloud audio request failed (HTTP {}).",
                response.status().as_u16()
            ));
        }
        super::validate_audio_url(response.url().as_str())?;
        Ok(response)
    })
    .await
}

/// Append MP3 or init+fMP4 segments to the already-created cache file. The caller owns the
/// completion marker and error publication, exactly as for progressive transfer.
/// Never resume this with a byte offset: HLS readers wait for sequential data.
pub async fn download(
    raw: &str,
    path: &Path,
    shared: &Shared,
    cancel: &Cancel,
) -> Result<(), String> {
    if cancel.stopped() {
        return Err("SoundCloud playback superseded.".into());
    }
    let url = super::validate_audio_url(raw)?;
    if !is_manifest_url(raw) {
        return Err("Expected a SoundCloud HLS manifest URL.".into());
    }
    let client = http_client()?;
    let mut manifest = response(client, &url, cancel).await?;
    if manifest
        .content_length()
        .is_some_and(|size| size > MAX_MANIFEST as u64)
    {
        return Err("SoundCloud HLS manifest is too large.".into());
    }
    let base = manifest.url().as_str().to_owned();
    let mut bytes = Vec::new();
    while let Some(chunk) = cancellable(cancel, async {
        manifest
            .chunk()
            .await
            .map_err(|_| "SoundCloud HLS manifest transfer failed.".into())
    })
    .await?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_MANIFEST {
            return Err("SoundCloud HLS manifest is too large.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let source =
        std::str::from_utf8(&bytes).map_err(|_| "SoundCloud HLS manifest is invalid text.")?;
    let segments = parse_manifest(&base, source)?;
    let mut file = {
        let _guard = cancel.write_guard();
        if cancel.stopped() {
            return Err("SoundCloud playback superseded.".into());
        }
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|_| "Could not open SoundCloud audio buffer.")?
    };
    let mut written = 0u64;
    for url in segments {
        let mut segment = response(client, &url, cancel).await?;
        let expected = segment.content_length();
        if expected.is_some_and(|size| size > MAX_SEGMENT || size == 0) {
            return Err("Invalid SoundCloud HLS segment size.".into());
        }
        let mut received = 0u64;
        while let Some(chunk) = cancellable(cancel, async {
            segment
                .chunk()
                .await
                .map_err(|_| "SoundCloud HLS segment transfer failed.".into())
        })
        .await?
        {
            received = received.saturating_add(chunk.len() as u64);
            written = written.saturating_add(chunk.len() as u64);
            if received > MAX_SEGMENT
                || written > MAX_AUDIO
                || expected.is_some_and(|size| received > size)
            {
                return Err("SoundCloud HLS audio exceeds its transfer limit.".into());
            }
            let _guard = cancel.write_guard();
            if cancel.stopped() {
                return Err("SoundCloud playback superseded.".into());
            }
            file.write_all(&chunk)
                .map_err(|_| "Could not write SoundCloud audio buffer.")?;
            shared
                .0
                .lock()
                .expect("cache state mutex")
                .note_written(chunk.len() as u64);
            shared.1.notify_all();
        }
        if received == 0 || expected.is_some_and(|size| received != size) {
            return Err("SoundCloud HLS segment ended early.".into());
        }
    }
    let _guard = cancel.write_guard();
    if cancel.stopped() {
        return Err("SoundCloud playback superseded.".into());
    }
    shared
        .0
        .lock()
        .expect("cache state mutex")
        .set_total(Some(written));
    shared.1.notify_all();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_an_inflight_request_interrupts_the_wait() {
        let cancel = Cancel::default();
        let stopping = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            stopping.stop();
        });
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            cancellable(&cancel, std::future::pending::<Result<(), String>>()),
        )
        .await;
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn manifest_detection_does_not_treat_progressive_media_as_hls() {
        assert!(is_manifest_url(
            "https://cf-hls-media.sndcdn.com/playlist/a"
        ));
        assert!(is_manifest_url("https://cf-media.sndcdn.com/playlist/a"));
        assert!(is_manifest_url("https://cf-media.sndcdn.com/a.m3u8"));
        assert!(!is_manifest_url("https://cf-media.sndcdn.com/a.mp3"));
        assert!(!is_manifest_url("https://evil.test/a.m3u8"));
    }

    #[test]
    fn parses_full_vod_mp3_playlist_in_order_with_relative_segments() {
        let list = parse_manifest("https://cf-hls-media.sndcdn.com/tracks/playlist.m3u8?x=1", "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\n../one.mp3?token=a\n#EXTINF:2.5,\nhttps://cf-hls-media.sndcdn.com/two.mp3\n#EXT-X-ENDLIST\n").unwrap();
        assert_eq!(
            list[0].as_str(),
            "https://cf-hls-media.sndcdn.com/one.mp3?token=a"
        );
        assert_eq!(list[1].as_str(), "https://cf-hls-media.sndcdn.com/two.mp3");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn prepends_aac_fmp4_initialization_before_media_segments() {
        let items = parse_manifest("https://cf-hls-media.sndcdn.com/playlist/audio.m3u8", "#EXTM3U\n#EXT-X-MAP:URI=\"https://cf-hls-media.sndcdn.com/media/init.mp4\"\n#EXTINF:10.0078,\nhttps://cf-hls-media.sndcdn.com/media/first.m4s\n#EXTINF:5,\nhttps://cf-hls-media.sndcdn.com/media/second.m4s\n#EXT-X-ENDLIST").unwrap();
        assert_eq!(items.len(), 3);
        assert!(items[0].path().ends_with("init.mp4"));
        assert!(items[1].path().ends_with("first.m4s"));
        assert!(items[2].path().ends_with("second.m4s"));
    }

    #[test]
    fn refuses_encryption_live_master_and_untrusted_segments() {
        let base = "https://cf-hls-media.sndcdn.com/a.m3u8";
        for invalid in [
            "#EXTM3U\n#EXTINF:1,\none.mp3\n",
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:1,\none.mp3\n#EXT-X-ENDLIST",
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100000\nchild.m3u8\n#EXT-X-ENDLIST",
            "#EXTM3U\n#EXTINF:1,\nhttps://evil.test/file.mp3\n#EXT-X-ENDLIST",
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\none.mp4\n#EXT-X-ENDLIST",
            "#EXTM3U\n#EXT-X-BYTERANGE:100@0\n#EXTINF:1,\none.mp3\n#EXT-X-ENDLIST",
            "not a playlist",
        ] {
            assert!(parse_manifest(base, invalid).is_err(), "{invalid}");
        }
        assert!(parse_manifest(
            base,
            "#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:1,\none.mp3\n#EXT-X-ENDLIST"
        )
        .is_ok());
    }

    #[test]
    fn caps_manifest_segments_and_rejects_missing_duration_or_post_end_data() {
        let base = "https://cf-hls-media.sndcdn.com/a.m3u8";
        let oversized = format!(
            "#EXTM3U\n{}#EXT-X-ENDLIST",
            "#EXTINF:1,\na.mp3\n".repeat(4097)
        );
        assert!(parse_manifest(base, &oversized).is_err());
        for invalid in [
            "#EXTM3U\na.mp3\n#EXT-X-ENDLIST",
            "#EXTM3U\n#EXT-X-ENDLIST\n#EXTINF:1,\na.mp3",
            "#EXTM3U\n#EXTINF:-1,\na.mp3\n#EXT-X-ENDLIST",
        ] {
            assert!(parse_manifest(base, invalid).is_err());
        }
    }
}
