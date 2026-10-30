//! Anonymous metadata from the Apple Music public website.
use super::{
    http::Cancellation,
    model::*,
    urls::{ContentKind, ImportUrl},
};
use base64::Engine;
use serde_json::Value;
use std::collections::HashSet;

const TRACK_CAP: usize = 10_000;
const PAGE_CAP: usize = 200;
const API: &str = "https://amp-api.music.apple.com";

fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k)?.as_str().filter(|s| !s.trim().is_empty())
}
fn artwork(v: &Value) -> Option<String> {
    let raw = v
        .pointer("/artwork/dictionary/url")
        .or_else(|| v.pointer("/artwork/url"))?
        .as_str()?;
    let raw = raw
        .replace("{w}", "600")
        .replace("{h}", "600")
        .replace("{f}", "jpg");
    let u = url::Url::parse(&raw).ok()?;
    (u.scheme() == "https" && u.username().is_empty() && u.password().is_none()).then_some(raw)
}
fn section_items(v: &Value) -> &[Value] {
    v.get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn scripts(html: &str) -> Vec<(&str, &str)> {
    let mut rest = html;
    let mut out = Vec::new();
    while let Some(start) = rest.find("<script") {
        rest = &rest[start + 7..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        let Some(end) = rest.find("</script>") else {
            break;
        };
        out.push((tag, &rest[..end]));
        rest = &rest[end + 9..];
        if out.len() >= 256 {
            break;
        }
    }
    out
}
fn attr<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let at = tag.find(&needle)?;
    if at != 0 && !tag.as_bytes()[at - 1].is_ascii_whitespace() {
        return None;
    }
    let raw = &tag[at + needle.len()..];
    match raw.as_bytes().first()? {
        b'\'' | b'"' => {
            let quote = raw.as_bytes()[0] as char;
            let raw = &raw[1..];
            Some(&raw[..raw.find(quote)?])
        }
        _ => Some(raw.split_ascii_whitespace().next()?.trim_end_matches('/')),
    }
}
fn identifier(v: &Value) -> Option<&str> {
    let id = v
        .pointer("/contentDescriptor/identifiers/storeAdamID")?
        .as_str()?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}
fn page_track(v: &Value, storefront: &str, album: Option<&str>) -> Option<ImportedTrack> {
    if text(&v["contentDescriptor"], "kind") != Some("song") {
        return None;
    }
    let id = identifier(v)?;
    let artists: Vec<String> = text(v, "artistName")
        .or_else(|| text(v, "artists"))
        .map(|s| vec![s.to_string()])
        .unwrap_or_else(|| {
            v.get("subtitleLinks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| text(v, "title").map(str::to_owned))
                .collect()
        });
    Some(ImportedTrack {
        origin: Origin {
            platform: Platform::AppleMusic,
            external_id: id.into(),
            url: Some(format!("https://music.apple.com/{storefront}/song/{id}")),
        },
        metadata: Metadata {
            title: text(v, "title")?.into(),
            artists,
            album: text(v, "album")
                .or_else(|| v.pointer("/tertiaryLinks/0/title").and_then(Value::as_str))
                .or(album)
                .map(str::to_owned),
            duration_ms: v.get("duration").and_then(Value::as_u64),
            artwork_url: artwork(v),
            isrc: None,
            explicit: v.get("showExplicitBadge").and_then(Value::as_bool),
        },
    })
}

fn parse_page(html: &str, target: &ImportUrl) -> Result<(ImportedCollection, Option<u64>), String> {
    let (_, raw) = scripts(html)
        .into_iter()
        .find(|(tag, _)| attr(tag, "id") == Some("serialized-server-data"))
        .ok_or("Apple Music public page metadata is unavailable.")?;
    let data: Value = serde_json::from_str(raw)
        .map_err(|_| "Apple Music public page metadata could not be read.")?;
    let sections = data
        .pointer("/data/0/data/sections")
        .and_then(Value::as_array)
        .ok_or("Apple Music public collection is unavailable.")?;
    let expected_kind = if target.kind == ContentKind::Track {
        "song"
    } else if target.kind == ContentKind::Album {
        "album"
    } else {
        "playlist"
    };
    let header = sections
        .iter()
        .filter(|s| {
            matches!(
                text(s, "itemKind"),
                Some("containerDetailHeaderLockup" | "songDetailHeader")
            )
        })
        .flat_map(section_items)
        .find(|v| {
            text(&v["contentDescriptor"], "kind") == Some(expected_kind)
                && v.pointer("/contentDescriptor/identifiers/storeAdamID")
                    .and_then(Value::as_str)
                    == Some(target.id.as_str())
        })
        .ok_or("Apple Music returned a different or unavailable collection.")?;
    let mut out = ImportedCollection {
        source_url: target.url.clone(),
        provider: Platform::AppleMusic,
        kind: target.kind,
        title: text(header, "title")
            .ok_or("Apple Music collection title is unavailable.")?
            .into(),
        artwork_url: artwork(header),
        tracks: Vec::new(),
        warnings: Vec::new(),
    };
    let album = (target.kind == ContentKind::Album).then_some(out.title.clone());
    let rows: Vec<&Value> = if target.kind == ContentKind::Track {
        vec![header]
    } else {
        sections
            .iter()
            .filter(|s| text(s, "itemKind") == Some("trackLockup"))
            .flat_map(section_items)
            .collect()
    };
    for row in rows.iter().take(TRACK_CAP) {
        if let Some(track) = page_track(
            row,
            target.storefront.as_deref().unwrap_or("us"),
            album.as_deref(),
        ) {
            out.tracks.push(track);
        } else {
            out.warnings
                .push("A missing or unsupported Apple Music track was skipped.".into());
        }
    }
    if rows.len() > TRACK_CAP {
        out.warnings
            .push("Import incomplete: Apple Music track limit reached.".into());
    }
    let count = if target.kind == ContentKind::Track {
        Some(1)
    } else {
        header.get("trackCount").and_then(Value::as_u64)
    };
    Ok((out, count))
}
fn collection_path(target: &ImportUrl) -> String {
    format!(
        "/v1/catalog/{}/{}/{}",
        target.storefront.as_deref().unwrap_or("us"),
        match target.kind {
            ContentKind::Track => "songs",
            ContentKind::Album => "albums",
            _ => "playlists",
        },
        target.id
    )
}
fn continuation(raw: &str, target: &ImportUrl) -> Result<String, String> {
    let base = url::Url::parse(API).unwrap();
    let u = base
        .join(raw)
        .map_err(|_| "Invalid Apple Music continuation.")?;
    let expected = format!("{}/tracks", collection_path(target));
    if u.scheme() != "https"
        || u.host_str() != Some("amp-api.music.apple.com")
        || u.port_or_known_default() != Some(443)
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.path() != expected
    {
        return Err("Unsafe Apple Music continuation was rejected.".into());
    }
    let mut seen = HashSet::new();
    for (k, v) in u.query_pairs() {
        let valid = match k.as_ref() {
            "offset" | "limit" => !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()),
            "l" => !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            _ => false,
        };
        if !valid || !seen.insert(k.into_owned()) {
            return Err("Unsafe Apple Music pagination parameters were rejected.".into());
        }
    }
    Ok(u.to_string())
}

fn public_token(js: &str) -> Option<String> {
    // Only Apple's public web application token. Never inspect local credentials,
    // cookies, Music User Tokens, or user storage.
    for candidate in
        js.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
    {
        if candidate.len() > 4096 || !candidate.starts_with("eyJ") {
            continue;
        }
        let parts: Vec<_> = candidate.split('.').collect();
        if parts.len() != 3 {
            continue;
        }
        let Some(payload) = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        if text(&payload, "iss") == Some("AMPWebPlay")
            && payload
                .get("root_https_origin")
                .and_then(Value::as_array)
                .is_some_and(|v| v.iter().any(|s| s.as_str() == Some("apple.com")))
        {
            return Some(candidate.into());
        }
    }
    None
}
async fn web_token(html: &str, cancel: &Cancellation) -> Result<String, String> {
    if let Some(token) = public_token(html) {
        return Ok(token);
    }
    let src = scripts(html)
        .into_iter()
        .filter_map(|(tag, _)| attr(tag, "src"))
        .find(|s| {
            s.starts_with("/assets/index~")
                && s.ends_with(".js")
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/._~-".contains(&b))
        })
        .ok_or("Apple Music public catalog configuration is unavailable.")?;
    let js = super::public_http::get_text(
        Platform::AppleMusic,
        &format!("https://music.apple.com{src}"),
        None,
        cancel,
    )
    .await?;
    public_token(&js).ok_or_else(|| "Apple Music public catalog access is unavailable.".into())
}
fn catalog_track(v: &Value, storefront: &str) -> Option<ImportedTrack> {
    if text(v, "type") != Some("songs") {
        return None;
    }
    let id = text(v, "id")?;
    if !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let a = &v["attributes"];
    Some(ImportedTrack {
        origin: Origin {
            platform: Platform::AppleMusic,
            external_id: id.into(),
            url: Some(format!("https://music.apple.com/{storefront}/song/{id}")),
        },
        metadata: Metadata {
            title: text(a, "name")?.into(),
            artists: text(a, "artistName")
                .map(|s| vec![s.into()])
                .unwrap_or_default(),
            album: text(a, "albumName").map(str::to_owned),
            duration_ms: a.get("durationInMillis").and_then(Value::as_u64),
            artwork_url: artwork(a),
            isrc: text(a, "isrc").map(str::to_owned),
            explicit: text(a, "contentRating").map(|v| v == "explicit"),
        },
    })
}
async fn catalog(
    target: &ImportUrl,
    html: &str,
    cancel: &Cancellation,
) -> Result<ImportedCollection, String> {
    let token = web_token(html, cancel).await?;
    let authorization = format!("Bearer {token}");
    let root = super::public_http::get_json(
        Platform::AppleMusic,
        &format!("{API}{}", collection_path(target)),
        Some(&authorization),
        cancel,
    )
    .await?;
    let item = root
        .pointer("/data/0")
        .ok_or("Apple Music public catalog collection is unavailable.")?;
    if text(item, "id") != Some(target.id.as_str()) {
        return Err("Apple Music returned a different catalog collection.".into());
    }
    let mut out = ImportedCollection {
        source_url: target.url.clone(),
        provider: Platform::AppleMusic,
        kind: target.kind,
        title: text(&item["attributes"], "name")
            .ok_or("Apple Music collection title is unavailable.")?
            .into(),
        artwork_url: artwork(&item["attributes"]),
        tracks: Vec::new(),
        warnings: Vec::new(),
    };
    if target.kind == ContentKind::Track {
        out.tracks.push(
            catalog_track(item, target.storefront.as_deref().unwrap_or("us"))
                .ok_or("Apple Music song metadata is unavailable.")?,
        );
        return Ok(out);
    }
    let mut page = item
        .pointer("/relationships/tracks")
        .cloned()
        .ok_or("Apple Music tracks are unavailable.")?;
    let mut seen = HashSet::new();
    for n in 0..PAGE_CAP {
        cancel.check()?;
        let rows = match page.get("data").and_then(Value::as_array) {
            Some(rows) => rows,
            None if !out.tracks.is_empty() => {
                out.warnings.push(
                    "Import incomplete: Apple Music returned an unreadable track page.".into(),
                );
                break;
            }
            None => return Err("Apple Music track page is unavailable.".into()),
        };
        for row in rows {
            if out.tracks.len() + out.warnings.len() >= TRACK_CAP {
                out.warnings
                    .push("Import incomplete: Apple Music track limit reached.".into());
                break;
            }
            if let Some(track) = catalog_track(row, target.storefront.as_deref().unwrap_or("us")) {
                out.tracks.push(track);
            } else {
                out.warnings
                    .push("A missing or unsupported Apple Music track was skipped.".into());
            }
        }
        let Some(raw) = text(&page, "next") else {
            break;
        };
        if n + 1 == PAGE_CAP || out.tracks.len() + out.warnings.len() >= TRACK_CAP {
            out.warnings
                .push("Import incomplete: Apple Music pagination limit reached.".into());
            break;
        }
        let next = match continuation(raw, target) {
            Ok(v) => v,
            Err(e) => {
                out.warnings.push(format!("Import incomplete: {e}"));
                break;
            }
        };
        if !seen.insert(next.clone()) {
            out.warnings
                .push("Import incomplete: Apple Music repeated a track page.".into());
            break;
        }
        match super::public_http::get_json(
            Platform::AppleMusic,
            &next,
            Some(&authorization),
            cancel,
        )
        .await
        {
            Ok(v) => page = v,
            Err(e) => {
                cancel.check()?;
                out.warnings.push(format!("Import incomplete: {e}"));
                break;
            }
        }
    }
    Ok(out)
}

pub async fn fetch(
    target: &ImportUrl,
    cancel: &Cancellation,
) -> Result<ImportedCollection, String> {
    cancel.check()?;
    let validated = super::urls::detect(&target.url)?;
    if &validated != target
        || target.provider != Platform::AppleMusic
        || !matches!(
            target.kind,
            ContentKind::Track | ContentKind::Album | ContentKind::Playlist
        )
    {
        return Err("Use a public Apple Music song, album, or playlist link.".into());
    }
    let html =
        super::public_http::get_text(Platform::AppleMusic, &target.url, None, cancel).await?;
    let parsed = parse_page(&html, target);
    if let Ok((out, Some(expected))) = &parsed {
        if expected == &(out.tracks.len() as u64)
            && (target.kind != ContentKind::Track
                || out
                    .tracks
                    .first()
                    .is_some_and(|t| t.metadata.duration_ms.is_some()))
        {
            cancel.check()?;
            return Ok(out.clone());
        }
    }
    match catalog(target, &html, cancel).await {
        Ok(mut out) => {
            cancel.check()?;
            if let Ok((_, Some(expected))) = &parsed {
                // The public page count is independent evidence against a catalog
                // response silently returning only a preview without `next`.
                let accounted = out.tracks.len() + out.warnings.len();
                if *expected > accounted as u64
                    && !out.warnings.iter().any(|w| w.contains("incomplete"))
                {
                    out.warnings.push(format!("Import incomplete: Apple Music returned {} of {expected} collection entries.",out.tracks.len()));
                }
            }
            Ok(out)
        }
        Err(error) => {
            cancel.check()?;
            let (mut out, expected) = parsed.map_err(|_| error.clone())?;
            if out.tracks.is_empty() {
                return Err(error);
            }
            if target.kind != ContentKind::Track && expected != Some(out.tracks.len() as u64) {
                out.warnings.push(format!("Import incomplete: imported {} of {} Apple Music tracks; additional public metadata is unavailable. {error}",out.tracks.len(),expected.map(|n|n.to_string()).unwrap_or_else(||"an unknown number of".into())));
            }
            if target.kind == ContentKind::Track
                && out
                    .tracks
                    .first()
                    .is_some_and(|t| t.metadata.duration_ms.is_none())
            {
                out.warnings.push("Apple Music song duration is unavailable; public catalog metadata could not be retrieved.".into());
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn target() -> ImportUrl {
        super::super::urls::detect("https://music.apple.com/us/playlist/mix/pl.test").unwrap()
    }
    fn row(id: &str) -> Value {
        json!({"title":"Actual song", "artistName":"Actual artist", "duration":123456,"showExplicitBadge":false,"contentDescriptor":{"kind":"song","identifiers":{"storeAdamID":id}}})
    }
    fn html(rows: Vec<Value>, count: u64) -> String {
        format!(
            "<script id=\"serialized-server-data\" type=\"application/json\">{}</script>",
            json!({"data":[{"data":{"sections":[{"itemKind":"containerDetailHeaderLockup","items":[{"title":"Actual mix","trackCount":count,"contentDescriptor":{"kind":"playlist","identifiers":{"storeAdamID":"pl.test"}}}]},{"itemKind":"trackLockup","items":rows},{"itemKind":"squareLockup","items":[row("999")]}]}}]})
        )
    }
    #[test]
    fn page_tracks_preserve_duplicates_and_ignore_recommendations() {
        let (out, count) = parse_page(&html(vec![row("123"), row("123")], 2), &target()).unwrap();
        assert_eq!(out.title, "Actual mix");
        assert_eq!(count, Some(2));
        assert_eq!(out.tracks.len(), 2);
        assert_eq!(out.tracks[0], out.tracks[1]);
        assert_eq!(out.tracks[0].metadata.duration_ms, Some(123456));
        assert_eq!(out.tracks[0].metadata.album, None);
    }
    #[test]
    fn malformed_rows_are_not_fabricated_and_missing_rows_warn() {
        let (out, _) = parse_page(
            &html(vec![row("123"), json!({"title":"Missing identifier"})], 3),
            &target(),
        )
        .unwrap();
        assert_eq!(out.tracks.len(), 1);
        assert!(!out.warnings.is_empty());
    }
    #[test]
    fn continuation_is_bound_to_same_catalog_collection() {
        assert!(continuation(
            "/v1/catalog/us/playlists/pl.test/tracks?offset=100",
            &target()
        )
        .is_ok());
        for raw in [
            "https://evil.test/v1/catalog/us/playlists/pl.test/tracks",
            "//evil.test/x",
            "/v1/me/library/playlists/x",
            "/v1/catalog/us/playlists/pl.other/tracks",
            "/v1/catalog/us/playlists/pl.test/tracks?token=secret",
            "/v1/catalog/us/playlists/pl.test/tracks?offset=1#x",
        ] {
            assert!(continuation(raw, &target()).is_err(), "{raw}");
        }
    }
    #[test]
    fn catalog_metadata_does_not_treat_a_preview_as_the_song_duration() {
        let v = json!({"id":"123","type":"songs","attributes":{"name":"Song","artistName":"Artist","durationInMillis":210000,"previews":[{"duration":30}],"isrc":"USABC1234567","artwork":{"url":"https://example.com/{w}x{h}.{f}"}}});
        let track = catalog_track(&v, "us").unwrap();
        assert_eq!(track.metadata.duration_ms, Some(210000));
        assert_eq!(track.metadata.isrc.as_deref(), Some("USABC1234567"));
        assert_eq!(track.metadata.explicit, None);
        assert!(catalog_track(&json!({"id":"123","type":"songs","attributes":{}}), "us").is_none());
        assert!(catalog_track(
            &json!({"id":"../../x","type":"songs","attributes":{"name":"x"}}),
            "us"
        )
        .is_none());
    }
    #[test]
    fn rejects_unrelated_headers_and_only_reads_apple_public_web_tokens() {
        let mut other = target();
        other.id = "pl.other".into();
        assert!(parse_page(&html(vec![row("123")], 1), &other).is_err());
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"iss":"AMPWebPlay","root_https_origin":["apple.com"]}"#);
        let token = format!("eyJ0eXAiOiJKV1QifQ.{encoded}.signature");
        assert_eq!(
            public_token(&format!("const publicToken=\"{token}\";")),
            Some(token)
        );
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"iss":"User","root_https_origin":["apple.com"]}"#);
        assert!(public_token(&format!("eyJ0eXAiOiJKV1QifQ.{encoded}.signature")).is_none());
        assert!(public_token("eyJnot-valid.x.y").is_none());
    }
    #[tokio::test]
    #[ignore = "Live Apple Music public HTML and catalog; requires network"]
    async fn live_public_song() {
        let target =
            super::super::urls::detect("https://music.apple.com/us/song/1441164430").unwrap();
        let out = fetch(&target, &Cancellation::default()).await.unwrap();
        assert_eq!(out.tracks.len(), 1);
        assert_eq!(out.tracks[0].metadata.title, "Come Together");
        assert_eq!(out.tracks[0].metadata.duration_ms, Some(258947));
    }
    #[tokio::test]
    #[ignore = "Live Apple Music public HTML and catalog; requires network"]
    async fn live_public_album() {
        let target =
            super::super::urls::detect("https://music.apple.com/us/album/1441164426").unwrap();
        let out = fetch(&target, &Cancellation::default()).await.unwrap();
        assert_eq!(out.title, "Abbey Road (Remastered)");
        assert_eq!(out.tracks.len(), 17);
        assert_eq!(out.tracks[0].metadata.title, "Come Together");
    }
    #[tokio::test]
    #[ignore = "Live Apple Music public HTML and catalog; requires network"]
    async fn live_public_playlist() {
        let target = super::super::urls::detect(
            "https://music.apple.com/us/playlist/pl.1a7fd42205674dd282d106f533f4bea6",
        )
        .unwrap();
        let out = fetch(&target, &Cancellation::default()).await.unwrap();
        assert_eq!(out.title, "Classic Rock Essentials");
        assert_eq!(out.tracks.len(), 100);
        assert!(out.warnings.is_empty());
    }
    #[tokio::test]
    #[ignore = "Live Apple Music public catalog pagination; requires network"]
    async fn live_public_playlist_pagination() {
        let target = super::super::urls::detect(
            "https://music.apple.com/us/playlist/pl.1a7fd42205674dd282d106f533f4bea6",
        )
        .unwrap();
        let cancel = Cancellation::default();
        let html =
            super::super::public_http::get_text(Platform::AppleMusic, &target.url, None, &cancel)
                .await
                .unwrap();
        let token = web_token(&html, &cancel).await.unwrap();
        let authorization = format!("Bearer {token}");
        let mut next = format!("{API}{}/tracks?limit=25", collection_path(&target));
        let mut ids = Vec::new();
        let mut pages = 0;
        loop {
            let page = super::super::public_http::get_json(
                Platform::AppleMusic,
                &next,
                Some(&authorization),
                &cancel,
            )
            .await
            .unwrap();
            pages += 1;
            ids.extend(
                page["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v["id"].as_str().unwrap().to_owned()),
            );
            let Some(raw) = text(&page, "next") else {
                break;
            };
            next = continuation(raw, &target).unwrap();
            assert!(pages < 10);
        }
        assert_eq!(ids.len(), 100);
        assert!(pages > 1);
    }
}
