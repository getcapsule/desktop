use crate::{
    import::{
        http::Cancellation,
        model::{ImportedTrack, Metadata, Origin, Platform},
    },
    player::{EngineCommand, Status},
    AppState,
};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

fn load_audio(
    engine: &crate::audio::Engine,
    stream: crate::soundcloud::Playable,
    path: std::path::PathBuf,
    key: crate::audio_queue::Key,
) -> Result<(), String> {
    if crate::soundcloud::hls::is_manifest_url(&stream.url) {
        // The decoder can clamp seeks in fragmented AAC to zero because the
        // HLS init segment doesn't carry the duration of the whole track.
        engine.load_gapless(stream.url, path, key);
        engine.set_gapless_format(key, stream.audio_info);
    } else {
        engine
            .load(stream.url, path)
            .map_err(|_| "Could not start the SoundCloud audio buffer.")?;
        engine.set_format(engine.generation(), stream.audio_info);
    }
    Ok(())
}

#[derive(Default)]
pub struct State(Mutex<Control>);
#[derive(Default)]
struct Control {
    generation: u64,
    track: String,
    external: String,
    retries: u8,
    pending: bool,
    paused: bool,
    seek: Option<u64>,
    cancel: Cancellation,
}
pub fn refresh_current(app: &AppHandle, error: &str) -> bool {
    if !crate::youtube_native::refreshable_error(error) && !error.contains("denied") {
        return false;
    }
    let core = app.state::<AppState>();
    let state = app.state::<State>();
    let mut control = state.0.lock().expect("SoundCloud playback");
    let player = core.player.lock().expect("player mutex");
    if control.track.is_empty()
        || control.retries >= 2
        || player
            .state()
            .current()
            .is_none_or(|track| track.id != control.track)
    {
        return false;
    }
    control.retries += 1;
    let track = control.track.clone();
    let external = control.external.clone();
    let commands = [
        EngineCommand::Seek {
            ms: player.state().position_ms,
        },
        if control.paused {
            EngineCommand::Pause
        } else {
            EngineCommand::Play
        },
    ];
    drop(player);
    drop(control);
    send(app, &track, &external, true, &commands).is_ok()
}
pub fn stop(app: &AppHandle) {
    if let Some(state) = app.try_state::<State>() {
        let mut control = state.0.lock().expect("SoundCloud playback");
        control.cancel.cancel();
        let generation = control.generation.wrapping_add(1);
        *control = Control {
            generation,
            ..Default::default()
        };
    }
}
pub fn is_resolving(app: &AppHandle) -> bool {
    app.try_state::<State>()
        .is_some_and(|state| state.0.lock().expect("SoundCloud playback").pending)
}
pub fn send(
    app: &AppHandle,
    track_id: &str,
    external_id: &str,
    load: bool,
    commands: &[EngineCommand],
) -> Result<(), String> {
    let state = app.state::<State>();
    let core = app.state::<AppState>();
    let revision = core
        .player
        .lock()
        .expect("player mutex")
        .state()
        .playback_revision;
    let mut control = state.0.lock().expect("SoundCloud playback");
    if load {
        if control.track != track_id {
            control.retries = 0;
        }
        control.external = external_id.into();
        control.cancel.cancel();
        control.cancel = Cancellation::default();
        control.generation = control.generation.wrapping_add(1);
        control.track = track_id.into();
        control.pending = true;
        control.paused = false;
        control.seek = None;
    }
    for command in commands {
        match command {
            EngineCommand::Pause => control.paused = true,
            EngineCommand::Play => control.paused = false,
            EngineCommand::Seek { ms } => control.seek = Some(*ms),
            _ => {}
        }
        if matches!(command, EngineCommand::Pause) {
            core.player
                .lock()
                .expect("player mutex")
                .set_status(Status::Paused);
        } else if matches!(command, EngineCommand::Play) && control.pending {
            core.player
                .lock()
                .expect("player mutex")
                .set_status(Status::Loading);
        }
        if control.pending {
            if let EngineCommand::Seek { ms } = command {
                core.player.lock().expect("player mutex").on_position(*ms);
            }
        }
        if !control.pending {
            if let Some(engine) = core.audio.lock().expect("audio mutex").as_ref() {
                match command {
                    EngineCommand::Pause => engine.pause(),
                    EngineCommand::Play => engine.play(),
                    EngineCommand::Seek { ms } => engine.seek(*ms),
                    EngineCommand::SetVolume { percent } => engine.set_volume(*percent),
                    _ => {}
                }
            }
        }
    }
    if load {
        let generation = control.generation;
        let cancel = control.cancel.clone();
        let app = app.clone();
        let track_id = track_id.to_string();
        let external_id = external_id.to_string();
        tauri::async_runtime::spawn(async move {
            let result = app
                .state::<crate::soundcloud::commands::State>()
                .0
                .playable(&external_id, &cancel)
                .await;
            let state = app.state::<State>();
            let core = app.state::<AppState>();
            let mut control = state.0.lock().expect("SoundCloud playback");
            if control.generation != generation || cancel.is_cancelled() {
                return;
            }
            let mut player = core.player.lock().expect("player mutex");
            if player
                .state()
                .current()
                .is_none_or(|track| track.id != track_id)
                || player.state().playback_revision != revision
                || matches!(player.state().status, Status::Ended | Status::Idle)
            {
                control.pending = false;
                return;
            }
            let result = result.and_then(|stream| {
                let mut audio = core.audio.lock().expect("audio mutex");
                if audio.is_none() {
                    *audio = Some(Arc::new(
                        crate::desktop::open_engine(&app)
                            .map_err(|_| "No audio output device is available.")?,
                    ));
                }
                let engine = audio.as_ref().unwrap();
                let directory = core
                    .data_dir
                    .lock()
                    .expect("data dir mutex")
                    .clone()
                    .ok_or("No playback cache directory is available.")?
                    .join("audio")
                    .join("soundcloud");
                let path = crate::stream::cache_path(
                    &directory,
                    &format!("{}:{}:{}", external_id, stream.quality, generation),
                );
                crate::stream::prune(&directory, 128 * 1024 * 1024, Some(&path));
                let duration_ms = stream.track.duration_ms;
                let key = crate::audio_queue::Key {
                    revision,
                    index: player.state().index.unwrap_or(0),
                };
                load_audio(engine, stream, path, key)?;
                player.on_now_playing(&track_id, duration_ms);
                engine.set_volume(player.state().volume);
                if let Some(ms) = control.seek {
                    engine.seek(ms);
                }
                if control.paused {
                    engine.pause();
                    player.set_status(Status::Paused);
                } else {
                    engine.play();
                }
                Ok::<(), String>(())
            });
            control.pending = false;
            let failure = result.err();
            drop(player);
            drop(control);
            if let Some(message) = failure {
                if crate::import::playback::report_native_failure(&app, &track_id, &message) {
                    return;
                }
                core.player
                    .lock()
                    .expect("player mutex")
                    .set_status(Status::Paused);
                let _ = app.emit("playback://error", message);
            }
            crate::commands::publish(&app, &core);
        });
    }
    Ok(())
}

#[tauri::command]
pub fn soundcloud_play(
    app: AppHandle,
    tracks: Vec<crate::soundcloud::Track>,
    start_index: usize,
) -> Result<(), String> {
    if tracks.is_empty() || tracks.len() > 500 || start_index >= tracks.len() {
        return Err("Choose between 1 and 500 tracks to play.".into());
    }
    let imports = app.state::<crate::import::State>();
    let queue = {
        let mut db = imports
            .db
            .lock()
            .map_err(|_| "The library is unavailable.")?;
        prepare_queue(&mut db, tracks)?
    };
    crate::commands::apply(&app, |player| player.play_queue(queue, start_index));
    let _ = app.emit("library://updated", crate::sync::counts(&app));
    Ok(())
}

fn prepare_queue(
    db: &mut crate::db::Db,
    tracks: Vec<crate::soundcloud::Track>,
) -> Result<Vec<crate::player::Track>, String> {
    tracks
        .into_iter()
        .map(|track| {
            let (saved, playback_route) = save_discovered_track(db, track)?;
            Ok(crate::player::Track {
                id: saved.id,
                title: saved.metadata.title,
                artist: saved.metadata.artists.join(", "),
                album: saved.metadata.album.unwrap_or_default(),
                duration_ms: saved.metadata.duration_ms.unwrap_or(0),
                playback_route: Some(playback_route),
            })
        })
        .collect()
}

pub(crate) fn save_discovered_track(
    db: &mut crate::db::Db,
    track: crate::soundcloud::Track,
) -> Result<(crate::import::model::CapsuleTrack, crate::import::playback::Route), String> {
    if track.id.is_empty()
        || track.id.len() > 20
        || !track.id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("Invalid SoundCloud track.".into());
    }
    let route = crate::import::playback::Route {
        provider: Platform::Soundcloud,
        external_id: track.id.clone(),
    };
    let saved = db.save_source_track(&ImportedTrack {
        origin: Origin {
            platform: Platform::Soundcloud,
            external_id: track.id,
            url: track.permalink_url,
        },
        metadata: Metadata {
            title: track.title,
            artists: vec![track.artist],
            album: track.album,
            duration_ms: Some(track.duration_ms),
            artwork_url: track.artwork_url,
            isrc: None,
            explicit: None,
        },
    })
    .map_err(|_| "Could not save the searched track.".to_string())?;
    Ok((saved, route))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::playback::{select_recording, AvailableRoute, Route};

    #[test]
    fn hls_seeks_play_audio_at_the_selected_timestamp_in_both_directions() {
        use rodio::Source;
        use std::time::{Duration, Instant};

        let bytes = include_bytes!("../testdata/resume-two-tones-fragmented.m4a");
        let path = std::env::temp_dir().join(format!("capsule-hls-seek-{}.dat", uuid::Uuid::new_v4()));
        std::fs::write(&path, bytes).unwrap();
        std::fs::write(path.with_extension("done"), bytes.len().to_string()).unwrap();
        let (engine, mut output) = crate::audio::Engine::headless();
        let key = crate::audio_queue::Key {
            revision: 1,
            index: 0,
        };
        let stream = crate::soundcloud::Playable {
            audio_info: crate::audio::AudioInfo {
                bitrate_kbps: Some(160),
                ..Default::default()
            },
            url: "https://example.sndcdn.com/stream.m3u8".into(),
            mime_type: "audio/mpegurl".into(),
            quality: "hq".into(),
            track: discovered_track("42"),
        };
        load_audio(&engine, stream, path.clone(), key).unwrap();
        engine.play();

        let mut reach = |ms: u64| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while engine.is_loading() || engine.position_ms().abs_diff(ms) >= 25 {
                assert!(engine.current_error().is_none(), "{:?}", engine.current_error());
                assert!(
                    Instant::now() < deadline,
                    "target {ms} ms, at {} ms",
                    engine.position_ms()
                );
                for _ in 0..128 {
                    output.next();
                }
                std::thread::sleep(Duration::from_millis(3));
            }
            let channels = usize::from(output.channels());
            let count = output.sample_rate() as usize * channels / 10;
            let samples: Vec<_> = output.by_ref().take(count).step_by(channels).collect();
            samples
                .windows(2)
                .filter(|s| s[0] <= 0.0 && s[1] > 0.0)
                .count()
        };
        assert!((40..50).contains(&reach(50)));
        engine.seek(2500);
        let forward = reach(2500);
        assert!(
            (80..95).contains(&forward),
            "expected the second tone, got {forward} crossings"
        );
        engine.seek(500);
        let backward = reach(500);
        assert!(
            (40..50).contains(&backward),
            "expected the first tone, got {backward} crossings"
        );
        assert_eq!(engine.audio_info().bitrate_kbps, Some(160));
        assert_eq!(engine.gapless_snapshot().unwrap().key, Some(key));
        engine.stop();
        drop(output);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(path.with_extension("done")).unwrap();
    }

    fn discovered_track(id: &str) -> crate::soundcloud::Track {
        crate::soundcloud::Track {
            id: id.into(),
            title: "Selected recording".into(),
            artist: "Artist".into(),
            artist_id: None,
            album: None,
            duration_ms: 200_000,
            artwork_url: None,
            permalink_url: None,
        }
    }

    #[test]
    fn discovery_playback_does_not_add_search_results_to_the_songs_library() {
        let mut db = crate::db::Db::open_in_memory().unwrap();
        let queue = prepare_queue(
            &mut db,
            vec![discovered_track("42"), discovered_track("43")],
        )
        .unwrap();
        assert!(
            db.canonical_track(&queue[1].id).unwrap().is_some(),
            "queue entries must remain available to playback"
        );
        let provider = crate::db::Db::open_in_memory().unwrap();
        let library = crate::db::LibraryView::load(&provider, Some(&db), "soundcloud").unwrap();
        let counts = library.db.counts().unwrap();
        assert_eq!(
            counts.songs, 0,
            "search-result playback must not save unheard songs to the library"
        );
        assert_eq!(counts.albums, 0);
        assert_eq!(counts.artists, 0);
        assert!(!db.has_imported_tracks().unwrap());
    }

    #[test]
    fn discovery_keeps_soundcloud_selected_over_an_existing_youtube_preference() {
        let mut db = crate::db::Db::open_in_memory().unwrap();
        let first = prepare_queue(&mut db, vec![discovered_track("42")]).unwrap();
        db.set_source_preference(&first[0].id, Some(Platform::Youtube))
            .unwrap();

        let queue = prepare_queue(&mut db, vec![discovered_track("42")]).unwrap();
        assert_eq!(queue[0].id, first[0].id);
        let mut player = crate::player::Player::new();
        player.play_queue(queue.clone(), 0);
        assert_eq!(
            db.source_preference(&queue[0].id).unwrap(),
            Some(Platform::Youtube)
        );
        let routes = [Platform::Youtube, Platform::Soundcloud].map(|provider| AvailableRoute {
            route: Route {
                provider,
                external_id: if provider == Platform::Soundcloud {
                    "42".into()
                } else {
                    "abcdefghijk".into()
                },
            },
            verified: true,
            available: true,
        });
        let selected = select_recording(
            &routes,
            db.source_preference(&queue[0].id).unwrap(),
            &[Platform::Youtube],
            player.state().current().unwrap().playback_route.as_ref(),
        )
        .unwrap();
        assert_eq!(selected.provider, Platform::Soundcloud);
    }

    #[test]
    fn discovery_queue_entries_reject_alternative_sources_without_changing_library_preferences() {
        let mut db = crate::db::Db::open_in_memory().unwrap();
        let queue = prepare_queue(
            &mut db,
            vec![discovered_track("42"), discovered_track("43")],
        )
        .unwrap();
        assert_ne!(queue[0].id, queue[1].id);
        let mut player = crate::player::Player::new();
        player.play_queue(queue.clone(), 0);
        for track in queue {
            assert_eq!(db.source_preference(&track.id).unwrap(), None);
            assert_eq!(
                track.playback_route.as_ref().map(|route| route.provider),
                Some(Platform::Soundcloud)
            );
            let alternative = AvailableRoute {
                route: Route {
                    provider: Platform::Youtube,
                    external_id: "abcdefghijk".into(),
                },
                verified: true,
                available: true,
            };
            assert!(select_recording(
                &[alternative],
                db.source_preference(&track.id).unwrap(),
                &[],
                track.playback_route.as_ref(),
            )
            .is_none());
        }
    }

    #[test]
    fn discovery_selects_the_exact_origin_when_canonical_tracks_have_multiple_soundcloud_origins() {
        let mut db = crate::db::Db::open_in_memory().unwrap();
        let first = prepare_queue(&mut db, vec![discovered_track("42")]).unwrap();
        db.attach_playback_origin(
            &first[0].id,
            &Origin {
                platform: Platform::Soundcloud,
                external_id: "43".into(),
                url: None,
            },
        )
        .unwrap();
        let queue = prepare_queue(
            &mut db,
            vec![discovered_track("42"), discovered_track("43")],
        )
        .unwrap();
        assert_eq!(queue[0].id, queue[1].id);
        let routes = ["42", "43"].map(|id| AvailableRoute {
            route: Route {
                provider: Platform::Soundcloud,
                external_id: id.into(),
            },
            verified: true,
            available: true,
        });
        for (track, expected) in queue.iter().zip(["42", "43"]) {
            let selected =
                select_recording(&routes, None, &[], track.playback_route.as_ref()).unwrap();
            assert_eq!(selected.external_id, expected);
        }
    }

    #[test]
    fn exact_recording_choices_survive_queue_edits_and_restore_until_changed_explicitly() {
        let mut db = crate::db::Db::open_in_memory().unwrap();
        let queue = prepare_queue(
            &mut db,
            vec![discovered_track("42"), discovered_track("43")],
        )
        .unwrap();
        let mut player = crate::player::Player::new();
        player.play_queue(queue.clone(), 0);
        player.queue_move(0, 1);
        assert_eq!(
            player
                .state()
                .current()
                .unwrap()
                .playback_route
                .as_ref()
                .unwrap()
                .external_id,
            "42"
        );
        player.queue_remove(1);
        player.queue_undo();
        let saved = serde_json::to_string(player.state()).unwrap();
        let mut restored = crate::player::Player::new();
        restored.restore(serde_json::from_str(&saved).unwrap());
        assert_eq!(
            restored
                .state()
                .current()
                .unwrap()
                .playback_route
                .as_ref()
                .unwrap()
                .external_id,
            "42"
        );
        restored.clear_recording_choice(&queue[0].id);
        assert!(restored.state().current().unwrap().playback_route.is_none());
        assert!(restored
            .state()
            .queue
            .iter()
            .find(|track| track.id == queue[1].id)
            .unwrap()
            .playback_route
            .is_some());
        let mut library_track = queue[0].clone();
        library_track.playback_route = None;
        restored.play_queue(vec![library_track], 0);
        assert!(restored.state().current().unwrap().playback_route.is_none());
    }
}
