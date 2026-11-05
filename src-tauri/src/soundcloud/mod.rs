//! Anonymous SoundCloud catalog and progressive audio resolution.
pub mod commands;
pub mod hls;
pub use commands::State;
// Public v2 endpoints are the same anonymous web surface used by the importer;
// they are not the authenticated, registered-app SoundCloud API.
#[cfg(test)]
mod live_tests;
use crate::import::{
    http::{Cancellation, JsonFuture, JsonTransport},
    model::Platform,
    public_http, public_soundcloud,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub(crate) const FULL_PLAYBACK_DENIED: &str =
    "SoundCloud does not allow full anonymous playback for this track.";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Tracks,
    Artists,
    Albums,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub duration_ms: u64,
    pub artwork_url: Option<String>,
    pub permalink_url: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub artwork_url: Option<String>,
    pub permalink_url: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub artwork_url: Option<String>,
    pub permalink_url: Option<String>,
    pub track_count: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playable {
    #[serde(skip)]
    pub audio_info: crate::audio::AudioInfo,
    pub url: String,
    pub mime_type: String,
    pub quality: String,
    pub track: Track,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumTracks {
    pub album: Album,
    pub tracks: Vec<Track>,
    pub unavailable_count: usize,
    pub truncated: bool,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}
fn id(value: &Value, kind: &str) -> Option<String> {
    if let Some(urn) = value["urn"].as_str() {
        return numeric(urn.strip_prefix(&format!("soundcloud:{kind}:"))?).ok();
    }
    let raw = value["id"]
        .as_u64()
        .map(|n| n.to_string())
        .or_else(|| text(value, "id"))?;
    numeric(&raw).ok()
}
fn numeric(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 20 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid SoundCloud item ID.".into());
    }
    Ok(raw.into())
}
fn parse_track(value: &Value) -> Option<Track> {
    if value["sharing"] == "private" || value["kind"].as_str().is_some_and(|k| k != "track") {
        return None;
    }
    Some(Track {
        id: id(value, "tracks")?,
        title: text(value, "title")?,
        artist: text(&value["publisher_metadata"], "artist")
            .or_else(|| text(&value["user"], "username"))
            .unwrap_or_default(),
        artist_id: id(&value["user"], "users"),
        album: text(&value["publisher_metadata"], "album_title"),
        duration_ms: value["duration"].as_u64().unwrap_or(0),
        artwork_url: text(value, "artwork_url").or_else(|| text(&value["user"], "avatar_url")),
        permalink_url: text(value, "permalink_url"),
    })
}
fn parse_artist(value: &Value) -> Option<Artist> {
    if value["kind"] != "user" {
        return None;
    }
    Some(Artist {
        id: id(value, "users")?,
        name: text(value, "username")?,
        artwork_url: text(value, "avatar_url"),
        permalink_url: text(value, "permalink_url"),
    })
}
fn parse_album(value: &Value) -> Option<Album> {
    if value["kind"] != "playlist" || value["sharing"] == "private" || value["is_album"] != true {
        return None;
    }
    Some(Album {
        id: id(value, "playlists")?,
        title: text(value, "title")?,
        artist: text(&value["user"], "username").unwrap_or_default(),
        artist_id: id(&value["user"], "users"),
        artwork_url: text(value, "artwork_url"),
        permalink_url: text(value, "permalink_url"),
        track_count: value["track_count"].as_u64().unwrap_or(0),
    })
}
fn parse_search(value: &Value, kind: SearchKind) -> SearchResults {
    let mut out = SearchResults::default();
    for item in value["collection"].as_array().into_iter().flatten() {
        match kind {
            SearchKind::Tracks => out.tracks.extend(parse_track(item)),
            SearchKind::Artists => out.artists.extend(parse_artist(item)),
            SearchKind::Albums => out.albums.extend(parse_album(item)),
        }
    }
    out
}
fn api_url(path: &str, client: &str) -> Result<url::Url, String> {
    let mut url = url::Url::parse("https://api-v2.soundcloud.com/")
        .unwrap()
        .join(path)
        .map_err(|_| "Invalid SoundCloud API URL.")?;
    if url.scheme() != "https"
        || url.host_str() != Some("api-v2.soundcloud.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Unsafe SoundCloud API URL.".into());
    }
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(k, _)| k != "client_id")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair("client_id", client);
    Ok(url)
}
/// Validate the returned CDN location again before passing it to the audio engine.
pub fn validate_audio_url(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|_| "Invalid SoundCloud audio URL.")?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https"
        || !(host.ends_with(".sndcdn.com") || host == "playback.media-streaming.soundcloud.cloud")
        || host.contains("preview")
        || url.path().contains("/preview/")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("SoundCloud did not return an allowed full audio stream.".into());
    }
    Ok(url)
}
fn public_playable(value: &Value) -> bool {
    value["sharing"] != "private"
        && value["streamable"] != false
        && value["policy"]
            .as_str()
            .is_none_or(|p| p == "ALLOW" || p == "MONETIZE")
}
struct Candidate {
    audio_info: crate::audio::AudioInfo,
    url: String,
    mime_type: String,
    quality: String,
}
fn source_audio_info(value: &Value) -> crate::audio::AudioInfo {
    let mime = value["format"]["mime_type"].as_str().unwrap_or("");
    let aac = mime.starts_with("audio/mp4") || mime.starts_with("audio/aac");
    let mp3 = mime.starts_with("audio/mpeg") || mime.starts_with("audio/mp3");
    let bitrate = value["preset"]
        .as_str()
        .and_then(|preset| {
            preset
                .split('_')
                .find_map(|part| part.strip_suffix('k')?.parse::<u32>().ok())
        })
        .filter(|n| (1..=10_000).contains(n));
    crate::audio::AudioInfo {
        codec: if aac {
            Some("AAC".into())
        } else if mp3 {
            Some("MP3".into())
        } else {
            None
        },
        container: if mime.starts_with("audio/mp4") {
            Some("MP4/M4A".into())
        } else if mp3 {
            Some("MPEG audio".into())
        } else if aac {
            Some("ADTS".into())
        } else {
            None
        },
        bitrate_kbps: bitrate,
        ..Default::default()
    }
}

fn progressive_candidates(value: &Value) -> Vec<Candidate> {
    let mut out: Vec<_> = value["media"]["transcodings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let mime = text(&v["format"], "mime_type")?;
            if v["snipped"] == true
                || v["format"]["protocol"] != "progressive"
                || !matches!(
                    mime.split(';').next()?.trim(),
                    "audio/mpeg" | "audio/mp3" | "audio/aac" | "audio/mp4" | "audio/x-m4a"
                )
            {
                return None;
            }
            let endpoint = text(v, "url")?;
            api_url(&endpoint, "validation").ok()?;
            Some(Candidate {
                audio_info: source_audio_info(v),
                url: endpoint,
                mime_type: mime,
                quality: text(v, "quality").unwrap_or_else(|| "sq".into()),
            })
        })
        .collect();
    out.sort_by_key(|v| {
        (
            v.quality != "hq",
            !v.mime_type.contains("aac") && !v.mime_type.contains("mp4"),
        )
    });
    out
}

fn native_candidates(value: &Value) -> Vec<Candidate> {
    let mut out = progressive_candidates(value);
    for v in value["media"]["transcodings"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if v["snipped"] == true
            || v["format"]["protocol"] != "hls"
            || !v["format"]["mime_type"].as_str().is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|v| matches!(v.trim(), "audio/mpeg" | "audio/mp3" | "audio/mp4"))
            })
        {
            continue;
        }
        if let Some(endpoint) = text(v, "url").filter(|v| api_url(v, "validation").is_ok()) {
            out.push(Candidate {
                audio_info: source_audio_info(v),
                url: endpoint,
                mime_type: "audio/mpegurl".into(),
                quality: text(v, "quality").unwrap_or_else(|| "sq".into()),
            });
        }
    }
    out.sort_by_key(|v| {
        (
            match v.quality.as_str() {
                "hq" => 0,
                "sq" => 1,
                _ => 2,
            },
            v.mime_type == "audio/mpegurl",
        )
    });
    out
}

#[derive(Default)]
struct Cache {
    entries: HashMap<String, (Instant, Value)>,
}
impl Cache {
    fn get(&mut self, key: &str, now: Instant) -> Option<Value> {
        let (when, value) = self.entries.get(key)?;
        if now.saturating_duration_since(*when) < Duration::from_secs(90) {
            return Some(value.clone());
        }
        self.entries.remove(key);
        None
    }
    fn put(&mut self, key: String, value: Value, now: Instant) {
        // A provider response can be 16MiB; don't multiply that transport cap
        // by 128 entries. Large albums still return normally without caching.
        if value.to_string().len() > 128 * 1024 {
            return;
        }
        self.entries
            .retain(|_, (when, _)| now.saturating_duration_since(*when) < Duration::from_secs(90));
        if self.entries.len() >= 128 {
            if let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, (when, _))| *when)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&key);
            }
        }
        self.entries.insert(key, (now, value));
    }
}
struct PublicApi;
impl JsonTransport for PublicApi {
    fn get<'a>(
        &'a self,
        _: Platform,
        path: &'a str,
        _: Option<&'a str>,
        cancel: &'a Cancellation,
    ) -> JsonFuture<'a> {
        Box::pin(public_http::get_json(
            Platform::Soundcloud,
            path,
            None,
            cancel,
        ))
    }
}

/// Reuse one client per application. Catalog cache is bounded to 128 entries / 90s.
/// Audio locations are always resolved fresh. No credentials or browser state.
pub struct Client {
    http: Box<dyn JsonTransport>,
    client_id: Mutex<Option<(Instant, String)>>,
    cache: Mutex<Cache>,
}
impl Default for Client {
    fn default() -> Self {
        Self {
            http: Box::new(PublicApi),
            client_id: Mutex::new(None),
            cache: Mutex::new(Cache::default()),
        }
    }
}
impl Client {
    async fn anonymous_client(&self, cancel: &Cancellation) -> Result<String, String> {
        cancel.check()?;
        // Serialize discovery, which otherwise fetches the same application JS
        // repeatedly when several UI panels search at once.
        let mut stored = tokio::select! {
            _ = cancel.cancelled() => return Err("SoundCloud request cancelled.".into()),
            guard = self.client_id.lock() => guard,
        };
        if let Some((when, id)) = stored
            .as_ref()
            .filter(|(when, _)| when.elapsed() < Duration::from_secs(1800))
        {
            let _ = when;
            return Ok(id.clone());
        }
        let html = public_http::get_text(
            Platform::Soundcloud,
            "https://soundcloud.com/",
            None,
            cancel,
        )
        .await?;
        let hydration = public_soundcloud::hydration(&html);
        let embedded = hydration
            .as_ref()
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|v| v["hydratable"] == "apiClient")
            .and_then(|v| v["data"]["id"].as_str())
            .filter(|v| v.len() == 32 && v.bytes().all(|b| b.is_ascii_alphanumeric()))
            .map(str::to_owned);
        let mut found = embedded.or_else(|| public_soundcloud::client_id(&html));
        if found.is_none() {
            for script in public_soundcloud::scripts(&html).into_iter().take(5) {
                cancel.check()?;
                if let Ok(source) =
                    public_http::get_text(Platform::Soundcloud, &script, None, cancel).await
                {
                    found = public_soundcloud::client_id(&source);
                    if found.is_some() {
                        break;
                    }
                }
            }
        }
        cancel.check()?;
        let id = found
            .ok_or("SoundCloud did not expose an anonymous public client. Try again later.")?;
        *stored = Some((Instant::now(), id.clone()));
        Ok(id)
    }

    async fn get(&self, path: &str, cached: bool, cancel: &Cancellation) -> Result<Value, String> {
        cancel.check()?;
        if cached {
            if let Some(value) = self.cache.lock().await.get(path, Instant::now()) {
                return Ok(value);
            }
        }
        for attempt in 0..2 {
            let id = self.anonymous_client(cancel).await?;
            let url = api_url(path, &id)?;
            match self
                .http
                .get(Platform::Soundcloud, url.as_str(), None, cancel)
                .await
            {
                Ok(value) => {
                    if cached {
                        self.cache
                            .lock()
                            .await
                            .put(path.into(), value.clone(), Instant::now());
                    }
                    return Ok(value);
                }
                Err(error)
                    if attempt == 0
                        && (error.contains("HTTP 401") || error.contains("HTTP 403")) =>
                {
                    let mut stored = self.client_id.lock().await;
                    if stored.as_ref().is_some_and(|(_, current)| current == &id) {
                        *stored = None;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err("SoundCloud public access is unavailable.".into())
    }

    /// Each call fetches one bounded page (max 50 results, offset max 10,000).
    /// Albums are public playlists explicitly marked `is_album` by SoundCloud.
    pub async fn search(
        &self,
        kind: SearchKind,
        query: &str,
        limit: usize,
        offset: usize,
        cancel: &Cancellation,
    ) -> Result<SearchResults, String> {
        cancel.check()?;
        let query = query.trim();
        if query.is_empty() {
            return Ok(SearchResults::default());
        }
        if query.len() > 512 {
            return Err("SoundCloud search query is too long.".into());
        }
        let endpoint = match kind {
            SearchKind::Tracks => "search/tracks",
            SearchKind::Artists => "search/users",
            SearchKind::Albums => "search/albums",
        };
        let path = format!(
            "{endpoint}?q={}&limit={}&offset={}",
            urlencoding::encode(query),
            limit.clamp(1, 50),
            offset.min(10_000)
        );
        Ok(parse_search(&self.get(&path, true, cancel).await?, kind))
    }

    /// Public profile's track page, ordered by the provider.
    pub async fn artist_tracks(
        &self,
        artist_id: &str,
        limit: usize,
        offset: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<Track>, String> {
        let id = numeric(artist_id)?;
        let path = format!(
            "users/{id}/tracks?limit={}&offset={}",
            limit.clamp(1, 50),
            offset.min(10_000)
        );
        Ok(parse_search(&self.get(&path, true, cancel).await?, SearchKind::Tracks).tracks)
    }

    async fn track_value(
        &self,
        target: &str,
        cached: bool,
        cancel: &Cancellation,
    ) -> Result<Value, String> {
        cancel.check()?;
        let target = target.strip_prefix("soundcloud:tracks:").unwrap_or(target);
        let path = if let Ok(id) = numeric(target) {
            format!("tracks/{id}")
        } else {
            let detected = crate::import::urls::detect(target)
                .map_err(|_| "Expected a public SoundCloud track URL or ID.")?;
            if detected.provider != Platform::Soundcloud
                || detected.kind != crate::import::urls::ContentKind::Track
            {
                return Err("Expected a public SoundCloud track URL or ID.".into());
            }
            format!("resolve?url={}", urlencoding::encode(&detected.url))
        };
        self.get(&path, cached, cancel).await
    }

    pub async fn resolve_track(
        &self,
        target: &str,
        cancel: &Cancellation,
    ) -> Result<Track, String> {
        parse_track(&self.track_value(target, true, cancel).await?)
            .ok_or_else(|| "SoundCloud track is private or unavailable.".into())
    }

    /// Full native audio, including unencrypted MP3 or AAC/fMP4 VOD HLS through
    /// our segment adapter. Encrypted and adaptive master playlists are excluded.
    pub async fn playable(&self, target: &str, cancel: &Cancellation) -> Result<Playable, String> {
        let value = self.track_value(target, false, cancel).await?;
        let track = parse_track(&value).ok_or("SoundCloud track is private or unavailable.")?;
        if !public_playable(&value) {
            return Err(FULL_PLAYBACK_DENIED.into());
        }
        let candidates = native_candidates(&value);
        if candidates.is_empty() {
            return Err("This SoundCloud track has no full native MP3/AAC stream. Encrypted or adaptive-only HLS streams are unsupported.".into());
        }
        for candidate in candidates.into_iter().take(4) {
            cancel.check()?;
            let mut endpoint = api_url(&candidate.url, "placeholder")?;
            // Track authorization is public per-track metadata, not an account token.
            if let Some(authorization) = value["track_authorization"].as_str() {
                endpoint
                    .query_pairs_mut()
                    .append_pair("track_authorization", authorization);
            }
            match self.get(endpoint.as_str(), false, cancel).await {
                Ok(resolved) => {
                    if let Some(url) = resolved["url"]
                        .as_str()
                        .filter(|u| validate_audio_url(u).is_ok())
                    {
                        if candidate.mime_type == "audio/mpegurl" && !hls::is_manifest_url(url) {
                            continue;
                        }
                        return Ok(Playable {
                            audio_info: candidate.audio_info,
                            url: url.into(),
                            mime_type: candidate.mime_type,
                            quality: candidate.quality,
                            track,
                        });
                    }
                }
                Err(_) => cancel.check()?,
            }
        }
        Err("SoundCloud did not expose a playable full audio stream.".into())
    }

    /// Preserve album order and duplicate entries. Hydrate ID-only entries in
    /// batches of 50 (max 500 entries) instead of one request per song.
    pub async fn album_tracks(
        &self,
        album_id: &str,
        cancel: &Cancellation,
    ) -> Result<AlbumTracks, String> {
        let id = numeric(album_id)?;
        let value = self.get(&format!("playlists/{id}"), true, cancel).await?;
        let album = parse_album(&value).ok_or("SoundCloud item is not a public album.")?;
        let entries = value["tracks"]
            .as_array()
            .ok_or("SoundCloud did not expose album tracks.")?;
        let mut mapped = HashMap::new();
        let mut missing = Vec::new();
        for entry in entries.iter().take(500) {
            if let Some(track) = parse_track(entry) {
                mapped.insert(track.id.clone(), track);
            } else if let Some(id) = id_of_track(entry) {
                if !missing.contains(&id) {
                    missing.push(id);
                }
            }
        }
        for chunk in missing.chunks(50) {
            let path = format!("tracks?ids={}", chunk.join(","));
            let response = self.get(&path, true, cancel).await?;
            for entry in response.as_array().into_iter().flatten() {
                if let Some(track) = parse_track(entry) {
                    mapped.insert(track.id.clone(), track);
                }
            }
        }
        let tracks: Vec<_> = entries
            .iter()
            .take(500)
            .filter_map(|entry| id_of_track(entry).and_then(|id| mapped.get(&id).cloned()))
            .collect();
        let unavailable_count = entries.len().min(500).saturating_sub(tracks.len());
        let truncated = album.track_count > entries.len().min(500) as u64 || entries.len() > 500;
        Ok(AlbumTracks {
            album,
            tracks,
            unavailable_count,
            truncated,
        })
    }
}
fn id_of_track(value: &Value) -> Option<String> {
    id(value, "tracks")
}

#[cfg(test)]
mod tests;
