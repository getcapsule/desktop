//! Tauri boundary for the anonymous public catalog. Playback lives with the player.
use super::{AlbumTracks, Client, SearchKind, SearchResults, Track};
use crate::import::http::Cancellation;
use crate::{import::public_youtube, settings::Source};

#[derive(Default)]
pub struct State(pub Client);

#[tauri::command]
pub async fn discover_search(
    state: tauri::State<'_, State>,
    kind: SearchKind,
    query: String,
    limit: usize,
    offset: usize,
    source: Option<Source>,
) -> Result<SearchResults, String> {
    if source == Some(Source::Youtube) {
        return public_youtube::discover_search(kind, &query).await;
    }
    state
        .0
        .search(kind, &query, limit, offset, &Cancellation::default())
        .await
}

#[tauri::command]
pub async fn discover_artist_tracks(
    state: tauri::State<'_, State>,
    artist_id: String,
    limit: usize,
    offset: usize,
    source: Option<Source>,
) -> Result<Vec<Track>, String> {
    if source == Some(Source::Youtube) {
        return public_youtube::discover_artist(&artist_id).await;
    }
    state
        .0
        .artist_tracks(&artist_id, limit, offset, &Cancellation::default())
        .await
}

#[tauri::command]
pub async fn discover_album_tracks(
    state: tauri::State<'_, State>,
    album_id: String,
    source: Option<Source>,
) -> Result<AlbumTracks, String> {
    if source == Some(Source::Youtube) {
        return public_youtube::discover_album(&album_id).await;
    }
    state
        .0
        .album_tracks(&album_id, &Cancellation::default())
        .await
}
