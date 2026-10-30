//! Validated, portable theme documents. Imports are previewed before saving.
use crate::theme::{Failure, Theme};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
};

const MAX_BYTES: usize = 64 * 1024;

#[derive(Serialize)]
pub struct Document {
    pub raw: String,
    pub name: String,
    pub colors: BTreeMap<String, String>,
    pub css: String,
    pub failures: Vec<Failure>,
}

fn colors(theme: &Theme) -> BTreeMap<String, String> {
    [
        ("ground", theme.ground),
        ("panel", theme.panel),
        ("ink", theme.ink),
        ("accent", theme.accent),
        ("rule", theme.rule),
        ("muted", theme.muted),
        ("ok", theme.ok),
        ("warn", theme.warn),
        ("crit", theme.crit),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.hex()))
    .collect()
}

pub fn document(raw: &str) -> Result<Document, String> {
    if raw.len() > MAX_BYTES {
        return Err("Theme files must be smaller than 64 KB.".into());
    }
    let theme = Theme::from_toml("preview", raw).map_err(|e| e.to_string())?;
    if theme.name.trim().is_empty() || theme.name.chars().count() > 120 {
        return Err("Give the theme a name of 1 to 120 characters.".into());
    }
    Ok(Document {
        raw: portable(&theme)?,
        name: theme.name.clone(),
        colors: colors(&theme),
        css: theme.css(),
        failures: theme.failures(),
    })
}
pub fn portable(theme: &Theme) -> Result<String, String> {
    let mut fields = colors(theme);
    fields.insert("name".into(), theme.name.clone());
    toml::to_string_pretty(&fields).map_err(|e| e.to_string())
}
pub fn save_copy(dir: &Path, raw: &str) -> Result<String, String> {
    let doc = document(raw)?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "Could not generate a theme ID.")?;
    let id = format!(
        "custom-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let path = dir.join(format!("{id}.toml"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    if let Err(error) = file
        .write_all(doc.raw.as_bytes())
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error.to_string());
    }
    Ok(id)
}

pub fn import_file(path: &Path) -> Result<Document, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Select a TOML theme file.".into());
    }
    let mut raw = String::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_string(&mut raw)
        .map_err(|e| e.to_string())?;
    document(&raw)
}

fn selected(state: &crate::AppState, id: &str) -> Result<Theme, String> {
    let dir = state.data_dir.lock().expect("data dir mutex").clone();
    crate::theme::load_all(dir.as_deref())
        .into_iter()
        .find(|theme| theme.id == id)
        .ok_or("Theme no longer exists. Choose another theme.".into())
}

#[tauri::command]
pub fn themes_document(
    state: tauri::State<'_, crate::AppState>,
    id: String,
) -> Result<Document, String> {
    document(&portable(&selected(&state, &id)?)?)
}
#[tauri::command]
pub fn themes_preview(raw: String) -> Result<Document, String> {
    document(&raw)
}
#[tauri::command]
pub fn themes_save(
    state: tauri::State<'_, crate::AppState>,
    raw: String,
) -> Result<String, String> {
    let dir = state
        .data_dir
        .lock()
        .expect("data dir mutex")
        .clone()
        .ok_or("App data directory is unavailable.")?;
    save_copy(&crate::theme::themes_dir(&dir), &raw)
}
#[tauri::command]
pub async fn themes_import(app: tauri::AppHandle) -> Result<Option<Document>, String> {
    use tauri_plugin_dialog::DialogExt;
    tauri::async_runtime::spawn_blocking(move || {
        let Some(file) = app
            .dialog()
            .file()
            .add_filter("capsule theme", &["toml"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        import_file(&file.into_path().map_err(|_| "Select a local theme file.")?).map(Some)
    })
    .await
    .map_err(|_| "Could not open the theme picker.".to_string())?
}
#[tauri::command]
pub async fn themes_export(app: tauri::AppHandle, id: String) -> Result<bool, String> {
    use tauri::Manager;
    use tauri_plugin_dialog::DialogExt;
    let raw = portable(&selected(&app.state::<crate::AppState>(), &id)?)?;
    tauri::async_runtime::spawn_blocking(move || {
        let Some(file) = app
            .dialog()
            .file()
            .add_filter("capsule theme", &["toml"])
            .set_file_name("capsule-theme.toml")
            .blocking_save_file()
        else {
            return Ok(false);
        };
        std::fs::write(
            file.into_path()
                .map_err(|_| "Select a local export location.")?,
            raw,
        )
        .map_err(|e| e.to_string())?;
        Ok(true)
    })
    .await
    .map_err(|_| "Could not open the export dialog.".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_round_trip_preserves_all_rendered_colors_and_escaped_name() {
        let mut theme = crate::theme::presets().remove(0);
        theme.name = "A \"quoted\" theme\nsecond line".into();
        let raw = portable(&theme).unwrap();
        let loaded = Theme::from_toml(&theme.id, &raw).unwrap();
        assert_eq!(loaded.name, theme.name);
        assert_eq!(loaded.css(), theme.css());
    }
    #[test]
    fn import_rejects_large_malformed_and_invalid_color_documents() {
        for raw in [
            "x".repeat(MAX_BYTES + 1),
            "ground = 1".into(),
            "ground = \"red\"".into(),
        ] {
            assert!(document(&raw).is_err());
        }
    }
    #[test]
    fn saving_same_name_twice_creates_distinct_copies_without_touching_presets() {
        let dir = std::env::temp_dir().join(format!("capsule-theme-copy-{}", std::process::id()));
        let mut theme = crate::theme::presets().remove(0);
        theme.name = "../../CON".into();
        let raw = portable(&theme).unwrap();
        let first = save_copy(&dir, &raw).unwrap();
        let second = save_copy(&dir, &raw).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with("custom-"));
        assert!(first
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-'));
        assert_eq!(
            document(&std::fs::read_to_string(dir.join(format!("{first}.toml"))).unwrap())
                .unwrap()
                .name,
            "../../CON"
        );
        assert!(!dir.join("capsule.toml").exists());
        std::fs::remove_file(dir.join(format!("{first}.toml"))).unwrap();
        std::fs::remove_file(dir.join(format!("{second}.toml"))).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
