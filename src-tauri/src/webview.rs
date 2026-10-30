//! Browser configuration shared by the app and provider webviews.
pub fn apply_webview_flags() {
    #[cfg(target_os = "windows")]
    std::env::set_var(
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "--disable-background-timer-throttling \
         --disable-renderer-backgrounding \
         --disable-backgrounding-occluded-windows \
         --disable-crash-reporter",
    );
}
