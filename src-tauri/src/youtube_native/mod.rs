//! Native YouTube playback; stream resolution stays separate from the queue.
mod control;
pub mod resolver;

use crate::{
    player::{EngineCommand, Status},
    AppState,
};
use control::Control;
use std::sync::{atomic::Ordering, Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Default)]
pub struct State {
    control: Mutex<Control>,
    resolver: tokio::sync::OnceCell<Arc<resolver::Resolver>>,
    prefetch: Mutex<Option<tokio::task::AbortHandle>>,
    prepared: Mutex<Option<NextPlan>>,
}

#[derive(Clone, PartialEq, Eq)]
struct NextPlan {
    owner: control::Ticket,
    revision: u64,
    key: crate::audio_queue::Key,
    track: String,
    video: String,
    duration_ms: u64,
    engine_generation: u64,
}

fn successor(snapshot: &crate::player::PlayerState) -> Option<usize> {
    let index = snapshot.index?;
    match snapshot.repeat {
        crate::player::Repeat::One => Some(index),
        _ if index + 1 < snapshot.queue.len() => Some(index + 1),
        crate::player::Repeat::All if !snapshot.queue.is_empty() => Some(0),
        _ => None,
    }
}

pub fn owns_current(app: &AppHandle) -> bool {
    app.try_state::<State>().is_some_and(|state| {
        state
            .control
            .lock()
            .expect("native YouTube")
            .current_video()
            .is_some()
    })
}
pub fn is_resolving(app: &AppHandle) -> bool {
    app.try_state::<State>()
        .is_some_and(|state| state.control.lock().expect("native YouTube").resolving())
}
pub fn stop(app: &AppHandle) {
    let Some(native) = app.try_state::<State>() else {
        return;
    };
    let mut control = native.control.lock().expect("native YouTube");
    let active = control.current_video().is_some();
    control.cancel();
    native.prepared.lock().expect("YouTube prepared").take();
    if let Some(task) = native.prefetch.lock().expect("YouTube prefetch").take() {
        task.abort();
    }
    if let Some(resolver) = native.resolver.get() {
        resolver.prioritize("");
    }
    if active {
        if let Some(engine) = app
            .state::<AppState>()
            .audio
            .lock()
            .expect("audio mutex")
            .as_ref()
        {
            engine.stop();
        }
    }
}
pub fn invalidate_current(app: &AppHandle) {
    let Some(native) = app.try_state::<State>() else {
        return;
    };
    let control = native.control.lock().expect("native YouTube");
    if let (Some(id), Some(resolver)) = (control.current_video(), native.resolver.get()) {
        resolver.invalidate(id);
    }
}

async fn resolver(app: &AppHandle) -> Result<Arc<resolver::Resolver>, String> {
    let native = app.state::<State>();
    native
        .resolver
        .get_or_try_init(|| async {
            let directory = app
                .state::<AppState>()
                .data_dir
                .lock()
                .expect("data dir mutex")
                .clone()
                .ok_or("No playback cache directory is available.")?
                .join("youtube-session");
            std::fs::create_dir_all(&directory)
                .map_err(|_| "Could not create the playback cache.")?;
            resolver::Resolver::public(&directory).map(Arc::new)
        })
        .await
        .cloned()
}

pub async fn probe(app: &AppHandle, video_id: &str) -> Result<(), String> {
    resolver(app)
        .await?
        .resolve_recovering(video_id)
        .await
        .map(|_| ())
}

pub fn reconcile_handoff(app: &AppHandle) {
    let Some(native) = app.try_state::<State>() else {
        return;
    };
    let state = app.state::<AppState>();
    let engine = state.audio.lock().expect("audio mutex").clone();
    let Some(engine) = engine else {
        return;
    };
    let Some(snapshot) = engine.gapless_snapshot() else {
        return;
    };
    let mut control = native.control.lock().expect("native YouTube");
    let mut prepared = native.prepared.lock().expect("YouTube prepared");
    let Some(plan) = prepared
        .as_ref()
        .filter(|p| Some(p.key) == snapshot.key && control.accepts(&p.owner))
        .cloned()
    else {
        return;
    };
    let mut player = state.player.lock().expect("player mutex");
    if player.state().playback_revision != plan.revision
        || successor(player.state()) != Some(plan.key.index)
        || player
            .state()
            .queue
            .get(plan.key.index)
            .is_none_or(|t| t.id != plan.track)
    {
        return;
    }
    let _ = player.jump_to(plan.key.index);
    player.on_now_playing(&plan.track, plan.duration_ms);
    player.on_position(snapshot.position_ms);
    player.set_status(if engine.is_paused() {
        Status::Paused
    } else {
        Status::Playing
    });
    let ticket = control.begin(plan.track.clone(), plan.video.clone());
    control.finish(&ticket);
    engine.adopt_gapless(plan.key);
    prepared.take();
    drop(player);
    drop(prepared);
    drop(control);
    crate::import::playback::accept_handoff(app, &plan.track, &plan.video, plan.key.revision);
    prewarm_next(app);
}

pub fn queue_changed(app: &AppHandle) {
    prewarm_next(app);
}

pub fn refreshable_error(error: &str) -> bool {
    error.starts_with("YouTube denied this audio stream.")
        || matches!(
            error,
            "server unreachable" | "connection lost" | "timed out waiting for audio data"
        )
}

/// Refresh the current signed stream URL, preserving the recording and queue.
/// Control tickets bound retries and discard a completion after a later selection.
pub fn refresh_current(app: &AppHandle, error: &str) -> bool {
    if !refreshable_error(error) {
        return false;
    }
    let Some(native) = app.try_state::<State>() else {
        return false;
    };
    let Some(ticket) = native
        .control
        .lock()
        .expect("native YouTube")
        .current_ticket()
    else {
        return false;
    };
    invalidate_current(app);
    send_inner(
        app,
        &ticket.track_id.clone(),
        &ticket.video_id.clone(),
        true,
        &[],
        Some(ticket),
        None,
    )
    .is_ok()
}

/// Queue identity and state continue to belong to the shared Capsule player.
pub fn send(
    app: &AppHandle,
    track_id: &str,
    video_id: &str,
    load: bool,
    commands: &[EngineCommand],
) -> Result<(), String> {
    send_inner(app, track_id, video_id, load, commands, None, None).map(|_| ())
}

pub fn send_if_current(
    app: &AppHandle,
    track_id: &str,
    video_id: &str,
    revision: u64,
) -> Result<bool, String> {
    send_inner(app, track_id, video_id, true, &[], None, Some(revision))
}

fn send_inner(
    app: &AppHandle,
    track_id: &str,
    video_id: &str,
    load: bool,
    commands: &[EngineCommand],
    refresh: Option<control::Ticket>,
    expected_revision: Option<u64>,
) -> Result<bool, String> {
    if !resolver::valid_id(video_id) {
        return Err("Choose a valid YouTube recording first.".into());
    }
    let native = app.state::<State>();
    let state = app.state::<AppState>();
    let mut control = native.control.lock().expect("native YouTube");
    let mut refresh_commands = Vec::new();
    if refresh.is_some() || expected_revision.is_some() {
        let player = state.player.lock().expect("player mutex");
        if refresh
            .as_ref()
            .is_some_and(|expected| !control.accepts(expected))
            || expected_revision
                .is_some_and(|revision| player.state().playback_revision != revision)
            || player
                .state()
                .current()
                .is_none_or(|track| track.id != track_id)
            || matches!(player.state().status, Status::Idle | Status::Ended)
        {
            return Ok(false); // A later queue selection owns playback now.
        }
        // Read intent under the control lock, so a completed pause/seek cannot
        // be overwritten by an older ticker snapshot during stream recovery.
        refresh_commands.push(EngineCommand::Seek {
            ms: player.state().position_ms,
        });
        refresh_commands.push(if player.state().status == Status::Paused {
            EngineCommand::Pause
        } else {
            EngineCommand::Play
        });
    }
    let commands = if refresh.is_some() || expected_revision.is_some() {
        refresh_commands.as_slice()
    } else {
        commands
    };
    if control.reconcile(state.player.lock().expect("player mutex").state(), commands) {
        if let Some(engine) = state.audio.lock().expect("audio mutex").as_ref() {
            engine.stop();
        }
        drop(control);
        stop(app);
        return Ok(false);
    }
    let ticket = if load {
        let ticket = if let Some(expected) = refresh.as_ref() {
            control.refresh(expected).ok_or(
                "Automatic stream refresh was exhausted. Retry or choose another recording.",
            )?
        } else {
            control.begin(track_id.into(), video_id.into())
        };
        if let Some(resolver) = native.resolver.get() {
            resolver.prioritize(video_id);
        }
        if let Some(engine) = state.audio.lock().expect("audio mutex").as_ref() {
            engine.stop();
        }
        state
            .player
            .lock()
            .expect("player mutex")
            .set_status(Status::Loading);
        Some(ticket)
    } else {
        None
    };
    for command in commands {
        control.command(command);
        if matches!(command, EngineCommand::Seek { .. }) {
            native.prepared.lock().expect("YouTube prepared").take();
        }
        if !control.resolving() {
            if let Some(engine) = state.audio.lock().expect("audio mutex").as_ref() {
                match command {
                    EngineCommand::Play => engine.play(),
                    EngineCommand::Pause => engine.pause(),
                    EngineCommand::Seek { ms } => engine.seek(*ms),
                    EngineCommand::SetVolume { percent } => engine.set_volume(*percent),
                    _ => {}
                }
            }
        }
        if matches!(command, EngineCommand::Pause) {
            state
                .player
                .lock()
                .expect("player mutex")
                .set_status(Status::Paused);
        }
        if matches!(command, EngineCommand::Play) && control.resolving() {
            state
                .player
                .lock()
                .expect("player mutex")
                .set_status(Status::Loading);
        }
    }
    if let Some(ticket) = ticket {
        let app = app.clone();
        let worker_ticket = ticket.clone();
        let task = tauri::async_runtime::spawn(async move {
            let result = async {
                let resolver = resolver(&app).await?;
                resolver.resolve_recovering(&worker_ticket.video_id).await
            }
            .await;
            let native = app.state::<State>();
            let state = app.state::<AppState>();
            let mut control = native.control.lock().expect("native YouTube");
            let intent = {
                let player = state.player.lock().expect("player mutex");
                control.complete_for(&worker_ticket, player.state())
            };
            let Some(intent) = intent else {
                return;
            };
            // Completion and source changes use the same lock, including decoder startup.
            let result = result.and_then(|stream| {
                let engine = {
                    let mut audio = state.audio.lock().expect("audio mutex");
                    if audio.is_none() {
                        *audio = Some(Arc::new(
                            crate::desktop::open_engine(&app)
                                .map_err(|_| "No audio output device is available.")?,
                        ));
                    }
                    audio.as_ref().unwrap().clone()
                };
                let directory = state
                    .data_dir
                    .lock()
                    .expect("data dir mutex")
                    .clone()
                    .ok_or("No playback cache directory is available.")?
                    .join("audio")
                    .join("youtube");
                // Each in-flight transfer owns its buffer; rapid repeats cannot overwrite it.
                let cache = crate::stream::cache_path(
                    &directory,
                    &format!(
                        "{}:{}:{}",
                        worker_ticket.video_id, stream.itag, worker_ticket.generation
                    ),
                );
                crate::stream::prune(&directory, 128 * 1024 * 1024, Some(&cache));
                let mut player = state.player.lock().expect("player mutex");
                if control
                    .complete_for(&worker_ticket, player.state())
                    .is_none()
                {
                    return Ok(());
                }
                let audio_key = crate::audio_queue::Key {
                    revision: player.state().playback_revision,
                    index: player.state().index.unwrap_or(0),
                };
                engine.load_gapless(
                    stream.url,
                    cache,
                    audio_key,
                );
                engine.set_gapless_format(audio_key, stream.audio_info);
                player.on_now_playing(&worker_ticket.track_id, stream.duration_ms);
                engine.set_volume(player.state().volume);
                if let Some(ms) = intent.seek {
                    engine.seek(ms);
                }
                if intent.paused {
                    engine.pause();
                } else {
                    engine.play();
                }
                state.engine_ready.store(true, Ordering::SeqCst);
                Ok(())
            });
            drop(control);
            if let Err(message) = &result {
                if crate::import::playback::report_native_failure(
                    &app,
                    &worker_ticket.track_id,
                    message,
                ) {
                    return;
                }
                // Terminal errors only: intermediate recovery must preserve playback intent.
                let control = native.control.lock().expect("native YouTube");
                if !control.accepts(&worker_ticket) {
                    return;
                }
                state
                    .player
                    .lock()
                    .expect("player mutex")
                    .set_status(Status::Paused);
                let _ = app.emit("playback://error", message);
            }
            crate::commands::publish(&app, &state);
            if result.is_ok() {
                prewarm_next(&app);
            }
        });
        control.set_task(&ticket, task.inner().abort_handle());
    }
    drop(control);
    if commands.iter().any(|c| {
        matches!(
            c,
            EngineCommand::SetShuffle { .. }
                | EngineCommand::SetRepeat { .. }
                | EngineCommand::Seek { .. }
        )
    }) {
        prewarm_next(app);
    }
    Ok(true)
}

fn prewarm_next(app: &AppHandle) {
    let native = app.state::<State>();
    let owner = native
        .control
        .lock()
        .expect("native YouTube")
        .current_ticket();
    let Some(owner) = owner else {
        return;
    };
    let state = app.state::<AppState>();
    let engine = state.audio.lock().expect("audio mutex").clone();
    let Some(engine) = engine else {
        return;
    };
    let expected = engine.gapless_snapshot().and_then(|s| s.key);
    let Some(expected) = expected else {
        return;
    };
    let next = {
        let player = state.player.lock().expect("player mutex");
        let snapshot = player.state();
        successor(snapshot).and_then(|index| {
            snapshot.queue.get(index).map(|track| {
                (
                    snapshot.playback_revision,
                    crate::audio_queue::Key {
                        revision: snapshot.playback_revision.wrapping_add(1),
                        index,
                    },
                    track.id.clone(),
                )
            })
        })
    };
    let Some((revision, key, next)) = next else {
        engine.plan_gapless(expected, None);
        native.prepared.lock().expect("YouTube prepared").take();
        if let Some(task) = native.prefetch.lock().expect("YouTube prefetch").take() {
            task.abort();
        }
        return;
    };
    let video = if crate::import::playback::owns_current(app) {
        app.try_state::<crate::import::State>().and_then(|imports| {
            let db = imports.db.lock().ok()?;
            if db
                .source_preference(&next)
                .ok()
                .flatten()
                .is_some_and(|p| p != crate::import::model::Platform::Youtube)
            {
                return None;
            }
            db.mappings_for(&next)
                .ok()?
                .into_iter()
                .find(|mapping| mapping.verified && mapping.provider == "youtube")
                .map(|mapping| mapping.external_id)
        })
    } else {
        Some(next.clone())
    };
    let Some(video) = video.filter(|id| resolver::valid_id(id)) else {
        engine.plan_gapless(expected, None);
        native.prepared.lock().expect("YouTube prepared").take();
        if let Some(task) = native.prefetch.lock().expect("YouTube prefetch").take() {
            task.abort();
        }
        return;
    };
    let plan = NextPlan {
        owner,
        revision,
        key,
        track: next,
        video,
        duration_ms: 0,
        engine_generation: engine.generation(),
    };
    {
        let mut prepared = native.prepared.lock().expect("YouTube prepared");
        if prepared.as_ref().is_some_and(|p| {
            p.owner == plan.owner
                && p.key == key
                && p.track == plan.track
                && p.video == plan.video
                && p.engine_generation == plan.engine_generation
        }) {
            return;
        }
        *prepared = Some(plan.clone());
    }
    if let Some(task) = native.prefetch.lock().expect("YouTube prefetch").take() {
        task.abort();
    }
    engine.plan_gapless(expected, None);
    let app = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        let result = async { resolver(&app).await?.resolve_recovering(&plan.video).await }.await;
        let native = app.state::<State>();
        let control = native.control.lock().expect("native YouTube");
        if !control.accepts(&plan.owner) {
            return;
        }
        let mut prepared = native.prepared.lock().expect("YouTube prepared");
        if prepared.as_ref() != Some(&plan) {
            return;
        }
        let Ok(stream) = result else {
            // Keep this attempted plan: the ticker must not retry an unavailable
            // successor every 250 ms. Its explicit load gets its own bounded retry.
            return;
        };
        let directory = app
            .state::<AppState>()
            .data_dir
            .lock()
            .expect("data dir mutex")
            .clone();
        let Some(directory) = directory else {
            return;
        };
        let cache = crate::stream::cache_path(
            &directory.join("audio").join("youtube"),
            &format!(
                "{}:{}:{}:{}:{}",
                plan.video,
                stream.itag,
                plan.owner.generation,
                plan.key.revision,
                uuid::Uuid::new_v4()
            ),
        );
        if let Some(prepared) = prepared.as_mut() {
            prepared.duration_ms = stream.duration_ms;
        }
        engine.plan_gapless(expected, Some((plan.key, stream.url, cache)));
        engine.set_gapless_format(plan.key, stream.audio_info);
    });
    *native.prefetch.lock().expect("YouTube prefetch") = Some(task.inner().abort_handle());
}

pub fn send_library(app: &AppHandle, commands: Vec<EngineCommand>) {
    let state = app.state::<AppState>();
    let id = state
        .player
        .lock()
        .expect("player mutex")
        .state()
        .current()
        .map(|t| t.id.clone());
    let Some(id) = id else {
        stop(app);
        return;
    };
    let empty = state
        .audio
        .lock()
        .expect("audio mutex")
        .as_ref()
        .is_none_or(|e| e.is_empty() && !e.is_loading());
    let load = commands.iter().any(|c| {
        matches!(
            c,
            EngineCommand::SetQueue { .. } | EngineCommand::SkipNext | EngineCommand::SkipPrevious
        )
    }) || (commands.iter().any(|c| matches!(c, EngineCommand::Play))
        && empty
        && !is_resolving(app));
    if let Err(message) = send(app, &id, &id, load, &commands) {
        state
            .player
            .lock()
            .expect("player mutex")
            .set_status(Status::Paused);
        let _ = app.emit("playback://error", message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn preparation_follows_queue_order_and_repeat_without_mutating_selection() {
        use crate::player::{PlayerState, Repeat, Track};
        let track = |id: &str| Track {
            id: id.into(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 1_000,
        };
        let mut state = PlayerState {
            queue: vec![track("duplicate"), track("other"), track("duplicate")],
            index: Some(0),
            shuffle: true,
            ..Default::default()
        };
        assert_eq!(
            successor(&state),
            Some(1),
            "shuffle is already represented by queue order"
        );
        assert_eq!(
            state.index,
            Some(0),
            "preparation never advances the player"
        );
        state.repeat = Repeat::One;
        assert_eq!(successor(&state), Some(0));
        state.index = Some(2);
        state.repeat = Repeat::Off;
        assert_eq!(successor(&state), None);
        state.repeat = Repeat::All;
        assert_eq!(
            successor(&state),
            Some(0),
            "repeat wraps to the first occurrence, not a matching UUID"
        );
        state.index = None;
        assert_eq!(successor(&state), None);
    }

    #[test]
    fn refreshes_only_denied_or_transient_streams() {
        assert!(refreshable_error(
            "YouTube denied this audio stream. Retry the recording or choose another source."
        ));
        assert!(refreshable_error("connection lost"));
        assert!(refreshable_error("server unreachable"));
        assert!(refreshable_error("timed out waiting for audio data"));
        assert!(!refreshable_error(
            "could not decode this track: unsupported codec"
        ));
        assert!(!refreshable_error("private recording"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires public YouTube access and a real audio output device"]
    async fn native_imported_music_starts_and_buffers_multiple_ranges() {
        let directory =
            std::env::temp_dir().join(format!("capsule-native-music-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let resolver = resolver::Resolver::public(&directory).unwrap();
        for id in ["k13FUGmRRHc", "ub7N1zW1z-4", "fn0EIOnK-D0"] {
            let resolving = Instant::now();
            let stream = resolver.resolve(id).await.unwrap();
            println!(
                "recording={id} native_cold_resolution_ms={}",
                resolving.elapsed().as_millis()
            );
            let cached = Instant::now();
            assert_eq!(resolver.resolve(id).await.unwrap().itag, stream.itag);
            println!(
                "recording={id} native_cached_resolution_ms={}",
                cached.elapsed().as_millis()
            );
            if id == "fn0EIOnK-D0" {
                assert!(
                    stream.duration_ms.abs_diff(212_992) <= 2_000,
                    "Poison playback keeps the imported recording duration"
                );
            }
            println!(
                "recording={id} itag={} duration_ms={}",
                stream.itag, stream.duration_ms
            );
            let engine = crate::audio::Engine::new().unwrap();
            engine.set_volume(0);
            let path = directory.join(format!("{id}.dat"));
            let loading = Instant::now();
            engine.load_gapless(
                stream.url,
                path.clone(),
                crate::audio_queue::Key {
                    revision: 1,
                    index: 0,
                },
            );
            engine.play();
            tokio::time::timeout(Duration::from_secs(45), async {
                while engine.position_ms() < 250 {
                    assert!(
                        engine.current_error().is_none(),
                        "{:?}",
                        engine.current_error()
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("native PCM output reaches 250 ms");
            println!(
                "recording={id} native_load_to_250ms_position_ms={}",
                loading.elapsed().as_millis()
            );
            tokio::time::timeout(Duration::from_secs(45), async {
                loop {
                    assert!(
                        engine.current_error().is_none(),
                        "{:?}",
                        engine.current_error()
                    );
                    if engine.position_ms() >= 1000 && path.with_extension("done").exists() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .expect("music starts and finishes buffering");
            assert!(std::fs::metadata(&path).unwrap().len() > 1024 * 1024);
            engine.stop();
        }
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires public YouTube access and a real audio output device"]
    async fn native_public_demo_plays_to_completion() {
        play_to_completion("jNQXAC9IVRw").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires public YouTube access and a real audio output device"]
    async fn native_imported_recording_plays_to_completion() {
        play_to_completion("k13FUGmRRHc").await;
    }

    async fn play_to_completion(video: &str) {
        let directory =
            std::env::temp_dir().join(format!("capsule-native-playback-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let started = Instant::now();
        let resolver = resolver::Resolver::public(&directory).unwrap();
        let stream = resolver.resolve(video).await.unwrap();
        println!(
            "native_resolution_ms={} duration_ms={}",
            started.elapsed().as_millis(),
            stream.duration_ms
        );
        let duration = stream.duration_ms;
        let cached = Instant::now();
        assert_eq!(resolver.resolve(video).await.unwrap().itag, stream.itag);
        println!(
            "native_cached_resolution_us={}",
            cached.elapsed().as_micros()
        );
        let engine = crate::audio::Engine::new().expect("real native output device");
        engine.set_volume(0); // Exercise the real output silently.
        let started = Instant::now();
        engine.load_gapless(
            stream.url,
            directory.join("audio.dat"),
            crate::audio_queue::Key {
                revision: 1,
                index: 0,
            },
        );
        engine.play();
        tokio::time::timeout(Duration::from_secs(20), async {
            while engine.position_ms() == 0 {
                assert!(
                    engine.current_error().is_none(),
                    "{:?}",
                    engine.current_error()
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("native decoding starts without downloading the full track");
        println!(
            "native_load_to_position_ms={}",
            started.elapsed().as_millis()
        );
        engine.pause();
        let paused_at = engine.position_ms();
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(engine.is_paused());
        assert!(engine.position_ms().abs_diff(paused_at) < 200);
        engine.seek(0);
        engine.play();
        let mut furthest = 0;
        tokio::time::timeout(Duration::from_millis(duration + 30_000), async {
            loop {
                assert!(
                    engine.current_error().is_none(),
                    "{:?}",
                    engine.current_error()
                );
                furthest = furthest.max(engine.position_ms());
                if engine.is_empty() && !engine.is_loading() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .expect("complete native playback");
        assert!(
            duration > 0 && furthest + 1000 >= duration,
            "ended early at {furthest} of {duration} ms"
        );
        println!("native_complete_position_ms={furthest}");
        engine.stop();
        drop(engine);
        let _ = std::fs::remove_dir_all(directory);
    }
}
