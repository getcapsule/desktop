//! Browser configuration shared by the app and provider webviews.

/// Call before Tauri starts GTK, WebKit or any background workers.
pub fn apply_webview_flags() {
    #[cfg(target_os = "windows")]
    std::env::set_var(
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "--disable-background-timer-throttling \
         --disable-renderer-backgrounding \
         --disable-backgrounding-occluded-windows \
         --disable-crash-reporter",
    );

    #[cfg(target_os = "linux")]
    if let Ok(version) = tauri::webview_version() {
        if let Some(flag) = linux_renderer_flag(|name| std::env::var_os(name), &version) {
            std::env::set_var(flag, "1");
            tracing::info!(webkit = %version, flag, "using X11 webview rendering fallback");
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn linux_renderer_flag(
    get_env: impl Fn(&str) -> Option<std::ffi::OsString>,
    version: &str,
) -> Option<&'static str> {
    const FORCE_SHM: &str = "WEBKIT_DMABUF_RENDERER_FORCE_SHM";
    const DISABLE_DMABUF: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

    if [
        FORCE_SHM,
        DISABLE_DMABUF,
        "WEBKIT_FORCE_DMABUF_RENDERER",
        "WEBKIT_DISABLE_COMPOSITING_MODE",
        "WEBKIT_DMABUF_RENDERER_BUFFER_FORMAT",
        "WEBKIT_DMABUF_RENDERER_DISABLE_GBM",
    ]
    .iter()
    .any(|name| get_env(name).is_some())
    {
        return None;
    }

    let has_value = |name| get_env(name).is_some_and(|value| !value.is_empty());
    if !has_value("DISPLAY") {
        return None;
    }
    let wayland = has_value("WAYLAND_DISPLAY")
        || has_value("WAYLAND_SOCKET")
        || get_env("XDG_SESSION_TYPE").is_some_and(|value| value == "wayland");
    // The AppImage launcher can choose X11 even in a Wayland session.
    let backends = get_env("GDK_BACKEND");
    let x11 = backends
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("*")
        .split(',')
        .find_map(|backend| match backend {
            "x11" => Some(true),
            "wayland" if wayland => Some(false),
            "broadway" => Some(false),
            "*" | "help" => Some(!wayland),
            _ => None,
        })
        .unwrap_or(false);
    if !x11 {
        return None;
    }

    let mut version = version.split('.');
    let major = version.next()?.parse::<u32>().ok()?;
    let minor = version.next()?.parse::<u32>().ok()?;
    match (major, minor) {
        // Keep WebKit's accelerated backing store, but avoid GPU buffer imports.
        (2, 44..) => Some(FORCE_SHM),
        (2, 42..=43) => Some(DISABLE_DMABUF),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::linux_renderer_flag;

    fn renderer(version: &str, env: &[(&str, &str)]) -> Option<&'static str> {
        linux_renderer_flag(
            |name| {
                env.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).into())
            },
            version,
        )
    }

    #[test]
    fn x11_uses_the_compatible_transport_for_the_loaded_webkit() {
        let x11 = [("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "x11")];
        assert_eq!(
            renderer("2.54.1", &x11),
            Some("WEBKIT_DMABUF_RENDERER_FORCE_SHM")
        );
        assert_eq!(
            renderer("2.44.0", &x11),
            Some("WEBKIT_DMABUF_RENDERER_FORCE_SHM")
        );
        assert_eq!(
            renderer("2.42.5", &x11),
            Some("WEBKIT_DISABLE_DMABUF_RENDERER")
        );
        assert_eq!(renderer("2.40.0", &x11), None);
    }

    #[test]
    fn appimage_x11_backend_takes_precedence_over_the_wayland_session() {
        let appimage = [
            ("DISPLAY", ":1"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_SESSION_TYPE", "wayland"),
            ("GDK_BACKEND", "x11"),
        ];
        assert_eq!(
            renderer("2.52.6", &appimage),
            Some("WEBKIT_DMABUF_RENDERER_FORCE_SHM")
        );
        assert_eq!(renderer("2.52.6", &appimage[..3]), None);
    }

    #[test]
    fn respects_backend_order_and_skips_headless_launches() {
        assert_eq!(renderer("2.52.6", &[]), None);
        assert_eq!(
            renderer(
                "2.52.6",
                &[("DISPLAY", ":0"), ("GDK_BACKEND", "wayland,x11")]
            ),
            Some("WEBKIT_DMABUF_RENDERER_FORCE_SHM")
        );
        assert_eq!(
            renderer(
                "2.52.6",
                &[
                    ("DISPLAY", ":0"),
                    ("WAYLAND_DISPLAY", "wayland-0"),
                    ("GDK_BACKEND", "wayland,x11"),
                ]
            ),
            None
        );
    }

    #[test]
    fn respects_an_explicit_renderer_choice_even_if_set_to_zero() {
        for (name, value) in [
            ("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "0"),
            ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
            ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
            ("WEBKIT_FORCE_DMABUF_RENDERER", "1"),
            ("WEBKIT_DMABUF_RENDERER_BUFFER_FORMAT", "XR24"),
            ("WEBKIT_DMABUF_RENDERER_DISABLE_GBM", "0"),
        ] {
            assert_eq!(
                renderer("2.52.6", &[("DISPLAY", ":0"), (name, value)]),
                None
            );
        }
    }
}
