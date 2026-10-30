//! Windows 11 native-material verification on an isolated, hidden host window.
#[cfg(target_os = "windows")]
fn main() {
    use capsule_lib::{appearance, theme};
    use std::{
        ffi::c_void,
        sync::{Arc, Mutex},
    };
    use tauri::{WebviewUrl, WebviewWindow, WebviewWindowBuilder};
    use windows::Win32::Graphics::Dwm::{
        DwmGetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    };

    fn check(window: WebviewWindow) -> Result<(), String> {
        let hwnd = window.hwnd().map_err(|error| error.to_string())?;
        let mut outer = windows::Win32::Foundation::RECT::default();
        let mut client_origin = windows::Win32::Foundation::POINT::default();
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut outer)
                .map_err(|error| error.to_string())?;
            windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut client_origin)
                .ok()
                .map_err(|error| error.to_string())?;
        }
        let top_frame = client_origin.y - outer.top;
        if top_frame != 0 {
            return Err(format!(
                "Native top frame still occupies {top_frame} physical pixels"
            ));
        }
        println!("Native client covers the top edge: no reserved frame pixels");
        let palettes = theme::presets();
        let dark = palettes.iter().find(|theme| theme.id == "capsule").unwrap();
        let light = palettes.iter().find(|theme| theme.id == "paper").unwrap();
        for (mode, expected) in [
            ("none", 1),
            ("acrylic-chrome", 3),
            ("acrylic-all", 3),
            ("mica", 2),
            ("none", 1),
        ] {
            appearance::apply(&window, mode, dark)?;
            let hwnd = window.hwnd().map_err(|error| error.to_string())?;
            let mut actual = 0u32;
            unsafe {
                DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE,
                    &mut actual as *mut _ as *mut c_void,
                    4,
                )
            }
            .map_err(|error| format!("Native backdrop query failed: {error}"))?;
            if actual != expected {
                return Err(format!(
                    "{mode}: expected native backdrop {expected}, got {actual}"
                ));
            }
            println!("{mode}: native backdrop {actual}");
            // Windows exposes border color through its setter; this attribute
            // is not supported by DwmGetWindowAttribute on every Windows build.
            appearance::suppress_window_border(&window)?;
            println!("{mode}: Windows accepted the border suppression");
        }
        for (palette, expected) in [(dark, 1u32), (light, 0), (dark, 1)] {
            appearance::apply(&window, "acrylic-all", palette)?;
            let hwnd = window.hwnd().map_err(|error| error.to_string())?;
            let mut actual = 0u32;
            unsafe {
                DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_USE_IMMERSIVE_DARK_MODE,
                    &mut actual as *mut _ as *mut c_void,
                    4,
                )
            }
            .map_err(|error| error.to_string())?;
            if actual != expected {
                return Err(format!(
                    "{}: incorrect native material polarity {actual}",
                    palette.name
                ));
            }
            println!("{}: native dark mode {actual}", palette.name);
        }
        Ok(())
    }

    let outcome = Arc::new(Mutex::new(None));
    let setup_outcome = outcome.clone();
    tauri::Builder::default()
        .setup(move |app| {
            let window = WebviewWindowBuilder::new(
                app,
                "appearance-probe",
                WebviewUrl::External("about:blank".parse().unwrap()),
            )
            .title("Capsule material verification")
            .visible(false)
            .decorations(false)
            .shadow(false)
            .transparent(true)
            .inner_size(880.0, 560.0)
            .data_directory(std::env::current_dir()?.join("target/appearance-probe-cache"))
            .build()?;
            let handle = app.handle().clone();
            let result = setup_outcome.clone();
            std::thread::spawn(move || {
                let checked = check(window);
                let code = if let Err(error) = &checked {
                    eprintln!("{error}");
                    1
                } else {
                    0
                };
                *result.lock().unwrap() = Some(checked);
                handle.exit(code);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("native probe could not start");
    let result = outcome
        .lock()
        .unwrap()
        .take()
        .expect("probe did not finish");
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("This probe verifies Windows 11 native backdrop attributes.");
}
