use crate::{
    db::SongRow,
    player::{PlayerState, Status, Track},
    settings::Source,
    AppState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

const FILE: &str = "listening-state.sqlite3";
const HISTORY_LIMIT: usize = 200;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongEntry {
    pub song: SongRow,
    pub source: Source,
    #[serde(default)]
    pub context: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPlaylist {
    pub id: String,
    pub name: String,
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
    let json = serde_json::to_string(saved).map_err(|e| e.to_string())?;
    let mut db = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS snapshot(id INTEGER PRIMARY KEY CHECK(id=1),payload TEXT NOT NULL)").map_err(|e|e.to_string())?;
    let transaction = db.transaction().map_err(|e| e.to_string())?;
    transaction.execute("INSERT INTO snapshot VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload",[json]).map_err(|e|e.to_string())?;
    transaction.commit().map_err(|e| e.to_string())
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
fn validate_playlist_sources(
    existing: &[SongEntry],
    incoming: &[SongEntry],
    mut canonical: impl FnMut(&str) -> bool,
) -> Result<(), String> {
    let entries: Vec<_> = existing.iter().chain(incoming).collect();
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
                song,
                source,
                context: context.clone(),
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
            song,
            source,
            context: context.clone(),
        })
        .collect();
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
        validate_playlist_sources(&lists[index].songs, &entries, |id| canonical(&app, id))?;
        index
    } else {
        let name = name
            .unwrap_or_else(|| "New playlist".into())
            .trim()
            .to_string();
        if name.is_empty() || name.len() > 200 {
            return Err("Enter a playlist name up to 200 characters".into());
        }
        lists.push(LocalPlaylist {
            id: format!("qol:{}", uuid::Uuid::new_v4()),
            name,
            songs: vec![],
        });
        lists.len() - 1
    };
    lists[index].songs.extend(entries);
    let id = lists[index].id.clone();
    drop(inner);
    emit(&app);
    Ok(id)
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
) -> Result<(), String> {
    let canonical_queue = !songs.is_empty() && songs.iter().all(|s| canonical(&app, &s.id));
    let tracks = tracks(&app, songs, source)?;
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
    fn entry(id: &str, source: Source, context: Option<&str>) -> SongEntry {
        SongEntry {
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
        }
    }
    #[test]
    fn mixed_playlist_sources_and_accounts_are_rejected_except_all_canonical() {
        let local = entry("one", Source::Local, None);
        let youtube = entry("two", Source::Youtube, None);
        assert!(validate_playlist_sources(
            std::slice::from_ref(&local),
            std::slice::from_ref(&youtube),
            |_| false
        )
        .is_err());
        assert!(validate_playlist_sources(&[local], &[youtube], |_| true).is_ok());
        let one = entry("one", Source::Navidrome, Some("server1"));
        let two = entry("two", Source::Navidrome, Some("server2"));
        assert!(validate_playlist_sources(std::slice::from_ref(&one), &[two], |_| false).is_err());
        assert!(validate_playlist_sources(
            std::slice::from_ref(&one),
            std::slice::from_ref(&one),
            |_| false
        )
        .is_ok());
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
        let saved = Saved {
            snapshot: Snapshot {
                navigation: serde_json::json!({"view":"songs"}),
                ..Snapshot::default()
            },
            ..Saved::default()
        };
        write(&path, &saved).unwrap();
        assert_eq!(read(&path).snapshot.navigation, saved.snapshot.navigation);
        std::fs::remove_file(path).unwrap();
    }
}
