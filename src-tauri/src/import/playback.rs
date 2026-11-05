//! Canonical import queues use the existing player and native audio engine.
//! A verified provider mapping selects playback; metadata alone never does.
use super::model::Platform;
use crate::{
    player::{EngineCommand, Status},
    AppState,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Route {
    pub provider: Platform,
    pub external_id: String,
}

#[derive(Debug, Clone)]
pub struct AvailableRoute {
    pub route: Route,
    pub verified: bool,
    pub available: bool,
}

const DEFAULT_PRIORITY: [Platform; 4] = [
    Platform::Local,
    Platform::Navidrome,
    Platform::Youtube,
    Platform::Soundcloud,
];

/// An explicit override is strict: unavailable selections require user action.
pub fn select_route(
    routes: &[AvailableRoute],
    preference: Option<Platform>,
    priority: &[Platform],
) -> Option<Route> {
    let eligible = |candidate: &&AvailableRoute| {
        candidate.verified
            && candidate.available
            && DEFAULT_PRIORITY.contains(&candidate.route.provider)
    };
    if let Some(provider) = preference {
        return routes
            .iter()
            .filter(eligible)
            .find(|candidate| candidate.route.provider == provider)
            .map(|candidate| candidate.route.clone());
    }
    priority
        .iter()
        .chain(DEFAULT_PRIORITY.iter())
        .find_map(|provider| {
            routes
                .iter()
                .filter(eligible)
                .find(|candidate| &candidate.route.provider == provider)
                .map(|candidate| candidate.route.clone())
        })
}

/// An exact queue recording takes precedence over saved library preferences.
pub fn select_recording(
    routes: &[AvailableRoute],
    preference: Option<Platform>,
    priority: &[Platform],
    requested: Option<&Route>,
) -> Option<Route> {
    if let Some(requested) = requested {
        return routes
            .iter()
            .find(|candidate| {
                candidate.verified
                    && candidate.available
                    && DEFAULT_PRIORITY.contains(&candidate.route.provider)
                    && &candidate.route == requested
            })
            .map(|candidate| candidate.route.clone());
    }
    select_route(routes, preference, priority)
}

fn allows_youtube_recovery(
    requested: Option<&Route>,
    saved: Option<Platform>,
    enabled: bool,
    denied_soundcloud: Option<&Route>,
) -> bool {
    enabled
        && saved.is_none_or(|source| source == Platform::Youtube)
        && (requested.is_none() || requested == denied_soundcloud)
}

#[derive(Default)]
pub struct State {
    active: Mutex<Option<Session>>,
    original_positions: Mutex<Vec<usize>>,
    recovery: Mutex<Recovery>,
}
#[derive(Default)]
struct Recovery {
    track: String,
    generation: u64,
    tried: HashSet<String>,
    busy: bool,
    searched: bool,
    denied_soundcloud: Option<Route>,
    fallback: Option<Route>,
}
impl Recovery {
    fn reset(&mut self, track_id: &str) {
        *self = Self {
            track: track_id.into(),
            generation: self.generation.wrapping_add(1),
            ..Self::default()
        };
    }

    fn record_failure(&mut self, route: Route, message: &str) {
        if route.provider == Platform::Youtube {
            self.tried.insert(route.external_id);
            self.fallback = None;
        } else if route.provider == Platform::Soundcloud
            && message == crate::soundcloud::FULL_PLAYBACK_DENIED
        {
            self.denied_soundcloud = Some(route);
        }
    }

    fn after_denial(&self, selected: Option<Route>, requested: Option<&Route>) -> Option<Route> {
        let denied = self.denied_soundcloud.as_ref();
        if denied.is_some_and(|route| {
            requested == Some(route)
                || (requested.is_none()
                    && selected.as_ref().is_some_and(|selected| {
                        selected == route || selected.provider == Platform::Youtube
                    }))
        }) {
            return self.fallback.clone();
        }
        selected
    }
}
struct Session {
    track_id: String,
    route: Option<Route>,
}

fn reset_recovery(playback: &State, track_id: &str) {
    playback
        .recovery
        .lock()
        .expect("playback recovery")
        .reset(track_id);
}

/// The native mixer already crossed the sample boundary; update ownership only.
pub fn accept_handoff(app: &AppHandle, track_id: &str, video_id: &str, revision: u64) {
    if let Some(playback) = app.try_state::<State>() {
        let mut active = playback.active.lock().expect("import playback");
        let state = app.state::<AppState>();
        let player = state.player.lock().expect("player mutex");
        if active.is_some()
            && player.state().playback_revision == revision
            && player
                .state()
                .current()
                .is_some_and(|track| track.id == track_id)
        {
            *active = Some(Session {
                track_id: track_id.into(),
                route: Some(Route {
                    provider: Platform::Youtube,
                    external_id: video_id.into(),
                }),
            });
        }
    }
}

fn apply_shuffle(
    app: &AppHandle,
    state: &AppState,
    playback: &State,
    commands: &[EngineCommand],
    first_import: bool,
) {
    let reset = first_import
        || commands
            .iter()
            .any(|command| matches!(command, EngineCommand::SetQueue { .. }));
    let mode = commands.iter().rev().find_map(|command| match command {
        EngineCommand::SetShuffle { on } => Some(*on),
        _ => None,
    });
    let mut player = state.player.lock().expect("player mutex");
    let mut positions = playback
        .original_positions
        .lock()
        .expect("import queue order");
    if reset || positions.len() != player.state().queue.len() {
        *positions = (0..player.state().queue.len()).collect();
    }
    let mode = mode.or_else(|| reset.then_some(player.state().shuffle));
    let Some(on) = mode else {
        return;
    };
    let permutation = if on {
        let mut random = [0u8; 8];
        if getrandom::fill(&mut random).is_err() {
            let _ = app.emit(
                "playback://error",
                "Could not shuffle the queue. Try toggling shuffle again.",
            );
            return;
        }
        let mut seed = u64::from_le_bytes(random);
        player.shuffle_upcoming(|bound| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 32) as usize % bound
        })
    } else {
        player.restore_upcoming(&positions)
    };
    *positions = permutation
        .into_iter()
        .map(|index| positions[index])
        .collect();
}

pub fn owns_current(app: &AppHandle) -> bool {
    app.try_state::<State>()
        .is_some_and(|state| state.active.lock().expect("import playback").is_some())
}

pub fn active_route(app: &AppHandle) -> Option<Route> {
    app.try_state::<State>().and_then(|state| {
        state
            .active
            .lock()
            .expect("import playback")
            .as_ref()
            .and_then(|session| session.route.clone())
    })
}

pub fn owns_native(app: &AppHandle) -> bool {
    app.try_state::<State>().is_some_and(|state| {
        state
            .active
            .lock()
            .expect("import playback")
            .as_ref()
            .and_then(|session| session.route.as_ref())
            .is_some_and(|route| {
                matches!(
                    route.provider,
                    Platform::Local
                        | Platform::Navidrome
                        | Platform::Youtube
                        | Platform::Soundcloud
                )
            })
    })
}

fn suspend_sources(state: &AppState) {
    if let Some(engine) = state.audio.lock().expect("audio mutex").as_ref() {
        engine.stop();
    }
}

fn fail(app: &AppHandle, state: &AppState, message: &str) {
    crate::soundcloud_playback::stop(app);
    crate::youtube_native::stop(app);
    if let Some(playback) = app.try_state::<State>() {
        if let Some(session) = playback.active.lock().expect("import playback").as_mut() {
            session.route = None;
        }
    }
    if let Some(engine) = state.audio.lock().expect("audio mutex").take() {
        engine.stop();
    }
    crate::youtube_embed::stop(app);
    state
        .player
        .lock()
        .expect("player mutex")
        .set_status(Status::Paused);
    let _ = app.emit("playback://error", message);
}

fn native_error_message(error: &str) -> &'static str {
    if error.starts_with("YouTube denied this audio stream.") {
        "YouTube rejected this audio stream. Try playing the recording again."
    } else if matches!(
        error,
        "server unreachable" | "connection lost" | "timed out waiting for audio data"
    ) {
        "The audio connection was interrupted. Check your connection and try again."
    } else if error.starts_with("could not decode this track:") {
        "This recording's audio format could not be decoded. Choose another recording."
    } else {
        "The linked recording could not be played. Review this recording's playback source."
    }
}

pub fn report_native_error(app: &AppHandle, state: &AppState, error: &str) {
    if crate::soundcloud_playback::refresh_current(app, error) {
        return;
    }
    if crate::youtube_native::refresh_current(app, error) {
        return;
    }
    crate::youtube_native::invalidate_current(app);
    let track = state
        .player
        .lock()
        .expect("player mutex")
        .state()
        .current()
        .map(|track| track.id.clone());
    if track.is_some_and(|id| report_native_failure(app, &id, error)) {
        return;
    }
    fail(app, state, native_error_message(error));
}

/// Native failures exhaust plausible recordings before asking the listener.
pub fn report_native_failure(app: &AppHandle, track_id: &str, message: &str) -> bool {
    let Some(playback) = app.try_state::<State>() else {
        return false;
    };
    let failed = playback
        .active
        .lock()
        .expect("import playback")
        .as_ref()
        .filter(|session| session.track_id == track_id)
        .and_then(|session| session.route.clone());
    let Some(route) =
        failed.filter(|route| matches!(route.provider, Platform::Youtube | Platform::Soundcloud))
    else {
        return false;
    };
    {
        let mut recovery = playback.recovery.lock().expect("playback recovery");
        if recovery.track != track_id {
            recovery.reset(track_id);
        }
        recovery.record_failure(route, message);
    }
    start_automatic_playback(app, track_id)
}

fn start_automatic_playback(app: &AppHandle, track_id: &str) -> bool {
    let (revision, index, requested) = {
        let state = app.state::<AppState>();
        let player = state.player.lock().expect("player mutex");
        if matches!(player.state().status, Status::Ended | Status::Idle)
            || player
                .state()
                .current()
                .is_none_or(|track| track.id != track_id)
        {
            return false;
        }
        (
            player.state().playback_revision,
            player.state().index,
            player
                .state()
                .current()
                .and_then(|track| track.playback_route.clone()),
        )
    };
    let Some(playback) = app.try_state::<State>() else {
        return false;
    };
    let Some(imports) = app.try_state::<super::State>() else {
        return false;
    };
    let generation = {
        let mut recovery = playback.recovery.lock().expect("playback recovery");
        if recovery.track != track_id {
            recovery.reset(track_id);
        }
        {
            let db = imports.db.lock().expect("import db");
            if !allows_youtube_recovery(
                requested.as_ref(),
                db.source_preference(track_id).ok().flatten(),
                db.meta("youtube_native_enabled").ok().flatten().as_deref() != Some("false"),
                recovery.denied_soundcloud.as_ref(),
            ) {
                return false;
            }
        }
        if recovery.busy {
            return true;
        }
        recovery.busy = true;
        recovery.generation
    };
    let app = app.clone();
    let track_id = track_id.to_string();
    tauri::async_runtime::spawn(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let playback = app.state::<State>();
            let state = app.state::<AppState>();
            let imports = app.state::<super::State>();
            let current = {
                let player = state.player.lock().expect("player mutex");
                player
                    .state()
                    .current()
                    .is_some_and(|track| track.id == track_id)
                    && player.state().playback_revision == revision
                    && player.state().index == index
                    && !matches!(player.state().status, Status::Ended | Status::Idle)
            };
            let tried = {
                let mut recovery = playback.recovery.lock().expect("playback recovery");
                if recovery.generation != generation || recovery.track != track_id {
                    return;
                }
                if !current {
                    recovery.busy = false;
                    return;
                }
                recovery.tried.clone()
            };
            let candidates = {
                let db = imports.db.lock().expect("import db");
                db.canonical_track(&track_id)
                    .ok()
                    .flatten()
                    .map(|track| {
                        super::matching::autoplay_candidates(
                            &track.metadata,
                            &db.resolution_candidates(&track_id).unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default()
            };
            let candidate = candidates.into_iter().find(|candidate| {
                tokio::time::Instant::now() < deadline
                    && tried.len() < 5
                    && !tried.contains(&candidate.external_id)
            });
            let Some(candidate) = candidate else {
                let search = {
                    let mut recovery = playback.recovery.lock().expect("playback recovery");
                    if recovery.generation != generation {
                        return;
                    }
                    let search = !recovery.searched
                        && recovery.tried.len() < 5
                        && tokio::time::Instant::now() < deadline;
                    recovery.searched = true;
                    search
                };
                if search {
                    let _ = tokio::time::timeout_at(
                        deadline,
                        super::jobs::import_resolve(app.clone(), track_id.clone()),
                    )
                    .await;
                    continue;
                }
                let mut recovery = playback.recovery.lock().expect("playback recovery");
                if recovery.generation != generation {
                    return;
                }
                recovery.busy = false;
                drop(recovery);
                if state
                    .player
                    .lock()
                    .expect("player mutex")
                    .state()
                    .current()
                    .is_some_and(|track| track.id == track_id)
                {
                    fail(&app, &state, "No suitable recording could be played after automatic recovery. Retry or choose another recording.");
                    crate::commands::publish(&app, &state);
                }
                return;
            };
            {
                let mut recovery = playback.recovery.lock().expect("playback recovery");
                if recovery.generation != generation {
                    return;
                }
                recovery.tried.insert(candidate.external_id.clone());
            }
            if !matches!(
                tokio::time::timeout_at(
                    deadline.min(tokio::time::Instant::now() + std::time::Duration::from_secs(8)),
                    crate::youtube_native::probe(&app, &candidate.external_id)
                )
                .await,
                Ok(Ok(()))
            ) {
                continue;
            }
            // Guard identity after every await; a late search must never hijack another song.
            let mut recovery = playback.recovery.lock().expect("playback recovery");
            if recovery.generation != generation || recovery.track != track_id {
                return;
            }
            {
                let player = state.player.lock().expect("player mutex");
                if player
                    .state()
                    .current()
                    .is_none_or(|track| track.id != track_id)
                    || player.state().playback_revision != revision
                    || player.state().index != index
                    || matches!(player.state().status, Status::Ended | Status::Idle)
                {
                    recovery.busy = false;
                    return;
                }
            };
            if imports
                .db
                .lock()
                .expect("import db")
                .verify_candidate(&track_id, Platform::Youtube, &candidate.external_id)
                .is_err()
            {
                recovery.busy = false;
                return;
            }
            if crate::youtube_native::send_if_current(
                &app,
                &track_id,
                &candidate.external_id,
                revision,
            ) == Ok(true)
            {
                let mut active = playback.active.lock().expect("import playback");
                let mut player = state.player.lock().expect("player mutex");
                if player.state().playback_revision == revision
                    && player
                        .state()
                        .current()
                        .is_some_and(|track| track.id == track_id)
                {
                    let route = Route {
                        provider: Platform::Youtube,
                        external_id: candidate.external_id,
                    };
                    if recovery.denied_soundcloud.is_some() {
                        recovery.fallback = Some(route.clone());
                    }
                    *active = Some(Session {
                        track_id: track_id.clone(),
                        route: Some(route),
                    });
                    player.set_playback_source(Some("youtube"), true);
                }
            }
            recovery.busy = false;
            drop(recovery);
            crate::commands::publish(&app, &state);
            return;
        }
    });
    true
}

/// An explicit recording choice cancels any older in-flight stream refresh.
pub fn recording_changed(app: &AppHandle, track_id: &str) {
    app.state::<AppState>()
        .player
        .lock()
        .expect("player mutex")
        .clear_recording_choice(track_id);
    let Some(playback) = app.try_state::<State>() else {
        return;
    };
    reset_recovery(&playback, track_id);
    let mut active = playback.active.lock().expect("import playback");
    if let Some(session) = active
        .as_mut()
        .filter(|session| session.track_id == track_id)
    {
        session.route = None;
        crate::soundcloud_playback::stop(app);
        crate::youtube_native::stop(app);
        let state = app.state::<AppState>();
        let still_current = {
            let mut player = state.player.lock().expect("player mutex");
            if player
                .state()
                .current()
                .is_some_and(|track| track.id == track_id)
            {
                player.set_status(Status::Paused);
                true
            } else {
                false
            }
        };
        if still_current {
            if let Some(engine) = state.audio.lock().expect("audio mutex").as_ref() {
                engine.stop();
            }
        }
        drop(active);
        crate::commands::publish(app, &state);
    }
}

#[tauri::command]
pub fn import_retry_playback(app: AppHandle, track_id: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let position = {
        let player = state.player.lock().expect("player mutex");
        if player
            .state()
            .current()
            .is_none_or(|track| track.id != track_id)
        {
            return Err("The selected track changed. Retry the current track.".into());
        }
        player.state().position_ms
    };
    let imports = app
        .try_state::<super::State>()
        .ok_or("The import library is unavailable.")?;
    if imports
        .db
        .lock()
        .map_err(|_| "The import library is unavailable.")?
        .canonical_track(&track_id)
        .map_err(|_| "Could not read the imported track.")?
        .is_none()
    {
        return Err("This track is not an imported recording.".into());
    }
    reset_recovery(&app.state::<State>(), &track_id);
    crate::soundcloud_playback::stop(&app);
    crate::youtube_native::invalidate_current(&app);
    crate::youtube_native::stop(&app);
    if let Some(playback) = app.try_state::<State>() {
        if let Some(session) = playback.active.lock().expect("import playback").as_mut() {
            session.route = None;
        }
    }
    dispatch(
        &app,
        &state,
        &[EngineCommand::Play, EngineCommand::Seek { ms: position }],
    );
    crate::commands::publish(&app, &state);
    Ok(())
}

fn native_engine(app: &AppHandle, state: &AppState) -> Result<Arc<crate::audio::Engine>, String> {
    let mut audio = state.audio.lock().expect("audio mutex");
    if let Some(engine) = audio.as_ref() {
        return Ok(engine.clone());
    }
    let engine = Arc::new(
        crate::desktop::open_engine(app).map_err(|_| "No audio output device is available.")?,
    );
    *audio = Some(engine.clone());
    Ok(engine)
}

fn navidrome_client(state: &AppState) -> (Option<Arc<crate::subsonic::Client>>, String) {
    let mut settings = state.settings.lock().expect("settings mutex").clone();
    settings.source = crate::settings::Source::Navidrome;
    let (resolved, _) = crate::source::resolve(&settings);
    (
        crate::source::navidrome_client(&settings).map(Arc::new),
        resolved.navidrome.url.trim_end_matches('/').to_string(),
    )
}
fn server_matches(origin: Option<&str>, configured: &str) -> bool {
    !configured.is_empty()
        && origin
            .is_some_and(|origin| origin.trim_end_matches('/') == configured.trim_end_matches('/'))
}
fn navidrome_cache_key(server: &str, track: &str) -> String {
    let digest = Sha256::digest(server.trim_end_matches('/').as_bytes());
    format!("import-{:x}-{track}", digest)
}

fn should_load(changed: bool, commands: &[EngineCommand], native_empty: bool) -> bool {
    changed
        || commands.iter().any(|command| {
            matches!(
                command,
                EngineCommand::SetQueue { .. }
                    | EngineCommand::SkipNext
                    | EngineCommand::SkipPrevious
            )
        })
        || (native_empty
            && commands
                .iter()
                .any(|command| matches!(command, EngineCommand::Play)))
}

/// Returns true only when the current queue entry is a persisted canonical track.
pub fn dispatch(app: &AppHandle, state: &AppState, commands: &[EngineCommand]) -> bool {
    let Some(imports) = app.try_state::<super::State>() else {
        return false;
    };
    let current = state
        .player
        .lock()
        .expect("player mutex")
        .state()
        .current()
        .map(|track| (track.id.clone(), track.playback_route.clone()));
    let Some((track_id, requested)) = current else {
        return false;
    };
    let track = {
        imports
            .db
            .lock()
            .expect("import db")
            .canonical_track(&track_id)
    };
    let track = match track {
        Ok(Some(track)) => track,
        Ok(None) => return false,
        Err(_) => {
            fail(app, state, "Could not read the imported recording.");
            return true;
        }
    };
    if commands.is_empty()
        && matches!(
            state.player.lock().expect("player mutex").state().status,
            Status::Ended | Status::Idle
        )
    {
        reset_recovery(&app.state::<State>(), "");
        crate::youtube_native::stop(app);
        crate::soundcloud_playback::stop(app);
        if let Some(engine) = state.audio.lock().expect("audio mutex").as_ref() {
            engine.stop();
        }
        return true;
    }
    let loaded = {
        let mut db = imports.db.lock().expect("import db");
        // Existing imports may already contain one conclusive result from an older resolver.
        if db.auto_verify_cached_candidate(&track_id).is_err() {
            fail(
                app,
                state,
                "Could not check this recording's saved playback match.",
            );
            return true;
        }
        (
            db.mappings_for(&track_id),
            db.source_preference(&track_id),
            db.meta("import_playback_priority"),
            db.meta("youtube_native_enabled"),
        )
    };
    let playback = app.state::<State>();
    {
        let changed = playback.recovery.lock().expect("playback recovery").track != track_id
            || commands.iter().any(|command| {
                matches!(
                    command,
                    EngineCommand::SetQueue { .. }
                        | EngineCommand::SkipNext
                        | EngineCommand::SkipPrevious
                )
            });
        if changed {
            reset_recovery(&playback, &track_id);
        }
    }
    let first_import = playback.active.lock().expect("import playback").is_none();
    // Claim ownership before any callback can report a provider's own track ID.
    if first_import {
        *playback.active.lock().expect("import playback") = Some(Session {
            track_id: track_id.clone(),
            route: None,
        });
        suspend_sources(state);
    }
    apply_shuffle(app, state, &playback, commands, first_import);
    let (mappings, preference, priority, youtube_enabled) = loaded;
    let (mappings, preference, priority, youtube_enabled) =
        match (mappings, preference, priority, youtube_enabled) {
            (Ok(mappings), Ok(preference), Ok(priority), Ok(enabled)) => (
                mappings,
                preference,
                priority,
                enabled.as_deref() != Some("false"),
            ),
            _ => {
                fail(
                    app,
                    state,
                    "Could not read this recording's playback choices.",
                );
                return true;
            }
        };
    let priority: Vec<Platform> = priority
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| DEFAULT_PRIORITY.to_vec());
    let (navidrome, server) = if track
        .origins
        .iter()
        .any(|origin| origin.platform == Platform::Navidrome)
    {
        navidrome_client(state)
    } else {
        (None, String::new())
    };
    let mut routes: Vec<_> = track
        .origins
        .iter()
        .filter_map(|origin| match origin.platform {
            Platform::Local => Some(AvailableRoute {
                route: Route {
                    provider: Platform::Local,
                    external_id: origin.external_id.clone(),
                },
                verified: true,
                available: std::path::Path::new(&origin.external_id).is_file(),
            }),
            Platform::Navidrome => Some(AvailableRoute {
                route: Route {
                    provider: Platform::Navidrome,
                    external_id: origin.external_id.clone(),
                },
                verified: true,
                available: navidrome.is_some()
                    && !origin.external_id.trim().is_empty()
                    && server_matches(origin.url.as_deref(), &server),
            }),
            Platform::Soundcloud => Some(AvailableRoute {
                route: Route {
                    provider: Platform::Soundcloud,
                    external_id: origin.external_id.clone(),
                },
                verified: true,
                available: !origin.external_id.is_empty(),
            }),
            _ => None,
        })
        .collect();
    routes.extend(mappings.into_iter().map(|mapping| AvailableRoute {
        route: Route {
            provider: Platform::Youtube,
            external_id: mapping.external_id.clone(),
        },
        verified: mapping.verified && mapping.provider == "youtube",
        available: youtube_enabled && strict_video_id(&mapping.external_id),
    }));
    let route = select_recording(&routes, preference, &priority, requested.as_ref());
    let (route, allow_recovery) = {
        let recovery = playback.recovery.lock().expect("playback recovery");
        let allowed = allows_youtube_recovery(
            requested.as_ref(),
            preference,
            youtube_enabled,
            recovery.denied_soundcloud.as_ref(),
        );
        let route = if allowed {
            recovery.after_denial(route, requested.as_ref())
        } else {
            route
        };
        (route, allowed)
    };
    let Some(route) = route else {
        let resolution = imports
            .db
            .lock()
            .expect("import db")
            .resolution_state(&track_id)
            .ok()
            .flatten();
        state
            .player
            .lock()
            .expect("player mutex")
            .set_playback_source(None, true);
        *playback.active.lock().expect("import playback") = Some(Session {
            track_id: track_id.clone(),
            route: None,
        });
        if allow_recovery {
            if commands
                .iter()
                .any(|command| matches!(command, EngineCommand::Pause))
            {
                state
                    .player
                    .lock()
                    .expect("player mutex")
                    .set_status(Status::Paused);
            } else {
                let mut player = state.player.lock().expect("player mutex");
                if commands
                    .iter()
                    .any(|command| matches!(command, EngineCommand::Play))
                    || player.state().status != Status::Paused
                {
                    player.set_status(Status::Loading);
                }
            }
            for command in commands {
                if let EngineCommand::Seek { ms } = command {
                    state.player.lock().expect("player mutex").on_position(*ms);
                }
            }
            if start_automatic_playback(app, &track_id) {
                return true;
            }
        }
        let message = if preference.is_some() {
            "The selected playback source is unavailable. Choose another source."
        } else {
            match resolution.as_deref() {
                Some("needs_review") => "This recording has multiple or uncertain matches. Review its match to choose the correct recording.",
                Some("unavailable") => "No matching recording was found. Retry its match search or choose another source.",
                Some("matched") => "This recording's playback source is unavailable. Choose another source.",
                _ => "Finding a playback match for this recording. Try Play again when matching finishes.",
            }
        };
        fail(app, state, message);
        return true;
    };
    state
        .player
        .lock()
        .expect("player mutex")
        .set_playback_source(Some(route.provider.as_str()), true);
    let changed = {
        let mut active = playback.active.lock().expect("import playback");
        let changed = active.as_ref().is_none_or(|session| {
            session.track_id != track_id || session.route.as_ref() != Some(&route)
        });
        *active = Some(Session {
            track_id: track_id.clone(),
            route: Some(route.clone()),
        });
        changed
    };
    let native_empty = !crate::youtube_native::is_resolving(app)
        && !crate::soundcloud_playback::is_resolving(app)
        && state
            .audio
            .lock()
            .expect("audio mutex")
            .as_ref()
            .is_none_or(|engine| engine.is_empty() && !engine.is_loading());
    let loading = should_load(changed, commands, native_empty);
    if loading {
        state
            .player
            .lock()
            .expect("player mutex")
            .set_status(Status::Loading);
    }
    if changed {
        crate::soundcloud_playback::stop(app);
        crate::youtube_native::stop(app);
        crate::youtube_embed::stop(app);
        if let Some(audio) = state.audio.lock().expect("audio mutex").as_ref() {
            audio.stop();
        }
    }
    let result = if route.provider == Platform::Soundcloud {
        crate::soundcloud_playback::send(app, &track_id, &route.external_id, loading, commands)
    } else if route.provider == Platform::Youtube {
        crate::youtube_native::send(app, &track_id, &route.external_id, loading, commands)
    } else {
        native_engine(app, state).and_then(|engine| {
            if loading {
                if route.provider == Platform::Local {
                    engine
                        .load_file(std::path::PathBuf::from(&route.external_id))
                        .map_err(|_| "Could not open the linked local recording.".to_string())?;
                } else {
                    let client = navidrome
                        .as_ref()
                        .ok_or("Connect the linked Navidrome server first.")?;
                    let dir = crate::stream::cache_dir(
                        state.data_dir.lock().expect("data dir mutex").clone(),
                    )
                    .ok_or("No audio cache directory is available.")?;
                    let path =
                        crate::stream::cache_path(&dir, &navidrome_cache_key(&server, &track_id));
                    crate::stream::prune(&dir, crate::stream::CACHE_CAP_BYTES, Some(&path));
                    engine
                        .load(client.stream_url(&route.external_id), path)
                        .map_err(|_| {
                            "Could not load the linked Navidrome recording.".to_string()
                        })?;
                }
                engine.set_volume(state.player.lock().expect("player mutex").state().volume);
                engine.play();
            }
            for command in commands {
                match command {
                    EngineCommand::Play => engine.play(),
                    EngineCommand::Pause => engine.pause(),
                    EngineCommand::Seek { ms } => engine.seek(*ms),
                    EngineCommand::SetVolume { percent } => engine.set_volume(*percent),
                    _ => {}
                }
            }
            if state.player.lock().expect("player mutex").state().status == Status::Ended {
                engine.stop();
            }
            Ok(())
        })
    };
    if let Err(message) = result {
        fail(app, state, &message);
    }
    true
}

fn strict_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

/// Called before a regular source queue takes control again.
pub fn release(app: &AppHandle) {
    crate::soundcloud_playback::stop(app);
    let Some(playback) = app.try_state::<State>() else {
        return;
    };
    reset_recovery(&playback, "");
    if playback
        .active
        .lock()
        .expect("import playback")
        .take()
        .is_none()
    {
        return;
    }
    crate::youtube_native::stop(app);
    playback
        .original_positions
        .lock()
        .expect("import queue order")
        .clear();
    crate::youtube_embed::stop(app);
    let state = app.state::<AppState>();
    state
        .player
        .lock()
        .expect("player mutex")
        .set_playback_source(None, false);
    if let Some(engine) = state.audio.lock().expect("audio mutex").as_ref() {
        engine.stop();
    }
    let source = state.settings.lock().expect("settings mutex").source;
    if source == crate::settings::Source::Youtube {
        state.audio.lock().expect("audio mutex").take();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn exact_soundcloud_recording_recovers_only_after_access_denial() {
        let requested = route(Platform::Soundcloud, true, true).route;
        let mut recovery = Recovery::default();
        recovery.record_failure(requested.clone(), "connection lost");
        assert!(!allows_youtube_recovery(
            Some(&requested),
            None,
            true,
            recovery.denied_soundcloud.as_ref(),
        ));
        recovery.record_failure(requested.clone(), crate::soundcloud::FULL_PLAYBACK_DENIED);
        let denied = recovery.denied_soundcloud.as_ref();
        assert!(allows_youtube_recovery(Some(&requested), None, true, denied));
        assert!(!allows_youtube_recovery(Some(&requested), None, false, denied));
        assert!(!allows_youtube_recovery(
            Some(&requested),
            Some(Platform::Local),
            true,
            denied,
        ));
        let different = Route {
            external_id: "different".into(),
            ..requested
        };
        assert!(!allows_youtube_recovery(Some(&different), None, true, denied));
        assert!(allows_youtube_recovery(None, None, true, None));
        assert!(!allows_youtube_recovery(None, None, false, None));
    }

    #[test]
    fn playback_controls_keep_the_verified_fallback_after_soundcloud_denial() {
        let requested = route(Platform::Soundcloud, true, true).route;
        let youtube = route(Platform::Youtube, true, true).route;
        let mut recovery = Recovery::default();
        recovery.record_failure(requested.clone(), crate::soundcloud::FULL_PLAYBACK_DENIED);
        let select = |recovery: &Recovery| {
            recovery.after_denial(Some(requested.clone()), Some(&requested))
        };
        // Pause and seek can reach dispatch while the alternate recording is still resolving.
        assert_eq!(select(&recovery), None);
        recovery.fallback = Some(youtube.clone());
        assert_eq!(select(&recovery), Some(youtube));
        recovery.reset("next-occurrence");
        assert_eq!(select(&recovery), Some(requested));
    }
    #[test]
    fn native_errors_identify_the_failure_without_exposing_raw_details() {
        assert!(super::native_error_message(
            "YouTube denied this audio stream. Retry the recording or choose another source."
        )
        .contains("YouTube rejected"));
        assert!(
            super::native_error_message("connection lost").contains("connection was interrupted")
        );
        assert!(
            super::native_error_message("could not decode this track: unsupported codec")
                .contains("audio format")
        );
        assert!(
            !super::native_error_message("https://secret.googlevideo.com/?token=secret")
                .contains("secret")
        );
    }
    use super::*;
    fn route(provider: Platform, verified: bool, available: bool) -> AvailableRoute {
        AvailableRoute {
            route: Route {
                provider,
                external_id: provider.as_str().into(),
            },
            verified,
            available,
        }
    }
    #[test]
    fn verified_availability_precedes_priority() {
        let routes = vec![
            route(Platform::Local, true, false),
            route(Platform::Navidrome, false, true),
            route(Platform::Youtube, true, true),
        ];
        assert_eq!(
            select_route(&routes, None, &[]).unwrap().provider,
            Platform::Youtube
        );
        assert!(select_route(&[route(Platform::Youtube, false, true)], None, &[]).is_none());
    }
    #[test]
    fn priority_prefers_local_by_default_and_can_be_overridden() {
        let routes = vec![
            route(Platform::Youtube, true, true),
            route(Platform::Local, true, true),
        ];
        assert_eq!(
            select_route(&routes, None, &[]).unwrap().provider,
            Platform::Local
        );
        assert_eq!(
            select_route(&routes, None, &[Platform::Youtube])
                .unwrap()
                .provider,
            Platform::Youtube
        );
    }
    #[test]
    fn youtube_is_the_default_online_recording() {
        let routes = [
            route(Platform::Soundcloud, true, true),
            route(Platform::Youtube, true, true),
        ];
        assert_eq!(
            select_route(&routes, None, &[]).unwrap().provider,
            Platform::Youtube
        );
        assert_eq!(
            select_route(&routes, None, &[Platform::Soundcloud])
                .unwrap()
                .provider,
            Platform::Soundcloud
        );
    }
    #[test]
    fn explicit_preference_never_silently_falls_back() {
        let routes = vec![
            route(Platform::Local, true, true),
            route(Platform::Youtube, true, false),
        ];
        assert!(select_route(&routes, Some(Platform::Youtube), &[]).is_none());
        assert!(select_route(
            &[route(Platform::Spotify, true, true)],
            None,
            &[Platform::Spotify]
        )
        .is_none());
    }

    #[test]
    fn navidrome_links_and_cache_are_bound_to_the_configured_server() {
        assert!(server_matches(
            Some("https://music.example/"),
            "https://music.example"
        ));
        assert!(!server_matches(
            Some("https://old.example"),
            "https://new.example"
        ));
        assert!(!server_matches(None, "https://music.example"));
        assert_ne!(
            navidrome_cache_key("https://old.example", "track"),
            navidrome_cache_key("https://new.example", "track")
        );
        assert_eq!(
            navidrome_cache_key("https://music.example/", "track"),
            navidrome_cache_key("https://music.example", "track")
        );
    }

    #[test]
    fn repeating_an_ended_native_recording_reloads_the_empty_sink() {
        assert!(should_load(
            false,
            &[EngineCommand::Seek { ms: 0 }, EngineCommand::Play],
            true
        ));
        assert!(!should_load(false, &[EngineCommand::Pause], true));
        assert!(!should_load(false, &[EngineCommand::Play], false));
    }
}
