//! Anonymous public metadata only. No cookies, account session or audio requests.
use super::{
    http::{Cancellation, JsonFuture, JsonTransport},
    model::{ImportedCollection, ImportedTrack, Metadata, Origin, Platform},
    urls::{ContentKind, ImportUrl},
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const TRACK_CAP: usize = 10_000;
const PAGE_CAP: usize = 200;
const API: &str = "https://api-v2.soundcloud.com/";

struct PublicApi;
impl JsonTransport for PublicApi {
    fn get<'a>(
        &'a self,
        _: Platform,
        path: &'a str,
        _: Option<&'a str>,
        cancel: &'a Cancellation,
    ) -> JsonFuture<'a> {
        Box::pin(super::public_http::get_json(
            Platform::Soundcloud,
            path,
            None,
            cancel,
        ))
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}

// Numeric IDs and new SoundCloud URNs identify the same item. Keep a URN as the
// origin when present, but use its numeric suffix for anonymous v2 requests.
fn numeric_id(value: &Value) -> Option<String> {
    if let Some(urn) = value["urn"].as_str() {
        let id = urn
            .strip_prefix("soundcloud:tracks:")
            .or_else(|| urn.strip_prefix("soundcloud:playlists:"))?;
        return (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.into());
    }
    let id = value["id"]
        .as_u64()
        .map(|n| n.to_string())
        .or_else(|| text(value, "id"))?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

fn track(value: &Value) -> Option<ImportedTrack> {
    if value["kind"].as_str().is_some_and(|kind| kind != "track")
        || value["urn"]
            .as_str()
            .is_some_and(|urn| !urn.starts_with("soundcloud:tracks:"))
        || value["sharing"].as_str() == Some("private")
    {
        return None;
    }
    let id = numeric_id(value)?;
    Some(ImportedTrack {
        origin: Origin {
            platform: Platform::Soundcloud,
            external_id: text(value, "urn").unwrap_or(id),
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
            artwork_url: text(value, "artwork_url").or_else(|| text(&value["user"], "avatar_url")),
            isrc: text(&value["publisher_metadata"], "isrc"),
            explicit: value["explicit"]
                .as_bool()
                .or_else(|| value["publisher_metadata"]["explicit"].as_bool()),
        },
    })
}

pub(crate) fn hydration(html: &str) -> Option<Value> {
    let tail = html
        .split_once("window.__sc_hydration")?
        .1
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    // Deserialize one JSON value: semicolons and other JS after it are harmless,
    // including a literal `];` inside a JSON string.
    Value::deserialize(&mut serde_json::Deserializer::from_str(tail))
        .ok()
        .filter(Value::is_array)
}

pub(crate) fn client_id(source: &str) -> Option<String> {
    for key in ["client_id", "clientId"] {
        for (_, tail) in source
            .match_indices(key)
            .map(|(i, _)| (i, &source[i + key.len()..]))
        {
            let tail = tail.trim_start_matches(['"', '\'']).trim_start();
            let Some(tail) = tail.strip_prefix(':') else {
                continue;
            };
            let tail = tail.trim_start();
            let Some(quote) = tail.chars().next().filter(|q| matches!(q, '"' | '\'')) else {
                continue;
            };
            let Some(value) = tail[1..].split(quote).next() else {
                continue;
            };
            if valid_client(value) {
                return Some(value.into());
            }
        }
    }
    None
}
fn valid_client(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub(crate) fn scripts(html: &str) -> Vec<String> {
    let mut result = Vec::new();
    for tail in html.split("<script").skip(1) {
        let Some(tag) = tail.split('>').next() else {
            continue;
        };
        let Some((_, src)) = tag.split_once("src=") else {
            continue;
        };
        let Some(quote) = src.chars().next().filter(|q| matches!(q, '"' | '\'')) else {
            continue;
        };
        let Some(src) = src[1..].split(quote).next() else {
            continue;
        };
        let Ok(url) = url::Url::parse(src) else {
            continue;
        };
        if url.scheme() == "https"
            && url.host_str().is_some_and(|h| h.ends_with(".sndcdn.com"))
            && url.path().ends_with(".js")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
        {
            result.push(src.into());
        }
    }
    result.reverse();
    result.truncate(12);
    result
}

async fn discover_client(
    html: &str,
    data: Option<&Value>,
    cancel: &Cancellation,
) -> Result<String, String> {
    if let Some(id) = data
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|v| v["hydratable"] == "apiClient")
        .and_then(|v| v["data"]["id"].as_str())
        .filter(|id| valid_client(id))
    {
        return Ok(id.into());
    }
    if let Some(id) = client_id(html) {
        return Ok(id);
    }
    for script in scripts(html) {
        cancel.check()?;
        match super::public_http::get_text(Platform::Soundcloud, &script, None, cancel).await {
            Ok(js) => {
                if let Some(id) = client_id(&js) {
                    return Ok(id);
                }
            }
            Err(_) => cancel.check()?,
        }
    }
    Err("SoundCloud did not expose an anonymous public client. Its page may require JavaScript or block automated access; try again later.".into())
}

fn api_url(path: &str, client: &str) -> Result<String, String> {
    let base = url::Url::parse(API).unwrap();
    let mut url = base
        .join(path)
        .map_err(|_| "Invalid SoundCloud pagination URL.")?;
    if url.scheme() != "https"
        || url.host_str() != Some("api-v2.soundcloud.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("SoundCloud returned an unsafe pagination URL.".into());
    }
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "client_id")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair("client_id", client);
    Ok(url.into())
}

pub async fn fetch(
    target: &ImportUrl,
    cancel: &Cancellation,
) -> Result<ImportedCollection, String> {
    cancel.check()?;
    if !matches!(target.kind, ContentKind::Track | ContentKind::Playlist) {
        return Err("Public SoundCloud import supports track and set/playlist links. Profiles and likes require a connected account.".into());
    }
    let html =
        super::public_http::get_text(Platform::Soundcloud, &target.url, None, cancel).await?;
    let data = hydration(&html);
    let kind = if target.kind == ContentKind::Track {
        "sound"
    } else {
        "playlist"
    };
    let embedded = data
        .as_ref()
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find(|v| v["hydratable"] == kind))
        .map(|v| v["data"].clone());
    // A full public track, or a fully embedded playlist, needs no API client.
    let needs_api = embedded.as_ref().is_none_or(|root| {
        if target.kind == ContentKind::Track {
            return track(root).is_none();
        }
        let tracks = root["tracks"].as_array();
        tracks.is_none_or(|items| {
            root["track_count"]
                .as_u64()
                .is_none_or(|count| count > items.len() as u64)
                || items.iter().any(|v| track(v).is_none())
                || root["next_href"].is_string()
        })
    });
    let client = if needs_api {
        discover_client(&html, data.as_ref(), cancel).await.ok()
    } else {
        None
    };
    cancel.check()?;
    let root = match embedded {
        Some(root) => root,
        None => {
            let id = client.as_deref().ok_or("SoundCloud's public page contains no usable metadata. It may require JavaScript, be private, or block automated access.")?;
            let mut url = url::Url::parse(&api_url("resolve", id)?).unwrap();
            url.query_pairs_mut().append_pair("url", &target.url);
            PublicApi
                .get(Platform::Soundcloud, url.as_str(), None, cancel)
                .await?
        }
    };
    collect(&PublicApi, target, root, client.as_deref(), cancel).await
}

async fn collect(
    client: &impl JsonTransport,
    target: &ImportUrl,
    mut root: Value,
    client_id: Option<&str>,
    cancel: &Cancellation,
) -> Result<ImportedCollection, String> {
    cancel.check()?;
    if root["sharing"].as_str() == Some("private") {
        return Err("Private SoundCloud links are not supported by public import.".into());
    }
    let mut out = ImportedCollection {
        source_url: target.url.clone(),
        provider: Platform::Soundcloud,
        kind: target.kind,
        title: text(&root, "title").unwrap_or_else(|| "SoundCloud import".into()),
        artwork_url: text(&root, "artwork_url"),
        tracks: Vec::new(),
        warnings: Vec::new(),
    };
    if target.kind == ContentKind::Track {
        out.tracks
            .push(track(&root).ok_or("SoundCloud did not expose metadata for this public track.")?);
        return Ok(out);
    }
    if root["kind"] != "playlist" {
        return Err("SoundCloud URL did not resolve to a public playlist.".into());
    }
    let advertised = root["track_count"].as_u64();
    let mut items = root["tracks"].as_array().cloned().unwrap_or_default();
    // The hydration normally includes all ordered IDs but only five full tracks.
    // Ask for the full playlist when even its ordered ID list was truncated.
    if advertised.is_some_and(|count| count > items.len() as u64) {
        if let (Some(id), Some(key)) = (numeric_id(&root), client_id) {
            match client
                .get(
                    Platform::Soundcloud,
                    &api_url(&format!("playlists/{id}?representation=full"), key)?,
                    None,
                    cancel,
                )
                .await
            {
                Ok(full)
                    if full["tracks"]
                        .as_array()
                        .is_some_and(|v| v.len() >= items.len()) =>
                {
                    items = full["tracks"].as_array().cloned().unwrap_or_default();
                    root = full;
                }
                _ => {
                    cancel.check()?;
                    out.warnings.push("SoundCloud's complete playlist listing was unavailable; retained its embedded entries.".into());
                }
            }
        }
    }
    let mut next = text(&root, "next_href");
    let mut seen = HashSet::new();
    for page in 0..PAGE_CAP {
        let Some(path) = next.take() else {
            break;
        };
        if items.len() >= TRACK_CAP || page + 1 == PAGE_CAP {
            out.warnings
                .push("SoundCloud pagination limit reached; the import is incomplete.".into());
            break;
        }
        let Some(key) = client_id else {
            out.warnings.push(
                "SoundCloud pagination needs an anonymous client that the page did not expose."
                    .into(),
            );
            break;
        };
        let url = match api_url(&path, key) {
            Ok(url) => url,
            Err(e) => {
                out.warnings.push(e);
                break;
            }
        };
        if !seen.insert(url.clone()) {
            out.warnings
                .push("SoundCloud repeated a pagination page; the import is incomplete.".into());
            break;
        }
        match client.get(Platform::Soundcloud, &url, None, cancel).await {
            Ok(page) => {
                if let Some(entries) = page["collection"]
                    .as_array()
                    .or_else(|| page["tracks"].as_array())
                    .or_else(|| page.as_array())
                {
                    items.extend(
                        entries
                            .iter()
                            .take(TRACK_CAP.saturating_sub(items.len()))
                            .cloned(),
                    );
                } else {
                    out.warnings.push(
                        "SoundCloud returned a malformed playlist page; the import is incomplete."
                            .into(),
                    );
                    break;
                }
                next = text(&page, "next_href");
            }
            Err(_) => {
                cancel.check()?;
                out.warnings.push(
                    "A SoundCloud playlist page was unavailable; the import is incomplete.".into(),
                );
                break;
            }
        }
    }
    if items.len() > TRACK_CAP {
        items.truncate(TRACK_CAP);
        out.warnings
            .push("SoundCloud's 10,000-entry import limit was reached.".into());
    }
    if let Some(count) = advertised.filter(|count| *count > items.len() as u64) {
        out.warnings.push(format!("SoundCloud lists {count} entries but exposed only {} ordered entries; the import is incomplete.", items.len()));
    }
    let mut details = HashMap::new();
    let mut ids = Vec::new();
    let mut requested = HashSet::new();
    for item in &items {
        if track(item).is_none() {
            if let Some(id) = numeric_id(item).filter(|id| requested.insert(id.clone())) {
                ids.push(id);
            }
        }
    }
    if let Some(key) = client_id {
        for batch in ids.chunks(50) {
            cancel.check()?;
            let url = api_url(&format!("tracks?ids={}", batch.join(",")), key)?;
            match client.get(Platform::Soundcloud, &url, None, cancel).await {
                Ok(page) => {
                    if let Some(tracks) = page.as_array().or_else(|| page["collection"].as_array())
                    {
                        for value in tracks {
                            if let (Some(id), Some(track)) = (numeric_id(value), track(value)) {
                                details.insert(id, track);
                            }
                        }
                    }
                }
                Err(_) => {
                    cancel.check()?;
                }
            }
        }
    }
    let mut missing = 0;
    for item in &items {
        cancel.check()?;
        match track(item).or_else(|| numeric_id(item).and_then(|id| details.get(&id).cloned())) {
            Some(track) => out.tracks.push(track),
            None => missing += 1,
        }
    }
    if missing > 0 {
        out.warnings.push(format!("{missing} SoundCloud playlist entries were unavailable, private, or missing public metadata and were skipped. The import is incomplete."));
    }
    if advertised.is_none() {
        out.warnings.push("SoundCloud did not publish a playlist entry count; completeness could not be verified.".into());
    }
    if out.tracks.is_empty() && !matches!(advertised, Some(0)) {
        return Err("SoundCloud exposed no accessible tracks for this playlist. It may be private, unavailable, or blocked.".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;
    struct Fixture {
        responses: Mutex<Vec<Value>>,
    }
    impl JsonTransport for Fixture {
        fn get<'a>(
            &'a self,
            _: Platform,
            _: &'a str,
            _: Option<&'a str>,
            cancel: &'a Cancellation,
        ) -> JsonFuture<'a> {
            Box::pin(async move {
                cancel.check()?;
                self.responses
                    .lock()
                    .unwrap()
                    .pop()
                    .ok_or("fixture unavailable".into())
            })
        }
    }
    fn target() -> ImportUrl {
        super::super::urls::detect("https://soundcloud.com/artist/sets/mix").unwrap()
    }
    #[test]
    fn structured_hydration_and_client_extraction() {
        let page = r#"<script>window.__sc_hydration = [{"hydratable":"sound","data":{"title":"bracket ]; and Unicode 音"}}]; trailing()</script>"#;
        assert_eq!(
            hydration(page).unwrap()[0]["data"]["title"],
            "bracket ]; and Unicode 音"
        );
        assert_eq!(
            client_id(r#"x={client_id:"abcdefghijklmnopqrstuvwxyz123456"}"#).as_deref(),
            Some("abcdefghijklmnopqrstuvwxyz123456")
        );
        assert!(client_id(r#"client_id:"bad""#).is_none());
        assert_eq!(scripts(r#"<script src="https://evil.test/a.js"></script><script src="https://a-v2.sndcdn.com/assets/a.js"></script>"#).len(), 1);
    }
    #[test]
    fn urn_mapping_and_pagination_safety() {
        let mapped = track(&json!({"urn":"soundcloud:tracks:42","title":"Title","publisher_metadata":{"artist":"Artist","isrc":"US1"},"duration":1234})).unwrap();
        assert_eq!(mapped.origin.external_id, "soundcloud:tracks:42");
        assert_eq!(mapped.metadata.artists, ["Artist"]);
        assert!(track(&json!({"urn":"soundcloud:playlists:42","title":"wrong"})).is_none());
        assert!(track(&json!({"id":42,"title":"private","sharing":"private"})).is_none());
        for url in [
            "https://evil.test/x",
            "//evil.test/x",
            "http://api-v2.soundcloud.com/x",
            "https://u@api-v2.soundcloud.com/x",
        ] {
            assert!(api_url(url, "id").is_err());
        }
        assert!(!api_url("tracks?client_id=old", "fresh")
            .unwrap()
            .contains("old"));
    }
    #[tokio::test]
    async fn batch_lookup_preserves_order_repeats_and_reports_missing() {
        let fixture = Fixture {
            responses: Mutex::new(vec![
                json!([{"urn":"soundcloud:tracks:2","title":"Two"},{"id":1,"title":"One"}]),
            ]),
        };
        let out = collect(&fixture, &target(), json!({"kind":"playlist","title":"Mix","track_count":4,"tracks":[{"id":2},{"id":1},{"id":2},{"id":3}]}), Some("client"), &Cancellation::default()).await.unwrap();
        assert_eq!(
            out.tracks
                .iter()
                .map(|t| t.metadata.title.as_str())
                .collect::<Vec<_>>(),
            ["Two", "One", "Two"]
        );
        assert!(out
            .warnings
            .iter()
            .any(|v| v.contains("1 SoundCloud playlist entries")));
    }
    #[tokio::test]
    async fn pagination_keeps_existing_entries_and_warns_on_failure() {
        let fixture = Fixture {
            responses: Mutex::new(vec![
                json!({"collection":[{"id":2,"title":"Two"}],"next_href":"https://api-v2.soundcloud.com/page3"}),
            ]),
        };
        let out = collect(&fixture, &target(), json!({"kind":"playlist","track_count":2,"tracks":[{"id":1,"title":"One"},{"id":1,"title":"One"}],"next_href":"https://api-v2.soundcloud.com/page2"}), Some("client"), &Cancellation::default()).await.unwrap();
        assert_eq!(out.tracks.len(), 3);
        assert!(out
            .warnings
            .iter()
            .any(|v| v.contains("page was unavailable")));
    }
    #[tokio::test]
    async fn absent_client_keeps_accessible_tracks_and_warns() {
        let fixture = Fixture {
            responses: Mutex::new(vec![]),
        };
        let out = collect(
            &fixture,
            &target(),
            json!({"kind":"playlist","track_count":3,"tracks":[{"id":1,"title":"One"},{"id":2}]}),
            None,
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.tracks.len(), 1);
        assert_eq!(out.warnings.len(), 2);
    }
    #[tokio::test]
    async fn truncated_ids_use_full_playlist_and_repeated_pages_are_bounded() {
        let fixture = Fixture {
            responses: Mutex::new(vec![
                json!({"collection":[{"id":2,"title":"Two"}],"next_href":"https://api-v2.soundcloud.com/page2"}),
                json!({"kind":"playlist","track_count":3,"tracks":[{"id":1,"title":"One"},{"id":1,"title":"One"}],"next_href":"https://api-v2.soundcloud.com/page2"}),
            ]),
        };
        let out = collect(
            &fixture,
            &target(),
            json!({"kind":"playlist","id":9,"track_count":3,"tracks":[{"id":1,"title":"One"}]}),
            Some("client"),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.tracks.len(), 3);
        assert!(out
            .warnings
            .iter()
            .any(|v| v.contains("repeated a pagination")));
    }
    #[tokio::test]
    async fn unavailable_empty_and_cancelled_imports_fail_clearly() {
        let fixture = Fixture {
            responses: Mutex::new(vec![]),
        };
        assert!(collect(
            &fixture,
            &target(),
            json!({"kind":"playlist","track_count":2,"tracks":[{"id":1}]}),
            None,
            &Cancellation::default()
        )
        .await
        .is_err());
        let empty = collect(
            &fixture,
            &target(),
            json!({"kind":"playlist","track_count":0,"tracks":[]}),
            None,
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert!(empty.tracks.is_empty());
        assert!(empty.warnings.is_empty());
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(collect(
            &fixture,
            &target(),
            json!({"kind":"playlist","track_count":0,"tracks":[]}),
            None,
            &cancel
        )
        .await
        .unwrap_err()
        .contains("cancelled"));
    }
    #[tokio::test]
    #[ignore = "Live public SoundCloud network verification"]
    async fn live_public_track_and_set() {
        for url in [
            "https://soundcloud.com/ethmusic/lostin-powers-she-so-heavy",
            "https://soundcloud.com/monstercat/sets/monstercat-025-threshold",
        ] {
            let out = fetch(
                &super::super::urls::detect(url).unwrap(),
                &Cancellation::default(),
            )
            .await
            .unwrap();
            assert!(!out.tracks.is_empty());
            assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        }
    }
}
