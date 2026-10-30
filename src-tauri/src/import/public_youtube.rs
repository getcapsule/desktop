//! Public, unauthenticated YouTube metadata. Never reads account/browser storage.
use super::{
    http::Cancellation,
    model::*,
    urls::{ContentKind, ImportUrl},
};
use rustypipe::{
    client::RustyPipe,
    model::{Thumbnail, TrackItem, VideoItem},
};
use std::{collections::HashSet, future::Future, sync::OnceLock, time::Duration};
use tokio::sync::Semaphore;

const PAGE_CAP: usize = 200;
const TRACK_CAP: usize = 10_000;

const UNAVAILABLE: &str = "YouTube could not load public metadata. The item may be private, unavailable, or temporarily blocked.";

fn client() -> Result<&'static RustyPipe, String> {
    static CLIENT: OnceLock<Result<RustyPipe, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            RustyPipe::builder()
                .no_storage()
                .no_botguard()
                .no_reporter()
                .timeout(Duration::from_secs(10))
                .n_http_retries(1)
                .build()
                .map_err(|_| "Could not initialize public YouTube metadata.".into())
        })
        .as_ref()
        .map_err(Clone::clone)
}

async fn request<T, E>(
    cancel: &Cancellation,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    static GATE: Semaphore = Semaphore::const_new(2);
    cancel.check()?;
    let permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("Import cancelled.".into()),
        permit = GATE.acquire() => permit.map_err(|_| UNAVAILABLE.to_string())?,
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Import cancelled.".into()),
        result = tokio::time::timeout(Duration::from_secs(20), future) =>
            result.map_err(|_| UNAVAILABLE.to_string())?.map_err(|_| UNAVAILABLE.to_string()),
    };
    drop(permit);
    cancel.check()?;
    result
}

fn thumbnail(images: &[Thumbnail]) -> Option<String> {
    images
        .iter()
        .filter(|image| image.url.starts_with("https://"))
        .max_by_key(|image| image.width)
        .map(|image| image.url.clone())
}

fn metadata(
    id: &str,
    title: &str,
    artist: Option<&str>,
    duration: Option<u32>,
    images: &[Thumbnail],
) -> Option<ImportedTrack> {
    let title = title.trim();
    if !super::urls::video_id(id)
        || title.is_empty()
        || matches!(
            title.to_ascii_lowercase().as_str(),
            "[private video]" | "[deleted video]" | "private video" | "deleted video"
        )
    {
        return None;
    }
    Some(ImportedTrack {
        origin: Origin {
            platform: Platform::Youtube,
            external_id: id.into(),
            url: Some(format!("https://www.youtube.com/watch?v={id}")),
        },
        metadata: Metadata {
            title: title.into(),
            artists: artist
                .filter(|v| !v.trim().is_empty())
                .map(|v| vec![v.to_owned()])
                .unwrap_or_default(),
            album: None,
            duration_ms: duration.map(|seconds| u64::from(seconds) * 1000),
            artwork_url: thumbnail(images),
            isrc: None,
            explicit: None,
        },
    })
}

fn track(item: &VideoItem) -> Option<ImportedTrack> {
    if item.is_live || item.is_upcoming {
        return None;
    }
    metadata(
        &item.id,
        &item.name,
        item.channel.as_ref().map(|channel| channel.name.as_str()),
        item.duration,
        &item.thumbnail,
    )
}

fn music_track(item: &TrackItem) -> Option<ImportedTrack> {
    let mut track = metadata(&item.id, &item.name, None, item.duration, &item.cover)?;
    track.metadata.artists = item
        .artists
        .iter()
        .map(|artist| artist.name.clone())
        .collect();
    track.metadata.album = item.album.as_ref().map(|album| album.name.clone());
    Some(track)
}

fn append(out: &mut ImportedCollection, items: &[VideoItem], visited: &mut usize) -> bool {
    for item in items {
        if *visited >= TRACK_CAP {
            return false;
        }
        *visited += 1;
        if let Some(track) = track(item) {
            out.tracks.push(track);
        } else {
            out.warnings
                .push("A missing, private, live, or unsupported recording was skipped.".into());
        }
    }
    *visited < TRACK_CAP
}

fn continuation_allowed(page: usize, token: &str, seen: &mut HashSet<String>) -> bool {
    page + 1 < PAGE_CAP && seen.insert(token.to_owned())
}

pub(super) async fn fetch(
    target: &ImportUrl,
    cancel: &Cancellation,
) -> Result<ImportedCollection, String> {
    cancel.check()?;
    if super::urls::detect(&target.url)? != *target || target.provider != Platform::Youtube {
        return Err("Invalid canonical YouTube import target.".into());
    }
    if !matches!(target.kind, ContentKind::Track | ContentKind::Playlist) {
        return Err("YouTube profile imports are not supported without an API key. Paste a public track, album playlist, or playlist link.".into());
    }
    let query = client()?.query().unauthenticated();
    let mut out = ImportedCollection {
        source_url: target.url.clone(),
        provider: Platform::Youtube,
        kind: target.kind,
        title: "YouTube playlist".into(),
        artwork_url: None,
        tracks: Vec::new(),
        warnings: Vec::new(),
    };
    if target.kind == ContentKind::Track {
        let player = request(cancel, query.player(&target.id)).await?;
        let details = &player.details;
        let track = metadata(
            &details.id,
            details.name.as_deref().unwrap_or_default(),
            details.channel_name.as_deref(),
            Some(details.duration),
            &details.thumbnail,
        )
        .ok_or(UNAVAILABLE)?;
        if details.is_live {
            return Err("Live recordings cannot be imported for native playback.".into());
        }
        out.title = track.metadata.title.clone();
        out.artwork_url = track.metadata.artwork_url.clone();
        out.tracks.push(track);
        return Ok(out);
    }
    let playlist = match request(cancel, query.playlist(&target.id)).await {
        Ok(playlist) => playlist,
        Err(_) => {
            cancel.check()?;
            // YouTube's newer desktop lockup layout is not understood by this
            // RustyPipe release. The public Music endpoint retains structured rows.
            return fetch_music_playlist(target, cancel, out).await;
        }
    };
    out.title = playlist.name;
    out.artwork_url = thumbnail(&playlist.thumbnail);
    let mut page = playlist.videos;
    let mut seen = HashSet::new();
    let mut visited = 0;
    for page_index in 0..PAGE_CAP {
        cancel.check()?;
        if !append(&mut out, &page.items, &mut visited) {
            out.warnings.push("Import incomplete: collection reached the 10,000 item limit (including unavailable entries).".into());
            break;
        }
        let Some(token) = page.ctoken.as_deref() else {
            break;
        };
        if !continuation_allowed(page_index, token, &mut seen) {
            out.warnings
                .push("Import incomplete: pagination limit or repeated continuation.".into());
            break;
        }
        match request(cancel, page.next(&query)).await {
            Ok(Some(next)) => page = next,
            Ok(None) => break,
            Err(error) => {
                cancel.check()?;
                out.warnings.push(format!("Import incomplete: {error}"));
                break;
            }
        }
    }
    if visited == 0 && playlist.video_count > 0 {
        return Err(UNAVAILABLE.into());
    }
    if (visited as u64) < playlist.video_count && out.warnings.is_empty() {
        out.warnings.push("Some playlist entries may be private or unavailable; only public recordings were imported.".into());
    }
    cancel.check()?;
    Ok(out)
}

async fn fetch_music_playlist(
    target: &ImportUrl,
    cancel: &Cancellation,
    mut out: ImportedCollection,
) -> Result<ImportedCollection, String> {
    let query = client()?.query().unauthenticated();
    let playlist = request(cancel, query.music_playlist(&target.id)).await?;
    out.title = playlist.name;
    out.artwork_url = thumbnail(&playlist.thumbnail);
    let mut page = playlist.tracks;
    let mut seen = HashSet::new();
    let mut visited = 0;
    for page_index in 0..PAGE_CAP {
        cancel.check()?;
        for item in &page.items {
            if visited >= TRACK_CAP {
                break;
            }
            visited += 1;
            if let Some(track) = music_track(item) {
                out.tracks.push(track);
            } else {
                out.warnings
                    .push("A missing, private, or unsupported recording was skipped.".into());
            }
        }
        if visited >= TRACK_CAP {
            out.warnings.push("Import incomplete: collection reached the 10,000 item limit (including unavailable entries).".into());
            break;
        }
        let Some(token) = page.ctoken.as_deref() else {
            break;
        };
        if !continuation_allowed(page_index, token, &mut seen) {
            out.warnings
                .push("Import incomplete: pagination limit or repeated continuation.".into());
            break;
        }
        match request(cancel, page.next(&query)).await {
            Ok(Some(next)) => page = next,
            Ok(None) => break,
            Err(error) => {
                cancel.check()?;
                out.warnings.push(format!("Import incomplete: {error}"));
                break;
            }
        }
    }
    if playlist
        .track_count
        .is_some_and(|count| (visited as u64) < count)
        && out.warnings.is_empty()
    {
        out.warnings.push("Some playlist entries may be private or unavailable; only public recordings were imported.".into());
    }
    cancel.check()?;
    if out.tracks.is_empty() && playlist.track_count.is_some_and(|count| count > 0) {
        return Err(UNAVAILABLE.into());
    }
    Ok(out)
}

pub(super) async fn search(
    query_text: &str,
    cancel: &Cancellation,
) -> Result<Vec<PlaybackCandidate>, String> {
    cancel.check()?;
    if query_text.trim().is_empty() || query_text.len() > 500 {
        return Err("Search query must be between 1 and 500 bytes.".into());
    }
    let query = client()?.query().unauthenticated();
    // Music results retain recording artists and album names for conservative matching.
    // Fall back to ordinary videos for recordings outside the Music catalog.
    if let Ok(result) = request(cancel, query.music_search_tracks(query_text)).await {
        let candidates: Vec<_> = result
            .items
            .items
            .iter()
            .filter_map(music_track)
            .take(10)
            .map(candidate)
            .collect();
        if !candidates.is_empty() {
            return Ok(candidates);
        }
    }
    cancel.check()?;
    let result = request(cancel, query.search::<VideoItem, _>(query_text)).await?;
    Ok(result
        .items
        .items
        .iter()
        .filter_map(track)
        .take(10)
        .map(candidate)
        .collect())
}

fn candidate(track: ImportedTrack) -> PlaybackCandidate {
    PlaybackCandidate {
        provider: Platform::Youtube,
        external_id: track.origin.external_id,
        metadata: track.metadata,
        confidence: 0.0,
        compatible: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn video(id: &str, title: &str) -> VideoItem {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": title, "duration": 123, "thumbnail": [],
            "channel": {"id":"channel", "name":"Artist", "avatar":[], "verification":"none", "subscriber_count":null},
            "publish_date":null, "publish_date_txt":null, "view_count":null,
            "is_live":false, "is_short":false, "is_upcoming":false, "short_description":null
        })).unwrap()
    }
    #[test]
    fn maps_public_video_metadata_without_inventing_album_or_isrc() {
        let imported = track(&video("jNQXAC9IVRw", "Public recording")).unwrap();
        assert_eq!(imported.origin.external_id, "jNQXAC9IVRw");
        assert_eq!(imported.metadata.duration_ms, Some(123_000));
        assert_eq!(imported.metadata.artists, ["Artist"]);
        assert_eq!(imported.metadata.album, None);
        assert_eq!(imported.metadata.isrc, None);
    }
    #[test]
    fn music_candidates_keep_recording_artists_and_album_for_matching() {
        let item: TrackItem = serde_json::from_value(serde_json::json!({
            "id":"jNQXAC9IVRw", "name":"Song", "duration":234, "cover":[],
            "artists":[{"id":null,"name":"First artist"},{"id":null,"name":"Second artist"}],
            "artist_id":null, "album":{"id":"album", "name":"Album"},
            "view_count":null, "track_type":"track", "track_nr":null, "by_va":false
        }))
        .unwrap();
        let result = candidate(music_track(&item).unwrap());
        assert_eq!(result.metadata.artists, ["First artist", "Second artist"]);
        assert_eq!(result.metadata.album.as_deref(), Some("Album"));
        assert_eq!(result.metadata.duration_ms, Some(234_000));
        assert_eq!(result.confidence, 0.0);
    }
    #[test]
    fn unavailable_or_invalid_recordings_are_not_candidates() {
        assert!(track(&video("bad", "Recording")).is_none());
        assert!(track(&video("jNQXAC9IVRw", "[Private video]")).is_none());
        let mut live = video("jNQXAC9IVRw", "Live");
        live.is_live = true;
        assert!(track(&live).is_none());
    }
    fn collection() -> ImportedCollection {
        ImportedCollection {
            source_url: String::new(),
            provider: Platform::Youtube,
            kind: ContentKind::Playlist,
            title: "Playlist".into(),
            artwork_url: None,
            tracks: Vec::new(),
            warnings: Vec::new(),
        }
    }
    #[test]
    fn duplicate_entries_keep_order_and_unavailable_rows_count_toward_cap() {
        let mut out = collection();
        let mut visited = 0;
        assert!(append(
            &mut out,
            &[
                video("jNQXAC9IVRw", "First"),
                video("jNQXAC9IVRw", "Again"),
                video("bad", "Private")
            ],
            &mut visited
        ));
        assert_eq!(
            out.tracks
                .iter()
                .map(|t| t.metadata.title.as_str())
                .collect::<Vec<_>>(),
            ["First", "Again"]
        );
        assert_eq!(visited, 3);
        assert_eq!(out.warnings.len(), 1);
        visited = TRACK_CAP - 1;
        assert!(!append(
            &mut out,
            &[video("bad", "Private"), video("jNQXAC9IVRw", "Over limit")],
            &mut visited
        ));
        assert_eq!(out.tracks.len(), 2);
        assert_eq!(visited, TRACK_CAP);
    }
    #[test]
    fn pagination_refuses_repeated_tokens_and_request_201() {
        let mut seen = HashSet::new();
        assert!(continuation_allowed(0, "next", &mut seen));
        assert!(!continuation_allowed(1, "next", &mut seen));
        assert!(!continuation_allowed(PAGE_CAP - 1, "different", &mut seen));
    }
    #[tokio::test]
    async fn cancellation_drops_inflight_request_and_redacts_provider_errors() {
        let cancel = Cancellation::default();
        let trigger = cancel.clone();
        let work = async move {
            trigger.cancel();
            std::future::pending::<Result<(), String>>().await
        };
        assert_eq!(
            request(&cancel, work).await,
            Err("Import cancelled.".into())
        );
        let error = request(&Cancellation::default(), async {
            Err::<(), _>("https://provider/?secret=token")
        })
        .await
        .unwrap_err();
        assert!(!error.contains("token"));
        assert_eq!(error, UNAVAILABLE);
    }
    #[tokio::test]
    async fn cancelled_public_import_and_search_require_no_network() {
        let cancel = Cancellation::default();
        cancel.cancel();
        let target =
            super::super::urls::detect("https://www.youtube.com/watch?v=jNQXAC9IVRw").unwrap();
        assert_eq!(
            fetch(&target, &cancel).await.unwrap_err(),
            "Import cancelled."
        );
        assert_eq!(
            search("Public recording", &cancel).await.unwrap_err(),
            "Import cancelled."
        );
    }
    #[tokio::test]
    #[ignore = "Live public YouTube metadata requires network access"]
    async fn live_public_video_without_credentials() {
        let target =
            super::super::urls::detect("https://www.youtube.com/watch?v=jNQXAC9IVRw").unwrap();
        let result =
            super::super::providers::fetch(&target, &Default::default(), &Cancellation::default())
                .await
                .unwrap();
        assert_eq!(result.tracks.len(), 1);
        assert_eq!(result.tracks[0].origin.external_id, "jNQXAC9IVRw");
        assert!(!result.tracks[0].metadata.title.is_empty());
    }
    #[tokio::test]
    #[ignore = "Live public YouTube search requires network access"]
    async fn live_public_search_without_credentials() {
        let result = super::super::providers::search_youtube(
            "Me at the zoo jawed",
            &Default::default(),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert!(!result.is_empty());
        assert!(result.len() <= 10);
        assert!(result.iter().all(|candidate| candidate.confidence == 0.0));
    }
    #[tokio::test]
    #[ignore = "Live public YouTube playlist requires network access"]
    async fn live_public_playlist_without_credentials() {
        // Public playlist; no account or API key.
        let target = super::super::urls::detect(
            "https://www.youtube.com/playlist?list=PLMC9KNkIncKtPzgY-5rmhvj7fax8fdxoj",
        )
        .unwrap();
        let result =
            super::super::providers::fetch(&target, &Default::default(), &Cancellation::default())
                .await
                .unwrap();
        assert!(
            result.tracks.len() > 100,
            "must import continuation pages, got {} tracks; warnings={:?}",
            result.tracks.len(),
            result.warnings
        );
        eprintln!(
            "Public playlist imported {} recordings; {} warnings.",
            result.tracks.len(),
            result.warnings.len()
        );
        assert!(result.tracks.len() <= TRACK_CAP);
    }
}
