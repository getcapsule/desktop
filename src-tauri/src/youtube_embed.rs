use crate::{
    player::{EngineCommand, Repeat, Status},
    AppState,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

pub const SCHEME: &str = "youtube-embed";
const PREFIX: &str = "youtube-embed-";
const CSP: &str = "default-src 'none'; script-src 'self' https://www.youtube.com https://s.ytimg.com; style-src 'unsafe-inline'; connect-src 'self' ipc: http://ipc.localhost https://ipc.localhost; frame-src https://www.youtube.com; img-src 'self' data:; base-uri 'none'; form-action 'none'; object-src 'none'";

#[derive(Default)]
pub struct State {
    generation: AtomicU64,
    host: Mutex<Option<Host>>,
}

struct Host {
    instance: u64,
    canonical_id: String,
    video_id: String,
    ready: bool,
    ended: bool,
    pending: Vec<Value>,
}

impl Host {
    fn accepts(&self, label: &str, event: &Event) -> bool {
        label == format!("{PREFIX}{}", self.instance)
            && event.instance == self.instance
            && (event.kind == "ready"
                || event.kind == "api-error"
                || event.video_id.as_deref() == Some(self.video_id.as_str()))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    instance: u64,
    kind: String,
    video_id: Option<String>,
    state: Option<i64>,
    position_ms: Option<u64>,
    duration_ms: Option<u64>,
    error_code: Option<u16>,
}

fn valid_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

pub fn resource(request: &http::Request<Vec<u8>>) -> http::Response<Vec<u8>> {
    let (status, mime, body) = match request.uri().path() {
        "/" | "/index.html" => (
            200,
            "text/html; charset=utf-8",
            include_str!("../resources/youtube-embed.html"),
        ),
        "/player.js" => (
            200,
            "application/javascript; charset=utf-8",
            include_str!("../resources/youtube-embed.js"),
        ),
        _ => (404, "text/plain; charset=utf-8", "Not found"),
    };
    http::Response::builder().status(status)
        .header("Content-Type", mime)
        .header("Content-Security-Policy", CSP)
        .header("Referrer-Policy", "strict-origin-when-cross-origin")
        .header("Permissions-Policy", "autoplay=(self \"https://www.youtube.com\"), fullscreen=(self \"https://www.youtube.com\")")
        .header("Cache-Control", "no-store")
        .body(body.as_bytes().to_vec()).expect("static response")
}

fn error_message(code: Option<u16>) -> &'static str {
    match code {
        Some(2) => "YouTube rejected this video ID (2).",
        Some(5) => "YouTube could not play this video in the embedded player (5).",
        Some(100) => "This YouTube video is unavailable or private (100).",
        Some(101 | 150) => "The video owner does not allow this video to be embedded (101/150).",
        Some(153) => "YouTube rejected the HTTP referrer or client identification (153). Playback from this experimental local host is unavailable.",
        _ => "The experimental YouTube player could not play this video.",
    }
}

fn eval(app: &AppHandle, instance: u64, command: &Value) -> Result<(), String> {
    let window = app
        .get_webview_window(&format!("{PREFIX}{instance}"))
        .ok_or("The experimental YouTube player is closed.")?;
    window
        .eval(format!(
            "window.__capsuleYoutubeEmbedCommand && window.__capsuleYoutubeEmbedCommand({command})"
        ))
        .map_err(|_| "Could not send a command to the experimental YouTube player.".into())
}

fn launch(app: &AppHandle, instance: u64) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let active = || {
            app.state::<State>()
                .host
                .lock()
                .expect("youtube embed host")
                .as_ref()
                .is_some_and(|host| host.instance == instance)
        };
        if !active() {
            return;
        }
        let built = WebviewWindowBuilder::new(
            &app,
            format!("{PREFIX}{instance}"),
            WebviewUrl::CustomProtocol(
                format!("{SCHEME}://localhost/index.html")
                    .parse()
                    .expect("static URL"),
            ),
        )
        .title("capsule · Experimental YouTube player")
        .inner_size(800.0, 520.0)
        .min_inner_size(500.0, 360.0)
        .visible(true)
        .skip_taskbar(false)
        .initialization_script(format!("window.__capsuleYoutubeEmbedInstance={instance};"))
        .on_navigation(|url| {
            (url.scheme() == SCHEME && url.host_str() == Some("localhost"))
                || (matches!(url.scheme(), "http" | "https")
                    && url.host_str() == Some("youtube-embed.localhost"))
        })
        .build();
        match built {
            Ok(window) => {
                if !active() {
                    let _ = window.destroy();
                    return;
                }
                let handle = app.clone();
                window.on_window_event(move |event| {
                    if !matches!(event, tauri::WindowEvent::Destroyed) {
                        return;
                    }
                    let state = handle.state::<State>();
                    let mut host = state.host.lock().expect("youtube embed host");
                    if host.as_ref().is_none_or(|host| host.instance != instance) {
                        return;
                    }
                    let canonical = host.take().map(|host| host.canonical_id);
                    drop(host);
                    let state = handle.state::<AppState>();
                    let mut player = state.player.lock().expect("player mutex");
                    if player.state().current().map(|track| &track.id) == canonical.as_ref() {
                        player.set_status(Status::Paused);
                    }
                    drop(player);
                    state.engine_ready.store(false, Ordering::SeqCst);
                    crate::commands::publish(&handle, &state);
                });
            }
            Err(_) => {
                if active() {
                    app.state::<State>()
                        .host
                        .lock()
                        .expect("youtube embed host")
                        .take();
                    let _ = app.emit(
                        "playback://error",
                        "Could not open the experimental YouTube player.",
                    );
                }
            }
        }
    });
}

/// `video_id` is the confirmed provider mapping for the canonical current track.
/// The parent dispatcher retains the canonical queue and supplies each mapping.
pub fn send(
    app: &AppHandle,
    command: &EngineCommand,
    video_id: Option<&str>,
) -> Result<(), String> {
    if !cfg!(target_os = "windows") {
        return Err("The experimental YouTube embedded player currently requires Windows.".into());
    }
    let command = match command {
        EngineCommand::SetQueue { .. } | EngineCommand::SkipNext | EngineCommand::SkipPrevious => {
            let id = video_id
                .filter(|id| valid_id(id))
                .ok_or("Choose a valid YouTube video match first.")?;
            json!({"type":"load", "videoId":id})
        }
        EngineCommand::Play => json!({"type":"play"}),
        EngineCommand::Pause => json!({"type":"pause"}),
        EngineCommand::Seek { ms } => json!({"type":"seek", "ms":ms}),
        EngineCommand::SetVolume { percent } => {
            json!({"type":"volume", "percent":percent.min(&100)})
        }
        _ => return Ok(()),
    };
    let canonical = app
        .state::<AppState>()
        .player
        .lock()
        .expect("player mutex")
        .state()
        .current()
        .map(|track| track.id.clone())
        .ok_or("No track is queued.")?;
    let state = app.state::<State>();
    let mut locked = state.host.lock().expect("youtube embed host");
    let mut launch_instance = None;
    if locked.is_none() {
        let id = video_id
            .filter(|id| valid_id(id))
            .ok_or("Choose a valid YouTube video match first.")?;
        let instance = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *locked = Some(Host {
            instance,
            canonical_id: canonical.clone(),
            video_id: id.into(),
            ready: false,
            ended: false,
            pending: vec![json!({"type":"load","videoId":id})],
        });
        launch_instance = Some(instance);
    }
    let host = locked.as_mut().expect("created host");
    if command["type"] == "load" {
        host.canonical_id = canonical;
        host.video_id = command["videoId"].as_str().expect("validated ID").into();
        host.ended = false;
        if !host.ready {
            host.pending.clear();
        }
    }
    let instance = host.instance;
    let ready = host.ready;
    if !ready {
        host.pending.push(command.clone());
    }
    // Release the mutex before WebView2 callbacks can enter the bridge.
    drop(locked);
    if ready {
        eval(app, instance, &command)?;
    }
    if let Some(instance) = launch_instance {
        launch(app, instance);
    }
    Ok(())
}

pub fn stop(app: &AppHandle) {
    let state = app.state::<State>();
    let instance = state
        .host
        .lock()
        .expect("youtube embed host")
        .take()
        .map(|host| host.instance);
    if let Some(instance) = instance {
        if let Some(window) = app.get_webview_window(&format!("{PREFIX}{instance}")) {
            let _ = window.destroy();
        }
    }
}

#[tauri::command]
pub fn youtube_embed_event(
    app: AppHandle,
    window: tauri::WebviewWindow,
    event: Event,
) -> Result<(), String> {
    let state = app.state::<State>();
    let mut locked = state.host.lock().expect("youtube embed host");
    let Some(host) = locked
        .as_mut()
        .filter(|host| host.accepts(window.label(), &event))
    else {
        return Ok(());
    };
    if event.kind == "ready" {
        host.ready = true;
        let pending = std::mem::take(&mut host.pending);
        let instance = host.instance;
        drop(locked);
        for command in pending {
            eval(&app, instance, &command)?;
        }
        app.state::<AppState>()
            .engine_ready
            .store(true, Ordering::SeqCst);
        let _ = app.emit("engine://ready", ());
        return Ok(());
    }
    let app_state = app.state::<AppState>();
    let mut player = app_state.player.lock().expect("player mutex");
    if player.state().current().map(|track| track.id.as_str()) != Some(host.canonical_id.as_str()) {
        return Ok(());
    }
    let mut ended = false;
    match event.kind.as_str() {
        "state" => {
            if let Some(mut track) = player.state().current().cloned() {
                if let Some(duration) = event.duration_ms.filter(|&ms| ms > 0) {
                    track.duration_ms = duration;
                }
                player.observe_track(track);
            }
            player.on_position(event.position_ms.unwrap_or(0));
            match event.state {
                Some(1) => {
                    host.ended = false;
                    player.set_status(Status::Playing);
                }
                Some(2) => player.set_status(Status::Paused),
                Some(3) => player.set_status(Status::Loading),
                Some(0) => {
                    ended = !host.ended;
                    host.ended = true;
                    player.set_status(Status::Ended);
                }
                Some(-1 | 5) => player.set_status(Status::Loading),
                _ => {}
            }
        }
        "autoplay-blocked" => {
            player.set_status(Status::Paused);
            let _ = window.show();
            let _ = app.emit(
                "playback://error",
                "Click Play in the visible YouTube player to allow playback.",
            );
        }
        "error" | "api-error" => {
            player.set_status(Status::Stalled);
            let _ = app.emit("playback://error", error_message(event.error_code));
        }
        _ => return Ok(()),
    }
    drop(player);
    drop(locked);
    if ended {
        crate::commands::apply(&app, |player| {
            if player.state().repeat == Repeat::One {
                player.on_playback_state(5)
            } else {
                player.next_track()
            }
        });
    } else {
        crate::commands::publish(&app, &app_state);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_serves_only_host_assets_with_honest_referrer_policy() {
        let request = http::Request::builder()
            .uri("/index.html")
            .body(Vec::new())
            .unwrap();
        let response = resource(&request);
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["Referrer-Policy"],
            "strict-origin-when-cross-origin"
        );
        assert!(String::from_utf8(response.body().clone())
            .unwrap()
            .contains("min-height: 270px"));
        let missing = http::Request::builder()
            .uri("/unknown")
            .body(Vec::new())
            .unwrap();
        assert_eq!(resource(&missing).status(), 404);
    }
    #[test]
    fn host_rejects_stale_instances_windows_and_video_observations() {
        let host = Host {
            instance: 7,
            canonical_id: "canonical".into(),
            video_id: "abcdefghijk".into(),
            ready: true,
            ended: false,
            pending: vec![],
        };
        let mut event: Event =
            serde_json::from_value(json!({"instance":7,"kind":"state","videoId":"abcdefghijk"}))
                .unwrap();
        assert!(host.accepts("youtube-embed-7", &event));
        assert!(!host.accepts("main", &event));
        event.instance = 6;
        assert!(!host.accepts("youtube-embed-7", &event));
        event.instance = 7;
        event.video_id = Some("differentid".into());
        assert!(!host.accepts("youtube-embed-7", &event));
    }
    #[test]
    fn embed_ids_and_referrer_failure_are_explicit() {
        assert!(valid_id("abc_DEF-123"));
        assert!(!valid_id("https://youtube.com/watch?v=abc_DEF-123"));
        assert!(error_message(Some(153)).contains("referrer"));
        assert!(error_message(Some(153)).contains("unavailable"));
    }
}
