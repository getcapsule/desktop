
use std::sync::Mutex;
use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tauri::{AppHandle, Manager};

use crate::player::{PlayerState, Status};
use crate::AppState;

struct Controls(MediaControls);
#[cfg(target_os = "windows")]
unsafe impl Send for Controls {}

#[derive(Default)]
pub struct Handle {
    inner: Mutex<Option<Controls>>,
}

impl Handle {
    pub fn init(&self, app: &AppHandle) {
        #[cfg(target_os = "windows")]
        let hwnd = {
            let Some(win) = app.get_webview_window("main") else {
                return;
            };
            match win.hwnd() {
                Ok(h) => Some(h.0),
                Err(_) => {
                    tracing::warn!("no HWND; media controls unavailable");
                    return;
                }
            }
        };
        #[cfg(not(target_os = "windows"))]
        let hwnd: Option<*mut std::ffi::c_void> = None;

        let config =
            PlatformConfig { dbus_name: "capsule", display_name: "capsule", hwnd };

        let mut controls = match MediaControls::new(config) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = ?e, "media controls init failed");
                return;
            }
        };

        let handle = app.clone();
        if let Err(e) = controls.attach(move |event| on_event(&handle, event)) {
            tracing::warn!(error = ?e, "media controls attach failed");
            return;
        }

        let mut guard = self.inner.lock().expect("media controls mutex");
        if let Some(Controls(old)) = guard.as_mut() {
            let _ = old.detach();
        }
        *guard = Some(Controls(controls));
        tracing::info!("media controls attached");
    }

    fn apply(&self, m: &Applied) {
        let mut guard = self.inner.lock().expect("media controls mutex");
        let Some(Controls(controls)) = guard.as_mut() else {
            return;
        };

        let _ = controls.set_metadata(MediaMetadata {
            title: Some(&m.title),
            artist: Some(&m.artist),
            album: Some(&m.album),
            cover_url: m.cover.as_deref(),
            duration: (m.duration_ms > 0).then(|| Duration::from_millis(m.duration_ms)),
        });

        let progress = Some(MediaPosition(Duration::from_millis(m.position_ms)));
        let playback = match m.status {
            Status::Playing | Status::Loading | Status::Stalled => {
                MediaPlayback::Playing { progress }
            }
            Status::Paused => MediaPlayback::Paused { progress },
            Status::Idle | Status::Ended => MediaPlayback::Stopped,
        };
        let _ = controls.set_playback(playback);
    }
}

struct Applied {
    title: String,
    artist: String,
    album: String,
    cover: Option<String>,
    duration_ms: u64,
    position_ms: u64,
    status: Status,
}

pub fn update(app: &AppHandle, snapshot: &PlayerState) {
    let track = snapshot.current();
    let applied = Applied {
        title: track.map(|t| t.title.clone()).unwrap_or_default(),
        artist: track.map(|t| t.artist.clone()).unwrap_or_default(),
        album: track.map(|t| t.album.clone()).unwrap_or_default(),
        cover: track.and_then(|t| cover_url(app, &t.id)),
        duration_ms: track.map(|t| t.duration_ms).unwrap_or(0),
        position_ms: snapshot.position_ms,
        status: snapshot.status,
    };

    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        app.state::<AppState>().media_controls.apply(&applied);
    });
}

fn cover_url(app: &AppHandle, track_id: &str) -> Option<String> {
    let state = app.state::<AppState>();
    let db = state.db.lock().expect("db mutex");
    let art = db.artwork_for(track_id).ok()??;
    Some(crate::artwork::resolve_template(&art.template, art.clamp(300)))
}

fn on_event(app: &AppHandle, event: MediaControlEvent) {
    use crate::commands::apply;
    match event {
        MediaControlEvent::Play => apply(app, |p| p.play()),
        MediaControlEvent::Pause => apply(app, |p| p.pause()),
        MediaControlEvent::Toggle => apply(app, |p| p.toggle()),
        MediaControlEvent::Next => apply(app, |p| p.next_track()),
        MediaControlEvent::Previous => apply(app, |p| p.previous_track()),
        MediaControlEvent::Stop => apply(app, |p| p.pause()),
        other => tracing::debug!(?other, "unhandled media control event"),
    }
}
