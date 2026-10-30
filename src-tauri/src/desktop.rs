//! Desktop preferences and one cancellable sleep timer, owned by Rust.
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub close_to_tray: bool,
    pub output_device: Option<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            output_device: None,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub close_to_tray: bool,
    pub output_device: Option<String>,
    pub sleep_deadline_ms: Option<u64>,
}
#[derive(Default)]
struct Timer {
    generation: u64,
    deadline: Option<u64>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Timer {
    fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.deadline = None;
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
    fn replace(
        &mut self,
        deadline: Option<u64>,
    ) -> (u64, Option<tokio::sync::oneshot::Receiver<()>>) {
        self.cancel();
        self.deadline = deadline;
        let receiver = deadline.map(|_| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.cancel = Some(tx);
            rx
        });
        (self.generation, receiver)
    }
    fn expire(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.deadline.is_none() {
            return false;
        }
        self.deadline = None;
        self.cancel = None;
        true
    }
}
struct State {
    preferences: Mutex<Preferences>,
    timer: Mutex<Timer>,
    path: Option<PathBuf>,
}
impl Drop for State {
    fn drop(&mut self) {
        self.timer.get_mut().expect("sleep timer").cancel();
    }
}
fn read_preferences(path: &Path) -> Preferences {
    if std::fs::metadata(path)
        .map(|metadata| metadata.len() >= 64 * 1024)
        .unwrap_or(true)
    {
        return Preferences::default();
    }
    std::fs::read(path)
        .ok()
        .filter(|bytes| bytes.len() < 64 * 1024)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
fn persist(state: &State, preferences: &Preferences) -> Result<(), String> {
    let path = state
        .path
        .as_ref()
        .ok_or("Desktop preferences cannot be saved without a data directory.")?;
    let parent = path.parent().ok_or("Invalid preferences directory.")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(preferences).map_err(|e| e.to_string())?;
    // Only explicit preference changes write this small file; sleep timers are transient.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, path).map_err(|e| e.to_string())
}
pub fn initialize(app: &AppHandle) {
    let path = app
        .path()
        .app_data_dir()
        .ok()
        .map(|directory| directory.join("desktop-preferences.json"));
    let preferences = path.as_deref().map(read_preferences).unwrap_or_default();
    app.manage(State {
        preferences: Mutex::new(preferences),
        timer: Mutex::new(Timer::default()),
        path,
    });
}
pub fn should_close_to_tray(app: &AppHandle) -> bool {
    app.try_state::<State>().is_none_or(|state| {
        state
            .preferences
            .lock()
            .expect("desktop preferences")
            .close_to_tray
    })
}
pub fn selected_output(app: &AppHandle) -> Option<String> {
    app.try_state::<State>().and_then(|state| {
        state
            .preferences
            .lock()
            .expect("desktop preferences")
            .output_device
            .clone()
    })
}

/// A disconnected saved device must not prevent playback after restarting.
/// Explicit device changes still fail without replacing the working sink.
pub fn open_engine(app: &AppHandle) -> Result<crate::audio::Engine, crate::audio::AudioError> {
    let selected = selected_output(app);
    match crate::audio::Engine::new_for_device(selected.as_deref()) {
        Err(_) if selected.is_some() => {
            tracing::warn!("Saved audio output unavailable; using system default");
            crate::audio::Engine::new()
        }
        result => result,
    }
}
pub fn shutdown(app: &AppHandle) {
    if let Some(state) = app.try_state::<State>() {
        state.timer.lock().expect("sleep timer").cancel();
    }
}
fn snapshot(app: &AppHandle) -> Snapshot {
    let state = app.state::<State>();
    let preferences = state
        .preferences
        .lock()
        .expect("desktop preferences")
        .clone();
    let sleep_deadline_ms = state.timer.lock().expect("sleep timer").deadline;
    Snapshot {
        close_to_tray: preferences.close_to_tray,
        output_device: preferences.output_device.clone(),
        sleep_deadline_ms,
    }
}
fn publish(app: &AppHandle) -> Snapshot {
    let snapshot = snapshot(app);
    let _ = app.emit("desktop://state", &snapshot);
    snapshot
}
#[tauri::command]
pub fn desktop_snapshot(app: AppHandle) -> Snapshot {
    snapshot(&app)
}
#[tauri::command]
pub fn desktop_preferences(app: AppHandle, close_to_tray: bool) -> Result<Snapshot, String> {
    let state = app.state::<State>();
    {
        let mut preferences = state.preferences.lock().expect("desktop preferences");
        let next = Preferences {
            close_to_tray,
            ..preferences.clone()
        };
        persist(&state, &next)?;
        *preferences = next;
    }
    Ok(publish(&app))
}
fn sleep_duration(minutes: u32) -> Result<Duration, String> {
    if !(1..=1440).contains(&minutes) {
        return Err("Choose a sleep timer from 1 to 1440 minutes.".into());
    }
    Ok(Duration::from_secs(u64::from(minutes) * 60))
}
fn resume_paused(status: crate::player::Status) -> bool {
    status != crate::player::Status::Playing
}
#[tauri::command]
pub fn desktop_sleep(app: AppHandle, minutes: Option<u32>) -> Result<Snapshot, String> {
    let duration = minutes.map(sleep_duration).transpose()?;
    let deadline = duration.map(|duration| {
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            + duration)
            .as_millis() as u64
    });
    let state = app.state::<State>();
    let (generation, receiver) = state.timer.lock().expect("sleep timer").replace(deadline);
    if let (Some(duration), Some(mut receiver)) = (duration, receiver) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::select! {
                _ = &mut receiver => {},
                _ = tokio::time::sleep(duration) => {
                    let state = app.state::<State>();
                    let mut timer = state.timer.lock().expect("sleep timer");
                    if timer.expire(generation) { crate::commands::apply(&app, |player| player.pause()); }
                    drop(timer);
                    publish(&app);
                }
            }
        });
    }
    Ok(publish(&app))
}
#[tauri::command]
pub fn desktop_devices() -> Result<Vec<String>, String> {
    crate::audio::output_devices().map_err(|e| e.to_string())
}
#[tauri::command]
pub fn desktop_output(app: AppHandle, device: Option<String>) -> Result<Snapshot, String> {
    let device = device.filter(|name| !name.is_empty());
    let desktop = app.state::<State>();
    // Provider workers acquire control -> audio -> preferences. Never query
    // resolver state while holding the audio or preferences mutex.
    if crate::youtube_native::is_resolving(&app) {
        return Err("Wait for this track to finish loading, then switch output.".into());
    }
    let state = app.state::<crate::AppState>();
    let mut audio = state.audio.lock().expect("audio mutex");
    if audio.as_ref().is_some_and(|engine| engine.is_loading()) {
        return Err("Wait for this track to finish loading, then switch output.".into());
    }
    let mut preferences = desktop.preferences.lock().expect("desktop preferences");
    let replacement = Arc::new(
        crate::audio::Engine::new_for_device(device.as_deref()).map_err(|e| e.to_string())?,
    );
    let next = Preferences {
        output_device: device,
        ..preferences.clone()
    };
    // A failed device open or preference write leaves current playback intact.
    persist(&desktop, &next)?;
    let player = state.player.lock().expect("player mutex").state().clone();
    if let Some(previous) = audio.as_ref() {
        if let Err(error) = previous.transfer_to(
            &replacement,
            player.position_ms,
            resume_paused(player.status),
            player.volume,
        ) {
            let _ = persist(&desktop, &preferences);
            return Err(error.to_string());
        }
    } else {
        replacement.set_volume(player.volume);
        replacement.pause();
    }
    *audio = Some(replacement);
    *preferences = next;
    drop(audio);
    drop(preferences);
    Ok(publish(&app))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_switch_only_resumes_a_playing_track() {
        use crate::player::Status;
        assert!(!resume_paused(Status::Playing));
        for status in [
            Status::Paused,
            Status::Idle,
            Status::Ended,
            Status::Loading,
            Status::Stalled,
        ] {
            assert!(resume_paused(status));
        }
    }
    #[test]
    fn missing_preferences_preserve_existing_close_to_tray_behavior() {
        assert_eq!(
            serde_json::from_str::<Preferences>("{}").unwrap(),
            Preferences::default()
        );
        assert!(Preferences::default().close_to_tray);
        assert!(Preferences::default().output_device.is_none());
    }
    #[test]
    fn preferences_round_trip_and_corrupt_file_falls_back() {
        let preferences = Preferences {
            close_to_tray: false,
            output_device: Some("Headphones".into()),
        };
        assert_eq!(
            serde_json::from_slice::<Preferences>(&serde_json::to_vec(&preferences).unwrap())
                .unwrap(),
            preferences
        );
        let path =
            std::env::temp_dir().join(format!("capsule-desktop-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"broken").unwrap();
        assert_eq!(read_preferences(&path), Preferences::default());
        let state = State {
            preferences: Mutex::new(Preferences::default()),
            timer: Mutex::new(Timer::default()),
            path: Some(path.clone()),
        };
        persist(&state, &preferences).unwrap();
        assert_eq!(read_preferences(&path), preferences);
        persist(&state, &Preferences::default()).unwrap();
        assert_eq!(read_preferences(&path), Preferences::default());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn timer_replacement_and_cancel_invalidate_stale_expiry() {
        let mut timer = Timer::default();
        let (first, mut canceled) = timer.replace(Some(100));
        let (second, _) = timer.replace(Some(200));
        assert_eq!(canceled.as_mut().unwrap().try_recv(), Ok(()));
        assert!(!timer.expire(first));
        assert_eq!(timer.deadline, Some(200));
        timer.cancel();
        assert!(!timer.expire(second));
        assert_eq!(timer.deadline, None);
    }
    #[test]
    fn timer_fires_only_once_and_bounds_are_enforced() {
        let mut timer = Timer::default();
        let (generation, _) = timer.replace(Some(100));
        assert!(timer.expire(generation));
        assert!(!timer.expire(generation));
        assert!(sleep_duration(0).is_err());
        assert!(sleep_duration(1441).is_err());
        assert_eq!(sleep_duration(15).unwrap(), Duration::from_secs(900));
    }
}
