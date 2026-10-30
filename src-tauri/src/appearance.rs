use crate::settings::{self, Appearance, Settings};
use std::path::Path;

pub fn save_with_material(
    dir: &Path,
    previous: &Settings,
    next: &Settings,
    mut apply: impl FnMut(&Appearance) -> Result<(), String>,
) -> Result<(), String> {
    let changed = previous.appearance != next.appearance;
    if changed {
        if let Err(error) = apply(&next.appearance) {
            if let Err(rollback) = apply(&previous.appearance) {
                tracing::warn!(error = %rollback, "could not restore previous window material");
            }
            return Err(format!("Could not apply window material: {error}"));
        }
    }
    if let Err(error) = settings::save(dir, next) {
        if changed {
            if let Err(rollback) = apply(&previous.appearance) {
                tracing::warn!(error = %rollback, "could not restore previous window material");
            }
        }
        return Err(error.to_string());
    }
    Ok(())
}

pub fn apply(
    window: &tauri::WebviewWindow,
    glass: &str,
    theme: &crate::theme::Theme,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use window_vibrancy::{apply_acrylic, apply_mica, clear_acrylic, clear_mica};
        window
            .set_theme(Some(if theme.is_dark() {
                tauri::Theme::Dark
            } else {
                tauri::Theme::Light
            }))
            .map_err(|error| error.to_string())?;
        // A material must not leave the previous native backdrop behind.
        // Clearing an unavailable, unused Windows effect is harmless.
        let _ = clear_acrylic(window);
        let _ = clear_mica(window);
        // Suppress Windows 11's focus-colored outer frame and request rounded
        // corners independently of Tao's bordered native shadow frame.
        // Older Windows versions do not support this cosmetic attribute.
        let _ = suppress_window_border(window);
        match glass {
            "acrylic-chrome" | "acrylic-all" => {
                let g = theme.ground;
                apply_acrylic(window, Some((g.r, g.g, g.b, 24)))
                    .map_err(|error| format!("Acrylic is unavailable: {error}"))
            }
            "mica" => apply_mica(window, Some(theme.is_dark()))
                .map_err(|error| format!("Mica is unavailable: {error}")),
            _ => Ok(()),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, glass, theme);
        // Other desktops control native blur through their compositor. CSS
        // translucency still changes immediately on the transparent host.
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub fn suppress_window_border(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE,
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    let no_border = DWMWA_COLOR_NONE;
    let corners = DWMWCP_ROUND;
    // Request the silhouette independently of Tao's bordered shadow frame.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&corners) as u32,
        )
    };
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &no_border as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&no_border) as u32,
        )
    }
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn dir() -> std::path::PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "capsule-material-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn every_material_choice_is_applied_and_saved_including_return_to_solid() {
        let dir = dir();
        let mut previous = Settings::default();
        let native = RefCell::new(previous.appearance.clone());
        for glass in ["acrylic-chrome", "none", "acrylic-all", "mica", "none"] {
            let mut next = previous.clone();
            next.appearance.glass = glass.into();
            save_with_material(&dir, &previous, &next, |appearance| {
                *native.borrow_mut() = appearance.clone();
                Ok(())
            })
            .unwrap();
            assert_eq!(native.borrow().glass, glass);
            assert_eq!(settings::load(&dir).appearance.glass, glass);
            previous = next;
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_native_material_restores_the_previous_material_without_saving_it() {
        let dir = dir();
        let mut previous = Settings::default();
        previous.appearance.glass = "acrylic-chrome".into();
        settings::save(&dir, &previous).unwrap();
        let native = RefCell::new(previous.appearance.clone());
        let mut next = previous.clone();
        next.appearance.glass = "mica".into();
        let result = save_with_material(&dir, &previous, &next, |appearance| {
            *native.borrow_mut() = appearance.clone();
            if appearance.glass == "mica" {
                Err("Mica unavailable".into())
            } else {
                Ok(())
            }
        });
        assert!(result.unwrap_err().contains("Mica unavailable"));
        assert_eq!(*native.borrow(), previous.appearance);
        assert_eq!(settings::load(&dir), previous);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_save_restores_the_native_material() {
        let dir = dir();
        let occupied = dir.join("occupied");
        fs::write(&occupied, "keep this file").unwrap();
        let previous = Settings::default();
        let native = RefCell::new(previous.appearance.clone());
        let mut next = previous.clone();
        next.appearance.glass = "acrylic-all".into();
        assert!(
            save_with_material(&occupied, &previous, &next, |appearance| {
                *native.borrow_mut() = appearance.clone();
                Ok(())
            })
            .is_err()
        );
        assert_eq!(*native.borrow(), previous.appearance);
        assert_eq!(fs::read_to_string(occupied).unwrap(), "keep this file");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_theme_change_updates_the_material_but_unrelated_settings_do_not() {
        let dir = dir();
        let mut previous = Settings::default();
        previous.appearance.glass = "mica".into();
        let native = RefCell::new(previous.appearance.clone());
        let mut next = previous.clone();
        next.appearance.theme = "paper".into();
        save_with_material(&dir, &previous, &next, |appearance| {
            *native.borrow_mut() = appearance.clone();
            Ok(())
        })
        .unwrap();
        assert_eq!(native.borrow().theme, "paper");
        let mut unrelated = next.clone();
        unrelated.lyrics.offset_ms = 100;
        save_with_material(&dir, &next, &unrelated, |_| {
            panic!("an unrelated settings change reapplied the window material")
        })
        .unwrap();
        assert_eq!(settings::load(&dir).lyrics.offset_ms, 100);
        fs::remove_dir_all(dir).unwrap();
    }
}
