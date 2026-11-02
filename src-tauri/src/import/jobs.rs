use super::{http::Cancellation, model::*, providers, urls};
use crate::db::{Db, ImportJobRow};
use futures_util::{stream, StreamExt};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Emitter, Manager};

pub struct State {
    pub db: Mutex<Db>,
    running: Mutex<HashMap<String, Cancellation>>,
    fetch_slots: Arc<tokio::sync::Semaphore>,
    resolution_gate: ResolutionGate,
}

// Exclude competing writes for a canonical track, while unrelated searches
// share the same two-request bound as the anonymous YouTube transport.
struct ResolutionGate {
    slots: tokio::sync::Semaphore,
    tracks: Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
}
impl ResolutionGate {
    fn new() -> Self {
        Self {
            slots: tokio::sync::Semaphore::new(2),
            tracks: Mutex::new(HashMap::new()),
        }
    }
    async fn acquire(
        &self,
        id: &str,
        cancel: &Cancellation,
    ) -> Result<
        (
            tokio::sync::OwnedMutexGuard<()>,
            tokio::sync::SemaphorePermit<'_>,
        ),
        String,
    > {
        cancel.check()?;
        let track = {
            let mut tracks = self.tracks.lock().map_err(err)?;
            tracks.retain(|_, value| value.strong_count() > 0);
            let track = tracks
                .get(id)
                .and_then(std::sync::Weak::upgrade)
                .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
            tracks.insert(id.to_owned(), Arc::downgrade(&track));
            track
        };
        let guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("Import cancelled.".into()),
            guard = track.lock_owned() => guard,
        };
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("Import cancelled.".into()),
            permit = self.slots.acquire() => permit.map_err(|_| "Import resolver stopped.")?,
        };
        cancel.check()?;
        Ok((guard, permit))
    }
}

#[derive(Clone, Serialize)]
pub struct Job {
    #[serde(flatten)]
    progress: ImportJobRow,
    playlist_id: Option<String>,
}

pub fn initialize(app: &AppHandle, directory: &Path) -> Result<(), String> {
    let db = Db::open_at(&directory.join("library-imports.sqlite3")).map_err(err)?;
    db.recover_imports().map_err(err)?;
    app.manage(super::metadata_repair::State::default());
    app.manage(State {
        db: Mutex::new(db),
        running: Mutex::new(HashMap::new()),
        fetch_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        resolution_gate: ResolutionGate::new(),
    });
    Ok(())
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn service(app: &AppHandle) -> Result<tauri::State<'_, State>, String> {
    app.try_state::<State>()
        .ok_or_else(|| "The import library could not be opened.".into())
}
pub fn repair_missing_metadata(app: &AppHandle) {
    super::metadata_repair::schedule(app);
}
fn job_view(db: &Db, id: &str) -> Result<Job, String> {
    Ok(Job {
        progress: db
            .get_import_job(id)
            .map_err(err)?
            .ok_or("Unknown import job.")?,
        playlist_id: db.import_job_playlist(id).map_err(err)?,
    })
}
fn notify(app: &AppHandle, id: &str) {
    if let Ok(state) = service(app) {
        if let Ok(db) = state.db.lock() {
            if let Ok(job) = job_view(&db, id) {
                let _ = app.emit("import://progress", job);
            }
        }
    }
}
pub(super) fn library_changed(app: &AppHandle) {
    let _ = app.emit("library://updated", crate::sync::counts(app));
}

fn key(provider: Platform) -> Result<keyring::Entry, String> {
    if !matches!(
        provider,
        Platform::Spotify | Platform::AppleMusic | Platform::Youtube | Platform::Soundcloud
    ) {
        return Err("This provider does not use metadata API credentials.".into());
    }
    keyring::Entry::new(
        "com.deniz.capsule",
        &format!("import:{}", provider.as_str()),
    )
    .map_err(|_| "Could not access the OS credential store.".into())
}
fn stored(provider: Platform) -> Option<String> {
    key(provider)
        .ok()?
        .get_password()
        .ok()
        .filter(|v| !v.trim().is_empty())
}

async fn credentials(app: &AppHandle, provider: Platform) -> providers::Credentials {
    // Fetching public YouTube metadata never restores unrelated account sessions.
    let mut credentials = providers::Credentials::default();
    match provider {
        Platform::Spotify => credentials.spotify_token = stored(provider),
        Platform::AppleMusic => credentials.apple_token = stored(provider),
        Platform::Youtube => credentials.youtube_key = stored(provider),
        Platform::Soundcloud => credentials.soundcloud_token = stored(provider),
        _ => return credentials,
    }
    if provider == Platform::AppleMusic && credentials.apple_token.is_none() {
        credentials.apple_token = app
            .state::<crate::AppState>()
            .tokens
            .lock()
            .ok()
            .and_then(|t| t.as_ref().map(|t| t.developer_token.clone()))
            .filter(|t| !t.is_empty());
        if credentials.apple_token.is_none() {
            credentials.apple_token = crate::auth::load()
                .ok()
                .flatten()
                .map(|t| t.developer_token)
                .filter(|t| !t.is_empty());
        }
    }
    // Browser authorization refreshes automatically and must supersede a stale
    // manually pasted bearer token. Keep manual access as a fallback only.
    if provider != Platform::Spotify {
        return credentials;
    }
    if let Ok(client) = crate::spotify::client(app) {
        if let Ok(token) = client.token().await {
            credentials.spotify_token = Some(token);
        }
    } else if credentials.spotify_token.is_none() {
        if let Ok(Some(session)) = crate::auth::load_spotify() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if session.expires_at > now {
                credentials.spotify_token = Some(session.access_token);
            }
        }
    }
    credentials
}

#[tauri::command]
pub fn import_detect(url: String) -> Result<urls::ImportUrl, String> {
    urls::detect(&url)
}
#[tauri::command]
pub fn import_credential(provider: Platform, secret: String) -> Result<(), String> {
    if secret.len() > 4096 || secret.chars().any(char::is_control) {
        return Err("Invalid metadata API credential.".into());
    }
    let entry = key(provider)?;
    if secret.trim().is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err("Could not remove the metadata credential.".into()),
        }
    } else {
        entry
            .set_password(secret.trim())
            .map_err(|_| "Could not save this credential in the OS credential store.".into())
    }
}
#[tauri::command]
pub fn import_credential_status() -> HashMap<String, bool> {
    [
        Platform::Spotify,
        Platform::AppleMusic,
        Platform::Youtube,
        Platform::Soundcloud,
    ]
    .into_iter()
    .map(|p| (p.as_str().into(), stored(p).is_some()))
    .collect()
}
#[tauri::command]
pub fn import_job(app: AppHandle, url: String) -> Result<Job, String> {
    let target = urls::detect(&url)?;
    let state = service(&app)?;
    let job = {
        let mut db = state.db.lock().map_err(err)?;
        let job = db.create_import_job(&target).map_err(err)?;
        job_view(&db, &job.id)?
    };
    fetch_in_background(&app, job.progress.id.clone(), target)?;
    Ok(job)
}

fn fetch_in_background(
    app: &AppHandle,
    job: String,
    target: urls::ImportUrl,
) -> Result<(), String> {
    let state = service(app)?;
    let cancel = Cancellation::default();
    {
        let mut running = state.running.lock().map_err(err)?;
        if running.contains_key(&job) {
            return Err("This import is already running.".into());
        }
        running.insert(job.clone(), cancel.clone());
    }
    let slots = state.fetch_slots.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let started = std::time::Instant::now();
        let result=async {
            let _slot=tokio::select! { value=slots.acquire()=>value.map_err(|_|"Import scheduler is closed.")?, _=cancel.cancelled()=>return Err("Import cancelled.".to_string()) };
            // Public imports do not restore an account or refresh an OAuth token.
            match providers::fetch(&target, &Default::default(), &cancel).await {
                Ok(collection) => Ok(collection),
                Err(public_error) => {
                    cancel.check()?;
                    let access = credentials(&app, target.provider).await;
                    let configured = match target.provider {
                        Platform::Spotify => &access.spotify_token,
                        Platform::AppleMusic => &access.apple_token,
                        Platform::Youtube => &access.youtube_key,
                        Platform::Soundcloud => &access.soundcloud_token,
                        _ => return Err(public_error),
                    }.as_deref().is_some_and(|secret| !secret.trim().is_empty());
                    if configured {
                        providers::fetch_authorized(&target, &access, &cancel).await
                    } else { Err(public_error) }
                }
            }
        }.await;
        tracing::info!(
            phase = "metadata",
            provider = target.provider.as_str(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            tracks = result
                .as_ref()
                .map(|collection| collection.tracks.len())
                .unwrap_or(0),
            success = result.is_ok(),
            "Import phase finished"
        );
        if let Ok(state) = service(&app) {
            if let Ok(db) = state.db.lock() {
                if let Ok(mut running) = state.running.lock() {
                    running.remove(&job);
                }
                match result {
                    Ok(collection) => {
                        let _ = db.set_import_preview(&job, &collection);
                    }
                    Err(error) => {
                        let _ = db.fail_import(&job, &error);
                    }
                }
            }
        }
        notify(&app, &job);
    });
    Ok(())
}

#[tauri::command]
pub fn import_status(app: AppHandle, job_id: String) -> Result<Job, String> {
    let state = service(&app)?;
    let db = state.db.lock().map_err(err)?;
    job_view(&db, &job_id)
}
#[tauri::command]
pub fn import_jobs(app: AppHandle) -> Result<Vec<Job>, String> {
    let state = service(&app)?;
    let db = state.db.lock().map_err(err)?;
    db.list_import_jobs()
        .map_err(err)?
        .iter()
        .map(|job| job_view(&db, &job.id))
        .collect()
}
#[tauri::command]
pub fn import_preview(
    app: AppHandle,
    job_id: String,
) -> Result<Option<ImportedCollection>, String> {
    let state = service(&app)?;
    let result = state
        .db
        .lock()
        .map_err(err)?
        .import_preview(&job_id)
        .map_err(err);
    result
}
#[tauri::command]
pub async fn import_confirm(app: AppHandle, job_id: String) -> Result<String, String> {
    let handle = app.clone();
    let id = job_id.clone();
    let playlist = tauri::async_runtime::spawn_blocking(move || {
        let state = service(&handle)?;
        let result = state
            .db
            .lock()
            .map_err(err)?
            .confirm_import(&id)
            .map_err(err);
        result
    })
    .await
    .map_err(|_| "Import persistence was interrupted.")??;
    match_in_background(&app, job_id)?;
    library_changed(&app);
    repair_missing_metadata(&app);
    Ok(playlist)
}

/// Resolve the requested occurrence and two upcoming entries through the
/// shared bounded scheduler, with exclusion for each canonical track.
pub fn prioritize(app: &AppHandle, track_id: &str) {
    let mut ids = vec![track_id.to_string()];
    {
        let app_state = app.state::<crate::AppState>();
        let player = app_state.player.lock().expect("player mutex");
        if let Some(index) = player.state().index {
            ids.extend(
                player
                    .state()
                    .queue
                    .iter()
                    .skip(index + 1)
                    .take(2)
                    .map(|track| track.id.clone()),
            );
        }
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for id in ids {
            let _ = resolve_one(&app, &id, false, &Cancellation::default()).await;
        }
    });
}
#[tauri::command]
pub fn import_cancel(app: AppHandle, job_id: String) -> Result<(), String> {
    let state = service(&app)?;
    state
        .db
        .lock()
        .map_err(err)?
        .cancel_import(&job_id)
        .map_err(err)?;
    if let Some(cancel) = state.running.lock().map_err(err)?.get(&job_id) {
        cancel.cancel();
    }
    notify(&app, &job_id);
    Ok(())
}
#[tauri::command]
pub fn import_resume(app: AppHandle, job_id: String) -> Result<(), String> {
    let state = service(&app)?;
    if state.running.lock().map_err(err)?.contains_key(&job_id) {
        return Err("The previous request is still stopping. Retry shortly.".into());
    }
    let (has_playlist, target) = {
        let db = state.db.lock().map_err(err)?;
        let info = db
            .get_import_job(&job_id)
            .map_err(err)?
            .ok_or("Unknown import job.")?;
        if !matches!(
            info.status.as_str(),
            "interrupted" | "cancelled" | "error" | "completed"
        ) {
            return Err("This import cannot be resumed yet.".into());
        }
        let target = urls::detect(&info.source_url)?;
        let has_playlist = db.resume_import_job(&job_id).map_err(err)?;
        (has_playlist, target)
    };
    if has_playlist {
        match_in_background(&app, job_id)?;
    } else {
        fetch_in_background(&app, job_id, target)?;
    }
    Ok(())
}

// Keep polling started resolutions after cancellation so their pending-state
// cleanup runs. Entries that have not started do no work after cancellation.
async fn run_matches<F, Fut>(
    ids: Vec<String>,
    cancel: &Cancellation,
    resolve: F,
) -> Result<(), String>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut work = stream::iter(ids)
        .map(|id| {
            let resolve = &resolve;
            async move {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                resolve(id).await
            }
        })
        .buffer_unordered(3);
    let mut error = None;
    while let Some(result) = work.next().await {
        if let Err(value) = result {
            error.get_or_insert(value);
        }
    }
    error.map_or(Ok(()), Err)
}

fn match_in_background(app: &AppHandle, job: String) -> Result<(), String> {
    let state = service(app)?;
    let cancel = Cancellation::default();
    {
        let mut running = state.running.lock().map_err(err)?;
        if running.contains_key(&job) {
            return Ok(());
        }
        running.insert(job.clone(), cancel.clone());
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result: Result<(), String> = async {
            let entries = {
                let state = service(&app)?;
                let db = state.db.lock().map_err(err)?;
                let playlist = db
                    .import_job_playlist(&job)
                    .map_err(err)?
                    .ok_or("Import has no local playlist.")?;
                db.import_entries(&playlist).map_err(err)?
            };
            let mut seen = std::collections::HashSet::new();
            let ids: Vec<String> = entries
                .into_iter()
                .map(|entry| entry.track.id)
                .filter(|id| seen.insert(id.clone()))
                .collect();
            let credentials = providers::Credentials {
                youtube_key: stored(Platform::Youtube),
                ..Default::default()
            };
            let started = std::time::Instant::now();
            let track_count = ids.len();
            run_matches(ids, &cancel, |id| {
                let app = &app;
                let cancel = &cancel;
                let job = &job;
                let credentials = &credentials;
                async move {
                    let _ = resolve_with_credentials(app, &id, false, cancel, credentials).await;
                    let state = service(app)?;
                    state
                        .db
                        .lock()
                        .map_err(err)?
                        .update_import_progress(job)
                        .map_err(err)?;
                    notify(app, job);
                    Ok(())
                }
            })
            .await?;
            tracing::info!(
                phase = "matching",
                tracks = track_count,
                elapsed_ms = started.elapsed().as_millis() as u64,
                cancelled = cancel.is_cancelled(),
                "Import phase finished"
            );
            if cancel.is_cancelled() {
                return Ok(());
            }
            let state = service(&app)?;
            state
                .db
                .lock()
                .map_err(err)?
                .finish_matching(&job)
                .map_err(err)?;
            Ok(())
        }
        .await;
        if let Ok(state) = service(&app) {
            if let Err(error) = result {
                if let Ok(db) = state.db.lock() {
                    let _ = db.fail_import(&job, &error);
                }
            }
            if let Ok(mut running) = state.running.lock() {
                running.remove(&job);
            }
        }
        notify(&app, &job);
        library_changed(&app);
    });
    Ok(())
}

async fn resolve_one(
    app: &AppHandle,
    track_id: &str,
    force: bool,
    cancel: &Cancellation,
) -> Result<(), String> {
    let credentials = providers::Credentials {
        youtube_key: stored(Platform::Youtube),
        ..Default::default()
    };
    resolve_with_credentials(app, track_id, force, cancel, &credentials).await
}

async fn resolve_with_credentials(
    app: &AppHandle,
    track_id: &str,
    force: bool,
    cancel: &Cancellation,
    credentials: &providers::Credentials,
) -> Result<(), String> {
    let state = service(app)?;
    let _guard = state.resolution_gate.acquire(track_id, cancel).await?;
    let track = {
        let mut db = state.db.lock().map_err(err)?;
        db.auto_verify_cached_candidate(track_id).map_err(err)?;
        let track = db
            .canonical_track(track_id)
            .map_err(err)?
            .ok_or("Unknown imported track.")?;
        if !force
            && matches!(
                db.resolution_state(track_id).map_err(err)?.as_deref(),
                Some("matched" | "needs_review" | "unavailable")
            )
        {
            return Ok(());
        }
        db.set_resolution(track_id, "searching", &[], None)
            .map_err(err)?;
        track
    };
    let metadata = &track.metadata;
    let query = format!(
        "{} {}{}",
        metadata.artists.join(" "),
        metadata.title,
        metadata
            .album
            .as_ref()
            .map(|a| format!(" {a}"))
            .unwrap_or_default()
    );
    let cache_key = format!(
        "resolver-v1:{}",
        serde_json::to_string(metadata).map_err(err)?
    );
    let cached = if force {
        None
    } else {
        state
            .db
            .lock()
            .map_err(err)?
            .cached_candidates(&cache_key)
            .map_err(err)?
    };
    let result = if let Some(candidates) = cached {
        Ok(candidates)
    } else {
        let mut result = if let Some(isrc) = metadata.isrc.as_deref() {
            providers::search_youtube(isrc, credentials, cancel).await
        } else {
            Ok(vec![])
        };
        if result.as_ref().is_ok_and(|candidates| {
            !candidates
                .iter()
                .any(|candidate| super::matching::score(metadata, &candidate.metadata).1)
        }) {
            result = providers::search_youtube(&query, credentials, cancel).await;
        }
        if let Ok(candidates) = &result {
            state
                .db
                .lock()
                .map_err(err)?
                .cache_candidates(&cache_key, candidates)
                .map_err(err)?;
        }
        result
    };
    if cancel.is_cancelled() {
        state
            .db
            .lock()
            .map_err(err)?
            .set_resolution(track_id, "pending", &[], None)
            .map_err(err)?;
        return Err("Import cancelled.".into());
    }
    match result {
        Ok(mut candidates) => {
            super::matching::rank(metadata, &mut candidates);
            let status = if candidates.is_empty() {
                "unavailable"
            } else {
                "needs_review"
            };
            let mut db = state.db.lock().map_err(err)?;
            let status = if db.mappings_for(track_id).map_err(err)?.iter().any(|mapping| mapping.verified) {
                "matched"
            } else {
                status
            };
            db.set_resolution(track_id, status, &candidates, None)
                .map_err(err)?;
            db.auto_verify_cached_candidate(track_id).map_err(err)?;
            Ok(())
        }
        Err(error) => {
            state
                .db
                .lock()
                .map_err(err)?
                .set_resolution(track_id, "error", &[], Some(&error))
                .map_err(err)?;
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn import_resolve(app: AppHandle, track_id: String) -> Result<(), String> {
    resolve_one(&app, &track_id, true, &Cancellation::default()).await
}
#[tauri::command]
pub fn import_entries(app: AppHandle, playlist_id: String) -> Result<Vec<PlaylistEntry>, String> {
    let state = service(&app)?;
    let result = state
        .db
        .lock()
        .map_err(err)?
        .import_entries(&playlist_id)
        .map_err(err);
    result
}
#[tauri::command]
pub fn import_candidates(
    app: AppHandle,
    track_id: String,
) -> Result<Vec<PlaybackCandidate>, String> {
    let state = service(&app)?;
    let result = state
        .db
        .lock()
        .map_err(err)?
        .resolution_candidates(&track_id)
        .map_err(err);
    result
}
#[tauri::command]
pub fn import_choose(
    app: AppHandle,
    track_id: String,
    provider: Platform,
    external_id: String,
) -> Result<(), String> {
    let state = service(&app)?;
    state
        .db
        .lock()
        .map_err(err)?
        .verify_candidate(&track_id, provider, &external_id)
        .map_err(err)?;
    super::playback::recording_changed(&app, &track_id);
    library_changed(&app);
    Ok(())
}
#[tauri::command]
pub fn import_invalidate(
    app: AppHandle,
    track_id: String,
    provider: Platform,
) -> Result<(), String> {
    let state = service(&app)?;
    state
        .db
        .lock()
        .map_err(err)?
        .invalidate_mapping(&track_id, provider)
        .map_err(err)?;
    Ok(())
}
#[tauri::command]
pub fn import_rename(app: AppHandle, playlist_id: String, name: String) -> Result<(), String> {
    let state = service(&app)?;
    state
        .db
        .lock()
        .map_err(err)?
        .rename_import_playlist(&playlist_id, &name)
        .map_err(err)?;
    library_changed(&app);
    Ok(())
}
#[tauri::command]
pub fn import_reorder(
    app: AppHandle,
    playlist_id: String,
    entry_ids: Vec<String>,
) -> Result<(), String> {
    let state = service(&app)?;
    state
        .db
        .lock()
        .map_err(err)?
        .reorder_import_playlist(&playlist_id, &entry_ids)
        .map_err(err)?;
    library_changed(&app);
    Ok(())
}
#[tauri::command]
pub fn import_remove(app: AppHandle, playlist_id: String, entry_id: String) -> Result<(), String> {
    let state = service(&app)?;
    state
        .db
        .lock()
        .map_err(err)?
        .remove_import_entry(&playlist_id, &entry_id)
        .map_err(err)?;
    library_changed(&app);
    Ok(())
}

pub fn playlist_songs(
    app: &AppHandle,
    id: &str,
) -> Result<Option<Vec<crate::db::SongRow>>, String> {
    let Some(state) = app.try_state::<State>() else {
        return Ok(None);
    };
    let db = state.db.lock().map_err(err)?;
    if !db.playlists().map_err(err)?.iter().any(|p| p.id == id) {
        return Ok(None);
    }
    Ok(Some(
        db.import_entries(id)
            .map_err(err)?
            .into_iter()
            .map(|entry| {
                let track = entry.track;
                crate::db::SongRow {
                    id: track.id.clone(),
                    catalog_id: Some(track.id),
                    name: track.metadata.title,
                    artist_name: track.metadata.artists.join(", "),
                    album_name: track.metadata.album.unwrap_or_default(),
                    duration_ms: track.metadata.duration_ms.unwrap_or(0),
                    artwork_url: track.metadata.artwork_url,
                }
            })
            .collect(),
    ))
}

#[derive(Clone, Serialize)]
pub struct PlaybackSettings {
    pub youtube_enabled: bool,
    pub priority: Vec<Platform>,
}
#[tauri::command]
pub fn import_playback_settings(app: AppHandle) -> Result<PlaybackSettings, String> {
    let state = service(&app)?;
    let db = state.db.lock().map_err(err)?;
    Ok(PlaybackSettings {
        youtube_enabled: db.meta("youtube_native_enabled").map_err(err)?.as_deref()
            != Some("false"),
        priority: db
            .meta("import_playback_priority")
            .map_err(err)?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_else(|| vec![Platform::Local, Platform::Navidrome, Platform::Youtube]),
    })
}
#[tauri::command]
pub fn import_set_playback_settings(
    app: AppHandle,
    youtube_enabled: bool,
    priority: Vec<Platform>,
) -> Result<(), String> {
    let required = [Platform::Local, Platform::Navidrome, Platform::Youtube];
    if priority.len() != 3 || required.iter().any(|p| !priority.contains(p)) {
        return Err("Choose each supported playback source exactly once.".into());
    }
    let state = service(&app)?;
    let db = state.db.lock().map_err(err)?;
    db.set_meta(
        "youtube_native_enabled",
        if youtube_enabled { "true" } else { "false" },
    )
    .map_err(err)?;
    db.set_meta(
        "import_playback_priority",
        &serde_json::to_string(&priority).map_err(err)?,
    )
    .map_err(err)?;
    drop(db);
    if !youtube_enabled
        && super::playback::active_route(&app)
            .is_some_and(|route| route.provider == Platform::Youtube)
    {
        crate::commands::apply(&app, |player| player.pause());
    }
    Ok(())
}
#[tauri::command]
pub fn import_source_preference(
    app: AppHandle,
    track_id: String,
    provider: Option<Platform>,
) -> Result<(), String> {
    if provider
        .is_some_and(|p| !matches!(p, Platform::Local | Platform::Navidrome | Platform::Youtube))
    {
        return Err("That source is not supported for imported-track playback.".into());
    }
    service(&app)?
        .db
        .lock()
        .map_err(err)?
        .set_source_preference(&track_id, provider)
        .map_err(err)?;
    super::playback::recording_changed(&app, &track_id);
    Ok(())
}

/// Linking an existing source is explicit; a shared title is never an identity.
#[tauri::command]
pub fn import_link_source(
    app: AppHandle,
    track_id: String,
    provider: Platform,
    external_id: String,
) -> Result<(), String> {
    let app_state = app.state::<crate::AppState>();
    let selected = app_state.settings.lock().map_err(err)?.clone();
    let expected = match provider {
        Platform::Local => crate::settings::Source::Local,
        Platform::Navidrome => crate::settings::Source::Navidrome,
        _ => {
            return Err(
                "Choose a local file or Navidrome recording from its cached library.".into(),
            )
        }
    };
    if selected.source != expected {
        return Err("Select the source library before linking a recording from it.".into());
    }
    if !app_state
        .db
        .lock()
        .map_err(err)?
        .contains_song(&external_id)
        .map_err(err)?
    {
        return Err("The recording is not in the selected source's cached library.".into());
    }
    let origin = Origin {
        platform: provider,
        external_id,
        url: if provider == Platform::Navidrome {
            Some(crate::source::resolve(&selected).0.navidrome.url)
        } else {
            None
        },
    };
    service(&app)?
        .db
        .lock()
        .map_err(err)?
        .attach_playback_origin(&track_id, &origin)
        .map_err(err)
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    #[tokio::test]
    async fn matching_pipeline_overlaps_latency_with_two_searches_at_most() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let gate = ResolutionGate::new();
        let cancel = Cancellation::default();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let serial_started = std::time::Instant::now();
        for _ in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        let serial = serial_started.elapsed();
        let parallel_started = std::time::Instant::now();
        run_matches((0..6).map(|n| n.to_string()).collect(), &cancel, |id| {
            let gate = &gate;
            let cancel = &cancel;
            let active = &active;
            let peak = &peak;
            let done = &done;
            async move {
                let _guard = gate.acquire(&id, cancel).await?;
                peak.fetch_max(active.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                done.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await
        .unwrap();
        eprintln!(
            "six simulated 40ms searches: serial={}ms, bounded={}ms",
            serial.as_millis(),
            parallel_started.elapsed().as_millis()
        );
        assert_eq!(done.load(Ordering::SeqCst), 6);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancelled_worker_drains_started_searches_without_starting_later_entries() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cancel = Cancellation::default();
        let started = AtomicUsize::new(0);
        let cleaned = AtomicUsize::new(0);
        let work = run_matches((0..20).map(|n| n.to_string()).collect(), &cancel, |_| {
            let started = &started;
            let cleaned = &cleaned;
            let cancel = &cancel;
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                cancel.cancelled().await;
                // Represents resolve_one's database transition back to pending.
                tokio::task::yield_now().await;
                cleaned.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        tokio::pin!(work);
        tokio::select! {
            _ = &mut work => panic!("searches have not been cancelled yet"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => cancel.cancel(),
        }
        tokio::time::timeout(std::time::Duration::from_millis(100), work)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 3);
        assert_eq!(cleaned.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn unrelated_tracks_overlap_but_same_track_waits_and_cancels() {
        let gate = ResolutionGate::new();
        let cancel = Cancellation::default();
        let first = gate.acquire("first", &cancel).await.unwrap();
        let second = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            gate.acquire("second", &cancel),
        )
        .await
        .expect("an unrelated track must overlap the first search")
        .unwrap();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            gate.acquire("third", &cancel)
        )
        .await
        .is_err());
        drop(second);
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            gate.acquire("first", &cancel)
        )
        .await
        .is_err());
        let cancelled = Cancellation::default();
        let waiting = gate.acquire("first", &cancelled);
        tokio::pin!(waiting);
        tokio::select! {
            _ = &mut waiting => panic!("same-track search acquired a held lock"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => cancelled.cancel(),
        }
        assert!(waiting.await.unwrap_err().contains("cancelled"));
        drop(first);
        assert!(gate.acquire("first", &cancel).await.is_ok());
    }
}
