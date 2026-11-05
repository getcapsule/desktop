use crate::{
    db::SongRow,
    import::playback::Route,
    player::{PlayerState, Status, Track},
    settings::Source,
    AppState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

const FILE: &str = "listening-state.sqlite3";
const HISTORY_LIMIT: usize = 200;
const MAX_COVER_BYTES: usize = 5 * 1024 * 1024;
const MAX_COVER_SIDE: u32 = 512;
pub(crate) const PLAYLIST_COVER_PREFIX: &str = "qol-cover:";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    pub song: SongRow,
    pub source: Source,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback_route: Option<Route>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPlaylist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub cover_id: Option<String>,
    pub songs: Vec<SongEntry>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Snapshot {
    pub favorites: Vec<SongEntry>,
    pub history: Vec<SongEntry>,
    pub playlists: Vec<LocalPlaylist>,
    pub navigation: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Saved {
    snapshot: Snapshot,
    session: Option<PlayerState>,
    source: Source,
    server: String,
    account: String,
}
struct Inner {
    saved: Saved,
    last_track: Option<(String, u64)>,
    last_position_bucket: u64,
}
pub struct State {
    io: Mutex<()>,
    inner: Mutex<Inner>,
    wake: mpsc::SyncSender<()>,
    path: Option<PathBuf>,
}

fn decode(json: &str) -> Saved {
    let mut saved: Saved = serde_json::from_str(json).unwrap_or_default();
    saved.snapshot.history.truncate(HISTORY_LIMIT);
    for playlist in &mut saved.snapshot.playlists {
        for entry in &mut playlist.songs {
            entry
                .entry_id
                .get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
        }
    }
    if saved
        .session
        .as_ref()
        .is_some_and(|s| s.queue.len() > 10000)
    {
        saved.session = None;
    }
    saved
}
fn read(path: &Path) -> Saved {
    let result = (|| -> Result<String, rusqlite::Error> {
        let db = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        db.query_row("SELECT payload FROM snapshot WHERE id=1", [], |r| r.get(0))
    })();
    result.ok().map(|s| decode(&s)).unwrap_or_default()
}
fn write(path: &Path, saved: &Saved) -> Result<(), String> {
    write_with_cover(path, saved, None, None)
}

fn write_with_cover(
    path: &Path,
    saved: &Saved,
    new_cover: Option<(&str, &[u8])>,
    old_cover: Option<&str>,
) -> Result<(), String> {
    let json = serde_json::to_string(saved).map_err(|e| e.to_string())?;
    let mut db = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    let transaction = db.transaction().map_err(|e| e.to_string())?;
    transaction
        .execute_batch("CREATE TABLE IF NOT EXISTS snapshot(id INTEGER PRIMARY KEY CHECK(id=1),payload TEXT NOT NULL);
                        CREATE TABLE IF NOT EXISTS playlist_covers(id TEXT PRIMARY KEY,bytes BLOB NOT NULL);")
        .map_err(|e| e.to_string())?;
    if let Some((id, bytes)) = new_cover {
        transaction
            .execute(
                "INSERT INTO playlist_covers VALUES(?1,?2)",
                rusqlite::params![id, bytes],
            )
            .map_err(|e| e.to_string())?;
    }
    if let Some(id) = old_cover {
        transaction
            .execute("DELETE FROM playlist_covers WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
    }
    transaction.execute("INSERT INTO snapshot VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload",[json]).map_err(|e|e.to_string())?;
    transaction.commit().map_err(|e| e.to_string())
}

fn read_playlist_cover(path: &Path, id: &str) -> Option<Vec<u8>> {
    uuid::Uuid::parse_str(id.strip_prefix(PLAYLIST_COVER_PREFIX)?).ok()?;
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    db.query_row("SELECT bytes FROM playlist_covers WHERE id=?1", [id], |row| row.get(0))
        .ok()
}

pub(crate) fn playlist_cover(app: &AppHandle, id: &str) -> Option<Vec<u8>> {
    let state = app.try_state::<State>()?;
    read_playlist_cover(state.path.as_deref()?, id)
}

fn encode_cover(image: image::DynamicImage) -> Result<Vec<u8>, String> {
    let mut bytes = Cursor::new(Vec::new());
    image
        .into_rgba8()
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|_| "Could not encode the playlist cover.")?;
    Ok(bytes.into_inner())
}

fn normalize_cover(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_COVER_BYTES {
        return Err("Choose a cover image up to 5 MiB.".into());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_COVER_SIDE);
    limits.max_image_height = Some(MAX_COVER_SIDE);
    limits.max_alloc = Some(8 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| "Could not read the playlist cover.")?;
    encode_cover(image)
}

enum CoverEdit {
    Keep,
    Remove,
    Replace(Vec<u8>),
}

fn playlist_name(name: String) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err("Enter a playlist name up to 200 characters.".into());
    }
    Ok(name.to_owned())
}

fn persist_playlist_edit(
    path: &Path,
    saved: &mut Saved,
    playlist_id: &str,
    name: String,
    description: String,
    cover: CoverEdit,
) -> Result<(), String> {
    let name = playlist_name(name)?;
    let description = description.trim();
    if description.chars().count() > 1000 {
        return Err("Enter a playlist description up to 1,000 characters.".into());
    }
    let playlists = &mut saved.snapshot.playlists;
    let index = playlists
        .iter()
        .position(|playlist| playlist.id == playlist_id)
        .ok_or("Playlist no longer exists.")?;
    let playlist = &mut playlists[index];
    let cover_id = match &cover {
        CoverEdit::Keep => playlist.cover_id.clone(),
        CoverEdit::Remove => None,
        CoverEdit::Replace(_) => Some(format!("{PLAYLIST_COVER_PREFIX}{}", uuid::Uuid::new_v4())),
    };
    let previous_name = std::mem::replace(&mut playlist.name, name);
    let previous_description = std::mem::replace(&mut playlist.description, description.to_owned());
    let previous_cover_id = std::mem::replace(&mut playlist.cover_id, cover_id.clone());
    let new_cover = match &cover {
        CoverEdit::Replace(bytes) => cover_id.as_deref().map(|id| (id, bytes.as_slice())),
        _ => None,
    };
    let old_cover = match &cover {
        CoverEdit::Keep => None,
        _ => previous_cover_id.as_deref(),
    };
    if let Err(error) = write_with_cover(path, saved, new_cover, old_cover) {
        let playlist = &mut saved.snapshot.playlists[index];
        playlist.name = previous_name;
        playlist.description = previous_description;
        playlist.cover_id = previous_cover_id;
        tracing::warn!(%error, "could not persist playlist edit");
        return Err("Could not save the playlist changes.".into());
    }
    Ok(())
}
fn source_context(source: Source, settings: &crate::settings::Settings) -> Option<String> {
    (source == Source::Navidrome).then(|| {
        format!(
            "{}\n{}",
            settings.navidrome.url.trim_end_matches('/'),
            settings.navidrome.username
        )
    })
}
fn check_context(
    source: Source,
    context: Option<&str>,
    settings: &crate::settings::Settings,
) -> Result<(), String> {
    if source == Source::Navidrome
        && context.is_some()
        && context != source_context(source, settings).as_deref()
    {
        return Err("This track belongs to another Navidrome server or account. Connect that account first.".into());
    }
    Ok(())
}
fn source_name(source: Source) -> String {
    serde_json::to_value(source)
        .unwrap_or_default()
        .as_str()
        .unwrap_or("local")
        .into()
}
fn canonical(app: &AppHandle, id: &str) -> bool {
    app.try_state::<crate::import::State>().is_some_and(|s| {
        s.db.lock()
            .expect("import db")
            .canonical_track(id)
            .ok()
            .flatten()
            .is_some()
    })
}
fn context_matches(saved: &Saved, source: Source, server: &str) -> bool {
    saved.source == source
        && (source != Source::Navidrome
            || saved.server.trim_end_matches('/') == server.trim_end_matches('/'))
}
pub fn initialize(app: &AppHandle) {
    let state = app.state::<AppState>();
    let path = state
        .data_dir
        .lock()
        .expect("data dir")
        .as_ref()
        .map(|p| p.join(FILE));
    let saved = path.as_deref().map(read).unwrap_or_default();
    let settings = state.settings.lock().expect("settings").clone();
    if let Some(session) = saved.session.clone() {
        let canonical_queue =
            !session.queue.is_empty() && session.queue.iter().all(|t| canonical(app, &t.id));
        if (!session.imported_queue
            && context_matches(&saved, settings.source, &settings.navidrome.url)
            && (settings.source != Source::Navidrome
                || saved.account == settings.navidrome.username))
            || canonical_queue
        {
            state.player.lock().expect("player").restore(session);
        }
    }
    let (wake, rx) = mpsc::sync_channel(1);
    app.manage(State {
        io: Mutex::new(()),
        inner: Mutex::new(Inner {
            saved,
            last_track: None,
            last_position_bucket: 0,
        }),
        wake,
        path,
    });
    let app = app.clone();
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            // A maximum two-second coalescing window, not an idle timer.
            let until = Instant::now() + Duration::from_secs(2);
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                if rx.recv_timeout(left).is_err() {
                    break;
                }
            }
            let state = app.state::<State>();
            let _io = state.io.lock().expect("listening persistence");
            let saved = state.inner.lock().expect("listening state").saved.clone();
            if let Some(path) = state.path.as_deref() {
                if let Err(error) = write(path, &saved) {
                    tracing::warn!(%error,"could not persist listening state");
                }
            }
        }
    });
}
fn changed(app: &AppHandle) {
    if let Some(state) = app.try_state::<State>() {
        let _ = state.wake.try_send(());
    }
}
fn emit(app: &AppHandle) {
    let snapshot = qol_snapshot(app.clone());
    let _ = app.emit("qol://updated", snapshot);
    changed(app);
}
/// Called after publishing player state. Progress updates persist at five-second granularity.
pub fn observe(app: &AppHandle, player: &PlayerState) {
    let Some(state) = app.try_state::<State>() else {
        return;
    };
    let app_state = app.state::<AppState>();
    let settings = app_state.settings.lock().expect("settings").clone();
    let mut inner = state.inner.lock().expect("listening state");
    let bucket = player.position_ms / 5000;
    let state_changed = inner.saved.session.as_ref().is_none_or(|previous| {
        previous.playback_revision != player.playback_revision
            || previous.status != player.status
            || previous.index != player.index
            || previous.volume != player.volume
            || previous.shuffle != player.shuffle
            || previous.repeat != player.repeat
            || previous.queue != player.queue
            || previous.imported_queue != player.imported_queue
            || previous.playback_source != player.playback_source
    }) || bucket != inner.last_position_bucket;
    if !state_changed {
        return;
    }
    let key = player
        .current()
        .map(|t| (t.id.clone(), player.playback_revision));
    let history_changed = player.status == Status::Playing && key != inner.last_track;
    if history_changed {
        if let Some(track) = player.current() {
            let entry = SongEntry {
                entry_id: None,
                song: SongRow {
                    id: track.id.clone(),
                    catalog_id: Some(track.id.clone()),
                    name: track.title.clone(),
                    artist_name: track.artist.clone(),
                    album_name: track.album.clone(),
                    duration_ms: track.duration_ms,
                    artwork_url: None,
                },
                source: settings.source,
                context: source_context(settings.source, &settings),
                playback_route: None,
            };
            inner.saved.snapshot.history.insert(0, entry);
            inner.saved.snapshot.history.truncate(HISTORY_LIMIT);
        }
        inner.last_track = key;
    }
    inner.saved.session = Some(player.clone());
    inner.saved.source = settings.source;
    inner.saved.server = settings.navidrome.url;
    inner.saved.account = settings.navidrome.username;
    inner.last_position_bucket = bucket;
    drop(inner);
    changed(app);
    if history_changed {
        emit(app);
    }
}
/// Flush on a deliberate application exit; no user data is touched during tests.
pub fn flush(app: &AppHandle) {
    if let Some(state) = app.try_state::<State>() {
        if let Some(path) = state.path.as_deref() {
            let _io = state.io.lock().expect("listening persistence");
            let saved = state.inner.lock().expect("listening state").saved.clone();
            if let Err(error) = write(path, &saved) {
                tracing::warn!(%error,"could not flush listening state");
            }
        }
    }
}
#[tauri::command]
pub fn qol_snapshot(app: AppHandle) -> Snapshot {
    app.state::<State>()
        .inner
        .lock()
        .expect("listening state")
        .saved
        .snapshot
        .clone()
}
#[tauri::command]
pub fn qol_navigation_set(app: AppHandle, value: Value) -> Result<(), String> {
    if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > 65536 {
        return Err("Navigation state is too large".into());
    }
    app.state::<State>()
        .inner
        .lock()
        .expect("listening state")
        .saved
        .snapshot
        .navigation = value;
    changed(&app);
    Ok(())
}
fn bind_playlist_sources(
    existing: &[SongEntry],
    incoming: &mut [SongEntry],
    mut canonical: impl FnMut(&str) -> bool,
) -> Result<(), String> {
    if let Some(binding) = existing.iter().find(|entry| !canonical(&entry.song.id)) {
        // Canonical recordings can play with the destination's library binding.
        for entry in incoming.iter_mut().filter(|entry| canonical(&entry.song.id)) {
            entry.source = binding.source;
            entry.context.clone_from(&binding.context);
        }
    }
    let entries: Vec<_> = existing.iter().chain(incoming.iter()).collect();
    if entries.iter().all(|entry| canonical(&entry.song.id)) {
        return Ok(());
    }
    if let Some(first) = entries.first() {
        if entries
            .iter()
            .any(|entry| entry.source != first.source || entry.context != first.context)
        {
            return Err(
                "Use separate playlists for different playback sources or Navidrome accounts."
                    .into(),
            );
        }
    }
    Ok(())
}
fn set_favorites(
    snapshot: &mut Snapshot,
    songs: Vec<SongRow>,
    source: Source,
    on: bool,
    context: Option<String>,
) {
    let mut ids = std::collections::HashSet::new();
    let songs: Vec<_> = songs
        .into_iter()
        .filter(|song| ids.insert(song.id.clone()))
        .collect();
    snapshot.favorites.retain(|entry| {
        entry.source != source || entry.context != context || !ids.contains(&entry.song.id)
    });
    if on {
        snapshot
            .favorites
            .extend(songs.into_iter().map(|song| SongEntry {
                entry_id: None,
                song,
                source,
                context: context.clone(),
                playback_route: None,
            }));
    }
}
#[tauri::command]
pub fn qol_favorite(
    app: AppHandle,
    song: SongRow,
    source: Source,
    on: bool,
    context: Option<String>,
) -> Result<(), String> {
    qol_favorites(app, vec![song], source, on, context)
}
#[tauri::command]
pub fn qol_favorites(
    app: AppHandle,
    songs: Vec<SongRow>,
    source: Source,
    on: bool,
    context: Option<String>,
) -> Result<(), String> {
    if songs.len() > 10000 {
        return Err("Select at most 10,000 tracks".into());
    }
    let settings = app
        .state::<AppState>()
        .settings
        .lock()
        .expect("settings")
        .clone();
    let context = context.or_else(|| source_context(source, &settings));
    if on && !songs.iter().all(|song| canonical(&app, &song.id)) {
        check_context(source, context.as_deref(), &settings)?;
    }
    let state = app.state::<State>();
    let mut inner = state.inner.lock().expect("listening state");
    set_favorites(&mut inner.saved.snapshot, songs, source, on, context);
    drop(inner);
    emit(&app);
    Ok(())
}
#[tauri::command]
pub fn qol_playlist_add(
    app: AppHandle,
    name: Option<String>,
    playlist_id: Option<String>,
    songs: Vec<SongRow>,
    source: Source,
    context: Option<String>,
) -> Result<String, String> {
    if songs.len() > 10000 {
        return Err("Select at most 10,000 tracks".into());
    }
    let settings = app
        .state::<AppState>()
        .settings
        .lock()
        .expect("settings")
        .clone();
    let context = context.or_else(|| source_context(source, &settings));
    if !songs.iter().all(|song| canonical(&app, &song.id)) {
        check_context(source, context.as_deref(), &settings)?;
    }
    let entries: Vec<_> = songs
        .into_iter()
        .map(|song| SongEntry {
            entry_id: None,
            song,
            source,
            context: context.clone(),
            playback_route: None,
        })
        .collect();
    append_playlist(&app, name, playlist_id, entries)
}

fn append_playlist(
    app: &AppHandle,
    name: Option<String>,
    playlist_id: Option<String>,
    mut entries: Vec<SongEntry>,
) -> Result<String, String> {
    let state = app.state::<State>();
    let mut inner = state.inner.lock().expect("listening state");
    let lists = &mut inner.saved.snapshot.playlists;
    let index = if let Some(id) = playlist_id {
        let index = lists
            .iter()
            .position(|p| p.id == id)
            .ok_or("Playlist no longer exists")?;
        if lists[index].songs.len() + entries.len() > 10000 {
            return Err("A playlist supports at most 10,000 tracks".into());
        }
        bind_playlist_sources(&lists[index].songs, &mut entries, |id| canonical(app, id))?;
        index
    } else {
        let name = playlist_name(name.unwrap_or_else(|| "New playlist".into()))?;
        lists.push(LocalPlaylist {
            id: format!("qol:{}", uuid::Uuid::new_v4()),
            name,
            description: String::new(),
            cover_id: None,
            songs: vec![],
        });
        lists.len() - 1
    };
    for entry in &mut entries {
        entry.entry_id = Some(uuid::Uuid::new_v4().to_string());
    }
    lists[index].songs.extend(entries);
    let id = lists[index].id.clone();
    drop(inner);
    emit(app);
    Ok(id)
}

#[tauri::command]
pub async fn qol_playlist_edit(
    app: AppHandle,
    playlist_id: String,
    name: String,
    description: String,
    cover: Option<Vec<u8>>,
    remove_cover: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cover = match (cover, remove_cover) {
            (Some(_), true) => {
                return Err("Choose a replacement cover or remove the existing one.".into());
            }
            (Some(bytes), false) => CoverEdit::Replace(normalize_cover(&bytes)?),
            (None, true) => CoverEdit::Remove,
            (None, false) => CoverEdit::Keep,
        };
        let state = app.state::<State>();
        let path = state
            .path
            .as_deref()
            .ok_or("Playlist storage is unavailable.")?;
        {
            let _io = state.io.lock().expect("listening persistence");
            let mut inner = state.inner.lock().expect("listening state");
            persist_playlist_edit(path, &mut inner.saved, &playlist_id, name, description, cover)?;
        }
        emit(&app);
        Ok(())
    })
    .await
    .map_err(|_| "Could not save the playlist changes.".to_string())?
}

#[tauri::command]
pub async fn qol_playlist_remove_song(
    app: AppHandle,
    playlist_id: String,
    entry_id: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<State>();
        let path = state
            .path
            .as_deref()
            .ok_or("Playlist storage is unavailable.")?;
        {
            let _io = state.io.lock().expect("listening persistence");
            let mut inner = state.inner.lock().expect("listening state");
            persist_playlist_removal(path, &mut inner.saved, &playlist_id, &entry_id)?;
        }
        emit(&app);
        Ok(())
    })
    .await
    .map_err(|_| "Could not remove the track from the playlist.".to_string())?
}

fn persist_playlist_removal(
    path: &Path,
    saved: &mut Saved,
    playlist_id: &str,
    entry_id: &str,
) -> Result<(), String> {
    let index = saved
        .snapshot
        .playlists
        .iter()
        .position(|playlist| playlist.id == playlist_id)
        .ok_or("Playlist no longer exists.")?;
    let songs = &mut saved.snapshot.playlists[index].songs;
    let Some(position) = songs
        .iter()
        .position(|entry| entry.entry_id.as_deref() == Some(entry_id))
    else {
        return Ok(());
    };
    let removed = songs.remove(position);
    if let Err(error) = write(path, saved) {
        saved.snapshot.playlists[index].songs.insert(position, removed);
        tracing::warn!(%error, "could not persist playlist removal");
        return Err("Could not remove the track from the playlist.".into());
    }
    Ok(())
}

#[tauri::command]
pub fn qol_playlist_add_discovered(
    app: AppHandle,
    playlist_id: Option<String>,
    name: Option<String>,
    track: crate::soundcloud::Track,
) -> Result<String, String> {
    let (track, route) = {
        let imports = app.state::<crate::import::State>();
        let mut db = imports
            .db
            .lock()
            .map_err(|_| "The library is unavailable.")?;
        crate::soundcloud_playback::save_discovered_track(&mut db, track)?
    };
    let settings = app
        .state::<AppState>()
        .settings
        .lock()
        .expect("settings")
        .clone();
    let entry = SongEntry {
        entry_id: None,
        song: track.into(),
        source: settings.source,
        context: source_context(settings.source, &settings),
        playback_route: Some(route),
    };
    append_playlist(&app, name, playlist_id, vec![entry])
}

fn tracks(app: &AppHandle, songs: Vec<SongRow>, source: Source) -> Result<Vec<Track>, String> {
    if songs.len() > 10000 {
        return Err("Select at most 10,000 tracks".into());
    }
    songs
        .into_iter()
        .map(|s| {
            let id = if canonical(app, &s.id) {
                s.id
            } else {
                match source {
                    Source::Youtube => s.catalog_id.ok_or("Track has no playable recording")?,
                    Source::Local | Source::Navidrome => s.id,
                    _ => return Err("Unsupported playback source".into()),
                }
            };
            Ok(Track {
                id,
                title: s.name,
                artist: s.artist_name,
                album: s.album_name,
                duration_ms: s.duration_ms,
                playback_route: None,
            })
        })
        .collect()
}
#[tauri::command]
pub fn qol_play(
    app: AppHandle,
    songs: Vec<SongRow>,
    source: Source,
    start_index: Option<usize>,
    context: Option<String>,
    playback_routes: Option<Vec<Option<Route>>>,
) -> Result<(), String> {
    let canonical_queue = !songs.is_empty() && songs.iter().all(|s| canonical(&app, &s.id));
    let mut tracks = tracks(&app, songs, source)?;
    if let Some(routes) = playback_routes {
        if routes.len() != tracks.len() {
            return Err("Playback routes must match the selected tracks.".into());
        }
        for (track, route) in tracks.iter_mut().zip(routes) {
            track.playback_route = route;
        }
    }
    if tracks.is_empty() {
        return Err("No tracks selected".into());
    }
    let state = app.state::<AppState>();
    let mut settings = state.settings.lock().expect("settings").clone();
    if !canonical_queue {
        check_context(source, context.as_deref(), &settings)?;
    }
    if !canonical_queue && settings.source != source {
        settings.source = source;
        crate::commands::settings_set(app.clone(), state, settings)?;
    }
    crate::commands::apply(&app, |p| {
        p.set_playback_source(Some(&source_name(source)), canonical_queue);
        p.play_queue(tracks, start_index.unwrap_or(0))
    });
    Ok(())
}
#[tauri::command]
pub fn qol_queue_add(
    app: AppHandle,
    songs: Vec<SongRow>,
    source: Source,
    next: bool,
    context: Option<String>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (queue, playback_source) = {
        let p = state.player.lock().expect("player");
        (p.state().queue.clone(), p.state().playback_source.clone())
    };
    let canonical_new = !songs.is_empty() && songs.iter().all(|s| canonical(&app, &s.id));
    let canonical_old = !queue.is_empty() && queue.iter().all(|s| canonical(&app, &s.id));
    let settings = state.settings.lock().expect("settings").clone();
    let current = settings.source;
    if !canonical_new {
        check_context(source, context.as_deref(), &settings)?;
    }
    if !queue.is_empty()
        && (canonical_new != canonical_old
            || (!canonical_new
                && playback_source.unwrap_or_else(|| source_name(current)) != source_name(source)))
    {
        return Err("Start a new queue to change playback sources".into());
    }
    if queue.len() + songs.len() > 10000 {
        return Err("A queue supports at most 10,000 tracks".into());
    }
    if !canonical_new && current != source {
        return Err("Switch to this source before adding tracks".into());
    }
    let tracks = tracks(&app, songs, source)?;
    crate::commands::apply(&app, |p| {
        if queue.is_empty() {
            p.set_playback_source(Some(&source_name(source)), canonical_new);
        }
        p.queue_add(tracks, next)
    });
    Ok(())
}
#[tauri::command]
pub fn qol_queue_move(app: AppHandle, from: usize, to: usize) {
    crate::commands::apply(&app, |p| p.queue_move(from, to));
}
#[tauri::command]
pub fn qol_queue_remove(app: AppHandle, index: usize) {
    crate::commands::apply(&app, |p| p.queue_remove(index));
}
#[tauri::command]
pub fn qol_queue_clear(app: AppHandle) {
    crate::commands::apply(&app, |p| p.queue_clear());
}
#[tauri::command]
pub fn qol_queue_undo(app: AppHandle) {
    crate::commands::apply(&app, |p| p.queue_undo());
}
#[tauri::command]
pub fn qol_queue_jump(app: AppHandle, index: usize) {
    crate::commands::apply(&app, |p| p.jump_to(index));
}
#[tauri::command]
pub fn qol_reveal(app: AppHandle, song: SongRow, source: Source) -> Result<(), String> {
    let imported_path = app
        .try_state::<crate::import::State>()
        .and_then(|state| {
            state
                .db
                .lock()
                .expect("import db")
                .canonical_track(&song.id)
                .ok()
                .flatten()
        })
        .and_then(|track| {
            track
                .origins
                .into_iter()
                .find(|origin| origin.platform == crate::import::model::Platform::Local)
                .map(|origin| origin.external_id)
        });
    if source != Source::Local && imported_path.is_none() {
        return Err("Only local files can be revealed".into());
    }
    let path = PathBuf::from(imported_path.unwrap_or(song.id));
    if !path.is_absolute() || !path.is_file() {
        return Err("Local file no longer exists".into());
    }
    #[cfg(windows)]
    std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn()
        .map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    std::process::Command::new("xdg-open")
        .arg(path.parent().ok_or("File has no parent")?)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_playlists_load_without_description_or_cover() {
        let saved = decode(r#"{"snapshot":{"playlists":[{"id":"qol:old","name":"Old mix","songs":[]} ]}}"#);
        let playlist = &saved.snapshot.playlists[0];
        assert_eq!(playlist.name, "Old mix");
        assert!(playlist.description.is_empty());
        assert!(playlist.cover_id.is_none());
    }

    #[test]
    fn playlist_metadata_and_cover_are_replaced_together() {
        let path =
            std::env::temp_dir().join(format!("capsule-playlist-{}.sqlite3", uuid::Uuid::new_v4()));
        let mut saved = Saved::default();
        saved.snapshot.playlists.push(LocalPlaylist {
            id: "qol:mix".into(),
            name: "Old".into(),
            songs: vec![entry("one", Source::Local, None)],
            description: String::new(),
            cover_id: None,
        });
        let png = encode_cover(image::DynamicImage::new_rgba8(2, 2)).unwrap();
        persist_playlist_edit(
            &path,
            &mut saved,
            "qol:mix",
            "New".into(),
            "Songs for tonight".into(),
            CoverEdit::Replace(png.clone()),
        )
        .unwrap();
        let playlist = &read(&path).snapshot.playlists[0];
        let first_cover = playlist.cover_id.as_deref().unwrap();
        assert_eq!(playlist.name, "New");
        assert_eq!(playlist.description, "Songs for tonight");
        assert_eq!(playlist.songs[0].song.id, "one");
        assert_eq!(read_playlist_cover(&path, first_cover), Some(png));

        persist_playlist_edit(
            &path,
            &mut saved,
            "qol:mix",
            "Renamed".into(),
            String::new(),
            CoverEdit::Keep,
        )
        .unwrap();
        assert_eq!(
            read(&path).snapshot.playlists[0].cover_id.as_deref(),
            Some(first_cover)
        );

        let replacement = encode_cover(image::DynamicImage::new_rgba8(4, 4)).unwrap();
        assert!(persist_playlist_edit(
            path.parent().unwrap(),
            &mut saved,
            "qol:mix",
            "Failed replacement".into(),
            String::new(),
            CoverEdit::Replace(replacement.clone()),
        )
        .is_err());
        assert_eq!(saved.snapshot.playlists[0].name, "Renamed");
        assert_eq!(
            saved.snapshot.playlists[0].cover_id.as_deref(),
            Some(first_cover)
        );
        assert!(read_playlist_cover(&path, first_cover).is_some());

        persist_playlist_edit(
            &path,
            &mut saved,
            "qol:mix",
            "Renamed".into(),
            String::new(),
            CoverEdit::Replace(replacement.clone()),
        )
        .unwrap();
        let stored = read(&path);
        let second_cover = stored.snapshot.playlists[0].cover_id.as_deref().unwrap();
        assert_ne!(first_cover, second_cover);
        assert_eq!(read_playlist_cover(&path, first_cover), None);
        assert_eq!(read_playlist_cover(&path, second_cover), Some(replacement));

        persist_playlist_edit(
            &path,
            &mut saved,
            "qol:mix",
            "Renamed".into(),
            String::new(),
            CoverEdit::Remove,
        )
        .unwrap();
        assert!(read(&path).snapshot.playlists[0].cover_id.is_none());
        assert_eq!(read_playlist_cover(&path, second_cover), None);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_and_oversized_covers_are_rejected() {
        assert!(normalize_cover(b"this is not an image").is_err());
        let oversized = encode_cover(image::DynamicImage::new_rgba8(513, 2)).unwrap();
        assert!(normalize_cover(&oversized).is_err());
    }
    fn entry(id: &str, source: Source, context: Option<&str>) -> SongEntry {
        SongEntry {
            entry_id: None,
            song: SongRow {
                id: id.into(),
                catalog_id: None,
                name: id.into(),
                artist_name: String::new(),
                album_name: String::new(),
                duration_ms: 0,
                artwork_url: None,
            },
            source,
            context: context.map(str::to_owned),
            playback_route: None,
        }
    }
    #[test]
    fn mixed_playlist_sources_and_accounts_are_rejected_except_all_canonical() {
        let local = entry("one", Source::Local, None);
        let youtube = entry("two", Source::Youtube, None);
        assert!(bind_playlist_sources(
            std::slice::from_ref(&local),
            &mut [youtube.clone()],
            |_| false
        )
        .is_err());
        assert!(bind_playlist_sources(&[local], &mut [youtube], |_| true).is_ok());
        let one = entry("one", Source::Navidrome, Some("server1"));
        let two = entry("two", Source::Navidrome, Some("server2"));
        assert!(bind_playlist_sources(std::slice::from_ref(&one), &mut [two], |_| false).is_err());
        assert!(bind_playlist_sources(
            std::slice::from_ref(&one),
            &mut [one.clone()],
            |_| false
        )
        .is_ok());
    }
    #[test]
    fn canonical_additions_use_the_destination_playlist_account() {
        let existing = entry("server-song", Source::Navidrome, Some("saved-account"));
        let mut incoming = [entry("discovered", Source::Local, None)];
        bind_playlist_sources(&[existing], &mut incoming, |id| id == "discovered").unwrap();
        assert_eq!(incoming[0].source, Source::Navidrome);
        assert_eq!(incoming[0].context.as_deref(), Some("saved-account"));
    }
    #[test]
    fn batch_favorites_deduplicates_and_removes_original_context() {
        let old = entry("one", Source::Navidrome, Some("old"));
        let current = entry("one", Source::Navidrome, Some("new"));
        let mut snapshot = Snapshot {
            favorites: vec![old.clone(), current.clone()],
            ..Snapshot::default()
        };
        set_favorites(
            &mut snapshot,
            vec![old.song],
            Source::Navidrome,
            false,
            Some("old".into()),
        );
        assert_eq!(snapshot.favorites.len(), 1);
        assert_eq!(snapshot.favorites[0].context.as_deref(), Some("new"));
        set_favorites(
            &mut snapshot,
            vec![current.song.clone(), current.song],
            Source::Navidrome,
            true,
            Some("new".into()),
        );
        assert_eq!(snapshot.favorites.len(), 1);
    }
    #[test]
    fn malformed_persistence_is_ignored() {
        assert!(decode("not json").session.is_none());
    }

    #[test]
    fn removes_one_legacy_playlist_occurrence_and_persists_the_result() {
        let path = std::env::temp_dir().join(format!(
            "capsule-playlist-removal-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let song = entry("repeated-song", Source::Local, None);
        let mut saved = Saved::default();
        saved.snapshot.playlists.push(LocalPlaylist {
            id: "qol:mix".into(),
            name: "Mix".into(),
            description: String::new(),
            cover_id: None,
            songs: vec![song.clone(), song],
        });
        write(&path, &saved).unwrap();
        let mut saved = read(&path);
        let ids: Vec<_> = saved.snapshot.playlists[0]
            .songs
            .iter()
            .map(|song| song.entry_id.clone().unwrap())
            .collect();
        assert_ne!(ids[0], ids[1]);
        assert!(
            persist_playlist_removal(&path.join("invalid"), &mut saved, "qol:mix", &ids[1]).is_err()
        );
        assert_eq!(saved.snapshot.playlists[0].songs.len(), 2);
        persist_playlist_removal(&path, &mut saved, "qol:mix", &ids[1]).unwrap();
        persist_playlist_removal(&path, &mut saved, "qol:mix", &ids[1]).unwrap();
        let restored = read(&path);
        let songs = &restored.snapshot.playlists[0].songs;
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].entry_id.as_deref(), Some(ids[0].as_str()));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn source_context_does_not_route_other_providers() {
        let s = Saved {
            source: Source::Local,
            ..Saved::default()
        };
        assert!(!context_matches(&s, Source::Youtube, ""));
    }
    #[test]
    fn navidrome_server_must_match() {
        let s = Saved {
            source: Source::Navidrome,
            server: "https://one/".into(),
            ..Saved::default()
        };
        assert!(!context_matches(&s, Source::Navidrome, "https://two"));
        assert!(context_matches(&s, Source::Navidrome, "https://one"));
    }
    #[test]
    fn history_is_bounded_when_reading_existing_state() {
        let entry = SongEntry {
            entry_id: None,
            song: SongRow {
                id: "one".into(),
                catalog_id: None,
                name: "One".into(),
                artist_name: "Artist".into(),
                album_name: String::new(),
                duration_ms: 1,
                artwork_url: None,
            },
            source: Source::Local,
            context: None,
            playback_route: None,
        };
        let mut saved = Saved::default();
        saved.snapshot.history = vec![entry; HISTORY_LIMIT + 20];
        assert_eq!(
            decode(&serde_json::to_string(&saved).unwrap())
                .snapshot
                .history
                .len(),
            HISTORY_LIMIT
        );
    }
    #[test]
    fn saved_account_context_rejects_server_or_account_changes() {
        let mut settings = crate::settings::Settings::default();
        settings.navidrome.url = "https://one/".into();
        settings.navidrome.username = "first".into();
        let context = source_context(Source::Navidrome, &settings).unwrap();
        assert!(check_context(Source::Navidrome, Some(&context), &settings).is_ok());
        settings.navidrome.username = "second".into();
        assert!(check_context(Source::Navidrome, Some(&context), &settings).is_err());
    }
    #[test]
    fn snapshot_round_trips_atomically() {
        let path =
            std::env::temp_dir().join(format!("capsule-qol-{}.sqlite3", uuid::Uuid::new_v4()));
        let route = Route {
            provider: crate::import::model::Platform::Soundcloud,
            external_id: "123".into(),
        };
        let mut discovered = entry("discovered", Source::Local, None);
        discovered.playback_route = Some(route.clone());
        let saved = Saved {
            snapshot: Snapshot {
                navigation: serde_json::json!({"view":"songs"}),
                playlists: vec![LocalPlaylist {
                    id: "qol:mix".into(),
                    name: "Mix".into(),
                    description: String::new(),
                    cover_id: None,
                    songs: vec![discovered, entry("imported", Source::Local, None)],
                }],
                ..Snapshot::default()
            },
            ..Saved::default()
        };
        write(&path, &saved).unwrap();
        let restored = read(&path).snapshot;
        assert_eq!(restored.navigation, saved.snapshot.navigation);
        assert_eq!(restored.playlists[0].songs[0].playback_route, Some(route));
        assert_eq!(restored.playlists[0].songs[1].playback_route, None);
        std::fs::remove_file(path).unwrap();
    }
}
