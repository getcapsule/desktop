//! Fill missing public metadata after save/startup without delaying imports.
use super::{http::Cancellation, jobs, public_spotify};
use futures_util::{stream, StreamExt};
use std::{
    collections::HashSet,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

#[derive(Default)]
pub(super) struct State {
    status: Mutex<Status>,
    attempted: Mutex<HashSet<(String, String)>>,
}
#[derive(Default)]
struct Status {
    running: bool,
    requested: bool,
}

pub(super) fn schedule(app: &AppHandle) {
    let Some(repair) = app.try_state::<State>() else {
        return;
    };
    {
        let mut status = repair.status.lock().expect("metadata repair status");
        status.requested = true;
        if status.running {
            return;
        }
        status.running = true;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Let the first window and saved-playlist feedback finish immediately.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let cancel = Cancellation::default();
        let mut changed = false;
        let mut last_notify = Instant::now();
        loop {
            app.state::<State>()
                .status
                .lock()
                .expect("metadata repair status")
                .requested = false;
            let Some(imports) = app.try_state::<super::State>() else {
                app.state::<State>()
                    .status
                    .lock()
                    .expect("metadata repair status")
                    .running = false;
                break;
            };
            let gaps = imports
                .db
                .lock()
                .ok()
                .and_then(|db| db.spotify_metadata_gaps().ok())
                .unwrap_or_default();
            let repair = app.state::<State>();
            let pending: Vec<_> = {
                let mut attempted = repair.attempted.lock().expect("metadata repair");
                gaps.into_iter()
                    .filter(|key| attempted.len() < 10000 && attempted.insert(key.clone()))
                    .collect()
            };
            if pending.is_empty() {
                let mut status = repair.status.lock().expect("metadata repair status");
                if status.requested {
                    continue;
                }
                status.running = false;
                break;
            }
            let mut work = stream::iter(pending)
                .map(|(track, external)| {
                    let app = app.clone();
                    let cancel = cancel.clone();
                    async move {
                        let Ok(Some(detail)) =
                            public_spotify::fetch_track_metadata(&external, &cancel).await
                        else {
                            return false;
                        };
                        let Some(imports) = app.try_state::<super::State>() else {
                            return false;
                        };
                        let result = imports
                            .db
                            .lock()
                            .ok()
                            .and_then(|db| {
                                db.fill_spotify_metadata(&track, &external, &detail.metadata)
                                    .ok()
                            })
                            .unwrap_or(false);
                        result
                    }
                })
                .buffer_unordered(2);
            while let Some(updated) = work.next().await {
                changed |= updated;
                if updated && last_notify.elapsed() >= Duration::from_millis(700) {
                    jobs::library_changed(&app);
                    last_notify = Instant::now();
                }
            }
        }
        if changed {
            jobs::library_changed(&app)
        }
    });
}
