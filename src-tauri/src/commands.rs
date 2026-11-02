use std::collections::HashSet;
use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::auth;
use crate::db::{AlbumRow, LibraryCounts, PlaylistRow, SongRow};

use crate::player::{EngineCommand, Player, PlayerState, Track};
use crate::{sync, AppState, NowReported};

#[tauri::command]
pub fn audio_info(state: State<'_, AppState>) -> crate::audio::AudioInfo {
    state
        .audio
        .lock()
        .expect("audio mutex")
        .as_ref()
        .map(|engine| engine.audio_info())
        .unwrap_or_default()
}

pub fn apply(app: &AppHandle, f: impl FnOnce(&mut Player) -> Vec<EngineCommand>) {
    crate::youtube_native::reconcile_handoff(app);
    let state = app.state::<AppState>();
    let cmds = {
        let mut p = state.player.lock().expect("player mutex");
        f(&mut p)
    };
    commit(app, &state, cmds);
    crate::youtube_native::queue_changed(app);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeAction {
    LoadCurrent,
    Play,
    Pause,
    Seek(u64),
    Volume(u8),
    Ignore,
}

pub fn native_action(cmd: &EngineCommand) -> NativeAction {
    match cmd {
        EngineCommand::SetQueue { .. } | EngineCommand::SkipNext | EngineCommand::SkipPrevious => {
            NativeAction::LoadCurrent
        }
        EngineCommand::Play => NativeAction::Play,
        EngineCommand::Pause => NativeAction::Pause,
        EngineCommand::Seek { ms } => NativeAction::Seek(*ms),
        EngineCommand::SetVolume { percent } => NativeAction::Volume(*percent),
        EngineCommand::Prewarm { .. }
        | EngineCommand::SetShuffle { .. }
        | EngineCommand::SetRepeat { .. } => NativeAction::Ignore,
    }
}

fn commit(app: &AppHandle, state: &AppState, cmds: Vec<EngineCommand>) {
    if cmds.is_empty()
        && state.player.lock().expect("player mutex").state().status != crate::player::Status::Ended
    {
        publish(app, state);
        return;
    }
    if crate::import::playback::dispatch(app, state, &cmds) {
        publish(app, state);
        return;
    }
    crate::import::playback::release(app);
    let source = state.settings.lock().expect("settings mutex").source;
    if source != crate::settings::Source::Youtube {
        crate::youtube_native::stop(app);
    }
    if source == crate::settings::Source::Youtube {
        crate::youtube_native::send_library(app, cmds);
        publish(app, state);
        return;
    }
    let engine = state.audio.lock().expect("audio mutex").clone();
    for c in &cmds {
        tracing::debug!(?c, "engine command");
        match &engine {
            Some(engine) => run_native(app, state, engine, c),
            None => {
                let _ = app.emit("playback://error", "No native audio output is available.");
            }
        }
    }
    publish(app, state);
}

fn run_native(
    app: &AppHandle,
    state: &AppState,
    engine: &crate::audio::Engine,
    cmd: &EngineCommand,
) {
    let action = match native_action(cmd) {
        NativeAction::Play if engine.is_empty() && !engine.is_loading() => {
            NativeAction::LoadCurrent
        }
        other => other,
    };
    match action {
        NativeAction::Ignore => {}
        NativeAction::Play => engine.play(),
        NativeAction::Pause => engine.pause(),
        NativeAction::Seek(ms) => engine.seek(ms),
        NativeAction::Volume(p) => engine.set_volume(p),
        NativeAction::LoadCurrent => {
            let track = state
                .player
                .lock()
                .expect("player mutex")
                .state()
                .current()
                .cloned();
            let Some(track) = track else { return };

            let source = state.settings.lock().expect("settings mutex").source;
            if source == crate::settings::Source::Local {
                if let Err(e) = engine.load_file(std::path::PathBuf::from(&track.id)) {
                    tracing::error!(error = %e, track = %track.id, "could not open local track");
                    let _ = app.emit("playback://error", e.to_string());
                    return;
                }
                engine.play();
                return;
            }

            let client = state.navidrome.lock().expect("navidrome mutex").clone();
            let Some(client) = client else {
                tracing::warn!("native load with no navidrome client");
                return;
            };

            let dir = {
                let data = state.data_dir.lock().expect("data dir mutex").clone();
                crate::stream::cache_dir(data)
            };
            let Some(dir) = dir else { return };
            let path = crate::stream::cache_path(&dir, &track.id);

            crate::stream::prune(&dir, crate::stream::CACHE_CAP_BYTES, Some(&path));
            let url = client.stream_url(&track.id);
            if let Err(e) = engine.load(url, path) {
                tracing::error!(error = %e, track = %track.id, "native load failed");
                let _ = app.emit("playback://error", e.to_string());
                return;
            }
            engine.play();
        }
    }
}

pub(crate) fn publish(app: &AppHandle, state: &AppState) {
    let snapshot = state.player.lock().expect("player mutex").state().clone();
    crate::qol::observe(app, &snapshot);
    if let Err(e) = app.emit("player://state", &snapshot) {
        tracing::warn!(error = %e, "failed to emit player state");
    }
    crate::media_controls::update(app, &snapshot);
    #[cfg(target_os = "windows")]
    crate::thumbbar::refresh(app, snapshot.status);
    report_now_playing(app, state, &snapshot);
}

fn report_now_playing(app: &AppHandle, state: &AppState, snapshot: &PlayerState) {
    use crate::player::Status;

    let track = snapshot.current().cloned();
    let stopped = matches!(snapshot.status, Status::Idle | Status::Ended) || track.is_none();

    let mut last = state.now_reported.lock().expect("now reported mutex");

    if stopped {
        if last.track_id.is_some() {
            last.track_id = None;
            last.queue_index = None;
            last.scrobbled = false;
            let presence = state.discord.lock().expect("discord mutex").clone();
            if let Some(p) = presence {
                let ticket = p.begin_update();
                std::thread::spawn(move || p.clear_current(ticket));
            }
        }
        return;
    }

    let Some(track) = track else { return };
    let changed =
        last.track_id.as_deref() != Some(track.id.as_str()) || last.queue_index != snapshot.index;
    if changed {
        last.track_id = Some(track.id.clone());
        last.queue_index = snapshot.index;
        last.scrobbled = false;
        scrobble(app, &track, snapshot.position_ms, false);
        present(app, track.clone(), snapshot.status);
    }
    if !last.scrobbled && crate::lastfm::scrobble_due(snapshot.position_ms, track.duration_ms) {
        last.scrobbled = true;
        scrobble(app, &track, snapshot.position_ms, true);
    }
}

fn scrobble(app: &AppHandle, track: &Track, position_ms: u64, submission: bool) {
    let state = app.state::<AppState>();

    let session = { state.lastfm.lock().expect("lastfm mutex").clone() };
    if let Some(session) = session {
        let track = track.clone();
        let started = crate::lastfm::started_at(crate::lastfm::now_unix(), position_ms);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let (api_key, shared_secret) = {
                let s = app.state::<AppState>();
                let s = s.settings.lock().expect("settings mutex");
                (s.api_key().to_string(), s.shared_secret().to_string())
            };
            let http = reqwest::Client::new();
            let auth = crate::lastfm::Auth {
                api_key: &api_key,
                shared_secret: &shared_secret,
                session_key: &session.key,
            };
            let play = crate::lastfm::Play {
                artist: &track.artist,
                title: &track.title,
                album: &track.album,
            };
            let result = if submission {
                crate::lastfm::scrobble(&http, auth, play, started).await
            } else {
                crate::lastfm::update_now_playing(&http, auth, play).await
            };
            if let Err(e) = result {
                tracing::debug!(error = %e, title = %track.title, submission, "last.fm scrobble rejected");
            }
        });
        return;
    }

    let client = { state.navidrome.lock().expect("navidrome mutex").clone() };
    let Some(client) = client else { return };
    let track_id = if crate::import::playback::owns_current(app) {
        match crate::import::playback::active_route(app) {
            Some(route) if route.provider == crate::import::model::Platform::Navidrome => {
                route.external_id
            }
            _ => return,
        }
    } else {
        track.id.clone()
    };
    tauri::async_runtime::spawn(async move {
        if let Err(e) = client.scrobble(&track_id, submission).await {
            tracing::debug!(error = %e, %track_id, submission, "scrobble rejected");
        }
    });
}

fn present(app: &AppHandle, track: Track, status: crate::player::Status) {
    let presence = {
        app.state::<AppState>()
            .discord
            .lock()
            .expect("discord mutex")
            .clone()
    };
    let Some(presence) = presence else { return };
    let ticket = presence.begin_update();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();

        let (from_server, api_key, source) = {
            let s = state.settings.lock().expect("settings mutex");
            (
                s.discord.serve_art_from_server,
                s.api_key().to_string(),
                s.source,
            )
        };

        let route = crate::import::playback::active_route(&app);
        let artwork = {
            let provider = state.db.lock().expect("db mutex");
            let imports = app.try_state::<crate::import::State>();
            let imports = imports
                .as_ref()
                .map(|state| state.db.lock().expect("import db mutex"));
            crate::discord::library_artwork(
                imports.as_deref(),
                &provider,
                &track.id,
                route.as_ref(),
                source,
                from_server,
            )
        };
        let image = match artwork {
            Some(crate::discord::CoverArt::PublicUrl(url)) => Some(url),
            Some(crate::discord::CoverArt::NavidromeId(id)) => {
                let client = { state.navidrome.lock().expect("navidrome mutex").clone() };
                client.map(|client| client.cover_art_url(&id, 300))
            }
            None => None,
        };
        let image = image.filter(|url| url.chars().count() <= 300);
        let image = if image.is_some() {
            image
        } else {
            let http = reqwest::Client::new();
            crate::lastfm::album_art(&http, &api_key, &track.artist, &track.album)
                .await
                .and_then(|url| crate::discord::public_artwork_url(&url))
        };

        std::thread::spawn(move || presence.show(ticket, &track, status, image.as_deref()));
    });
}

#[tauri::command]
pub fn player_snapshot(state: State<'_, AppState>) -> PlayerState {
    state.player.lock().expect("player mutex").state().clone()
}

#[tauri::command]
pub fn player_play(app: AppHandle, state: State<'_, AppState>) {
    let cmds = state.player.lock().expect("player mutex").play();
    commit(&app, &state, cmds);
}

#[tauri::command]
pub fn player_pause(app: AppHandle, state: State<'_, AppState>) {
    let cmds = state.player.lock().expect("player mutex").pause();
    commit(&app, &state, cmds);
}

#[tauri::command]
pub fn player_toggle(app: AppHandle, state: State<'_, AppState>) {
    let cmds = state.player.lock().expect("player mutex").toggle();
    commit(&app, &state, cmds);
}

#[tauri::command]
pub fn player_next(app: AppHandle, state: State<'_, AppState>) {
    let cmds = state.player.lock().expect("player mutex").next_track();
    commit(&app, &state, cmds);
}

#[tauri::command]
pub fn player_previous(app: AppHandle, state: State<'_, AppState>) {
    let cmds = state.player.lock().expect("player mutex").previous_track();
    commit(&app, &state, cmds);
}

#[tauri::command]
pub fn player_seek(app: AppHandle, state: State<'_, AppState>, ms: u64) {
    let cmds = state.player.lock().expect("player mutex").seek(ms);
    commit(&app, &state, cmds);
}

#[tauri::command]
pub fn player_set_volume(app: AppHandle, state: State<'_, AppState>, percent: u8) {
    let cmds = state
        .player
        .lock()
        .expect("player mutex")
        .set_volume(percent);
    commit(&app, &state, cmds);
}

#[tauri::command]
pub fn player_toggle_shuffle(app: AppHandle, state: State<'_, AppState>) {
    let cmds = state.player.lock().expect("player mutex").toggle_shuffle();
    commit(&app, &state, cmds);
}

#[tauri::command]
pub fn player_cycle_repeat(app: AppHandle, state: State<'_, AppState>) {
    let cmds = state.player.lock().expect("player mutex").cycle_repeat();
    commit(&app, &state, cmds);
}

fn db_err(e: crate::db::DbError) -> String {
    tracing::error!(error = %e, "library query failed");
    e.to_string()
}

/// Query the provider directly when no imports exist; otherwise use one merged
/// projection per pair of database revisions, shared by every paginated view.
pub(crate) fn with_library<T>(
    app: &AppHandle,
    state: &AppState,
    query: impl FnOnce(&crate::db::Db) -> Result<T, crate::db::DbError>,
) -> Result<T, String> {
    let source = state.settings.lock().expect("settings mutex").source;
    let platform = match source {
        crate::settings::Source::Apple => "apple_music",
        crate::settings::Source::Spotify => "spotify",
        crate::settings::Source::Youtube => "youtube",
        crate::settings::Source::Soundcloud => "soundcloud",
        crate::settings::Source::Navidrome => "navidrome",
        crate::settings::Source::Local => "local",
    };
    let provider = state.db.lock().expect("db mutex");
    let Some(imports) = app.try_state::<crate::import::State>() else {
        return query(&provider).map_err(db_err);
    };
    let imports = imports
        .db
        .lock()
        .map_err(|_| "Import database unavailable.")?;
    if !imports.has_imported_tracks().map_err(db_err)? {
        return query(&provider).map_err(db_err);
    }
    let view = crate::db::LibraryView::cached(
        &provider,
        &imports,
        platform,
        state.db_generation.load(Ordering::SeqCst),
    )
    .map_err(db_err)?;
    drop(imports);
    drop(provider);
    let view = view.lock().map_err(|_| "Library cache unavailable.")?;
    query(&view.db).map_err(db_err)
}

pub(crate) fn library_artwork_template(
    app: &AppHandle,
    id: &str,
) -> Result<Option<String>, String> {
    with_library(app, &app.state::<AppState>(), |db| {
        db.artwork_for(id).map(|art| art.map(|art| art.template))
    })
}

#[tauri::command]
pub fn library_songs(
    app: AppHandle,
    limit: Option<u32>,
    offset: Option<u32>,
    sort: Option<String>,
    direction: Option<String>,
    query: Option<String>,
    artist_name: Option<String>,
) -> Result<Vec<SongRow>, String> {
    let state = app.state::<AppState>();
    with_library(&app, &state, |db| {
        db.songs_query(
            limit.unwrap_or(200).min(1000),
            offset.unwrap_or(0),
            sort.as_deref().unwrap_or("title"),
            direction.as_deref().unwrap_or("asc"),
            query.as_deref(),
            artist_name.as_deref(),
        )
    })
}

#[tauri::command]
pub fn library_albums(
    app: AppHandle,
    state: State<'_, AppState>,
    limit: Option<u32>,
    offset: Option<u32>,
    sort: Option<String>,
    direction: Option<String>,
    artist_name: Option<String>,
) -> Result<Vec<AlbumRow>, String> {
    with_library(&app, &state, |db| {
        db.albums_query(
            limit.unwrap_or(200).min(1000),
            offset.unwrap_or(0),
            sort.as_deref().unwrap_or("title"),
            direction.as_deref().unwrap_or("asc"),
            artist_name.as_deref(),
        )
    })
}

#[tauri::command]
pub fn library_artists(
    app: AppHandle,
    state: State<'_, AppState>,
    limit: Option<u32>,
    offset: Option<u32>,
    sort: Option<String>,
    direction: Option<String>,
) -> Result<Vec<crate::db::ArtistRow>, String> {
    if sort.as_deref().is_some_and(|s| s != "name") {
        return Err("Unsupported artist sort.".into());
    }
    let mut rows = with_library(&app, &state, |db| db.artists())?;
    if direction.as_deref() == Some("desc") {
        rows.reverse();
    }
    Ok(rows
        .into_iter()
        .skip(offset.unwrap_or(0) as usize)
        .take(limit.unwrap_or(200).min(1000) as usize)
        .collect())
}

#[tauri::command]
pub fn library_playlists(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<PlaylistRow>, String> {
    let mut rows = state
        .db
        .lock()
        .expect("db mutex")
        .playlists()
        .map_err(db_err)?;
    if let Some(imports) = app.try_state::<crate::import::State>() {
        rows.extend(
            imports
                .db
                .lock()
                .map_err(|_| "Import database unavailable.")?
                .playlists()
                .map_err(db_err)?,
        );
    }
    rows.sort_by_key(|p| p.name.to_lowercase());
    Ok(rows)
}

#[tauri::command]
pub fn library_album_songs(
    app: AppHandle,
    state: State<'_, AppState>,
    album_id: String,
) -> Result<Vec<SongRow>, String> {
    with_library(&app, &state, |db| db.album_songs(&album_id))
}

#[tauri::command]
pub fn library_playlist_songs(
    app: AppHandle,
    state: State<'_, AppState>,
    playlist_id: String,
) -> Result<Vec<SongRow>, String> {
    if let Some(rows) = crate::import::jobs::playlist_songs(&app, &playlist_id)? {
        return Ok(rows);
    }
    let db = state.db.lock().expect("db mutex");
    db.playlist_songs(&playlist_id).map_err(db_err)
}

#[tauri::command]
pub fn library_search(
    app: AppHandle,
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<SongRow>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    with_library(&app, &state, |db| {
        db.search(query.trim(), limit.unwrap_or(100).min(500))
    })
}

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> crate::settings::Settings {
    state.settings.lock().expect("settings mutex").clone()
}

#[tauri::command]
pub fn settings_set(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: crate::settings::Settings,
) -> Result<(), String> {
    let dir = state.data_dir.lock().expect("data dir mutex").clone();
    let Some(dir) = dir else {
        return Err("no data directory; settings cannot be saved".into());
    };

    let previous_settings = state.settings.lock().expect("settings mutex").clone();
    let previous = previous_settings.source;
    let previous_theme = &previous_settings.appearance.theme;
    let source = settings.source;
    let theme_id = settings.appearance.theme.clone();
    let previous_palette = state.theme.lock().expect("theme mutex").clone();
    let next_palette = if &theme_id != previous_theme {
        crate::theme::resolve(&crate::theme::load_all(Some(&dir)), &theme_id)
    } else {
        previous_palette.clone()
    };
    let mut apply_material = |appearance: &crate::settings::Appearance| {
        let Some(window) = app.get_webview_window("main") else { return Ok(()) };
        let palette = if appearance.theme == theme_id { &next_palette } else { &previous_palette };
        crate::appearance::apply(&window, &appearance.glass, palette)
    };
    crate::appearance::save_with_material(&dir, &previous_settings, &settings, &mut apply_material)?;
    if source != previous {
        if let Err(error) = use_database_for(&app, source) {
            if let Err(rollback) = crate::appearance::save_with_material(&dir, &settings, &previous_settings, &mut apply_material) {
                tracing::warn!(error = %rollback, "could not restore settings after source failure");
            }
            return Err(error);
        }
    }
    let appearance_changed = settings.appearance != previous_settings.appearance;
    let glass = settings.appearance.glass.clone();
    *state.settings.lock().expect("settings mutex") = settings;

    if &theme_id != previous_theme {
        let css = next_palette.css();
        *state.theme.lock().expect("theme mutex") = next_palette;
        let _ = app.emit("theme://changed", css);
    }
    if appearance_changed {
        let _ = app.emit("appearance://changed", glass);
    }

    if source != previous {
        reconcile_backend(&app, source);
        let _ = app.emit("library://updated", sync::counts(&app));
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct ThemeSummary {
    pub id: String,
    pub name: String,
    pub polarity: crate::theme::Polarity,
    pub builtin: bool,
    pub failures: Vec<crate::theme::Failure>,
}

#[tauri::command]
pub fn themes_list(state: State<'_, AppState>) -> Vec<ThemeSummary> {
    let dir = state.data_dir.lock().expect("data dir mutex").clone();
    let on_disk: HashSet<String> = dir
        .as_deref()
        .map(crate::theme::themes_dir)
        .and_then(|d| std::fs::read_dir(d).ok())
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
                .filter_map(|e| {
                    e.path()
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    crate::theme::load_all(dir.as_deref())
        .into_iter()
        .map(|t| ThemeSummary {
            builtin: !on_disk.contains(&t.id),
            failures: t.failures(),
            id: t.id,
            name: t.name,
            polarity: t.polarity,
        })
        .collect()
}

#[derive(Debug, serde::Serialize)]
pub struct Lyrics {
    pub lines: Vec<crate::lyrics::Line>,
    pub plain: Option<String>,
}

#[tauri::command]
pub async fn lyrics_for(app: AppHandle, track_id: String) -> Result<Lyrics, String> {
    lookup_lyrics(app, track_id, false).await
}

#[tauri::command]
pub async fn lyrics_refresh(app: AppHandle, track_id: String) -> Result<Lyrics, String> {
    lookup_lyrics(app, track_id, true).await
}

#[tauri::command]
pub fn lyrics_set_offset(state: State<'_, AppState>, offset_ms: i64) -> Result<i64, String> {
    let dir = state
        .data_dir
        .lock()
        .expect("data dir mutex")
        .clone()
        .ok_or("no data directory; settings cannot be saved")?;
    let mut selected = state.settings.lock().expect("settings mutex");
    let mut updated = selected.clone();
    updated.lyrics.offset_ms = offset_ms.clamp(-5_000, 5_000);
    crate::settings::save(&dir, &updated).map_err(|e| e.to_string())?;
    *selected = updated;
    Ok(selected.lyrics.offset_ms)
}

async fn lookup_lyrics(app: AppHandle, track_id: String, refresh: bool) -> Result<Lyrics, String> {
    // Same-track lookups share a gate and recheck the cache after acquiring it.
    type Gate = std::sync::Arc<tokio::sync::Mutex<()>>;
    static GATES: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
        >,
    > = std::sync::OnceLock::new();
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(3);
    let gate: Gate = {
        let mut gates = GATES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "lyrics unavailable")?;
        gates.retain(|_, gate| gate.strong_count() > 0);
        let gate = gates
            .get(&track_id)
            .and_then(|g| g.upgrade())
            .unwrap_or_else(|| std::sync::Arc::new(tokio::sync::Mutex::new(())));
        gates.insert(track_id.clone(), std::sync::Arc::downgrade(&gate));
        gate
    };
    let _guard = gate.lock().await;
    let state = app.state::<AppState>();
    let imports = app.try_state::<crate::import::State>();
    let canonical = imports
        .as_ref()
        .map(|imports| {
            imports
                .db
                .lock()
                .expect("import db mutex")
                .song_for_lyrics(&track_id)
        })
        .transpose()
        .map_err(db_err)?
        .flatten();
    let song = match canonical {
        Some(song) => Some(song),
        None => state
            .db
            .lock()
            .expect("db mutex")
            .song_for_lyrics(&track_id)
            .map_err(db_err)?,
    };
    // A single stable cache owner survives provider database switching.
    let cached = if refresh {
        None
    } else if let Some(imports) = imports.as_ref() {
        imports
            .db
            .lock()
            .expect("import db mutex")
            .lyrics(&track_id)
            .map_err(db_err)?
    } else {
        state
            .db
            .lock()
            .expect("db mutex")
            .lyrics(&track_id)
            .map_err(db_err)?
    };
    if let Some((synced, plain)) = cached {
        return Ok(Lyrics {
            lines: synced
                .as_deref()
                .map(crate::lyrics::parse_lrc)
                .unwrap_or_default(),
            plain,
        });
    }
    let Some(song) = song else {
        return Ok(Lyrics {
            lines: Vec::new(),
            plain: None,
        });
    };
    let _slot = SLOTS.acquire().await.map_err(|_| "lyrics unavailable")?;
    let fetched = crate::lyrics::fetch(&song).await.map_err(|_| {
        tracing::warn!("lyrics lookup failed");
        "Could not load lyrics. Please retry.".to_string()
    })?;
    if let Some(imports) = imports.as_ref() {
        imports
            .db
            .lock()
            .expect("import db mutex")
            .save_lyrics(
                &track_id,
                fetched.synced.as_deref(),
                fetched.plain.as_deref(),
            )
            .map_err(db_err)?;
    } else {
        state
            .db
            .lock()
            .expect("db mutex")
            .save_lyrics(
                &track_id,
                fetched.synced.as_deref(),
                fetched.plain.as_deref(),
            )
            .map_err(db_err)?;
    }
    Ok(Lyrics {
        lines: fetched
            .synced
            .as_deref()
            .map(crate::lyrics::parse_lrc)
            .unwrap_or_default(),
        plain: fetched.plain,
    })
}

#[tauri::command]
pub fn library_counts(app: AppHandle) -> Result<LibraryCounts, String> {
    Ok(crate::sync::counts(&app))
}

#[tauri::command]
pub fn library_sync(app: AppHandle) {
    tauri::async_runtime::spawn(async move { sync::run(app).await });
}

fn playable_id(source: crate::settings::Source, s: &SongRow) -> Option<String> {
    use crate::settings::Source;
    if s.catalog_id.as_deref() == Some(s.id.as_str()) && uuid::Uuid::parse_str(&s.id).is_ok() {
        return Some(s.id.clone());
    }
    match source {
        Source::Apple | Source::Spotify | Source::Soundcloud => None,
        Source::Youtube => s.catalog_id.clone(),
        Source::Navidrome | Source::Local => Some(s.id.clone()),
    }
}

fn queue_selection(
    source: crate::settings::Source,
    songs: Vec<SongRow>,
    start_index: usize,
    dead: &HashSet<String>,
) -> (Vec<Track>, usize) {
    let mut selected = None;
    let playable: Vec<Track> = songs
        .into_iter()
        .enumerate()
        .filter_map(|(original_index, s)| {
            let id = playable_id(source, &s)?;
            if dead.contains(&id) {
                return None;
            }
            Some((
                original_index,
                Track {
                    id,
                    title: s.name,
                    artist: s.artist_name,
                    album: s.album_name,
                    duration_ms: s.duration_ms,
                    playback_route: None,
                },
            ))
        })
        .enumerate()
        .map(|(playable_index, (original_index, track))| {
            if selected.is_none() && original_index >= start_index {
                selected = Some(playable_index);
            }
            track
        })
        .collect();
    let start = selected.unwrap_or_else(|| playable.len().saturating_sub(1));
    (playable, start)
}

fn enqueue(app: &AppHandle, songs: Vec<SongRow>, start_index: usize) -> bool {
    let state = app.state::<AppState>();

    let source = state.settings.lock().expect("settings mutex").source;

    let dead = state
        .db
        .lock()
        .expect("db mutex")
        .unresolvable_ids()
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not read unresolvable ids; queueing unfiltered");
            HashSet::new()
        });

    let (playable, start) = queue_selection(source, songs, start_index, &dead);

    if playable.is_empty() {
        return false;
    }

    let cmds = state
        .player
        .lock()
        .expect("player mutex")
        .play_queue(playable, start);
    commit(app, &state, cmds);
    true
}

#[tauri::command]
pub fn play_songs(app: AppHandle, songs: Vec<SongRow>, start_index: Option<usize>) {
    if !enqueue(&app, songs, start_index.unwrap_or(0)) {
        tracing::warn!("play_songs: nothing in the selection was playable");
    }
}

fn api_client(app: &AppHandle) -> Result<crate::api::Client, String> {
    let tokens = {
        app.state::<AppState>()
            .tokens
            .lock()
            .expect("tokens mutex")
            .clone()
    };
    let tokens = tokens.filter(|t| t.is_complete()).ok_or("not signed in")?;
    crate::api::Client::new(tokens).map_err(|e| e.to_string())
}

async fn ensure_album_songs(app: &AppHandle, album_id: &str) -> Result<Vec<SongRow>, String> {
    if album_id.starts_with("import-album:") {
        return with_library(app, &app.state::<AppState>(), |db| db.album_songs(album_id));
    }
    let cached = with_library(app, &app.state::<AppState>(), |db| db.album_songs(album_id));
    if let Ok(rows) = cached {
        if !rows.is_empty() {
            return Ok(rows);
        }
    }

    let state = app.state::<AppState>();
    let source = state.settings.lock().expect("settings mutex").source;
    let generation = state.db_generation.load(Ordering::SeqCst);
    let fetched = match source {
        crate::settings::Source::Apple => api_client(app)?
            .album_tracks(album_id)
            .await
            .map_err(|e| e.to_string())?,
        _ => return Err("no tracks for this album yet - try syncing your library".into()),
    };
    if fetched.is_empty() {
        return Err("album has no tracks".into());
    }
    {
        let state = app.state::<AppState>();
        let mut db = state.db.lock().expect("db mutex");
        if state.db_generation.load(Ordering::SeqCst) != generation {
            return Err("Library changed while loading this album. Try again.".into());
        }
        db.upsert_songs(&fetched).map_err(db_err)?;
    }
    app.state::<AppState>()
        .db
        .lock()
        .expect("db mutex")
        .album_songs(album_id)
        .map_err(db_err)
}

async fn ensure_playlist_songs(app: &AppHandle, playlist_id: &str) -> Result<Vec<SongRow>, String> {
    if let Some(rows) = crate::import::jobs::playlist_songs(app, playlist_id)? {
        return Ok(rows);
    }
    let cached = {
        app.state::<AppState>()
            .db
            .lock()
            .expect("db mutex")
            .playlist_songs(playlist_id)
    };
    if let Ok(rows) = cached {
        if !rows.is_empty() {
            return Ok(rows);
        }
    }
    let state = app.state::<AppState>();
    let source = state.settings.lock().expect("settings mutex").source;
    let generation = state.db_generation.load(Ordering::SeqCst);
    let fetched = match source {
        crate::settings::Source::Apple => api_client(app)?
            .playlist_tracks(playlist_id)
            .await
            .map_err(|e| e.to_string())?,
        _ => return Err("no tracks for this playlist yet - try syncing your library".into()),
    };
    if fetched.is_empty() {
        return Err("playlist has no tracks".into());
    }
    let ids: Vec<String> = fetched.iter().map(|s| s.id.clone()).collect();
    {
        let state = app.state::<AppState>();
        let mut db = state.db.lock().expect("db mutex");
        if state.db_generation.load(Ordering::SeqCst) != generation {
            return Err("Library changed while loading this playlist. Try again.".into());
        }
        db.upsert_songs(&fetched).map_err(db_err)?;
        db.set_playlist_tracks(playlist_id, &ids).map_err(db_err)?;
    }
    app.state::<AppState>()
        .db
        .lock()
        .expect("db mutex")
        .playlist_songs(playlist_id)
        .map_err(db_err)
}

#[tauri::command]
pub async fn play_album(app: AppHandle, album_id: String) -> Result<(), String> {
    let songs = ensure_album_songs(&app, &album_id).await?;
    if !enqueue(&app, songs, 0) {
        return Err("no tracks in this album are playable".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn play_playlist(app: AppHandle, playlist_id: String) -> Result<(), String> {
    let songs = ensure_playlist_songs(&app, &playlist_id).await?;
    if !enqueue(&app, songs, 0) {
        return Err("no tracks in this playlist are playable".into());
    }
    Ok(())
}

pub(crate) fn reconcile_backend(app: &AppHandle, source: crate::settings::Source) {
    crate::import::playback::release(app);
    use crate::settings::Source;
    let state = app.state::<AppState>();

    crate::youtube_native::stop(app);

    if let Some(engine) = state.audio.lock().expect("audio mutex").clone() {
        engine.stop();
    }
    state.player.lock().expect("player mutex").reset_queue();
    *state.now_reported.lock().expect("now reported mutex") = NowReported::default();
    if let Some(presence) = state.discord.lock().expect("discord mutex").clone() {
        let ticket = presence.begin_update();
        std::thread::spawn(move || presence.clear_current(ticket));
    }

    if source == Source::Youtube {
        *state.audio.lock().expect("audio mutex") = None;
        *state.navidrome.lock().expect("navidrome mutex") = None;
        return;
    }

    let settings = state.settings.lock().expect("settings mutex").clone();

    *state.navidrome.lock().expect("navidrome mutex") =
        crate::source::navidrome_client(&settings).map(std::sync::Arc::new);

    let has_engine = state.audio.lock().expect("audio mutex").is_some();
    if !has_engine {
        match crate::desktop::open_engine(app) {
            Ok(e) => {
                *state.audio.lock().expect("audio mutex") = Some(std::sync::Arc::new(e));
            }
            Err(e) => {
                tracing::error!(error = %e, "no audio output; playback disabled");
                let _ = app.emit("playback://error", e.to_string());
            }
        }
    }
}

fn use_database_for(app: &AppHandle, source: crate::settings::Source) -> Result<(), String> {
    let state = app.state::<AppState>();
    let dir = state.data_dir.lock().expect("data dir mutex").clone();
    let path = crate::db::default_db_path(dir, source).map_err(|e| e.to_string())?;
    let opened = crate::db::Db::open_at(&path).map_err(|e| e.to_string())?;
    tracing::info!(path = %path.display(), ?source, "switching library database");
    {
        let mut db = state.db.lock().expect("db mutex");
        *db = opened;
        state.db_generation.fetch_add(1, Ordering::SeqCst);
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavidromeStatus {
    pub configured: bool,
    pub url: String,
    pub username: String,
    pub insecure: bool,
}

#[tauri::command]
pub fn navidrome_status(state: State<'_, AppState>) -> NavidromeStatus {
    let settings = state.settings.lock().expect("settings mutex").clone();
    let has_password =
        crate::config::navidrome_env().is_some() || auth::load_navidrome().ok().flatten().is_some();
    NavidromeStatus {
        insecure: crate::subsonic::is_insecure(&settings.navidrome.url),
        configured: has_password
            && !settings.navidrome.url.trim().is_empty()
            && !settings.navidrome.username.trim().is_empty(),
        url: settings.navidrome.url,
        username: settings.navidrome.username,
    }
}

#[tauri::command]
pub async fn navidrome_connect(
    app: AppHandle,
    url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let client = crate::subsonic::Client::new(crate::subsonic::Credentials {
        base_url: url,
        username: username.clone(),
        password: password.clone(),
    })
    .map_err(|e| e.to_string())?;

    client.ping().await.map_err(|e| match e {
        crate::subsonic::SubsonicError::Unauthorized => "wrong username or password".to_string(),
        crate::subsonic::SubsonicError::Http(_) => "server unreachable".to_string(),
        other => other.to_string(),
    })?;

    auth::save_navidrome(&auth::NavidromeCredentials { password }).map_err(|e| e.to_string())?;

    {
        let state = app.state::<AppState>();
        let dir = state.data_dir.lock().expect("data dir mutex").clone();
        let mut settings = state.settings.lock().expect("settings mutex");
        settings.source = crate::settings::Source::Navidrome;
        settings.navidrome.url = client.base_url().to_string();
        settings.navidrome.username = username;
        match dir {
            Some(dir) => {
                if let Err(e) = crate::settings::save(&dir, &settings) {
                    tracing::error!(error = %e, "failed to persist navidrome settings");
                }
            }
            None => tracing::warn!("no data directory; navidrome settings not persisted"),
        }
    }

    use_database_for(&app, crate::settings::Source::Navidrome)?;
    reconcile_backend(&app, crate::settings::Source::Navidrome);
    let _ = app.emit("library://updated", sync::counts(&app));

    let handle = app.clone();
    tauri::async_runtime::spawn(async move { sync::run(handle).await });
    Ok(())
}

const LASTFM_AUTH_WINDOW: &str = "lastfm-auth";

fn close_lastfm_window(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(w) = app.get_webview_window(LASTFM_AUTH_WINDOW) {
            let _ = w.close();
        }
    });
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastfmStatus {
    pub configured: bool,
    pub linked: bool,
    pub username: Option<String>,
}

#[tauri::command]
pub fn lastfm_status(state: State<'_, AppState>) -> LastfmStatus {
    let configured = state
        .settings
        .lock()
        .expect("settings mutex")
        .lastfm_enabled();
    let session = state.lastfm.lock().expect("lastfm mutex").clone();
    LastfmStatus {
        configured,
        linked: session.is_some(),
        username: session.map(|s| s.username),
    }
}

#[tauri::command]
pub async fn lastfm_connect(app: AppHandle) -> Result<(), String> {
    let (api_key, shared_secret) = {
        let state = app.state::<AppState>();
        let s = state.settings.lock().expect("settings mutex");
        (s.api_key().to_string(), s.shared_secret().to_string())
    };
    if api_key.trim().is_empty() || shared_secret.trim().is_empty() {
        return Err("add a Last.fm api key and shared secret first".to_string());
    }

    let http = reqwest::Client::new();
    let token = crate::lastfm::request_token(&http, &api_key, &shared_secret)
        .await
        .map_err(|e| e.to_string())?;

    let url = crate::lastfm::authorize_url(&api_key, &token);
    let parsed: url::Url = url
        .parse()
        .map_err(|_| "could not build the sign-in url".to_string())?;

    let opener = app.clone();
    app.run_on_main_thread(move || {
        let built = tauri::WebviewWindowBuilder::new(
            &opener,
            LASTFM_AUTH_WINDOW,
            tauri::WebviewUrl::External(parsed),
        )
        .title("Authorise capsule on Last.fm")
        .inner_size(560.0, 760.0)
        .build();
        if let Err(e) = built {
            tracing::error!(error = %e, "could not open the last.fm sign-in window");
        }
    })
    .map_err(|e| e.to_string())?;

    tauri::async_runtime::spawn(async move {
        for _ in 0..90 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            match crate::lastfm::fetch_session(&http, &api_key, &shared_secret, &token).await {
                Ok(session) => {
                    if let Err(e) = auth::save_lastfm(&session) {
                        tracing::warn!(error = %e, "could not store the last.fm session");
                    }
                    {
                        let state = app.state::<AppState>();
                        *state.lastfm.lock().expect("lastfm mutex") = Some(session);
                        let _ = app.emit("lastfm://linked", lastfm_status(state));
                    }
                    close_lastfm_window(&app);
                    return;
                }
                Err(crate::lastfm::LastfmError::NotAuthorized) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "last.fm authorisation failed");
                    close_lastfm_window(&app);
                    let _ = app.emit("lastfm://failed", e.to_string());
                    return;
                }
            }
        }
        close_lastfm_window(&app);
        let _ = app.emit(
            "lastfm://failed",
            "timed out waiting for authorisation".to_string(),
        );
    });

    Ok(())
}

#[tauri::command]
pub fn lastfm_disconnect(app: AppHandle) -> Result<(), String> {
    auth::clear_lastfm().map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    *state.lastfm.lock().expect("lastfm mutex") = None;
    let _ = app.emit("lastfm://linked", lastfm_status(state));
    Ok(())
}

#[tauri::command]
pub fn dev_diagnostics(app: AppHandle) -> String {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().expect("settings mutex").clone();
    let player = state.player.lock().expect("player mutex").state().clone();
    let counts = sync::counts(&app);

    let has_audio = state.audio.lock().expect("audio mutex").is_some();
    let has_navidrome = state.navidrome.lock().expect("navidrome mutex").is_some();
    let has_discord = state.discord.lock().expect("discord mutex").is_some();

    let db_path = {
        let dir = state.data_dir.lock().expect("data dir mutex").clone();
        crate::db::default_db_path(dir, settings.source)
            .map(|p| {
                p.file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .unwrap_or_else(|_| "unknown".into())
    };

    format!(
        "capsule {}\n\
         source: {:?}   glass: {}\n\
         backend: audio={} navidrome={} discord={}\n\
         database: {}\n\
         library: {} songs, {} albums, {} playlists, {} artists\n\
         player: {:?}  queue={} index={:?} position={}ms volume={} shuffle={} repeat={:?}\n\
         navidrome: configured={} https={}\n\
         lastfm_key={} discord_id={} serve_art_from_server={}\n\
         onboarded: {}  developer: {}",
        env!("CARGO_PKG_VERSION"),
        settings.source,
        settings.appearance.glass,
        has_audio,
        has_navidrome,
        has_discord,
        db_path,
        counts.songs,
        counts.albums,
        counts.playlists,
        counts.artists,
        player.status,
        player.queue.len(),
        player.index,
        player.position_ms,
        player.volume,
        player.shuffle,
        player.repeat,
        !settings.navidrome.url.trim().is_empty(),
        !crate::subsonic::is_insecure(&settings.navidrome.url),
        !settings.api_key().trim().is_empty(),
        !settings.discord_client_id().trim().is_empty(),
        settings.discord.serve_art_from_server,
        settings.onboarded,
        settings.developer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::EngineCommand;

    #[test]
    fn musickit_only_commands_are_no_ops_natively() {
        assert_eq!(
            native_action(&EngineCommand::Prewarm { id: "x".into() }),
            NativeAction::Ignore
        );
        assert_eq!(
            native_action(&EngineCommand::SetShuffle { on: true }),
            NativeAction::Ignore
        );
        assert_eq!(
            native_action(&EngineCommand::SetRepeat { mode: 2 }),
            NativeAction::Ignore
        );
    }

    #[test]
    fn transport_commands_map_directly() {
        assert_eq!(native_action(&EngineCommand::Play), NativeAction::Play);
        assert_eq!(native_action(&EngineCommand::Pause), NativeAction::Pause);
        assert_eq!(
            native_action(&EngineCommand::Seek { ms: 5000 }),
            NativeAction::Seek(5000)
        );
        assert_eq!(
            native_action(&EngineCommand::SetVolume { percent: 40 }),
            NativeAction::Volume(40)
        );
    }

    fn row(id: &str, catalog: Option<&str>) -> SongRow {
        SongRow {
            id: id.into(),
            catalog_id: catalog.map(|c| c.into()),
            name: "t".into(),
            artist_name: "a".into(),
            album_name: "al".into(),
            duration_ms: 1000,
            artwork_url: None,
        }
    }

    #[test]
    fn native_sources_queue_by_library_id_not_catalog_id() {
        use crate::settings::Source;

        let navidrome_track = row("tr-1", None);
        assert_eq!(
            playable_id(Source::Navidrome, &navidrome_track).as_deref(),
            Some("tr-1")
        );
        assert_eq!(
            playable_id(Source::Local, &navidrome_track).as_deref(),
            Some("tr-1")
        );
    }

    #[test]
    fn canonical_tracks_route_by_id_for_every_active_source_and_keep_occurrences() {
        use crate::settings::Source;
        let id = "c584f4f4-628c-43c8-b619-27c2ee7523d3";
        for source in [
            Source::Local,
            Source::Navidrome,
            Source::Youtube,
            Source::Soundcloud,
            Source::Apple,
            Source::Spotify,
        ] {
            assert_eq!(playable_id(source, &row(id, Some(id))).as_deref(), Some(id));
            let songs = vec![row(id, Some(id)), row(id, Some(id))];
            let (tracks, start) = queue_selection(source, songs, 1, &HashSet::new());
            assert_eq!((tracks.len(), start), (2, 1));
            assert_eq!(tracks[1].id, id);
        }
    }
    #[test]
    fn retired_apple_source_cannot_enqueue_tracks() {
        use crate::settings::Source;
        assert_eq!(
            playable_id(Source::Apple, &row("lib-1", Some("cat-1"))).as_deref(),
            None
        );
        assert_eq!(playable_id(Source::Apple, &row("lib-1", None)), None);
    }

    #[test]
    fn queue_and_skip_commands_load_the_current_index() {
        assert_eq!(
            native_action(&EngineCommand::SetQueue {
                ids: vec!["a".into()],
                start_index: 0
            }),
            NativeAction::LoadCurrent
        );
        assert_eq!(
            native_action(&EngineCommand::SkipNext),
            NativeAction::LoadCurrent
        );
        assert_eq!(
            native_action(&EngineCommand::SkipPrevious),
            NativeAction::LoadCurrent
        );
    }

    #[test]
    fn queue_selection_keeps_clicked_duplicate_occurrence_after_filtering() {
        use crate::settings::Source;
        let songs = vec![
            row("missing", None),
            row("first", Some("same")),
            row("second", Some("same")),
        ];
        let (tracks, start) = queue_selection(Source::Youtube, songs, 2, &HashSet::new());
        assert_eq!(tracks.len(), 2);
        assert_eq!(start, 1);
    }
}
