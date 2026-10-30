use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, UriSchemeContext, UriSchemeResponder};

use crate::AppState;

pub const SCHEME: &str = "artwork";

fn cache_dir(app_data: Option<PathBuf>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CAPSULE_ARTWORK_CACHE_PATH") {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    Some(app_data?.join("artwork"))
}

pub fn is_subsonic_ref(template: &str) -> bool {
    template.starts_with(crate::subsonic::ARTWORK_PREFIX)
}

pub fn subsonic_id(template: &str) -> Option<&str> {
    template.strip_prefix(crate::subsonic::ARTWORK_PREFIX)
}

pub fn resolve_template(template: &str, size: u32) -> String {
    if is_subsonic_ref(template) || template.starts_with(crate::local::ARTWORK_PREFIX) {
        return template.to_string();
    }
    if template.contains("{w}") || template.contains("{h}") {
        return template
            .replace("{w}", &size.to_string())
            .replace("{h}", &size.to_string())
            .replace("{f}", "jpg")
            .replace("{c}", "bb");
    }
    rewrite_literal_size(template, size).unwrap_or_else(|| template.to_string())
}

fn rewrite_literal_size(url: &str, size: u32) -> Option<String> {
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (url, None),
    };
    let slash = path.rfind('/')?;
    let (dir, file) = path.split_at(slash + 1);

    let bytes = file.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'x' {
            continue;
        }
        let x = i;
        i += 1;
        let h_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == h_start {
            continue;
        }
        let _ = x;
        let replaced = format!("{dir}{}{size}x{size}{}", &file[..start], &file[i..]);
        return Some(match query {
            Some(q) => format!("{replaced}?{q}"),
            None => replaced,
        });
    }
    None
}

fn cache_key(url: &str) -> String {
    let stable = match url.split_once('?') {
        None => url.to_string(),
        Some((path, query)) => {
            let kept: Vec<&str> = query
                .split('&')
                .filter(|p| !p.to_ascii_lowercase().starts_with("x-amz-"))
                .collect();
            if kept.is_empty() {
                path.to_string()
            } else {
                format!("{path}?{}", kept.join("&"))
            }
        }
    };
    let mut h = Sha256::new();
    h.update(stable.as_bytes());
    format!("{:x}", h.finalize())
}

fn content_type(url: &str) -> &'static str {
    let lower = url.to_ascii_lowercase();
    if lower.contains(".png") {
        "image/png"
    } else if lower.contains(".webp") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

pub fn parse_request(uri: &str) -> Option<(String, u32)> {
    let parsed = url::Url::parse(uri).ok()?;
    let id = parsed
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|s| !s.is_empty())
        .map(|s| {
            urlencoding::decode(s)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| s.to_string())
        })?;
    let raw = parsed
        .query_pairs()
        .find(|(k, _)| k == "w")
        .and_then(|(_, v)| v.parse::<u32>().ok())
        .unwrap_or(300);
    let size = if raw == 0 {
        0
    } else {
        raw.clamp(32, crate::db::MAX_ARTWORK)
    };
    Some((id, size))
}

pub fn handle(
    ctx: UriSchemeContext<'_, tauri::Wry>,
    req: http::Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    let uri = req.uri().to_string();

    tauri::async_runtime::spawn(async move {
        let response = serve(&app, &uri).await.unwrap_or_else(|e| {
            tracing::debug!(%uri, error = %e, "artwork miss");
            http::Response::builder()
                .status(404)
                .header("Content-Type", "text/plain")
                .body(Vec::new())
                .expect("static response builds")
        });
        responder.respond(response);
    });
}

pub fn cache_path(dir: &Path, template: &str, size: u32) -> PathBuf {
    dir.join(format!("{}-{}.img", cache_key(template), size))
}

pub fn signer(app: &AppHandle) -> Option<std::sync::Arc<crate::subsonic::Client>> {
    app.state::<AppState>()
        .navidrome
        .lock()
        .expect("navidrome mutex")
        .clone()
}

pub async fn ensure_cached(
    dir: &Path,
    template: &str,
    size: u32,
    signer: Option<&crate::subsonic::Client>,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let path = cache_path(dir, template, size);
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes);
    }

    if let Some(file) = template.strip_prefix(crate::local::ARTWORK_PREFIX) {
        let file = std::path::PathBuf::from(file);
        let bytes = tokio::task::spawn_blocking(move || crate::local::embedded_picture(&file))
            .await
            .map_err(|e| format!("artwork read failed: {e}"))?
            .ok_or("this file has no embedded cover")?;
        if let Err(e) = tokio::fs::create_dir_all(dir).await {
            tracing::debug!(error = %e, "artwork cache dir");
        } else if let Err(e) = tokio::fs::write(&path, &bytes).await {
            tracing::debug!(error = %e, "artwork cache write");
        }
        return Ok(bytes);
    }

    let url = match subsonic_id(template) {
        Some(id) => {
            let client = signer.ok_or("subsonic artwork requested with no active client")?;
            client.signed_url(
                "getCoverArt",
                &[("id", id.to_string()), ("size", size.to_string())],
            )
        }
        None => resolve_template(template, size),
    };

    let resp = reqwest::get(&url).await.map_err(|e| {
        let what = if e.is_timeout() {
            "timed out"
        } else {
            "could not be fetched"
        };
        format!("artwork {what}")
    })?;
    if !resp.status().is_success() {
        return Err(format!("cdn {}", resp.status()).into());
    }
    let bytes = resp.bytes().await?.to_vec();

    if let Err(e) = tokio::fs::create_dir_all(dir).await {
        tracing::debug!(error = %e, "artwork cache dir");
    } else if let Err(e) = tokio::fs::write(&path, &bytes).await {
        tracing::debug!(error = %e, "artwork cache write");
    }
    Ok(bytes)
}

pub async fn prefetch(app: tauri::AppHandle, size: u32) {
    let Some(dir) = cache_dir(app.path().app_data_dir().ok()) else {
        return;
    };

    let art: Vec<crate::db::Artwork> = {
        let state = app.state::<AppState>();
        let db = state.db.lock().expect("db mutex");
        match db.all_artwork() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "artwork prefetch query failed");
                return;
            }
        }
    };

    let nd = signer(&app);

    let total = art.len();
    let mut ok = 0usize;
    let mut bytes = 0usize;
    let mut unresizable = 0usize;
    for a in art {
        let want = a.clamp(size);
        match ensure_cached(&dir, &a.template, want, nd.as_deref()).await {
            Ok(b) => {
                ok += 1;
                bytes += b.len();
                if b.len() > 40_000 {
                    let resolved = resolve_template(&a.template, want);
                    if resolved == a.template {
                        unresizable += 1;
                        tracing::debug!(
                            kb = b.len() / 1024,
                            "artwork has no resizable size in its url; stored full-size"
                        );
                    } else {
                        tracing::warn!(
                            kb = b.len() / 1024,
                            requested = want,
                            url = %resolved,
                            "artwork resized but still large"
                        );
                    }
                }
            }
            Err(e) => tracing::debug!(error = %e, "artwork prefetch miss"),
        }
    }
    tracing::info!(
        cached = ok,
        total,
        size,
        kb = bytes / 1024,
        unresizable,
        "artwork prefetch done"
    );
}

fn imported_artwork_for(
    db: &crate::db::Db,
    id: &str,
) -> Result<Option<crate::db::Artwork>, crate::db::DbError> {
    let template = if let Some(track) = db.canonical_track(id)? {
        match track.metadata.artwork_url.filter(|url| !url.trim().is_empty()) {
            Some(url) => Some(url),
            None => {
                let mappings = db.mappings_for(id)?;
                db.resolution_candidates(id)?.into_iter()
                    .find(|candidate| mappings.iter().any(|mapping|
                        mapping.verified && mapping.provider == candidate.provider.as_str()
                            && mapping.external_id == candidate.external_id))
                    .and_then(|candidate| candidate.metadata.artwork_url)
            }
        }
    } else if let Some(playlist) = db
        .playlists()?
        .into_iter()
        .find(|playlist| playlist.id == id)
    {
        playlist.artwork_url
    } else {
        db.get_import_job(id)?
            .and_then(|job| job.collection_artwork)
    };
    Ok(template.map(|template| crate::db::Artwork {
        template,
        max_side: None,
    }))
}

// Imported provider metadata is untrusted. These image-only CDNs are the only
// destinations this branch can contact, including when a disk cache exists.
fn imported_artwork_url(raw: &str) -> Result<url::Url, &'static str> {
    if raw.len() > 8192 || raw.chars().any(|c| c.is_control() || c == '\\') {
        return Err("Invalid imported artwork URL.");
    }
    let url = url::Url::parse(raw).map_err(|_| "Invalid imported artwork URL.")?;
    let host = url.host_str().unwrap_or_default();
    let trusted = matches!(
        host,
        "i.scdn.co"
            | "mosaic.scdn.co"
            | "image-cdn-ak.spotifycdn.com"
            | "image-cdn-fa.spotifycdn.com"
            | "i.ytimg.com"
            | "yt3.ggpht.com"
            | "yt3.googleusercontent.com"
    ) || host.ends_with(".mzstatic.com")
        || host.ends_with(".sndcdn.com");
    if !trusted
        || url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Untrusted imported artwork URL.");
    }
    Ok(url)
}

const IMPORTED_ARTWORK_LIMIT: usize = 8 * 1024 * 1024;

fn raster_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if bytes.get(4..8) == Some(b"ftyp")
        && matches!(bytes.get(8..12), Some(b"avif" | b"avis"))
    {
        Some("image/avif")
    } else {
        None
    }
}

async fn imported_response_bytes(
    mut response: reqwest::Response,
) -> Result<(Vec<u8>, &'static str), &'static str> {
    if !response.status().is_success() {
        return Err("Imported artwork request was unsuccessful.");
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        mime.as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "image/avif"
    ) {
        return Err("Imported artwork must be a raster image.");
    }
    if response
        .content_length()
        .is_some_and(|length| length > IMPORTED_ARTWORK_LIMIT as u64)
    {
        return Err("Imported artwork exceeds the size limit.");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not read imported artwork.")?
    {
        if bytes.len().saturating_add(chunk.len()) > IMPORTED_ARTWORK_LIMIT {
            return Err("Imported artwork exceeds the size limit.");
        }
        bytes.extend_from_slice(&chunk);
    }
    let actual = raster_type(&bytes).ok_or("Imported artwork is not a supported raster image.")?;
    if actual != mime {
        return Err("Imported artwork has an unexpected image format.");
    }
    Ok((bytes, actual))
}

async fn ensure_import_cached(
    dir: &Path,
    template: &str,
    size: u32,
) -> Result<(Vec<u8>, &'static str), &'static str> {
    use std::{sync::OnceLock, time::Duration};
    static CLIENT: OnceLock<Result<reqwest::Client, reqwest::Error>> = OnceLock::new();
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let url = imported_artwork_url(&resolve_template(template, size))?;
    let path = cache_path(dir, template, size);
    if tokio::fs::metadata(&path)
        .await
        .is_ok_and(|metadata| metadata.len() <= IMPORTED_ARTWORK_LIMIT as u64)
    {
        if let Ok(bytes) = tokio::fs::read(&path).await {
            if bytes.len() <= IMPORTED_ARTWORK_LIMIT {
                if let Some(mime) = raster_type(&bytes) {
                    return Ok((bytes, mime));
                }
            }
        }
    }
    let _permit = tokio::time::timeout(Duration::from_secs(15), SLOTS.acquire())
        .await
        .map_err(|_| "Imported artwork service is busy.")?
        .map_err(|_| "Imported artwork service stopped.")?;
    let client = CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .connect_timeout(Duration::from_secs(5))
                .build()
        })
        .as_ref()
        .map_err(|_| "Could not initialize imported artwork service.")?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Imported artwork could not be fetched.")?;
    let (bytes, mime) = imported_response_bytes(response).await?;
    if tokio::fs::create_dir_all(dir).await.is_ok() {
        let _ = tokio::fs::write(&path, &bytes).await;
    }
    Ok((bytes, mime))
}

async fn serve(
    app: &tauri::AppHandle,
    uri: &str,
) -> Result<http::Response<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
    let (id, size) = parse_request(uri).ok_or("malformed artwork uri")?;

    let mut art = {
        let state = app.state::<AppState>();
        let db = state.db.lock().expect("db mutex");
        db.artwork_for(&id)?
    };

    // Imported albums have derived IDs; their covers live in the merged view,
    // not the selected provider's persisted cache. Keep its image-only transport.
    let mut projected_import = false;
    if art.is_none() {
        if let Some(template) = crate::commands::library_artwork_template(app, &id)? {
            art = Some(crate::db::Artwork { template, max_side: None });
            projected_import = true;
        }
    }

    if art.is_none() || projected_import {
        let imported = if projected_import { art } else {
            let state = app
                .try_state::<crate::import::State>()
                .ok_or("no artwork for id")?;
            let db = state
                .db
                .lock()
                .map_err(|_| "import artwork database is unavailable")?;
            imported_artwork_for(&db, &id)?
        }
        .ok_or("no artwork for id")?;
        let want = if size == 0 {
            imported.best_size()
        } else {
            imported.clamp(size)
        };
        let dir = cache_dir(app.path().app_data_dir().ok()).ok_or("no cache dir")?;
        let (bytes, mime) = ensure_import_cached(&dir, &imported.template, want).await?;
        return Ok(ok_response(bytes, mime));
    }
    let art = art.ok_or("no artwork for id")?;

    let want = if size == 0 {
        art.best_size()
    } else {
        art.clamp(size)
    };
    let dir = cache_dir(app.path().app_data_dir().ok()).ok_or("no cache dir")?;
    let bytes = ensure_cached(&dir, &art.template, want, signer(app).as_deref()).await?;
    Ok(ok_response(
        bytes,
        content_type(&resolve_template(&art.template, want)),
    ))
}

fn ok_response(bytes: Vec<u8>, mime: &str) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(200)
        .header("Content-Type", mime)
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "public, max-age=31536000, immutable")
        .body(bytes)
        .expect("response builds")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "Live public Spotify metadata and image-only CDN check"]
    async fn live_imported_spotify_covers_use_the_guarded_image_transport() {
        let dir = std::env::temp_dir().join(format!("capsule-live-art-{}", uuid::Uuid::new_v4()));
        for id in ["5NijSs5dAwaIybq1GaRTIe", "3a3dQOO19moXPeTt2PomoT", "0oufSLnKQDoBFX5mgkDCgR"] {
            let track = crate::import::public_spotify::fetch_track_metadata(id, &crate::import::http::Cancellation::default()).await.unwrap().unwrap();
            let url = track.metadata.artwork_url.expect("public track embed has cover art");
            let (bytes, mime) = ensure_import_cached(&dir, &url, 56).await.unwrap();
            assert!(!bytes.is_empty());
            assert!(mime.starts_with("image/"));
        }
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }

    #[test]
    fn imported_artwork_accepts_only_official_https_image_cdns() {
        for raw in [
            "https://i.scdn.co/image/a",
            "https://mosaic.scdn.co/300/a",
            "https://image-cdn-ak.spotifycdn.com/image/a",
            "https://image-cdn-fa.spotifycdn.com/image/ab67616d0000b2731a0d446b811ad07512eb19df",
            "https://is1-ssl.mzstatic.com/image/a.jpg",
            "https://i.ytimg.com/vi/a/default.jpg",
            "https://yt3.ggpht.com/a",
            "https://yt3.googleusercontent.com/a",
            "https://i1.sndcdn.com/a.jpg",
        ] {
            assert!(imported_artwork_url(raw).is_ok(), "{raw}");
        }
        for raw in [
            "http://i.scdn.co/a",
            "https://i.scdn.co.evil.example/a",
            "https://image-cdn-fa.spotifycdn.com.evil.example/a",
            "https://evil-mzstatic.com/a",
            "https://mzstatic.com.evil.example/a",
            "https://evil-sndcdn.com/a",
            "https://user:secret@i.scdn.co/a",
            "https://i.scdn.co:444/a",
            "https://i.scdn.co/a#fragment",
            "https://127.0.0.1/a",
            "file:///image.jpg",
            "local:C:/image.jpg",
            "https://i.scdn.co\\@evil.example/a",
            "https://evil.example/?cdn=i.scdn.co",
        ] {
            assert!(imported_artwork_url(raw).is_err(), "{raw}");
        }
    }

    fn image_response(status: u16, mime: &str, body: Vec<u8>) -> reqwest::Response {
        http::Response::builder()
            .status(status)
            .header("Content-Type", mime)
            .body(reqwest::Body::from(body))
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn imported_artwork_rejects_redirects_non_rasters_and_oversized_bodies() {
        let png = b"\x89PNG\r\n\x1a\nimage".to_vec();
        let (bytes, mime) = imported_response_bytes(image_response(200, "image/png", png.clone()))
            .await
            .unwrap();
        assert_eq!(bytes, png);
        assert_eq!(mime, "image/png");
        assert!(
            imported_response_bytes(image_response(302, "image/png", png.clone()))
                .await
                .is_err()
        );
        assert!(
            imported_response_bytes(image_response(200, "image/svg+xml", b"<svg/>".to_vec()))
                .await
                .is_err()
        );
        assert!(
            imported_response_bytes(image_response(200, "image/png", b"<html/>".to_vec()))
                .await
                .is_err()
        );
        assert!(imported_response_bytes(image_response(
            200,
            "image/png",
            vec![0; 8 * 1024 * 1024 + 1]
        ))
        .await
        .is_err());
    }

    #[tokio::test]
    async fn imported_artwork_cache_keeps_raster_mime_and_never_bypasses_host_validation() {
        let dir = std::env::temp_dir().join(format!("capsule-import-art-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let template = "https://i.scdn.co/image/cached-without-extension";
        let png = b"\x89PNG\r\n\x1a\ncached-image";
        tokio::fs::write(cache_path(&dir, template, 56), png)
            .await
            .unwrap();
        let (bytes, mime) = ensure_import_cached(&dir, template, 56).await.unwrap();
        assert_eq!(bytes, png);
        assert_eq!(mime, "image/png");
        let hostile = "https://evil.example/image.png";
        tokio::fs::write(cache_path(&dir, hostile, 56), png)
            .await
            .unwrap();
        assert!(ensure_import_cached(&dir, hostile, 56).await.is_err());
        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[test]
    fn imported_artwork_lookup_covers_tracks_playlists_and_pending_previews() {
        use crate::import::{
            model::*,
            urls::{detect, ContentKind},
        };
        let mut db = crate::db::Db::open_in_memory().unwrap();
        let job = db
            .create_import_job(&detect("https://open.spotify.com/playlist/abc").unwrap())
            .unwrap();
        let mut track = crate::import::model::fixture("a", "Song");
        track.metadata.artwork_url = Some("https://i.scdn.co/image/track".into());
        let collection = ImportedCollection {
            source_url: job.source_url.clone(),
            provider: Platform::Spotify,
            kind: ContentKind::Playlist,
            title: "Mix".into(),
            artwork_url: Some("https://i.scdn.co/image/mix".into()),
            tracks: vec![track],
            warnings: vec![],
        };
        db.set_import_preview(&job.id, &collection).unwrap();
        assert_eq!(
            imported_artwork_for(&db, &job.id)
                .unwrap()
                .unwrap()
                .template,
            "https://i.scdn.co/image/mix"
        );
        let playlist = db.confirm_import(&job.id).unwrap();
        assert_eq!(
            imported_artwork_for(&db, &playlist)
                .unwrap()
                .unwrap()
                .template,
            "https://i.scdn.co/image/mix"
        );
        let track = db.import_entries(&playlist).unwrap().remove(0).track;
        assert_eq!(
            imported_artwork_for(&db, &track.id)
                .unwrap()
                .unwrap()
                .template,
            "https://i.scdn.co/image/track"
        );
        assert!(imported_artwork_for(&db, "unknown").unwrap().is_none());
    }

    #[test]
    fn imported_artwork_uses_only_the_verified_recording_when_origin_cover_is_missing() {
        use crate::import::{model::*, urls::{detect, ContentKind}};
        let mut db = crate::db::Db::open_in_memory().unwrap();
        let job = db.create_import_job(&detect("https://open.spotify.com/playlist/abc").unwrap()).unwrap();
        let imported = fixture("a", "Song");
        db.set_import_preview(&job.id, &ImportedCollection {
            source_url: job.source_url.clone(), provider: Platform::Spotify,
            kind: ContentKind::Playlist, title: "Mix".into(), artwork_url: None,
            tracks: vec![imported.clone()], warnings: vec![],
        }).unwrap();
        let playlist = db.confirm_import(&job.id).unwrap();
        let id = db.import_entries(&playlist).unwrap().remove(0).track.id;
        let mut metadata = imported.metadata;
        metadata.artwork_url = Some("https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg".into());
        let candidate = PlaybackCandidate { provider: Platform::Youtube,
            external_id: "abcdefghijk".into(), metadata, confidence: 0.99, compatible: true };
        db.set_resolution(&id, "needs_review", std::slice::from_ref(&candidate), None).unwrap();
        assert!(imported_artwork_for(&db, &id).unwrap().is_none());
        db.verify_candidate(&id, Platform::Youtube, "abcdefghijk").unwrap();
        assert_eq!(imported_artwork_for(&db, &id).unwrap().unwrap().template,
            "https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg");
    }

    #[test]
    fn resolves_every_placeholder_apple_uses() {
        let t = "https://is1.mzstatic.com/image/thumb/x/{w}x{h}{c}.{f}";
        let out = resolve_template(t, 300);
        assert_eq!(out, "https://is1.mzstatic.com/image/thumb/x/300x300bb.jpg");
        assert!(!out.contains('{'), "unresolved placeholder: {out}");
    }

    #[test]
    fn template_without_any_size_is_left_alone() {
        let t = "https://example.com/cover.png";
        assert_eq!(resolve_template(t, 300), t);
    }

    #[test]
    fn subsonic_refs_are_recognised_and_parsed() {
        assert!(is_subsonic_ref("subsonic:al-1"));
        assert!(!is_subsonic_ref("https://example.com/{w}x{h}bb.jpg"));
        assert_eq!(subsonic_id("subsonic:al-1"), Some("al-1"));
        assert_eq!(subsonic_id("https://example.com/a.jpg"), None);
    }

    #[test]
    fn resolve_template_leaves_subsonic_refs_alone() {
        assert_eq!(resolve_template("subsonic:al-1", 300), "subsonic:al-1");
    }

    #[test]
    fn cache_key_is_stable_for_a_subsonic_ref() {
        let dir = Path::new(r"C:\cache");
        assert_eq!(
            cache_path(dir, "subsonic:al-1", 300),
            cache_path(dir, "subsonic:al-1", 300)
        );
        assert_ne!(
            cache_path(dir, "subsonic:al-1", 300),
            cache_path(dir, "subsonic:al-2", 300)
        );
    }

    #[tokio::test]
    async fn subsonic_artwork_without_a_client_errors_rather_than_fetching() {
        let dir = std::env::temp_dir().join(format!("saint-art-nosign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let res = ensure_cached(&dir, "subsonic:al-1", 56, None).await;
        assert!(
            res.is_err(),
            "must not attempt a fetch with no way to sign it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_playlist_cover_gets_its_literal_size_rewritten() {
        let t = "https://is1-ssl.mzstatic.com/image/thumb/gen/1200x1200AM.PDCXS11.jpg\
                 ?c1=9DB3F2&signature=6a9f5e93&t=abc%3D%3D&vkey=1";
        let out = resolve_template(t, 56);
        assert!(out.contains("/gen/56x56AM.PDCXS11.jpg"), "{out}");
        assert!(out.contains("signature=6a9f5e93"), "{out}");
        assert!(out.contains("c1=9DB3F2"), "{out}");
        assert!(!out.contains("1200x1200"), "{out}");
    }

    #[test]
    fn digits_in_the_query_string_are_never_rewritten() {
        let t = "https://cdn/image/gen/600x600A.jpg?signature=12x34&t=9x9";
        let out = resolve_template(t, 56);
        assert!(out.contains("/gen/56x56A.jpg"), "{out}");
        assert!(out.contains("signature=12x34"), "{out}");
        assert!(out.contains("t=9x9"), "{out}");
    }

    #[test]
    fn directory_names_with_digits_are_not_mistaken_for_a_size() {
        let t = "https://is1.mzstatic.com/image/thumb/Music116/v4/ab/cd/file.jpg/300x300bb.jpg";
        let out = resolve_template(t, 56);
        assert!(out.ends_with("/56x56bb.jpg"), "{out}");
        assert!(out.contains("Music116/v4"), "{out}");
    }

    #[test]
    fn placeholder_form_still_takes_precedence() {
        let t = "https://is1.mzstatic.com/image/thumb/x/{w}x{h}bb.{f}";
        assert_eq!(
            resolve_template(t, 300),
            "https://is1.mzstatic.com/image/thumb/x/300x300bb.jpg"
        );
    }

    #[test]
    fn a_lone_number_without_an_x_is_not_a_size() {
        let t = "https://cdn/image/gen/1200AM.jpg";
        assert_eq!(resolve_template(t, 56), t);
    }

    #[test]
    fn parses_id_and_size() {
        let (id, size) = parse_request("artwork://localhost/l.abc123?w=600").unwrap();
        assert_eq!(id, "l.abc123");
        assert_eq!(size, 600);
    }

    #[test]
    fn size_defaults_and_clamps() {
        assert_eq!(parse_request("artwork://localhost/x").unwrap().1, 300);
        assert_eq!(
            parse_request("artwork://localhost/x?w=99999").unwrap().1,
            3000
        );
        assert_eq!(parse_request("artwork://localhost/x?w=1").unwrap().1, 32);
        assert_eq!(parse_request("artwork://localhost/x?w=abc").unwrap().1, 300);
    }

    #[test]
    fn zero_is_the_sentinel_for_full_size() {
        assert_eq!(parse_request("artwork://localhost/x?w=0").unwrap().1, 0);
    }

    #[test]
    fn requests_never_exceed_the_source_resolution() {
        use crate::db::Artwork;
        let small = Artwork {
            template: "t".into(),
            max_side: Some(600),
        };
        assert_eq!(small.clamp(3000), 600);
        assert_eq!(small.best_size(), 600);
        assert_eq!(small.clamp(56), 56);
    }

    #[test]
    fn unknown_dimensions_fall_back_to_apples_ceiling() {
        use crate::db::{Artwork, MAX_ARTWORK};
        let unknown = Artwork {
            template: "t".into(),
            max_side: None,
        };
        assert_eq!(unknown.best_size(), MAX_ARTWORK);
        assert_eq!(unknown.clamp(56), 56);
    }

    #[test]
    fn oversized_source_is_still_capped_at_apples_ceiling() {
        use crate::db::{Artwork, MAX_ARTWORK};
        let huge = Artwork {
            template: "t".into(),
            max_side: Some(10_000),
        };
        assert_eq!(huge.best_size(), MAX_ARTWORK);
    }

    #[test]
    fn handles_the_http_rewrite_tauri_uses_on_windows() {
        let (id, size) = parse_request("http://artwork.localhost/i.999?w=64").unwrap();
        assert_eq!(id, "i.999");
        assert_eq!(size, 64);
    }

    #[test]
    fn percent_encoded_ids_round_trip() {
        let (id, _) = parse_request("artwork://localhost/l.a%20b?w=300").unwrap();
        assert_eq!(id, "l.a b");
    }

    #[test]
    fn empty_path_is_rejected_rather_than_served() {
        assert!(parse_request("artwork://localhost/").is_none());
    }

    #[tokio::test]
    async fn cache_hit_returns_bytes_without_network() {
        let dir = std::env::temp_dir().join(format!("saint-art-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let template = "https://never-fetched.invalid/{w}x{h}.jpg";
        let path = cache_path(&dir, template, 56);
        std::fs::write(&path, b"cached-bytes").unwrap();

        let got = ensure_cached(&dir, template, 56, None).await.unwrap();
        assert_eq!(got, b"cached-bytes");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unreachable_host_errors_rather_than_panicking() {
        let dir = std::env::temp_dir().join(format!("saint-art-fail-{}", std::process::id()));
        let res = ensure_cached(&dir, "https://nonexistent.invalid/{w}x{h}.jpg", 56, None).await;
        assert!(res.is_err(), "a dead CDN must surface as an error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_path_separates_sizes_of_the_same_art() {
        let dir = Path::new("C:/tmp");
        let t = "https://a/{w}x{h}.jpg";
        assert_ne!(cache_path(dir, t, 56), cache_path(dir, t, 300));
    }

    #[test]
    fn cache_key_is_stable_and_distinct() {
        assert_eq!(
            cache_key("https://a/{w}x{h}.jpg"),
            cache_key("https://a/{w}x{h}.jpg")
        );
        assert_ne!(cache_key("https://a/1.jpg"), cache_key("https://a/2.jpg"));
    }

    #[test]
    fn presigned_urls_key_the_same_despite_a_new_signature() {
        let base = "https://store-033.blobstore.apple.com/x/y/image";
        let a =
            format!("{base}?X-Amz-Date=20260726T153726Z&X-Amz-Signature=aaa&X-Amz-Expires=86400");
        let b =
            format!("{base}?X-Amz-Date=20260727T090000Z&X-Amz-Signature=bbb&X-Amz-Expires=86400");
        assert_eq!(cache_key(&a), cache_key(&b));
        assert_eq!(cache_key(&a), cache_key(base));
    }

    #[test]
    fn generated_covers_sharing_a_path_do_not_collide() {
        let one = "https://cdn/image/gen/1200x1200AM.jpg?c1=9DB3F2&t=aaa";
        let two = "https://cdn/image/gen/1200x1200AM.jpg?c1=F29D9D&t=bbb";
        assert_ne!(cache_key(one), cache_key(two));
    }

    #[test]
    fn presigned_case_variants_are_still_stripped() {
        let a = "https://s/x/image?X-Amz-Signature=aaa";
        let b = "https://s/x/image?x-amz-signature=bbb";
        assert_eq!(cache_key(a), cache_key(b));
    }

    #[test]
    fn content_type_follows_extension() {
        assert_eq!(content_type("https://a/x.png"), "image/png");
        assert_eq!(content_type("https://a/x.webp"), "image/webp");
        assert_eq!(content_type("https://a/x.jpg"), "image/jpeg");
    }

    #[test]
    fn env_override_wins_for_cache_dir() {
        std::env::set_var("CAPSULE_ARTWORK_CACHE_PATH", r"C:\tmp\art");
        let d = cache_dir(Some(PathBuf::from(r"C:\appdata")));
        std::env::remove_var("CAPSULE_ARTWORK_CACHE_PATH");
        assert_eq!(d, Some(PathBuf::from(r"C:\tmp\art")));
    }
}
