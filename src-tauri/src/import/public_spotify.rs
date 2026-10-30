//! Public Spotify embed metadata; anonymous session tokens never leave memory.

use super::{
    http::Cancellation,
    model::*,
    public_http,
    urls::{ContentKind, ImportUrl},
};
use futures_util::{stream, StreamExt};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const TRACK_CAP: usize = 10_000;
const PAGE_CAP: usize = 200;

fn text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}

fn parse_embed(html: &str) -> Result<Value, String> {
    // Read only inert Next.js JSON; never evaluate provider JavaScript.
    for chunk in html.split("<script").skip(1) {
        let Some((tag, body)) = chunk.split_once('>') else {
            continue;
        };
        if !tag.contains("id=\"__NEXT_DATA__\"") && !tag.contains("id='__NEXT_DATA__'") {
            continue;
        }
        let body = body
            .split_once("</script>")
            .ok_or("Spotify public metadata is malformed.")?
            .0;
        return serde_json::from_str(body)
            .map_err(|_| "Spotify public metadata is malformed.".into());
    }
    Err("Spotify public metadata is unavailable. The item may be private, removed, or unavailable in this region.".into())
}

fn entity(data: &Value) -> Result<&Value, String> {
    ["/props/pageProps/state/data/entity", "/props/pageProps/data/entity", "/props/pageProps/entity"]
        .iter().find_map(|path| data.pointer(path).filter(|v| v.is_object()))
        .ok_or_else(|| "Spotify public metadata is unavailable. The item may be private, removed, or unavailable in this region.".into())
}

fn token(data: &Value) -> Option<String> {
    [
        "/props/pageProps/state/settings/session",
        "/props/pageProps/settings/session",
        "/props/pageProps/session",
    ]
    .iter()
    .find_map(|path| data.pointer(path).and_then(|v| text(v, "accessToken")))
    .filter(|v| v.len() <= 8192 && !v.bytes().any(|b| b.is_ascii_control()))
}

fn track_id(uri: &str) -> Option<String> {
    let id = uri.strip_prefix("spotify:track:")?;
    (!id.is_empty() && id.len() <= 256 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| id.to_owned())
}

fn artwork(value: &Value) -> Option<String> {
    ["/coverArt/sources", "/visualIdentity/image", "/images"]
        .iter()
        .filter_map(|p| value.pointer(p).and_then(Value::as_array))
        .flat_map(|a| a.iter().rev())
        .find_map(|v| text(v, "url"))
}

fn track(value: &Value, album: Option<&str>, image: Option<&str>) -> Option<ImportedTrack> {
    let external_id = track_id(value["uri"].as_str()?)?;
    let artists = value["artists"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|v| text(v, "name"))
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            text(value, "subtitle")
                .map(|v| {
                    // Spotify separates subtitle artists with comma + NBSP;
                    // a plain comma can be part of an artist's real name.
                    v.split(",\u{a0}")
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        });
    Some(ImportedTrack {
        origin: Origin {
            platform: Platform::Spotify,
            url: Some(format!("https://open.spotify.com/track/{external_id}")),
            external_id,
        },
        metadata: Metadata {
            title: text(value, "title").or_else(|| text(value, "name"))?,
            artists,
            album: text(&value["album"], "name").or_else(|| album.map(str::to_owned)),
            duration_ms: value["duration"]
                .as_u64()
                .or_else(|| value["duration_ms"].as_u64()),
            artwork_url: artwork(value)
                .or_else(|| artwork(&value["album"]))
                .or_else(|| image.map(str::to_owned)),
            isrc: text(&value["external_ids"], "isrc"),
            explicit: value["isExplicit"]
                .as_bool()
                .or_else(|| value["explicit"].as_bool()),
        },
    })
}

// Length counts slots, including episodes/local or unavailable entries. Keep
// those slots during pagination so duplicates and later tracks stay in order.
fn uri_page(page: &Value, offset: usize) -> Result<(Vec<Option<String>>, usize, bool), String> {
    let total = page["length"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or("Spotify omitted its playlist length.")?;
    let contents = &page["contents"];
    if contents["pos"].as_u64().unwrap_or(0) != offset as u64 {
        return Err("Spotify returned a noncontiguous playlist page.".into());
    }
    let rows = contents["items"]
        .as_array()
        .ok_or("Spotify omitted its playlist items.")?;
    if rows.is_empty() && offset < total {
        return Err("Spotify returned an empty playlist page.".into());
    }
    if offset.saturating_add(rows.len()) > total {
        return Err("Spotify returned an inconsistent playlist length.".into());
    }
    Ok((
        rows.iter()
            .map(|v| v["uri"].as_str().and_then(track_id))
            .collect(),
        total,
        contents["truncated"].as_bool().unwrap_or(false),
    ))
}

async fn embed(kind: &str, id: &str, cancel: &Cancellation) -> Result<Value, String> {
    let html = public_http::get_text(
        Platform::Spotify,
        &format!("https://open.spotify.com/embed/{kind}/{id}"),
        None,
        cancel,
    )
    .await?;
    parse_embed(&html)
}

/// Fetch richer details outside the playlist preview's critical path.
pub(crate) async fn fetch_track_metadata(
    id: &str,
    cancel: &Cancellation,
) -> Result<Option<ImportedTrack>, String> {
    if track_id(&format!("spotify:track:{id}")).as_deref() != Some(id) {
        return Err("Invalid Spotify track identifier.".into());
    }
    let data = embed("track", id, cancel).await?;
    cancel.check()?;
    Ok(track(entity(&data)?, None, None).filter(|track| track.origin.external_id == id))
}

/// Enrichment fills unknown fields without replacing imported or edited values.
pub(crate) fn merge_missing_metadata(current: &mut Metadata, detail: Metadata) {
    if current.title.trim().is_empty() { current.title = detail.title; }
    if current.artists.is_empty() { current.artists = detail.artists; }
    if current.album.as_ref().is_none_or(|v| v.trim().is_empty()) { current.album = detail.album; }
    if current.artwork_url.as_ref().is_none_or(|v| v.trim().is_empty()) { current.artwork_url = detail.artwork_url; }
    if current.duration_ms.is_none() { current.duration_ms = detail.duration_ms; }
    if current.isrc.is_none() { current.isrc = detail.isrc; }
    if current.explicit.is_none() { current.explicit = detail.explicit; }
}

fn incomplete(out: &mut ImportedCollection, reason: &str) {
    out.warnings.push(format!("Import incomplete: {reason}"));
}

// Metadata requests are unique; playlist occurrences remain authoritative.
async fn hydrate<F, Fut>(
    ids: Vec<Option<String>>,
    mut cache: HashMap<String, ImportedTrack>,
    cancel: &Cancellation,
    fetch: F,
) -> Result<(Vec<ImportedTrack>, usize, bool), String>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<ImportedTrack>, String>>,
{
    cancel.check()?;
    let mut seen = HashSet::new();
    let missing = ids
        .iter()
        .flatten()
        .filter(|id| !cache.contains_key(*id) && seen.insert((*id).clone()))
        .cloned()
        .collect::<Vec<_>>();
    // Match the existing transport's two permits; keep its request spacing.
    let mut pending = stream::iter(missing)
        .map(|id| {
            let request = fetch(id.clone());
            async move { (id, request.await) }
        })
        .buffered(2);
    while let Some((id, result)) = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("Import cancelled.".into()),
        value = pending.next() => value,
    } {
        cancel.check()?;
        match result? {
            Some(value) if value.origin.external_id == id => {
                cache.insert(id, value);
            }
            _ => break,
        }
    }
    // Rebuild from slots, retaining duplicates and the verified prefix on failure.
    let mut out = Vec::new();
    let mut skipped = 0;
    for id in ids {
        cancel.check()?;
        let Some(id) = id else {
            skipped += 1;
            continue;
        };
        let Some(value) = cache.get(&id) else {
            return Ok((out, skipped, true));
        };
        out.push(value.clone());
    }
    Ok((out, skipped, false))
}

pub async fn fetch(
    target: &ImportUrl,
    cancel: &Cancellation,
) -> Result<ImportedCollection, String> {
    cancel.check()?;
    if super::urls::detect(&target.url)? != *target || target.provider != Platform::Spotify {
        return Err("Invalid canonical Spotify target.".into());
    }
    let kind = match target.kind {
        ContentKind::Track => "track",
        ContentKind::Album => "album",
        ContentKind::Playlist => "playlist",
        _ => return Err("Use a public Spotify track, album, or playlist link.".into()),
    };
    let data = embed(kind, &target.id, cancel).await?;
    let root = entity(&data)?;
    let expected_uri = format!("spotify:{kind}:{}", target.id);
    if root["uri"].as_str() != Some(expected_uri.as_str()) {
        return Err("Spotify returned metadata for a different item.".into());
    }
    let title = text(root, "name")
        .or_else(|| text(root, "title"))
        .ok_or("Spotify omitted the collection title.")?;
    let mut out = ImportedCollection {
        source_url: target.url.clone(),
        provider: Platform::Spotify,
        kind: target.kind,
        title,
        artwork_url: artwork(root),
        tracks: vec![],
        warnings: vec![],
    };
    if target.kind == ContentKind::Track {
        out.tracks
            .push(track(root, None, None).ok_or("Spotify track metadata is unavailable.")?);
        cancel.check()?;
        return Ok(out);
    }
    let rows = root["trackList"]
        .as_array()
        .ok_or("Spotify omitted the public track list.")?;
    let album = (target.kind == ContentKind::Album).then_some(out.title.as_str());
    let mut cache = HashMap::new();
    for row in rows.iter().take(TRACK_CAP) {
        if let Some(track) = track(
            row,
            album,
            if album.is_some() {
                out.artwork_url.as_deref()
            } else {
                None
            },
        ) {
            cache.insert(track.origin.external_id.clone(), track.clone());
            out.tracks.push(track);
        } else {
            out.warnings
                .push("An unavailable or unsupported Spotify entry was skipped.".into());
        }
    }
    if target.kind == ContentKind::Album {
        // Embed has no authoritative album total. Never silently certify a
        // capped list as complete (long box sets can exceed this size).
        if rows.len() >= 100 {
            incomplete(
                &mut out,
                "Spotify's album embed may contain only the first 100 entries.",
            );
        }
        cancel.check()?;
        return Ok(out);
    }
    let Some(token) = token(&data) else {
        incomplete(
            &mut out,
            "Spotify did not expose an anonymous public session; the embed list may be truncated.",
        );
        cancel.check()?;
        return Ok(out);
    };
    let auth = format!("Bearer {token}");
    let mut ids = Vec::new();
    let mut expected_total = None;
    let mut complete = false;
    for page_number in 0..PAGE_CAP {
        cancel.check()?;
        let endpoint = format!(
            "https://spclient.wg.spotify.com/playlist/v2/playlist/{}?from={}&length=100",
            target.id,
            ids.len()
        );
        let page = match public_http::get_json(Platform::Spotify, &endpoint, Some(&auth), cancel)
            .await
        {
            Ok(v) => v,
            Err(_) => {
                cancel.check()?;
                incomplete(&mut out, "Spotify could not provide the full public playlist; only verified entries are shown.");
                break;
            }
        };
        let (mut page_ids, total, truncated) = match uri_page(&page, ids.len()) {
            Ok(v) => v,
            Err(e) => {
                incomplete(&mut out, &e);
                break;
            }
        };
        if expected_total.is_some_and(|n| n != total) {
            incomplete(&mut out, "Spotify changed the playlist while importing.");
            break;
        }
        expected_total = Some(total);
        let remaining = TRACK_CAP.saturating_sub(ids.len());
        page_ids.truncate(remaining);
        ids.extend(page_ids);
        if ids.len() == total && !truncated {
            complete = true;
            break;
        }
        if ids.len() >= TRACK_CAP || page_number + 1 == PAGE_CAP {
            incomplete(&mut out, "Spotify playlist pagination limit reached.");
            break;
        }
    }
    if !ids.is_empty() || complete {
        out.tracks.clear();
        // Canonical slots supersede the embed sample, including its missing
        // rows; report skips only if they also occur in the canonical list.
        out.warnings
            .retain(|w| w != "An unavailable or unsupported Spotify entry was skipped.");
        let (tracks, skipped, missing) = hydrate(ids, cache, cancel, |id| async move {
            let value = match embed("track", &id, cancel).await {
                Ok(data) => entity(&data)
                    .ok()
                    .and_then(|v| track(v, None, None))
                    .filter(|t| t.origin.external_id == id),
                Err(_) => {
                    cancel.check()?;
                    None
                }
            };
            Ok(value)
        })
        .await?;
        out.tracks = tracks;
        for _ in 0..skipped {
            out.warnings
                .push("An unavailable or unsupported Spotify entry was skipped.".into());
        }
        if missing {
            incomplete(&mut out, "Spotify could not provide a track's public metadata; only the verified ordered prefix is shown.");
        }
    }
    cancel.check()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: &str) -> Value {
        json!({"uri":format!("spotify:track:{id}"),"title":"Song","subtitle":"First,\u{a0}Second","duration":123456,"isExplicit":false})
    }

    #[test]
    fn enrichment_fills_partial_embed_metadata_without_overwriting_known_fields() {
        let mut cached = track(&row("a"), None, None).unwrap();
        let mut detail = track(&row("a"), Some("Album"), Some("https://i.scdn.co/image/cover")).unwrap();
        detail.metadata.title = "Alternate title".into();
        detail.metadata.duration_ms = Some(99);
        detail.metadata.explicit = Some(true);
        merge_missing_metadata(&mut cached.metadata, detail.metadata);
        assert_eq!(cached.metadata.title, "Song");
        assert_eq!(cached.metadata.duration_ms, Some(123456));
        assert_eq!(cached.metadata.explicit, Some(false));
        assert_eq!(cached.metadata.album.as_deref(), Some("Album"));
        assert_eq!(cached.metadata.artwork_url.as_deref(), Some("https://i.scdn.co/image/cover"));
    }

    #[tokio::test]
    async fn hydration_retains_partial_metadata_if_optional_enrichment_fails() {
        let cached_track = track(&row("a"), None, None).unwrap();
        let cached = HashMap::from([("a".into(), cached_track.clone())]);
        let (tracks, _, incomplete) = hydrate(vec![Some("a".into())], cached,
            &Cancellation::default(), |_| async { Ok(None) }).await.unwrap();
        assert_eq!(tracks, [cached_track]);
        assert!(!incomplete);
    }

    #[tokio::test]
    async fn hydration_overlaps_unique_requests_and_preserves_occurrences() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let calls = std::sync::Mutex::new(Vec::new());
        let ids = ["a", "b", "a", "c", "b"]
            .map(|id| Some(id.to_owned()))
            .to_vec();
        let (tracks, skipped, incomplete) =
            hydrate(ids, HashMap::new(), &Cancellation::default(), |id| {
                let active = &active;
                let peak = &peak;
                let calls = &calls;
                async move {
                    calls.lock().unwrap().push(id.clone());
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(if id == "a" {
                        40
                    } else {
                        5
                    }))
                    .await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(track(&row(&id), None, None))
                }
            })
            .await
            .unwrap();
        assert_eq!(
            tracks
                .iter()
                .map(|t| t.origin.external_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "a", "c", "b"]
        );
        assert_eq!(calls.lock().unwrap().len(), 3);
        assert_eq!(
            peak.load(Ordering::SeqCst),
            2,
            "overlap latency without exceeding the existing HTTP bound"
        );
        assert_eq!(skipped, 0);
        assert!(!incomplete);
    }

    #[tokio::test]
    async fn hydration_reuses_cached_details_and_keeps_verified_prefix_on_failure() {
        let ids = [
            Some("a".into()),
            None,
            Some("a".into()),
            Some("b".into()),
            Some("c".into()),
        ]
        .to_vec();
        let cached = HashMap::from([("a".into(), track(&row("a"), None, None).unwrap())]);
        let calls = std::sync::Mutex::new(Vec::new());
        let (tracks, skipped, incomplete) = hydrate(ids, cached, &Cancellation::default(), |id| {
            calls.lock().unwrap().push(id.clone());
            async move {
                Ok(if id == "b" {
                    None
                } else {
                    track(&row(&id), None, None)
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(
            tracks
                .iter()
                .map(|t| t.origin.external_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "a"]
        );
        assert_eq!(skipped, 1);
        assert!(incomplete);
        assert!(!calls.lock().unwrap().iter().any(|id| id == "a"));
    }

    #[tokio::test]
    async fn hydration_cancellation_drops_pending_metadata_requests() {
        let cancel = Cancellation::default();
        let fetches = std::sync::atomic::AtomicUsize::new(0);
        let hydrate = hydrate(
            vec![Some("a".into()), Some("b".into())],
            HashMap::new(),
            &cancel,
            |_| {
                fetches.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::pending::<Result<Option<ImportedTrack>, String>>()
            },
        );
        tokio::pin!(hydrate);
        tokio::select! {
            _ = &mut hydrate => panic!("unfinished requests completed"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => cancel.cancel(),
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), hydrate)
                .await
                .unwrap()
                .unwrap_err()
                .contains("cancelled")
        );
        assert_eq!(fetches.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn structured_embed_extracts_entity_without_executing_scripts() {
        let data = json!({"props":{"pageProps":{"state":{"data":{"entity":{"name":"Mix","trackList":[row("abc")]}}}}}});
        let html =
            format!("<script nonce='x' id='__NEXT_DATA__' type='application/json'>{data}</script>");
        let parsed = parse_embed(&html).unwrap();
        assert_eq!(entity(&parsed).unwrap()["name"], "Mix");
        assert!(parse_embed("<script>window.secret='x'</script>").is_err());
        assert!(parse_embed("<script id='__NEXT_DATA__'>{bad}</script>").is_err());
    }

    #[test]
    fn track_metadata_is_conservative_and_retains_artists() {
        let track = track(&row("abc"), Some("Album"), None).unwrap();
        assert_eq!(track.origin.external_id, "abc");
        assert_eq!(track.metadata.artists, vec!["First", "Second"]);
        assert_eq!(track.metadata.album.as_deref(), Some("Album"));
        assert_eq!(track.metadata.duration_ms, Some(123456));
        assert_eq!(track.metadata.explicit, Some(false));
        assert!(track.metadata.isrc.is_none());
        let artist_with_comma = super::track(
            &json!({"uri":"spotify:track:abc","title":"Song","subtitle":"Tyler, The Creator,\u{a0}Guest"}),
            None,
            None,
        ).unwrap();
        assert_eq!(
            artist_with_comma.metadata.artists,
            vec!["Tyler, The Creator", "Guest"]
        );
        assert!(super::track(
            &json!({"uri":"spotify:episode:a","title":"Episode"}),
            None,
            None
        )
        .is_none());
        assert!(super::track(&json!({"uri":"spotify:track:a"}), None, None).is_none());
        assert!(track_id("spotify:track:../../evil").is_none());
    }

    #[test]
    fn canonical_uri_pages_keep_duplicates_and_reject_noncontiguous_pages() {
        let page = json!({"length":3,"contents":{"pos":0,"truncated":true,"items":[{"uri":"spotify:track:b"},{"uri":"spotify:track:a"},{"uri":"spotify:track:b"}]}});
        let (ids, total, more) = uri_page(&page, 0).unwrap();
        assert_eq!(
            ids,
            vec![Some("b".into()), Some("a".into()), Some("b".into())]
        );
        assert_eq!(total, 3);
        assert!(more);
        assert!(uri_page(&page, 3).is_err());
        assert!(uri_page(&json!({"length":5,"contents":{"pos":0,"items":[]}}), 0).is_err());
        let final_page = json!({"length":150,"contents":{"pos":100,"truncated":false,"items":vec![json!({"uri":"spotify:track:abc"});50]}});
        let (ids, total, truncated) = uri_page(&final_page, 100).unwrap();
        assert_eq!(ids.len(), 50);
        assert_eq!(total, 150);
        assert!(!truncated);
        assert!(uri_page(&json!({"length":1,"contents":{"pos":0,"items":[{},{}]}}), 0).is_err());
    }

    #[test]
    fn missing_fields_stay_unknown_and_structured_artists_take_precedence() {
        let data = json!({"uri":"spotify:track:abc","name":"Song","artists":[{"name":"Real, Artist"}],"subtitle":"Collection owner"});
        let result = track(&data, None, None).unwrap();
        assert_eq!(result.metadata.artists, vec!["Real, Artist"]);
        assert_eq!(result.metadata.duration_ms, None);
        assert_eq!(result.metadata.explicit, None);
        assert_eq!(result.metadata.artwork_url, None);
        assert_eq!(result.metadata.album, None);
    }

    #[tokio::test]
    async fn pre_cancelled_import_never_requests_public_metadata() {
        let target = super::super::urls::detect("https://open.spotify.com/playlist/abc").unwrap();
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(fetch(&target, &cancel)
            .await
            .unwrap_err()
            .contains("cancelled"));
    }

    #[tokio::test]
    #[ignore = "Live anonymous Spotify network check; provider and region availability vary"]
    async fn live_public_track_and_album_without_credentials() {
        for path in [
            "track/4uLU6hMCjMI75M1A2tKUQC",
            "album/4aawyAB9vmqN3uQ7FjRGTy",
        ] {
            let target =
                super::super::urls::detect(&format!("https://open.spotify.com/{path}")).unwrap();
            let out = fetch(&target, &Cancellation::default()).await.unwrap();
            assert!(!out.tracks.is_empty());
            assert!(!out.title.is_empty());
            assert!(out
                .tracks
                .iter()
                .all(|t| !t.metadata.title.is_empty() && !t.metadata.artists.is_empty()));
            assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        }
    }

    #[tokio::test]
    #[ignore = "Live anonymous Spotify pagination + up to 50 extra track metadata requests"]
    async fn live_public_playlist_beyond_one_hundred_without_credentials() {
        let target =
            super::super::urls::detect("https://open.spotify.com/playlist/37i9dQZF1DX5Ejj0EkURtP")
                .unwrap();
        let out = fetch(&target, &Cancellation::default()).await.unwrap();
        // This public editorial playlist contained 150 entries when checked;
        // require crossing the embed cap without hardcoding a mutable total.
        assert!(
            out.tracks.len() > 100,
            "{} tracks; {:?}",
            out.tracks.len(),
            out.warnings
        );
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert!(out
            .tracks
            .iter()
            .all(|t| !t.metadata.title.is_empty() && !t.metadata.artists.is_empty()));
    }
}
