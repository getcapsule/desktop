use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::db::LibraryCounts;
use crate::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub stage: &'static str,
    pub songs: u32,
    pub albums: u32,
    pub playlists: u32,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncFailed {
    pub reason: String,
    pub needs_auth: bool,
}

#[derive(Default)]
pub struct SyncGuard(Arc<AtomicBool>);

impl SyncGuard {
    pub fn try_begin(&self) -> Option<SyncLease> {
        if self.0.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(SyncLease(self.0.clone()))
        }
    }
}

pub struct SyncLease(Arc<AtomicBool>);

impl Drop for SyncLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn emit(app: &AppHandle, p: &Progress) {
    let _ = app.emit("library://progress", p);
}

pub async fn run(app: AppHandle) {
    let state = app.state::<AppState>();

    let Some(lease) = state.sync.try_begin() else {
        tracing::info!("sync already running; skipping");
        return;
    };

    let (settings, generation) = {
        let settings = state.settings.lock().expect("settings mutex");
        let _db = state.db.lock().expect("db mutex");
        (settings.clone(), state.db_generation.load(Ordering::SeqCst))
    };
    let tokens = { state.tokens.lock().expect("tokens mutex").clone() };

    let (settings, navidrome_password) = crate::source::resolve(&settings);

    if settings.source == crate::settings::Source::Local {
        return scan_local(
            app.clone(),
            settings.local.folders.clone(),
            generation,
            lease,
        )
        .await;
    }

    // Public collections are added on demand, with no private library session.
    if matches!(settings.source, crate::settings::Source::Youtube | crate::settings::Source::Soundcloud) {
        let _ = app.emit("library://updated", counts(&app));
        emit(
            &app,
            &Progress {
                stage: "done",
                songs: 0,
                albums: 0,
                playlists: 0,
                done: true,
            },
        );
        return;
    }
    let connected = crate::source::connect(&settings, tokens, navidrome_password);
    let client = match connected {
        Ok(c) => c,
        Err(e) => {
            let needs_auth = e.needs_auth();
            tracing::warn!(error = %e, "cannot start sync");
            let _ = app.emit(
                "library://failed",
                SyncFailed {
                    reason: e.to_string(),
                    needs_auth,
                },
            );
            return;
        }
    };

    if let crate::source::SourceClient::Navidrome(ref nd) = client {
        let shared = std::sync::Arc::new(nd.clone_for_artwork());
        *app.state::<AppState>()
            .navidrome
            .lock()
            .expect("navidrome mutex") = Some(shared);
    }

    let still_ours = {
        let app = app.clone();
        move || app.state::<AppState>().db_generation.load(Ordering::SeqCst) == generation
    };

    let songs = AtomicU32::new(0);
    let albums = AtomicU32::new(0);
    let playlists = AtomicU32::new(0);
    let bump = |c: &AtomicU32, n: usize| {
        c.fetch_add(n as u32, Ordering::Relaxed);
    };

    let snapshot = |stage: &'static str, done: bool| Progress {
        stage,
        songs: songs.load(Ordering::Relaxed),
        albums: albums.load(Ordering::Relaxed),
        playlists: playlists.load(Ordering::Relaxed),
        done,
    };

    emit(&app, &snapshot("songs", false));

    let songs_res = client
        .library_songs(|rows| {
            if rows.is_empty() {
                return;
            }
            if !still_ours() {
                return;
            }
            let state = app.state::<AppState>();
            let mut db = state.db.lock().expect("db mutex");
            if !still_ours() {
                return;
            }
            let written = db.upsert_songs(&rows);
            drop(db);
            match written {
                Ok(n) => bump(&songs, n),
                Err(e) => tracing::error!(error = %e, "song batch write failed"),
            }
            emit(&app, &snapshot("songs", false));
        })
        .await;
    if let Err(e) = songs_res {
        return fail(&app, "songs", e, lease);
    }

    let albums_res = client
        .library_albums(|rows| {
            if rows.is_empty() {
                return;
            }
            if !still_ours() {
                return;
            }
            let state = app.state::<AppState>();
            let mut db = state.db.lock().expect("db mutex");
            if !still_ours() {
                return;
            }
            let written = db.upsert_albums(&rows);
            drop(db);
            match written {
                Ok(n) => bump(&albums, n),
                Err(e) => tracing::error!(error = %e, "album batch write failed"),
            }
            emit(&app, &snapshot("albums", false));
        })
        .await;
    if let Err(e) = albums_res {
        return fail(&app, "albums", e, lease);
    }

    let playlists_res = client
        .library_playlists(|rows| {
            if rows.is_empty() {
                return;
            }
            if !still_ours() {
                return;
            }
            let state = app.state::<AppState>();
            let mut db = state.db.lock().expect("db mutex");
            if !still_ours() {
                return;
            }
            let written = db.upsert_playlists(&rows);
            drop(db);
            match written {
                Ok(n) => bump(&playlists, n),
                Err(e) => tracing::error!(error = %e, "playlist batch write failed"),
            }
            emit(&app, &snapshot("playlists", false));
        })
        .await;
    if let Err(e) = playlists_res {
        return fail(&app, "playlists", e, lease);
    }

    if let crate::source::SourceClient::Navidrome(ref nd) = client {
        let ids: Vec<String> = {
            let state = app.state::<AppState>();
            let db = state.db.lock().expect("db mutex");
            db.playlist_ids().unwrap_or_default()
        };
        for pid in ids {
            if !still_ours() {
                tracing::info!("library switched mid-sync; abandoning");
                drop(lease);
                return;
            }
            match nd.playlist_track_ids(&pid).await {
                Ok(track_ids) => {
                    let state = app.state::<AppState>();
                    let mut db = state.db.lock().expect("db mutex");
                    if !still_ours() {
                        return;
                    }
                    if let Err(e) = db.set_playlist_tracks(&pid, &track_ids) {
                        tracing::error!(error = %e, %pid, "playlist track write failed");
                    }
                }
                Err(e) => tracing::warn!(error = %e, %pid, "playlist fetch failed; skipping"),
            }
        }
    }

    let artists_res = client
        .library_artists(|rows| {
            if rows.is_empty() {
                return;
            }
            if !still_ours() {
                return;
            }
            let state = app.state::<AppState>();
            let mut db = state.db.lock().expect("db mutex");
            if !still_ours() {
                return;
            }
            let written = db.upsert_artists(&rows);
            drop(db);
            if let Err(e) = written {
                tracing::error!(error = %e, "artist batch write failed");
            }
        })
        .await;
    if let Err(e) = artists_res {
        tracing::warn!(error = %e, "artist sync failed; continuing");
    }

    if !still_ours() {
        tracing::info!("library switched mid-sync; abandoning without marking it complete");
        drop(lease);
        return;
    }

    emit(&app, &snapshot("done", true));

    {
        let state = app.state::<AppState>();
        let db = state.db.lock().expect("db mutex");
        if state.db_generation.load(Ordering::SeqCst) == generation {
            let _ = db.set_meta("last_sync_ok", "1");
        }
    }

    tracing::info!(
        songs = songs.load(Ordering::Relaxed),
        albums = albums.load(Ordering::Relaxed),
        playlists = playlists.load(Ordering::Relaxed),
        "sync complete"
    );
    let _ = app.emit("library://updated", counts(&app));
    drop(lease);

    let handle = app.clone();
    tauri::async_runtime::spawn(async move { crate::artwork::prefetch(handle, 56).await });
}

fn fail(app: &AppHandle, stage: &str, e: crate::source::SourceError, lease: SyncLease) {
    let needs_auth = e.needs_auth();
    tracing::error!(stage, error = %e, "sync stage failed");
    let _ = app.emit(
        "library://failed",
        SyncFailed {
            reason: e.to_string(),
            needs_auth,
        },
    );
    drop(lease);
}

pub fn counts(app: &AppHandle) -> LibraryCounts {
    let state = app.state::<AppState>();
    crate::commands::with_library(app, &state, |db| db.counts()).unwrap_or_default()
}
async fn scan_local(
    app: AppHandle,
    folders: Vec<std::path::PathBuf>,
    generation: u64,
    lease: SyncLease,
) {
    use crate::local;

    if folders.is_empty() {
        tracing::info!("local source with no folders configured");
        let _ = app.emit(
            "library://failed",
            SyncFailed {
                reason: "no music folders chosen yet".into(),
                needs_auth: false,
            },
        );
        drop(lease);
        return;
    }

    let emit_progress = |songs: u32, albums: u32, done: bool| {
        emit(
            &app,
            &Progress {
                stage: "songs",
                songs,
                albums,
                playlists: 0,
                done,
            },
        );
    };
    emit_progress(0, 0, false);

    let scanned = tauri::async_runtime::spawn_blocking(move || {
        let files = local::walk(&folders);
        let mut songs = Vec::with_capacity(files.len());
        let mut tags = std::collections::HashMap::new();
        for path in files {
            let Some(t) = local::read_tags(&path) else {
                tracing::debug!(path = %path.display(), "unreadable file; skipping");
                continue;
            };
            let song = local::song_from(&path, &t);
            tags.insert(song.id.clone(), t);
            songs.push(song);
        }
        (songs, tags)
    })
    .await;

    let (songs, tags) = match scanned {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "folder scan failed");
            let _ = app.emit(
                "library://failed",
                SyncFailed {
                    reason: "could not read your music folders".into(),
                    needs_auth: false,
                },
            );
            drop(lease);
            return;
        }
    };

    let albums = local::albums_from(&songs, &tags);
    let artists = local::artists_from(&songs);

    let state = app.state::<AppState>();
    if state.db_generation.load(Ordering::SeqCst) != generation {
        tracing::info!("library switched mid-scan; abandoning");
        drop(lease);
        return;
    }

    {
        let mut db = state.db.lock().expect("db mutex");
        if state.db_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        if let Err(e) = db.upsert_songs(&songs) {
            tracing::error!(error = %e, "song write failed");
        }
        if let Err(e) = db.upsert_albums(&albums) {
            tracing::error!(error = %e, "album write failed");
        }
        if let Err(e) = db.upsert_artists(&artists) {
            tracing::error!(error = %e, "artist write failed");
        }
    }

    emit_progress(songs.len() as u32, albums.len() as u32, true);
    tracing::info!(
        songs = songs.len(),
        albums = albums.len(),
        "local scan complete"
    );

    {
        let db = state.db.lock().expect("db mutex");
        if state.db_generation.load(Ordering::SeqCst) == generation {
            let _ = db.set_meta("last_sync_ok", "1");
        }
    }
    let _ = app.emit("library://updated", counts(&app));
    drop(lease);

    let handle = app.clone();
    tauri::async_runtime::spawn(async move { crate::artwork::prefetch(handle, 56).await });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_allows_one_sync_at_a_time() {
        let g = SyncGuard::default();
        let first = g.try_begin();
        assert!(first.is_some(), "first sync should start");
        assert!(
            g.try_begin().is_none(),
            "second concurrent sync must be refused"
        );
        drop(first);
        assert!(
            g.try_begin().is_some(),
            "guard must release after the lease drops"
        );
    }
}
