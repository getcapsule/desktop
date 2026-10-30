//! Tauri boundary for the anonymous public catalog. Playback lives with the player.
use super::{AlbumTracks, Client, SearchKind, SearchResults, Track};
use crate::import::http::Cancellation;

#[derive(Default)]
pub struct State(pub Client);

#[tauri::command]
pub async fn discover_search(
    state: tauri::State<'_, State>,
    kind: SearchKind,
    query: String,
    limit: usize,
    offset: usize,
) -> Result<SearchResults, String> {
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
) -> Result<Vec<Track>, String> {
    state
        .0
        .artist_tracks(&artist_id, limit, offset, &Cancellation::default())
        .await
}

#[tauri::command]
pub async fn discover_album_tracks(
    state: tauri::State<'_, State>,
    album_id: String,
) -> Result<AlbumTracks, String> {
    state
        .0
        .album_tracks(&album_id, &Cancellation::default())
        .await
}
