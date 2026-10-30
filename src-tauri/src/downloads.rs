mod paths;
use crate::{
    db::SongRow,
    import::{http::Cancellation, model::Platform},
    settings::Source,
    AppState,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_JOBS: usize = 500;
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadRequest {
    Canonical {
        track_id: String,
    },
    Soundcloud {
        track: crate::soundcloud::Track,
    },
    Library {
        song: SongRow,
        source: Source,
        context: Option<String>,
    },
}
#[derive(Clone, Serialize)]
pub struct DownloadJob {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub source: String,
    pub status: String,
    pub bytes: u64,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
    pub path: Option<String>,
    pub format: Option<String>,
    pub directory: String,
}
#[derive(Clone)]
struct Entry {
    job: DownloadJob,
    input: DownloadRequest,
    cancel: Cancellation,
    attempts: u8,
}
pub struct State {
    jobs: Mutex<Vec<Entry>>,
    slots: Arc<tokio::sync::Semaphore>,
    resolver: tokio::sync::OnceCell<Arc<crate::youtube_native::resolver::Resolver>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            jobs: Mutex::default(),
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
            resolver: tokio::sync::OnceCell::new(),
        }
    }
}
fn jobs(app: &AppHandle) -> Vec<DownloadJob> {
    app.state::<State>()
        .jobs
        .lock()
        .expect("downloads")
        .iter()
        .map(|e| e.job.clone())
        .collect()
}
fn update(app: &AppHandle, id: &str, f: impl FnOnce(&mut DownloadJob)) {
    if let Some(e) = app
        .state::<State>()
        .jobs
        .lock()
        .expect("downloads")
        .iter_mut()
        .find(|e| e.job.id == id)
    {
        f(&mut e.job);
    }
    let _ = app.emit("downloads://changed", jobs(app));
}
fn directory(raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw);
    if !path.is_absolute() || !path.is_dir() {
        return Err("Choose an existing absolute download folder.".into());
    }
    path.canonicalize()
        .map_err(|_| "The selected download folder is unavailable.".into())
}
#[tauri::command]
pub fn downloads_snapshot(app: AppHandle) -> Vec<DownloadJob> {
    jobs(&app)
}
#[tauri::command]
pub fn downloads_directory(app: AppHandle) -> Option<String> {
    let core = app.state::<AppState>();
    let base = core.data_dir.lock().ok()?.clone()?;
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(base.join("downloads-settings.json")).ok()?).ok()?;
    value["directory"]
        .as_str()
        .filter(|p| Path::new(p).is_dir())
        .map(str::to_owned)
}
#[tauri::command]
pub fn downloads_start(
    app: AppHandle,
    requests: Vec<DownloadRequest>,
    directory: String,
) -> Result<Vec<DownloadJob>, String> {
    let target = self::directory(&directory)?;
    if requests.is_empty() || requests.len() > 500 {
        return Err("Choose between 1 and 500 tracks per download batch.".into());
    }
    let state = app.state::<State>();
    let mut entries = state.jobs.lock().expect("downloads");
    if entries.len() + requests.len() > MAX_JOBS {
        return Err(
            "Download history is full for this session. Restart the app to clear it.".into(),
        );
    }
    let mut ids = Vec::new();
    let mut pending = Vec::new();
    for input in requests {
        let (title, artist) = match &input {
            DownloadRequest::Soundcloud { track } => (track.title.clone(), track.artist.clone()),
            DownloadRequest::Library { song, .. } => (song.name.clone(), song.artist_name.clone()),
            DownloadRequest::Canonical { track_id } => {
                let imports = app.state::<crate::import::State>();
                let track = imports
                    .db
                    .lock()
                    .expect("import db")
                    .canonical_track(track_id)
                    .map_err(|_| "Could not read library track.")?
                    .ok_or("Track is no longer in the library.")?;
                (track.metadata.title, track.metadata.artists.join(", "))
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        pending.push(Entry {
            job: DownloadJob {
                id: id.clone(),
                title,
                artist,
                source: String::new(),
                status: "queued".into(),
                bytes: 0,
                total_bytes: None,
                error: None,
                path: None,
                format: None,
                directory: target.to_string_lossy().into_owned(),
            },
            input,
            cancel: Cancellation::default(),
            attempts: 1,
        });
        ids.push(id);
    }
    entries.extend(pending);
    drop(entries);
    if let Some(base) = app
        .state::<AppState>()
        .data_dir
        .lock()
        .expect("data dir")
        .as_ref()
    {
        // Only the user-selected folder is persisted; provider addresses never are.
        let content =
            serde_json::to_vec(&serde_json::json!({"directory": target.to_string_lossy()}))
                .unwrap();
        let _ = std::fs::write(base.join("downloads-settings.json"), content);
    }
    for id in ids {
        launch(app.clone(), id);
    }
    Ok(jobs(&app))
}
#[tauri::command]
pub fn downloads_cancel(app: AppHandle, id: String) -> Result<(), String> {
    let state = app.state::<State>();
    let mut entries = state.jobs.lock().expect("downloads");
    let entry = entries
        .iter_mut()
        .find(|e| e.job.id == id)
        .ok_or("Unknown download.")?;
    if matches!(
        entry.job.status.as_str(),
        "queued" | "resolving" | "downloading"
    ) {
        entry.cancel.cancel();
        entry.job.status = "cancelled".into();
        entry.job.error = Some("Cancelling…".into());
    }
    drop(entries);
    let _ = app.emit("downloads://changed", jobs(&app));
    Ok(())
}
#[tauri::command]
pub fn downloads_retry(app: AppHandle, id: String) -> Result<(), String> {
    let state = app.state::<State>();
    let mut entries = state.jobs.lock().expect("downloads");
    let entry = entries
        .iter_mut()
        .find(|e| e.job.id == id)
        .ok_or("Unknown download.")?;
    // Cancelling a running job does not make its worker finished; retry waits for cleanup.
    if entry.job.status != "failed" && entry.job.status != "cancelled" {
        return Err("Only finished or cancelled downloads can be retried.".into());
    }
    if entry.attempts >= 4 {
        return Err("Retry limit reached. Start a new download batch.".into());
    }
    if entry.job.error.as_deref() == Some("Cancelling…") {
        return Err("Wait for download cleanup before retrying.".into());
    }
    entry.attempts += 1;
    entry.cancel = Cancellation::default();
    entry.job.status = "queued".into();
    entry.job.error = None;
    entry.job.bytes = 0;
    entry.job.total_bytes = None;
    drop(entries);
    let _ = app.emit("downloads://changed", jobs(&app));
    launch(app.clone(), id);
    Ok(())
}
#[tauri::command]
pub fn downloads_reveal(app: AppHandle, id: String) -> Result<(), String> {
    let path = jobs(&app)
        .into_iter()
        .find(|j| j.id == id && j.status == "completed")
        .and_then(|j| j.path)
        .ok_or("No completed download to show.")?;
    if !Path::new(&path).is_file() {
        return Err("Downloaded file has moved or been removed.".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe")
            .arg(format!("/select,{path}"))
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| "Could not show download in Explorer.")?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(Path::new(&path).parent().ok_or("Invalid download path.")?)
            .spawn()
            .map_err(|_| "Could not open the download folder.")?;
    }
    Ok(())
}
struct Resolved {
    provider: Platform,
    external: String,
    title: String,
    artist: String,
}
fn resolve_input(app: &AppHandle, input: &DownloadRequest) -> Result<Resolved, String> {
    let core = app.state::<AppState>();
    match input {
        DownloadRequest::Soundcloud { track } => Ok(Resolved {
            provider: Platform::Soundcloud,
            external: track.id.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
        }),
        DownloadRequest::Library {
            song,
            source,
            context,
        } => {
            if core.settings.lock().expect("settings").source != *source {
                return Err(
                    "The library source changed. Select tracks from the current library.".into(),
                );
            }
            let saved = core
                .db
                .lock()
                .expect("db")
                .song_by_id(&song.id)
                .map_err(|_| "Could not read library.")?
                .ok_or("Track is no longer in this library.")?;
            if *source == Source::Navidrome {
                let settings = core.settings.lock().expect("settings");
                let expected = format!(
                    "{}\n{}",
                    settings.navidrome.url.trim_end_matches('/'),
                    settings.navidrome.username
                );
                if context.as_deref() != Some(expected.as_str()) {
                    return Err("This track belongs to another Navidrome server or account. Reload the current library first.".into());
                }
            }
            let provider = match source {
                Source::Youtube => Platform::Youtube,
                Source::Soundcloud => Platform::Soundcloud,
                Source::Local => Platform::Local,
                Source::Navidrome => Platform::Navidrome,
                _ => return Err("This library source does not provide downloadable audio.".into()),
            };
            let external = if provider == Platform::Youtube {
                saved
                    .catalog_id
                    .ok_or("No YouTube recording is linked to this track.")?
            } else {
                saved.id
            };
            Ok(Resolved {
                provider,
                external,
                title: saved.name,
                artist: saved.artist_name,
            })
        }
        DownloadRequest::Canonical { track_id } => {
            let imports = app.state::<crate::import::State>();
            let mut db = imports.db.lock().expect("import db");
            db.auto_verify_cached_candidate(track_id)
                .map_err(|_| "Could not check the recording match.")?;
            let track = db
                .canonical_track(track_id)
                .map_err(|_| "Could not read track.")?
                .ok_or("Track is no longer in the library.")?;
            let preference = db
                .source_preference(track_id)
                .map_err(|_| "Could not read source selection.")?;
            let mappings = db
                .mappings_for(track_id)
                .map_err(|_| "Could not read recording links.")?;
            let selected = core.settings.lock().expect("settings").clone();
            let configured_server = crate::source::resolve(&selected)
                .0
                .navidrome
                .url
                .trim_end_matches('/')
                .to_string();
            let mut choices: Vec<(Platform, String)> = track
                .origins
                .iter()
                .filter(|origin| {
                    origin.platform != Platform::Navidrome
                        || (!configured_server.is_empty()
                            && origin
                                .url
                                .as_deref()
                                .is_some_and(|url| url.trim_end_matches('/') == configured_server))
                })
                .filter(|o| {
                    matches!(
                        o.platform,
                        Platform::Youtube
                            | Platform::Soundcloud
                            | Platform::Local
                            | Platform::Navidrome
                    )
                })
                .map(|o| (o.platform, o.external_id.clone()))
                .collect();
            choices.extend(mappings.into_iter().filter(|m| m.verified).filter_map(|m| {
                match m.provider.as_str() {
                    "youtube" => Some((Platform::Youtube, m.external_id)),
                    "soundcloud" => Some((Platform::Soundcloud, m.external_id)),
                    _ => None,
                }
            }));
            for provider in [
                Platform::Local,
                Platform::Navidrome,
                Platform::Soundcloud,
                Platform::Youtube,
            ] {
                if preference.is_some_and(|p| p != provider) {
                    continue;
                }
                if let Some((_, external)) = choices.iter().find(|(p, id)| {
                    *p == provider
                        && match p {
                            Platform::Local => Path::new(id).is_file(),
                            Platform::Navidrome => {
                                core.navidrome.lock().expect("navidrome").is_some()
                            }
                            _ => true,
                        }
                }) {
                    return Ok(Resolved {
                        provider,
                        external: external.clone(),
                        title: track.metadata.title,
                        artist: track.metadata.artists.join(", "),
                    });
                }
            }
            Err(
                "No available recording is linked to this track. Choose a playback source first."
                    .into(),
            )
        }
    }
}
fn launch(app: AppHandle, id: String) {
    tauri::async_runtime::spawn(async move {
        let (entry, slots) = {
            let state = app.state::<State>();
            let entry = state
                .jobs
                .lock()
                .expect("downloads")
                .iter()
                .find(|e| e.job.id == id)
                .cloned();
            (entry, state.slots.clone())
        };
        let Some(entry) = entry else {
            return;
        };
        let _permit = tokio::select! { permit = slots.acquire_owned() => match permit { Ok(p) => p, Err(_) => return }, _ = entry.cancel.cancelled() => { update(&app, &id, |j| { j.status = "cancelled".into(); j.error = None; }); return; } };
        let target = match directory(&entry.job.directory) {
            Ok(d) => d,
            Err(e) => {
                update(&app, &id, |j| {
                    j.status = "failed".into();
                    j.error = Some(e);
                });
                return;
            }
        };
        let part = target.join(format!(".capsule-{id}-{}.part", entry.attempts));
        let result = execute(&app, &id, &entry, &target, &part).await;
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(part.with_extension("done"));
        update(&app, &id, |j| match result {
            Ok(path) => {
                j.status = "completed".into();
                j.path = Some(path.to_string_lossy().into_owned());
                j.error = None;
            }
            Err(e) => {
                j.status = if entry.cancel.is_cancelled() {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                j.error = if entry.cancel.is_cancelled() {
                    None
                } else {
                    Some(e)
                };
            }
        });
    });
}
async fn execute(
    app: &AppHandle,
    id: &str,
    entry: &Entry,
    target: &Path,
    part: &Path,
) -> Result<PathBuf, String> {
    if entry.cancel.is_cancelled() {
        return Err("Download cancelled.".into());
    }
    update(app, id, |j| j.status = "resolving".into());
    let resolved = resolve_input(app, &entry.input)?;
    update(app, id, |j| {
        j.title = resolved.title.clone();
        j.artist = resolved.artist.clone();
        j.source = resolved.provider.as_str().into();
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(part)
        .map_err(|_| "Could not create a partial file in the selected folder.")?;
    let mut extension;
    if resolved.provider == Platform::Local {
        let source = Path::new(&resolved.external)
            .canonicalize()
            .map_err(|_| "Local recording is unavailable.")?;
        if !crate::local::is_audio(&source) {
            return Err("This local file is not a supported audio recording.".into());
        }
        let mut input = File::open(&source).map_err(|_| "Could not open local recording.")?;
        let size = input
            .metadata()
            .map_err(|_| "Could not read local recording size.")?
            .len();
        check_size(size)?;
        extension = source
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("audio")
            .to_ascii_lowercase();
        update(app, id, |j| {
            j.status = "downloading".into();
            j.total_bytes = Some(size);
            j.format = Some(extension.clone());
        });
        let mut bytes = 0;
        let mut last_progress = std::time::Instant::now();
        let mut buffer = vec![0; 128 * 1024];
        loop {
            if entry.cancel.is_cancelled() {
                return Err("Download cancelled.".into());
            }
            let n = input
                .read(&mut buffer)
                .map_err(|_| "Could not read local audio.")?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > size {
                return Err("Local recording changed while copying.".into());
            }
            file.write_all(&buffer[..n])
                .map_err(|_| "Could not save audio. Check free disk space.")?;
            if last_progress.elapsed() >= Duration::from_millis(200) {
                update(app, id, |j| j.bytes = bytes);
                last_progress = std::time::Instant::now();
            }
        }
        if bytes != size {
            return Err("Local recording changed while copying.".into());
        }
        update(app, id, |job| job.bytes = bytes);
    } else {
        drop(file);
        let (url, ext) = resolve_url(app, &resolved, &entry.cancel).await?;
        extension = ext;
        update(app, id, |j| {
            j.status = "downloading".into();
            j.format = Some(extension.clone());
        });
        if resolved.provider == Platform::Youtube {
            download_buffered(app, id, url, part.to_owned(), &entry.cancel).await?;
        } else if crate::soundcloud::hls::is_manifest_url(&url) {
            let shared = Arc::new((Mutex::new(crate::stream::CacheState::new()), Condvar::new()));
            let cancel = crate::stream::Cancel::default();
            let transfer = crate::soundcloud::hls::download(&url, part, &shared, &cancel);
            tokio::pin!(transfer);
            let result = loop {
                tokio::select! {
                    r = &mut transfer => break r,
                    _ = entry.cancel.cancelled() => { cancel.stop(); break Err("Download cancelled.".into()); },
                    _ = tokio::time::sleep(Duration::from_millis(250)) => { let bytes = shared.0.lock().expect("download progress").available(); update(app, id, |j| j.bytes = bytes); if bytes > MAX_BYTES { cancel.stop(); break Err("Recording exceeds the 2 GB download limit.".into()); } }
                }
            };
            result?;
            let bytes = std::fs::metadata(part)
                .map_err(|_| "Could not read downloaded file.")?
                .len();
            check_size(bytes)?;
            update(app, id, |j| {
                j.bytes = bytes;
                j.total_bytes = Some(bytes);
            });
        } else {
            transfer(app, id, &url, part, &entry.cancel).await?;
        }
        extension = detect_format(part)?;
        update(app, id, |j| j.format = Some(extension.clone()));
        file = OpenOptions::new()
            .write(true)
            .open(part)
            .map_err(|_| "Could not open downloaded audio.")?;
    }
    file.sync_all()
        .map_err(|_| "Could not flush downloaded audio. Check free disk space.")?;
    drop(file);
    if entry.cancel.is_cancelled() {
        return Err("Download cancelled.".into());
    }
    // Cancellation and publication share this fence: a cancelled job never
    // publishes a partial file, and a completed publication cannot be cancelled.
    let state = app.state::<State>();
    let mut entries = state.jobs.lock().expect("downloads");
    let current = entries
        .iter_mut()
        .find(|job| job.job.id == id && job.attempts == entry.attempts)
        .ok_or("Download superseded.")?;
    if entry.cancel.is_cancelled() {
        return Err("Download cancelled.".into());
    }
    let path = paths::publish(
        part,
        target,
        &format!("{} - {}", resolved.artist, resolved.title),
        &extension,
    )?;
    current.job.status = "completed".into();
    current.job.path = Some(path.to_string_lossy().into_owned());
    drop(entries);
    link_offline(app, &entry.input, &path);
    Ok(path)
}

fn link_offline(app: &AppHandle, input: &DownloadRequest, path: &Path) {
    use crate::import::model::{ImportedTrack, Metadata, Origin};
    let imports = app.state::<crate::import::State>();
    let mut db = imports.db.lock().expect("import db");
    let track_id = match input {
        DownloadRequest::Canonical { track_id } => Some(track_id.clone()),
        DownloadRequest::Soundcloud { track } => db
            .save_source_track(&ImportedTrack {
                origin: Origin {
                    platform: Platform::Soundcloud,
                    external_id: track.id.clone(),
                    url: track.permalink_url.clone(),
                },
                metadata: Metadata {
                    title: track.title.clone(),
                    artists: vec![track.artist.clone()],
                    album: track.album.clone(),
                    duration_ms: Some(track.duration_ms),
                    artwork_url: track.artwork_url.clone(),
                    isrc: None,
                    explicit: None,
                },
            })
            .ok()
            .map(|track| track.id),
        _ => None,
    };
    if let Some(id) = track_id {
        let _ = db.attach_playback_origin(
            &id,
            &Origin {
                platform: Platform::Local,
                external_id: path.to_string_lossy().into_owned(),
                url: None,
            },
        );
    }
    drop(db);
    let _ = app.emit("library://updated", crate::sync::counts(app));
}

async fn download_buffered(
    app: &AppHandle,
    id: &str,
    url: String,
    path: PathBuf,
    cancel: &Cancellation,
) -> Result<(), String> {
    let shared = Arc::new((Mutex::new(crate::stream::CacheState::new()), Condvar::new()));
    let transfer = crate::stream::Cancel::default();
    crate::stream::spawn_fetch(url, path, shared.clone(), 0, transfer.clone())
        .map_err(|_| "Could not start the audio download.")?;
    loop {
        let (bytes, total, complete, error) = {
            let state = shared.0.lock().expect("download progress");
            (
                state.available(),
                state.total(),
                state.is_complete(),
                state.error().map(str::to_owned),
            )
        };
        update(app, id, |job| {
            job.bytes = bytes;
            job.total_bytes = total;
        });
        if let Some(error) = error {
            transfer.stop();
            return Err(error);
        }
        if bytes > MAX_BYTES || total.is_some_and(|total| total > MAX_BYTES) {
            transfer.stop();
            return Err("Recording exceeds the 2 GB download limit.".into());
        }
        if cancel.is_cancelled() {
            transfer.stop();
            return Err("Download cancelled.".into());
        }
        if complete {
            check_size(bytes)?;
            return Ok(());
        }
        tokio::select! { _ = cancel.cancelled() => { transfer.stop(); return Err("Download cancelled.".into()); }, _ = tokio::time::sleep(Duration::from_millis(250)) => {} }
    }
}
fn check_size(size: u64) -> Result<(), String> {
    if size == 0 || size > MAX_BYTES {
        Err("Recording is empty or exceeds the 2 GB download limit.".into())
    } else {
        Ok(())
    }
}
async fn resolve_url(
    app: &AppHandle,
    resolved: &Resolved,
    cancel: &Cancellation,
) -> Result<(String, String), String> {
    match resolved.provider {
        Platform::Soundcloud => {
            let state = app.state::<crate::soundcloud::State>();
            let playable = state.0.playable(&resolved.external, cancel).await?;
            let ext = match playable.audio_info.codec.as_deref() {
                Some("AAC") if playable.audio_info.container.as_deref() == Some("MP4/M4A") => "m4a",
                Some("AAC") => "aac",
                Some("MP3") => "mp3",
                _ => "",
            };
            Ok((playable.url, ext.into()))
        }
        Platform::Youtube => {
            let base = app
                .state::<AppState>()
                .data_dir
                .lock()
                .expect("data dir")
                .clone()
                .ok_or("App storage is unavailable.")?;
            let state = app.state::<State>();
            let resolver = state
                .resolver
                .get_or_try_init(|| async {
                    crate::youtube_native::resolver::Resolver::public(&base.join("youtube-session"))
                        .map(Arc::new)
                })
                .await?;
            let result = tokio::select! { r = resolver.resolve_recovering(&resolved.external) => r?, _ = cancel.cancelled() => return Err("Download cancelled.".into()) };
            Ok((result.url, "m4a".into()))
        }
        Platform::Navidrome => {
            let client = app
                .state::<AppState>()
                .navidrome
                .lock()
                .expect("navidrome")
                .clone()
                .ok_or("Connect to Navidrome before downloading.")?;
            Ok((
                client.signed_url("download", &[("id", resolved.external.clone())]),
                String::new(),
            ))
        }
        _ => Err("No downloadable audio source is available.".into()),
    }
}
async fn transfer(
    app: &AppHandle,
    id: &str,
    url: &str,
    path: &Path,
    cancel: &Cancellation,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "Could not initialize audio download.")?;
    let mut response = tokio::select! { r = client.get(url).send() => r.map_err(|_| "Audio connection failed. Retry this download.")?, _ = cancel.cancelled() => return Err("Download cancelled.".into()) };
    if !response.status().is_success() {
        return Err(
            "The source rejected this download. Retry to refresh the recording address.".into(),
        );
    }
    let total = response.content_length();
    if let Some(size) = total {
        check_size(size)?;
    }
    update(app, id, |j| j.total_bytes = total);
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|_| "Could not open partial download.")?;
    let mut bytes = 0u64;
    let mut last = std::time::Instant::now();
    loop {
        let chunk = tokio::select! { r = response.chunk() => r.map_err(|_| "Audio transfer interrupted. Retry this download.")?, _ = cancel.cancelled() => return Err("Download cancelled.".into()) };
        let Some(chunk) = chunk else {
            break;
        };
        bytes = bytes.saturating_add(chunk.len() as u64);
        if bytes > MAX_BYTES || total.is_some_and(|t| bytes > t) {
            return Err("Audio exceeded its declared size or the 2 GB limit.".into());
        }
        file.write_all(&chunk)
            .map_err(|_| "Could not save audio. Check free disk space.")?;
        if last.elapsed() >= Duration::from_millis(200) {
            update(app, id, |j| j.bytes = bytes);
            last = std::time::Instant::now();
        }
    }
    check_size(bytes)?;
    if total.is_some_and(|size| size != bytes) {
        return Err("Audio download ended early. Retry this download.".into());
    }
    file.sync_all()
        .map_err(|_| "Could not flush downloaded audio.")?;
    update(app, id, |j| {
        j.bytes = bytes;
        j.total_bytes = Some(bytes);
    });
    Ok(())
}

fn detect_format(path: &Path) -> Result<String, String> {
    let mut header = [0u8; 16];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut header))
        .map_err(|_| "Could not verify downloaded audio.")?;
    let ext = if n >= 4 && &header[..4] == b"fLaC" {
        "flac"
    } else if n >= 4 && &header[..4] == b"OggS" {
        "ogg"
    } else if n >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WAVE" {
        "wav"
    } else if n >= 8 && &header[4..8] == b"ftyp" {
        "m4a"
    } else if n >= 3 && &header[..3] == b"ID3" {
        "mp3"
    } else if n >= 2 && header[0] == 0xff && header[1] & 0xf6 == 0xf0 {
        "aac"
    } else if n >= 2 && header[0] == 0xff && header[1] & 0xe0 == 0xe0 {
        "mp3"
    } else {
        return Err("The source did not return a supported original audio file.".into());
    };
    Ok(ext.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_audio_formats_are_identified_from_content() {
        let directory =
            std::env::temp_dir().join(format!("capsule-download-format-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("untrusted.part");
        for (header, expected) in [
            (b"fLaCmetadata".as_slice(), "flac"),
            (b"RIFF1234WAVE".as_slice(), "wav"),
            (b"1234ftypM4A ".as_slice(), "m4a"),
            (b"ID3metadata".as_slice(), "mp3"),
        ] {
            std::fs::write(&path, header).unwrap();
            assert_eq!(detect_format(&path).unwrap(), expected);
        }
        std::fs::write(&path, b"<html>error</html>").unwrap();
        assert!(detect_format(&path).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires public YouTube access; downloads a temporary original audio file"]
    async fn original_youtube_download_is_complete_and_decodes_offline() {
        let directory =
            std::env::temp_dir().join(format!("capsule-download-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let resolver = crate::youtube_native::resolver::Resolver::public(&directory).unwrap();
        let stream = resolver.resolve_recovering("fn0EIOnK-D0").await.unwrap();
        let path = directory.join("recording.part");
        let shared = Arc::new((Mutex::new(crate::stream::CacheState::new()), Condvar::new()));
        let cancel = crate::stream::Cancel::default();
        crate::stream::spawn_fetch(stream.url, path.clone(), shared.clone(), 0, cancel.clone())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                {
                    let progress = shared.0.lock().unwrap();
                    assert!(progress.error().is_none(), "{:?}", progress.error());
                    if progress.is_complete() {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("whole original recording downloads");
        assert!(std::fs::metadata(&path).unwrap().len() > 1024 * 1024);
        assert_eq!(detect_format(&path).unwrap(), "m4a");
        let saved = paths::publish(&path, &directory, "Artist - Recording", "m4a").unwrap();
        let decoder =
            rodio::Decoder::new(std::io::BufReader::new(File::open(&saved).unwrap())).unwrap();
        assert_eq!(decoder.take(48_000).count(), 48_000);
        println!(
            "Full YouTube original download and offline file decoding passed ({} bytes).",
            std::fs::metadata(saved).unwrap().len()
        );
        cancel.stop();
        let _ = std::fs::remove_dir_all(directory);
    }
}
