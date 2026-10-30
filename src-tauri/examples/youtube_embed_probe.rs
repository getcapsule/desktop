//! Actual WebView2 smoke probe for the production official YouTube embed host.
//!
//! Build: cargo build --example youtube_embed_probe --offline
//! Run: target/debug/examples/youtube_embed_probe.exe [11-character-video-id]
//!
//! Uses the production resource handler and its unchanged player.js, an honest
//! local custom-protocol origin, and a visible incognito window. No application
//! state, account credentials, music library, or settings are initialized.
//! Exit codes: 0 ended/closed, 2 player error, 3 API error, 4 startup timeout,
//! 5 five-minute probe limit, 6 bridge/evaluation failure.

use capsule_lib::youtube_embed;
use serde_json::{json, Value};
use std::{
    io::Write,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const LABEL: &str = "youtube-embed-probe";
const INSTANCE: u64 = 1;

struct Probe {
    video_id: String,
    started: Instant,
    ready: AtomicBool,
    played: AtomicBool,
}

fn log(probe: &Probe, kind: &str, fields: Value) {
    println!(
        "{}",
        json!({"probe":"youtube_embed", "elapsedMs":probe.started.elapsed().as_millis(), "kind":kind, "details":fields})
    );
    let _ = std::io::stdout().flush();
}

#[tauri::command]
fn youtube_embed_event(
    app: AppHandle,
    window: WebviewWindow,
    event: Value,
    probe: tauri::State<'_, Probe>,
) -> Result<(), String> {
    if window.label() != LABEL || event["instance"].as_u64() != Some(INSTANCE) {
        return Err("Probe rejected an unexpected embed window or instance.".into());
    }
    let kind = event["kind"].as_str().unwrap_or("unknown");
    log(
        &probe,
        kind,
        json!({
            "videoId":event["videoId"], "state":event["state"],
            "positionMs":event["positionMs"], "durationMs":event["durationMs"],
            "errorCode":event["errorCode"]
        }),
    );
    match kind {
        "ready" if !probe.ready.swap(true, Ordering::SeqCst) => {
            let commands = json!([
                {"type":"load", "videoId":probe.video_id},
                {"type":"volume", "percent":10},
                {"type":"play"}
            ]);
            if window
                .eval(format!(
                    "for (const command of {commands}) window.__capsuleYoutubeEmbedCommand(command);"
                ))
                .is_err()
            {
                log(&probe, "bridge-error", json!({"message":"Could not evaluate production playback commands."}));
                app.exit(6);
                return Err("Could not evaluate production playback commands.".into());
            }
            log(&probe, "commands-sent", commands);
        }
        "state" if event["state"].as_i64() == Some(1) => {
            probe.played.store(true, Ordering::SeqCst);
        }
        "state" if event["state"].as_i64() == Some(0) => app.exit(0),
        "error" => app.exit(2),
        "api-error" => app.exit(3),
        "autoplay-blocked" => {
            log(
                &probe,
                "gesture-required",
                json!({"message":"Click Play in the visible official player; the 45-second startup deadline remains active."}),
            );
        }
        _ => {}
    }
    Ok(())
}

fn main() {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "M7lc1UVf-VE".into());
    if video_id.len() != 11
        || !video_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        eprintln!("Pass one 11-character YouTube video ID, without a URL or credentials.");
        std::process::exit(64);
    }
    let probe = Probe {
        video_id,
        started: Instant::now(),
        ready: AtomicBool::new(false),
        played: AtomicBool::new(false),
    };
    log(
        &probe,
        "starting",
        json!({"videoId":probe.video_id,"origin":"youtube-embed://localhost","incognito":true,"startupTimeoutSeconds":45,"maximumSeconds":300}),
    );
    tauri::Builder::default()
        .manage(probe)
        .register_uri_scheme_protocol(youtube_embed::SCHEME, |_context, request| {
            youtube_embed::resource(&request)
        })
        .invoke_handler(tauri::generate_handler![youtube_embed_event])
        .setup(|app| {
            WebviewWindowBuilder::new(
                app,
                LABEL,
                WebviewUrl::CustomProtocol(
                    format!("{}://localhost/index.html", youtube_embed::SCHEME).parse()?,
                ),
            )
            .title("capsule · Production YouTube embed smoke probe")
            .inner_size(800.0, 520.0)
            .min_inner_size(500.0, 360.0)
            .visible(true)
            .skip_taskbar(false)
            .incognito(true)
            .initialization_script(format!("window.__capsuleYoutubeEmbedInstance={INSTANCE};"))
            .on_navigation(|url| {
                (url.scheme() == youtube_embed::SCHEME && url.host_str() == Some("localhost"))
                    || (matches!(url.scheme(), "http" | "https")
                        && url.host_str() == Some("youtube-embed.localhost"))
            })
            .build()?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(45)).await;
                let probe = handle.state::<Probe>();
                if !probe.played.load(Ordering::SeqCst) {
                    log(
                        &probe,
                        "startup-timeout",
                        json!({"ready":probe.ready.load(Ordering::SeqCst),"played":false}),
                    );
                    handle.exit(4);
                    return;
                }
                tokio::time::sleep(Duration::from_secs(255)).await;
                log(&probe, "maximum-duration", json!({"seconds":300}));
                handle.exit(5);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                let probe = window.state::<Probe>();
                log(&probe, "window-closed", json!({}));
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("Could not run the production YouTube embed probe");
}
