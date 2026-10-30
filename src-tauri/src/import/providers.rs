//! Public-link metadata adapters, with optional authorized API fallbacks.
pub use super::http::Cancellation;
use super::{
    http::{next_page, ApiClient, JsonTransport},
    model::*,
    urls::{ContentKind, ImportUrl},
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct Credentials {
    pub spotify_token: Option<String>,
    pub apple_token: Option<String>,
    pub youtube_key: Option<String>,
    pub soundcloud_token: Option<String>,
}

pub async fn fetch(
    target: &ImportUrl,
    credentials: &Credentials,
    cancel: &Cancellation,
) -> Result<ImportedCollection, String> {
    if matches!(
        target.provider,
        Platform::Spotify | Platform::AppleMusic | Platform::Soundcloud
    ) {
        let public = match target.provider {
            Platform::Spotify => super::public_spotify::fetch(target, cancel).await,
            Platform::AppleMusic => super::public_apple::fetch(target, cancel).await,
            Platform::Soundcloud => super::public_soundcloud::fetch(target, cancel).await,
            _ => unreachable!(),
        };
        cancel.check()?;
        let authorized = match target.provider {
            Platform::Spotify => &credentials.spotify_token,
            Platform::AppleMusic => &credentials.apple_token,
            Platform::Soundcloud => &credentials.soundcloud_token,
            _ => unreachable!(),
        }
        .as_deref()
        .is_some_and(|secret| !secret.trim().is_empty());
        return match public {
            Ok(collection) => Ok(collection),
            Err(_) if authorized => {
                fetch_with(ApiClient::shared()?, target, credentials, cancel).await
            }
            Err(error) => Err(error),
        };
    }
    if target.provider == Platform::Youtube
        && credentials
            .youtube_key
            .as_deref()
            .is_none_or(|key| key.trim().is_empty())
    {
        return super::public_youtube::fetch(target, cancel).await;
    }
    let result = fetch_with(ApiClient::shared()?, target, credentials, cancel).await;
    if target.provider == Platform::Youtube
        && matches!(target.kind, ContentKind::Track | ContentKind::Playlist)
    {
        public_fallback(result, cancel, super::public_youtube::fetch(target, cancel)).await
    } else {
        result
    }
}

async fn public_fallback<T>(
    result: Result<T, String>,
    cancel: &Cancellation,
    fallback: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    cancel.check()?;
    match result {
        Ok(value) => Ok(value),
        Err(_) => fallback.await,
    }
}

const PAGE_CAP: usize = 200;
const TRACK_CAP: usize = 10_000;

/// Authorized access is an optional fallback, never a prerequisite for public URLs.
pub(crate) async fn fetch_authorized(
    target: &ImportUrl,
    credentials: &Credentials,
    cancel: &Cancellation,
) -> Result<ImportedCollection, String> {
    fetch_with(ApiClient::shared()?, target, credentials, cancel).await
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}
fn id(value: &Value) -> Option<String> {
    value.get("id").and_then(|id| {
        id.as_str()
            .map(str::to_owned)
            .or_else(|| id.as_u64().map(|n| n.to_string()))
    })
}
fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn artwork(value: &Value) -> Option<String> {
    array(value, "images").iter().find_map(|v| text(v, "url"))
}
fn credential<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str, String> {
    value
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("Configure {name} credentials before importing."))
}
fn query(path: &str, values: &[(&str, &str)]) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(values.iter().copied())
        .finish();
    format!("{path}?{query}")
}
fn collection(target: &ImportUrl) -> ImportedCollection {
    ImportedCollection {
        source_url: target.url.clone(),
        provider: target.provider,
        kind: target.kind,
        title: "Imported collection".into(),
        artwork_url: None,
        tracks: Vec::new(),
        warnings: Vec::new(),
    }
}
fn incomplete(out: &mut ImportedCollection, error: &str) {
    out.warnings.push(format!("Import incomplete: {error}"));
}
fn full(out: &ImportedCollection) -> bool {
    out.tracks.len().saturating_add(out.warnings.len()) >= TRACK_CAP
}
fn add(out: &mut ImportedCollection, track: Option<ImportedTrack>) {
    if full(out) {
        return;
    }
    if let Some(track) = track {
        out.tracks.push(track);
    } else {
        out.warnings
            .push("A missing, private, or unsupported track was skipped.".into());
    }
}

async fn fetch_with(
    client: &impl JsonTransport,
    target: &ImportUrl,
    credentials: &Credentials,
    cancel: &Cancellation,
) -> Result<ImportedCollection, String> {
    cancel.check()?;
    // Validate even programmatically constructed targets before interpolating IDs.
    let validated = super::urls::detect(&target.url)?;
    if &validated != target {
        return Err("Invalid canonical import target.".into());
    }
    let mut out = collection(target);
    match target.provider {
        Platform::Spotify => spotify(client, target, credentials, cancel, &mut out).await?,
        Platform::AppleMusic => apple_music(client, target, credentials, cancel, &mut out).await?,
        Platform::Youtube => youtube(client, target, credentials, cancel, &mut out).await?,
        Platform::Soundcloud => soundcloud(client, target, credentials, cancel, &mut out).await?,
        _ => return Err("Unsupported metadata provider.".into()),
    }
    cancel.check()?;
    if full(&out) {
        incomplete(
            &mut out,
            "collection reached the 10,000 item limit (including unavailable entries).",
        );
    }
    Ok(out)
}

fn spotify_track(value: &Value, album: Option<&Value>) -> Option<ImportedTrack> {
    if value.get("is_local").and_then(Value::as_bool) == Some(true)
        || value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t != "track")
    {
        return None;
    }
    let external_id = id(value)?;
    let album = value.get("album").filter(|a| a.is_object()).or(album);
    Some(ImportedTrack {
        origin: Origin {
            platform: Platform::Spotify,
            url: Some(format!("https://open.spotify.com/track/{external_id}")),
            external_id,
        },
        metadata: Metadata {
            title: text(value, "name")?,
            artists: array(value, "artists")
                .iter()
                .filter_map(|a| text(a, "name"))
                .collect(),
            album: album.and_then(|a| text(a, "name")),
            duration_ms: value["duration_ms"].as_u64(),
            artwork_url: album.and_then(artwork),
            isrc: text(&value["external_ids"], "isrc"),
            explicit: value["explicit"].as_bool(),
        },
    })
}

async fn spotify(
    client: &impl JsonTransport,
    target: &ImportUrl,
    credentials: &Credentials,
    cancel: &Cancellation,
    out: &mut ImportedCollection,
) -> Result<(), String> {
    let resource = match target.kind { ContentKind::Track => "tracks", ContentKind::Album => "albums", ContentKind::Playlist => "playlists", _ => return Err("Spotify artist and profile imports are unsupported. Paste a track, album, or playlist link.".into()) };
    let auth = format!(
        "Bearer {}",
        credential(&credentials.spotify_token, "Spotify OAuth access token")?
    );
    let base = format!("/v1/{resource}/{}", target.id);
    let root = client
        .get(Platform::Spotify, &base, Some(&auth), cancel)
        .await?;
    out.title = text(&root, "name").unwrap_or_else(|| out.title.clone());
    out.artwork_url = artwork(&root);
    if target.kind == ContentKind::Track {
        add(out, spotify_track(&root, None));
        return Ok(());
    }
    let (key, mut page) = if root["items"].is_object() {
        ("items", root["items"].clone())
    } else {
        ("tracks", root["tracks"].clone())
    };
    let mut path = format!("{base}/{key}");
    if !page.is_object() {
        path = format!(
            "{base}/{}",
            if target.kind == ContentKind::Playlist {
                "items"
            } else {
                "tracks"
            }
        );
        page = match client
            .get(Platform::Spotify, &path, Some(&auth), cancel)
            .await
        {
            Ok(page) => page,
            Err(e) if target.kind == ContentKind::Playlist && e.contains("missing or private") => {
                path = format!("{base}/tracks");
                client
                    .get(Platform::Spotify, &path, Some(&auth), cancel)
                    .await?
            }
            Err(e) => return Err(e),
        };
    }
    let mut seen = HashSet::from([super::http::api_url(Platform::Spotify, &path)?.to_string()]);
    for n in 0..PAGE_CAP {
        cancel.check()?;
        for item in array(&page, "items") {
            let value = if target.kind == ContentKind::Playlist {
                item.get("item")
                    .or_else(|| item.get("track"))
                    .unwrap_or(&Value::Null)
            } else {
                item
            };
            add(
                out,
                spotify_track(
                    value,
                    if target.kind == ContentKind::Album {
                        Some(&root)
                    } else {
                        None
                    },
                ),
            );
        }
        let Some(next) = text(&page, "next") else {
            break;
        };
        if n + 1 == PAGE_CAP || full(out) {
            incomplete(out, "pagination limit reached.");
            break;
        }
        let next = match next_page(Platform::Spotify, &path, &next) {
            Ok(next) => next,
            Err(e) => {
                incomplete(out, &e);
                break;
            }
        };
        if !seen.insert(next.clone()) {
            incomplete(out, "provider repeated a pagination page.");
            break;
        }
        match client
            .get(Platform::Spotify, &next, Some(&auth), cancel)
            .await
        {
            Ok(value) => {
                path = next;
                page = value;
            }
            Err(e) => {
                cancel.check()?;
                incomplete(out, &e);
                break;
            }
        }
    }
    Ok(())
}

fn apple_track(value: &Value, storefront: &str) -> Option<ImportedTrack> {
    if value["type"].as_str().is_some_and(|v| v != "songs") {
        return None;
    }
    let external_id = id(value)?;
    let a = &value["attributes"];
    Some(ImportedTrack {
        origin: Origin {
            platform: Platform::AppleMusic,
            url: Some(format!(
                "https://music.apple.com/{storefront}/song/{external_id}"
            )),
            external_id,
        },
        metadata: Metadata {
            title: text(a, "name")?,
            artists: text(a, "artistName").into_iter().collect(),
            album: text(a, "albumName"),
            duration_ms: a["durationInMillis"].as_u64(),
            artwork_url: apple_artwork(a),
            isrc: text(a, "isrc"),
            explicit: text(a, "contentRating").map(|v| v == "explicit"),
        },
    })
}
fn apple_artwork(value: &Value) -> Option<String> {
    text(&value["artwork"], "url").map(|u| u.replace("{w}", "600").replace("{h}", "600"))
}

async fn apple_music(
    client: &impl JsonTransport,
    target: &ImportUrl,
    credentials: &Credentials,
    cancel: &Cancellation,
    out: &mut ImportedCollection,
) -> Result<(), String> {
    let resource = match target.kind { ContentKind::Track => "songs", ContentKind::Album => "albums", ContentKind::Playlist => "playlists", _ => return Err("Apple Music artist and profile imports are unsupported by this catalog importer. Use a track, album, or playlist.".into()) };
    let auth = format!(
        "Bearer {}",
        credential(&credentials.apple_token, "Apple Music developer token")?
    );
    let storefront = target
        .storefront
        .as_deref()
        .ok_or("Apple Music storefront is required.")?;
    let base = format!("/v1/catalog/{storefront}/{resource}/{}", target.id);
    let root = client
        .get(
            Platform::AppleMusic,
            &query(&base, &[("include", "tracks")]),
            Some(&auth),
            cancel,
        )
        .await?;
    let first = array(&root, "data")
        .first()
        .ok_or("Apple Music catalog collection is missing or private.")?;
    out.title = text(&first["attributes"], "name").unwrap_or_else(|| out.title.clone());
    out.artwork_url = apple_artwork(&first["attributes"]);
    if target.kind == ContentKind::Track {
        add(out, apple_track(first, storefront));
        return Ok(());
    }
    let mut path = format!("{base}/tracks");
    let mut page = first["relationships"]["tracks"].clone();
    if !page.is_object() {
        page = client
            .get(Platform::AppleMusic, &path, Some(&auth), cancel)
            .await?;
    }
    let mut seen = HashSet::from([super::http::api_url(Platform::AppleMusic, &path)?.to_string()]);
    for n in 0..PAGE_CAP {
        cancel.check()?;
        for value in array(&page, "data") {
            add(out, apple_track(value, storefront));
        }
        let Some(next) = text(&page, "next") else {
            break;
        };
        if n + 1 == PAGE_CAP || full(out) {
            incomplete(out, "pagination limit reached.");
            break;
        }
        let next = match next_page(Platform::AppleMusic, &path, &next) {
            Ok(next) => next,
            Err(e) => {
                incomplete(out, &e);
                break;
            }
        };
        if !seen.insert(next.clone()) {
            incomplete(out, "provider repeated a pagination page.");
            break;
        }
        match client
            .get(Platform::AppleMusic, &next, Some(&auth), cancel)
            .await
        {
            Ok(value) => {
                path = next;
                page = value;
            }
            Err(e) => {
                cancel.check()?;
                incomplete(out, &e);
                break;
            }
        }
    }
    Ok(())
}

fn youtube_artwork(value: &Value) -> Option<String> {
    ["maxres", "high", "medium", "default"]
        .iter()
        .find_map(|key| text(&value["thumbnails"][key], "url"))
}
fn duration_ms(value: &str) -> Option<u64> {
    // YouTube video durations use the ISO-8601 day/time subset. Reject unknown
    // units rather than returning an incorrect match duration.
    let value = value.strip_prefix('P')?;
    let mut total = 0u64;
    let mut digits = String::new();
    let mut in_time = false;
    for c in value.chars() {
        if c == 'T' {
            if !digits.is_empty() || in_time {
                return None;
            }
            in_time = true;
            continue;
        }
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n = digits.parse::<u64>().ok()?;
        digits.clear();
        let factor = match c {
            'D' if !in_time => 86_400_000,
            'H' if in_time => 3_600_000,
            'M' if in_time => 60_000,
            'S' if in_time => 1_000,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(factor)?)?;
    }
    if digits.is_empty() {
        Some(total)
    } else {
        None
    }
}
fn youtube_track(value: &Value) -> Option<ImportedTrack> {
    let external_id = id(value)?;
    if !super::urls::video_id(&external_id) {
        return None;
    }
    let s = &value["snippet"];
    let title = text(s, "title")?;
    if title == "Deleted video" || title == "Private video" {
        return None;
    }
    // Data API is a video catalog, not a YouTube Music catalog. Channel names
    // are only an artist hint; downstream matching must remain conservative.
    Some(ImportedTrack {
        origin: Origin {
            platform: Platform::Youtube,
            url: Some(format!("https://music.youtube.com/watch?v={external_id}")),
            external_id,
        },
        metadata: Metadata {
            title,
            artists: text(s, "channelTitle")
                .map(|s| s.strip_suffix(" - Topic").unwrap_or(&s).to_owned())
                .into_iter()
                .collect(),
            album: None,
            duration_ms: text(&value["contentDetails"], "duration").and_then(|v| duration_ms(&v)),
            artwork_url: youtube_artwork(s),
            isrc: None,
            explicit: None,
        },
    })
}
async fn youtube_details(
    client: &impl JsonTransport,
    ids: &[String],
    key: &str,
    cancel: &Cancellation,
    out: &mut ImportedCollection,
) -> Result<(), String> {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        if seen.insert(id) {
            unique.push(id.as_str());
        }
    }
    let mut details = HashMap::new();
    for chunk in unique.chunks(50) {
        cancel.check()?;
        let joined = chunk.join(",");
        let path = query(
            "/youtube/v3/videos",
            &[
                ("part", "snippet,contentDetails,status"),
                ("id", &joined),
                ("key", key),
            ],
        );
        match client.get(Platform::Youtube, &path, None, cancel).await {
            Ok(page) => {
                for value in array(&page, "items") {
                    if let Some(track) = youtube_track(value) {
                        details.insert(track.origin.external_id.clone(), track);
                    }
                }
            }
            Err(e) => {
                cancel.check()?;
                incomplete(out, &e);
            }
        }
    }
    for id in ids {
        add(out, details.get(id).cloned());
    }
    Ok(())
}

async fn youtube(
    client: &impl JsonTransport,
    target: &ImportUrl,
    credentials: &Credentials,
    cancel: &Cancellation,
    out: &mut ImportedCollection,
) -> Result<(), String> {
    let key = credential(&credentials.youtube_key, "YouTube Data API key")?;
    if target.kind == ContentKind::Track {
        youtube_details(client, std::slice::from_ref(&target.id), key, cancel, out).await?;
        if let Some(track) = out.tracks.first() {
            out.title = track.metadata.title.clone();
            out.artwork_url = track.metadata.artwork_url.clone();
        }
        return Ok(());
    }
    let playlist = match target.kind {
        ContentKind::Playlist => target.id.clone(),
        ContentKind::Profile => {
            let selector = if target.id.starts_with('@') {
                "forHandle"
            } else {
                "id"
            };
            let path = query(
                "/youtube/v3/channels",
                &[
                    ("part", "snippet,contentDetails"),
                    (selector, &target.id),
                    ("key", key),
                ],
            );
            let response = client.get(Platform::Youtube, &path, None, cancel).await?;
            let channel = array(&response, "items")
                .first()
                .ok_or("YouTube channel is missing or private.")?;
            out.warnings.push("Channel import includes only its accessible uploads; saved playlists, subscriptions, and private history are excluded.".into());
            text(&channel["contentDetails"]["relatedPlaylists"], "uploads")
                .ok_or("YouTube channel has no accessible uploads playlist.")?
        }
        _ => return Err("YouTube supports video, playlist, and channel uploads links.".into()),
    };
    let path = query(
        "/youtube/v3/playlists",
        &[("part", "snippet"), ("id", &playlist), ("key", key)],
    );
    let response = client.get(Platform::Youtube, &path, None, cancel).await?;
    let first = array(&response, "items")
        .first()
        .ok_or("YouTube playlist is missing or private.")?;
    out.title = text(&first["snippet"], "title").unwrap_or_else(|| out.title.clone());
    out.artwork_url = youtube_artwork(&first["snippet"]);
    let mut token = String::new();
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for n in 0..PAGE_CAP {
        let path = query(
            "/youtube/v3/playlistItems",
            &[
                ("part", "snippet,contentDetails"),
                ("playlistId", &playlist),
                ("maxResults", "50"),
                ("pageToken", &token),
                ("key", key),
            ],
        );
        let page = match client.get(Platform::Youtube, &path, None, cancel).await {
            Ok(page) => page,
            Err(e) if n > 0 => {
                cancel.check()?;
                incomplete(out, &e);
                break;
            }
            Err(e) => return Err(e),
        };
        for item in array(&page, "items") {
            if ids.len() >= TRACK_CAP {
                break;
            }
            if let Some(id) = text(&item["contentDetails"], "videoId")
                .or_else(|| text(&item["snippet"]["resourceId"], "videoId"))
            {
                ids.push(id);
            } else {
                add(out, None);
            }
        }
        let Some(next) = text(&page, "nextPageToken") else {
            break;
        };
        if n + 1 == PAGE_CAP || ids.len() >= TRACK_CAP {
            incomplete(out, "pagination limit reached.");
            break;
        }
        if !seen.insert(next.clone()) {
            incomplete(out, "provider repeated a pagination token.");
            break;
        }
        token = next;
    }
    youtube_details(client, &ids, key, cancel, out).await
}

fn soundcloud_track(value: &Value) -> Option<ImportedTrack> {
    if value["kind"].as_str().is_some_and(|v| v != "track") {
        return None;
    }
    let external_id = soundcloud_id(value, "tracks")?;
    Some(ImportedTrack {
        origin: Origin {
            platform: Platform::Soundcloud,
            external_id,
            url: text(value, "permalink_url"),
        },
        metadata: Metadata {
            title: text(value, "title")?,
            artists: text(&value["publisher_metadata"], "artist")
                .or_else(|| text(&value["user"], "username"))
                .into_iter()
                .collect(),
            album: text(&value["publisher_metadata"], "album_title"),
            duration_ms: value["duration"].as_u64(),
            artwork_url: text(value, "artwork_url"),
            isrc: text(&value["publisher_metadata"], "isrc"),
            explicit: value["explicit"].as_bool(),
        },
    })
}
fn soundcloud_id(value: &Value, resource: &str) -> Option<String> {
    if let Some(urn) = text(value, "urn") {
        let prefix = format!("soundcloud:{resource}:");
        if urn
            .strip_prefix(&prefix)
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
        {
            return Some(urn);
        }
        return None;
    }
    id(value).filter(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
}
async fn soundcloud_items(
    client: &impl JsonTransport,
    items: &[Value],
    auth: &str,
    cancel: &Cancellation,
    out: &mut ImportedCollection,
) -> Result<(), String> {
    for item in items {
        cancel.check()?;
        if full(out) {
            break;
        }
        let value = item.get("track").unwrap_or(item);
        if let Some(track) = soundcloud_track(value) {
            add(out, Some(track));
            continue;
        }
        let Some(id) = soundcloud_id(value, "tracks") else {
            add(out, None);
            continue;
        };
        match client
            .get(
                Platform::Soundcloud,
                &format!("/tracks/{id}"),
                Some(auth),
                cancel,
            )
            .await
        {
            Ok(track) => add(out, soundcloud_track(&track)),
            Err(_) => {
                cancel.check()?;
                add(out, None);
            }
        }
    }
    Ok(())
}
async fn soundcloud(
    client: &impl JsonTransport,
    target: &ImportUrl,
    credentials: &Credentials,
    cancel: &Cancellation,
    out: &mut ImportedCollection,
) -> Result<(), String> {
    let auth = format!(
        "OAuth {}",
        credential(&credentials.soundcloud_token, "SoundCloud OAuth token")?
    );
    let resolve_url = if target.kind == ContentKind::Likes {
        format!("https://soundcloud.com/{}", target.id)
    } else {
        target.url.clone()
    };
    let mut root = client
        .get(
            Platform::Soundcloud,
            &query("/resolve", &[("url", &resolve_url)]),
            Some(&auth),
            cancel,
        )
        .await?;
    if let Some(resource) = text(&root, "capsule_resolved_resource") {
        super::http::api_url(Platform::Soundcloud, &resource)?;
        root = client
            .get(Platform::Soundcloud, &resource, Some(&auth), cancel)
            .await?;
    }
    out.title = text(&root, "title")
        .or_else(|| text(&root, "username"))
        .unwrap_or_else(|| out.title.clone());
    out.artwork_url = text(&root, "artwork_url").or_else(|| text(&root, "avatar_url"));
    if target.kind == ContentKind::Track {
        add(out, soundcloud_track(&root));
        return Ok(());
    }
    if target.kind == ContentKind::Playlist {
        if root["kind"].as_str() != Some("playlist") {
            return Err("SoundCloud URL did not resolve to a playlist.".into());
        }
        if root["track_count"]
            .as_u64()
            .is_none_or(|n| n <= array(&root, "tracks").len() as u64)
        {
            soundcloud_items(client, array(&root, "tracks"), &auth, cancel, out).await?;
            return Ok(());
        }
    }
    let base = if target.kind == ContentKind::Playlist {
        let playlist =
            soundcloud_id(&root, "playlists").ok_or("Invalid SoundCloud playlist identifier.")?;
        format!("/playlists/{playlist}/tracks")
    } else {
        if !matches!(target.kind, ContentKind::Profile | ContentKind::Likes)
            || root["kind"].as_str() != Some("user")
        {
            return Err("Unsupported SoundCloud collection.".into());
        }
        let user = soundcloud_id(&root, "users").ok_or("Invalid SoundCloud user identifier.")?;
        out.warnings.push(if target.kind == ContentKind::Likes { "Includes only likes accessible to the supplied official API token." } else { "Profile import includes only accessible uploads; reposts and private collections are excluded." }.into());
        if target.kind == ContentKind::Likes {
            format!("/users/{user}/likes/tracks")
        } else {
            format!("/users/{user}/tracks")
        }
    };
    let mut path = query(&base, &[("linked_partitioning", "true"), ("limit", "200")]);
    let mut seen = HashSet::from([super::http::api_url(Platform::Soundcloud, &path)?.to_string()]);
    for n in 0..PAGE_CAP {
        let page = match client
            .get(Platform::Soundcloud, &path, Some(&auth), cancel)
            .await
        {
            Ok(page) => page,
            Err(e)
                if n == 0
                    && target.kind == ContentKind::Playlist
                    && !array(&root, "tracks").is_empty() =>
            {
                cancel.check()?;
                soundcloud_items(client, array(&root, "tracks"), &auth, cancel, out).await?;
                incomplete(out, &e);
                break;
            }
            Err(e) if n > 0 => {
                cancel.check()?;
                incomplete(out, &e);
                break;
            }
            Err(e) => return Err(e),
        };
        let items = page
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_else(|| array(&page, "collection"));
        soundcloud_items(client, items, &auth, cancel, out).await?;
        let Some(next) = text(&page, "next_href") else {
            break;
        };
        if n + 1 == PAGE_CAP || full(out) {
            incomplete(out, "pagination limit reached.");
            break;
        }
        let next = match next_page(Platform::Soundcloud, &path, &next) {
            Ok(next) => next,
            Err(e) => {
                incomplete(out, &e);
                break;
            }
        };
        if !seen.insert(next.clone()) {
            incomplete(out, "provider repeated a pagination page.");
            break;
        }
        path = next;
    }
    Ok(())
}

/// Public video search, optionally using a configured official Data API key.
pub async fn search_youtube(
    query_text: &str,
    credentials: &Credentials,
    cancel: &Cancellation,
) -> Result<Vec<PlaybackCandidate>, String> {
    if credentials
        .youtube_key
        .as_deref()
        .is_none_or(|key| key.trim().is_empty())
    {
        return super::public_youtube::search(query_text, cancel).await;
    }
    let result = search_youtube_with(ApiClient::shared()?, query_text, credentials, cancel).await;
    public_fallback(
        result,
        cancel,
        super::public_youtube::search(query_text, cancel),
    )
    .await
}
async fn search_youtube_with(
    client: &impl JsonTransport,
    query_text: &str,
    credentials: &Credentials,
    cancel: &Cancellation,
) -> Result<Vec<PlaybackCandidate>, String> {
    cancel.check()?;
    if query_text.trim().is_empty() || query_text.len() > 500 {
        return Err("Search query must be between 1 and 500 bytes.".into());
    }
    let key = credential(&credentials.youtube_key, "YouTube Data API key")?;
    let path = query(
        "/youtube/v3/search",
        &[
            ("part", "snippet"),
            ("type", "video"),
            ("videoCategoryId", "10"),
            ("maxResults", "10"),
            ("q", query_text),
            ("key", key),
        ],
    );
    let page = client.get(Platform::Youtube, &path, None, cancel).await?;
    let ids: Vec<_> = array(&page, "items")
        .iter()
        .filter_map(|v| text(&v["id"], "videoId"))
        .collect();
    let target = ImportUrl {
        provider: Platform::Youtube,
        kind: ContentKind::Playlist,
        id: "search".into(),
        storefront: None,
        url: String::new(),
    };
    let mut out = collection(&target);
    youtube_details(client, &ids, key, cancel, &mut out).await?;
    if !out.warnings.is_empty() && out.tracks.is_empty() {
        return Err("YouTube candidate details are unavailable.".into());
    }
    Ok(out
        .tracks
        .into_iter()
        .map(|track| PlaybackCandidate {
            provider: Platform::Youtube,
            external_id: track.origin.external_id,
            metadata: track.metadata,
            confidence: 0.0,
            compatible: true,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::{http::JsonFuture, urls::detect};
    use super::*;
    use serde_json::json;
    use std::{collections::VecDeque, sync::Mutex};
    #[tokio::test]
    async fn rejected_optional_key_falls_back_but_cancellation_and_success_do_not() {
        let cancel = Cancellation::default();
        assert_eq!(
            public_fallback(Err::<u32, _>("invalid API key".into()), &cancel, async {
                Ok(7)
            })
            .await,
            Ok(7)
        );
        assert_eq!(
            public_fallback(Ok(5), &cancel, async {
                panic!("successful official result must be kept")
            })
            .await,
            Ok(5)
        );
        cancel.cancel();
        assert_eq!(
            public_fallback(Err::<u32, _>("Import cancelled.".into()), &cancel, async {
                panic!("cancelled request must not retry")
            })
            .await,
            Err("Import cancelled.".into())
        );
    }
    struct Fixture(Mutex<VecDeque<(&'static str, Result<Value, String>)>>);
    impl JsonTransport for Fixture {
        fn get<'a>(
            &'a self,
            _: Platform,
            path: &'a str,
            _: Option<&'a str>,
            cancel: &'a Cancellation,
        ) -> JsonFuture<'a> {
            Box::pin(async move {
                cancel.check()?;
                let (expected, result) = self
                    .0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected HTTP request");
                assert!(
                    path.contains(expected),
                    "expected {expected}, actual {path}"
                );
                result
            })
        }
    }
    fn fixture(pages: Vec<(&'static str, Value)>) -> Fixture {
        Fixture(Mutex::new(
            pages.into_iter().map(|(p, v)| (p, Ok(v))).collect(),
        ))
    }
    fn credentials() -> Credentials {
        Credentials {
            spotify_token: Some("secret".into()),
            apple_token: Some("secret".into()),
            youtube_key: Some("secret".into()),
            soundcloud_token: Some("secret".into()),
        }
    }
    fn sp(id: &str) -> Value {
        json!({"id":id,"type":"track","name":"Song","artists":[{"name":"Artist"}],"duration_ms":123000})
    }
    fn apple(id: &str) -> Value {
        json!({"id":id,"type":"songs","attributes":{"name":"Song","artistName":"Artist","durationInMillis":123000}})
    }
    fn sc(id: u64) -> Value {
        json!({"id":id,"kind":"track","title":"Song","user":{"username":"Artist"},"duration":123000})
    }
    #[tokio::test]
    async fn spotify_playlist_current_items_preserves_duplicates_and_warns_missing() {
        let client = fixture(vec![
            (
                "/playlists/list",
                json!({"name":"Mix","items":{"items":[{"item":sp("a")},{"item":null}],"next":"https://api.spotify.com/v1/playlists/list/items?offset=2"}}),
            ),
            ("offset=2", json!({"items":[{"track":sp("a")}],"next":null})),
        ]);
        let out = fetch_with(
            &client,
            &detect("https://open.spotify.com/playlist/list").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.tracks.len(), 2);
        assert_eq!(out.tracks[0], out.tracks[1]);
        assert_eq!(out.warnings.len(), 1);
    }
    #[tokio::test]
    async fn apple_playlist_follows_relative_relationship_pagination() {
        let client = fixture(vec![
            (
                "/playlists/pl.x",
                json!({"data":[{"attributes":{"name":"Mix"},"relationships":{"tracks":{"data":[apple("1")],"next":"/v1/catalog/us/playlists/pl.x/tracks?offset=1"}}}]}),
            ),
            ("offset=1", json!({"data":[apple("2")]})),
        ]);
        let out = fetch_with(
            &client,
            &detect("https://music.apple.com/us/playlist/mix/pl.x").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            out.tracks
                .iter()
                .map(|t| t.origin.external_id.as_str())
                .collect::<Vec<_>>(),
            vec!["1", "2"]
        );
    }
    #[tokio::test]
    async fn youtube_playlist_batches_details_and_restores_playlist_order() {
        let client = fixture(vec![
            (
                "/playlists?",
                json!({"items":[{"snippet":{"title":"Mix"}}]}),
            ),
            (
                "/playlistItems?",
                json!({"items":[{"contentDetails":{"videoId":"abcdefghijk"}},{"contentDetails":{"videoId":"12345678901"}}],"nextPageToken":"next"}),
            ),
            (
                "pageToken=next",
                json!({"items":[{"contentDetails":{"videoId":"abcdefghijk"}},{"contentDetails":{"videoId":"missing0000"}}]}),
            ),
            (
                "/videos?",
                json!({"items":[{"id":"12345678901","snippet":{"title":"B","channelTitle":"Artist"},"contentDetails":{"duration":"PT2M3S"}}, {"id":"abcdefghijk","snippet":{"title":"A","channelTitle":"Artist"},"contentDetails":{"duration":"PT1H2M3S"}}]}),
            ),
        ]);
        let out = fetch_with(
            &client,
            &detect("https://music.youtube.com/playlist?list=PLx").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            out.tracks
                .iter()
                .map(|t| t.metadata.title.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B", "A"]
        );
        assert_eq!(out.tracks[0].metadata.duration_ms, Some(3723000));
        assert_eq!(out.warnings.len(), 1);
    }
    #[tokio::test]
    async fn soundcloud_playlist_hydrates_unavailable_stubs_in_order() {
        let client = fixture(vec![
            (
                "/resolve?",
                json!({"id":8,"kind":"playlist","title":"Mix","tracks":[sc(1),{"id":2},sc(1)]}),
            ),
            (
                "/tracks/2",
                json!({"id":2,"kind":"track","title":"Second","user":{"username":"Artist"}}),
            ),
        ]);
        let out = fetch_with(
            &client,
            &detect("https://soundcloud.com/artist/sets/mix").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.tracks.len(), 3);
        assert_eq!(out.tracks[1].metadata.title, "Second");
        assert_eq!(out.tracks[0], out.tracks[2]);
    }
    #[tokio::test]
    async fn soundcloud_urn_playlist_paginates_and_retains_partial_results() {
        let client = Fixture(Mutex::new(VecDeque::from([
            (
                "/resolve?",
                Ok(
                    json!({"kind":"playlist","urn":"soundcloud:playlists:8","title":"Mix","track_count":3,"tracks":[]}),
                ),
            ),
            (
                "/playlists/soundcloud:playlists:8/tracks",
                Ok(
                    json!({"collection":[{"urn":"soundcloud:tracks:1","title":"First","kind":"track"}],"next_href":"https://api.soundcloud.com/playlists/soundcloud:playlists:8/tracks?cursor=2"}),
                ),
            ),
            ("cursor=2", Err("Provider rate limit reached.".into())),
        ])));
        let out = fetch_with(
            &client,
            &detect("https://soundcloud.com/artist/sets/mix").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.tracks.len(), 1);
        assert_eq!(out.tracks[0].origin.external_id, "soundcloud:tracks:1");
        assert!(out.warnings.iter().any(|w| w.contains("incomplete")));
    }
    #[tokio::test]
    async fn soundcloud_failed_track_page_retains_embedded_accessible_tracks() {
        let client = Fixture(Mutex::new(VecDeque::from([
            (
                "/resolve?",
                Ok(
                    json!({"kind":"playlist","id":8,"title":"Mix","track_count":3,"tracks":[sc(1)]}),
                ),
            ),
            ("/playlists/8/tracks", Err("Provider denied access.".into())),
        ])));
        let out = fetch_with(
            &client,
            &detect("https://soundcloud.com/artist/sets/mix").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.tracks.len(), 1);
        assert!(out.warnings.iter().any(|w| w.contains("incomplete")));
    }
    #[tokio::test]
    async fn youtube_search_uses_official_candidates_without_assigning_confidence() {
        let client = fixture(vec![
            (
                "/search?",
                json!({"items":[{"id":{"videoId":"abcdefghijk"}}]}),
            ),
            (
                "/videos?",
                json!({"items":[{"id":"abcdefghijk","snippet":{"title":"Song","channelTitle":"Artist - Topic"},"contentDetails":{"duration":"PT3M"}}]}),
            ),
        ]);
        let candidates = search_youtube_with(
            &client,
            "Artist Song",
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].metadata.artists, vec!["Artist"]);
        assert_eq!(candidates[0].confidence, 0.0);
        assert!(candidates[0].compatible);
    }
    #[tokio::test]
    async fn apple_and_youtube_partial_pages_retain_readable_tracks() {
        let apple_client = Fixture(Mutex::new(VecDeque::from([
            (
                "/playlists/pl.x",
                Ok(
                    json!({"data":[{"attributes":{"name":"Mix"},"relationships":{"tracks":{"data":[apple("1")],"next":"/v1/catalog/us/playlists/pl.x/tracks?offset=1"}}}]}),
                ),
            ),
            ("offset=1", Err("Provider denied access.".into())),
        ])));
        let apple_out = fetch_with(
            &apple_client,
            &detect("https://music.apple.com/us/playlist/mix/pl.x").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(apple_out.tracks.len(), 1);
        assert!(apple_out.warnings.iter().any(|w| w.contains("incomplete")));
        let youtube_client = Fixture(Mutex::new(VecDeque::from([
            (
                "/playlists?",
                Ok(json!({"items":[{"snippet":{"title":"Mix"}}]})),
            ),
            (
                "/playlistItems?",
                Ok(
                    json!({"items":[{"contentDetails":{"videoId":"abcdefghijk"}}],"nextPageToken":"next"}),
                ),
            ),
            ("pageToken=next", Err("Provider denied access.".into())),
            (
                "/videos?",
                Ok(json!({"items":[{"id":"abcdefghijk","snippet":{"title":"Song"}}]})),
            ),
        ])));
        let youtube_out = fetch_with(
            &youtube_client,
            &detect("https://youtube.com/playlist?list=PLx").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(youtube_out.tracks.len(), 1);
        assert!(youtube_out
            .warnings
            .iter()
            .any(|w| w.contains("incomplete")));
    }
    struct CancelOnRequest;
    impl JsonTransport for CancelOnRequest {
        fn get<'a>(
            &'a self,
            _: Platform,
            _: &'a str,
            _: Option<&'a str>,
            cancel: &'a Cancellation,
        ) -> JsonFuture<'a> {
            Box::pin(async move {
                cancel.cancel();
                Err("Import cancelled.".into())
            })
        }
    }
    #[tokio::test]
    async fn cancellation_during_each_adapter_is_an_error() {
        for url in [
            "https://open.spotify.com/playlist/list",
            "https://music.apple.com/us/playlist/mix/pl.x",
            "https://youtube.com/playlist?list=PLx",
            "https://soundcloud.com/artist/sets/mix",
        ] {
            let result = fetch_with(
                &CancelOnRequest,
                &detect(url).unwrap(),
                &credentials(),
                &Cancellation::default(),
            )
            .await;
            assert!(result.unwrap_err().contains("cancelled"));
        }
    }
    #[tokio::test]
    async fn repeated_pagination_stops_before_reimporting_first_page() {
        let client = fixture(vec![(
            "/playlists/list",
            json!({"name":"Mix","items":{"items":[{"item":sp("a")}],"next":"https://api.spotify.com/v1/playlists/list/items"}}),
        )]);
        let out = fetch_with(
            &client,
            &detect("https://open.spotify.com/playlist/list").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.tracks.len(), 1);
        assert!(out.warnings.iter().any(|w| w.contains("repeated")));
    }
    #[tokio::test]
    async fn missing_tracks_also_count_toward_import_size_limit() {
        let rows = vec![json!({"item":null}); TRACK_CAP + 100];
        let client = fixture(vec![(
            "/playlists/list",
            json!({"name":"Mix","items":{"items":rows}}),
        )]);
        let out = fetch_with(
            &client,
            &detect("https://open.spotify.com/playlist/list").unwrap(),
            &credentials(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.tracks.len(), 0);
        assert!(out.warnings.len() <= TRACK_CAP + 1);
        assert!(out.warnings.iter().any(|w| w.contains("limit")));
    }
    #[tokio::test]
    async fn pagination_failure_retains_prior_tracks_and_cancel_does_not_return_partial_success() {
        let client = Fixture(Mutex::new(VecDeque::from([
            (
                "/playlists/list",
                Ok(
                    json!({"name":"Mix","tracks":{"items":[{"track":sp("a")}],"next":"https://api.spotify.com/v1/playlists/list/tracks?offset=1"}}),
                ),
            ),
            ("offset=1", Err("Provider rate limit reached.".into())),
        ])));
        let target = detect("https://open.spotify.com/playlist/list").unwrap();
        let out = fetch_with(&client, &target, &credentials(), &Cancellation::default())
            .await
            .unwrap();
        assert_eq!(out.tracks.len(), 1);
        assert!(out.warnings.iter().any(|w| w.contains("incomplete")));
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(fetch_with(&client, &target, &credentials(), &cancel)
            .await
            .unwrap_err()
            .contains("cancelled"));
    }
}
