use std::path::{Path, PathBuf};
pub fn sanitize(text: &str) -> String {
    let mut value = String::new();
    for ch in text
        .trim_matches(|c: char| c.is_whitespace() || c == '.')
        .chars()
    {
        let ch = if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
            '_'
        } else {
            ch
        };
        if value.len() + ch.len_utf8() > 160 {
            break;
        }
        value.push(ch);
    }
    let value = value.trim_end_matches([' ', '.']);
    if value.is_empty() {
        return "Track".into();
    }
    let base = value.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if reserved.contains(&base.as_str()) {
        format!("_{value}")
    } else {
        value.into()
    }
}
/// Publish complete bytes atomically without replacing any existing destination.
/// Windows uses a same-directory move; other platforms use a hard link.
pub fn publish(
    part: &Path,
    directory: &Path,
    stem: &str,
    extension: &str,
) -> Result<PathBuf, String> {
    let stem = sanitize(stem);
    if extension.is_empty() || !extension.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Invalid audio extension.".into());
    }
    for suffix in 0..10_000 {
        let name = if suffix == 0 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem} ({suffix}).{extension}")
        };
        let destination = directory.join(name);
        if destination.exists() {
            continue;
        }
        match move_new(part, &destination) {
            Ok(()) => {
                let _ = std::fs::remove_file(part);
                return Ok(destination);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("Could not save completed audio in the selected folder.".into()),
        }
    }
    Err("Too many files with this name in the download folder.".into())
}
#[cfg(windows)]
fn move_new(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileW(source: *const u16, target: *const u16) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileW(source.as_ptr(), target.as_ptr()) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn move_new(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::hard_link(source, target)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filenames_cannot_escape_or_create_windows_devices() {
        assert_eq!(sanitize(" ../CON: <track>? "), "_CON_ _track__");
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize(".."), "Track");
        assert!(sanitize(&"🎵".repeat(300)).len() <= 160);
    }
    #[test]
    fn publishing_never_replaces_an_existing_file() {
        let dir = std::env::current_dir()
            .unwrap()
            .join(format!("capsule-download-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Track.m4a"), b"old").unwrap();
        let part = dir.join("new.part");
        std::fs::write(&part, b"new").unwrap();
        let result = publish(&part, &dir, "Track", "m4a").unwrap();
        assert_eq!(std::fs::read(dir.join("Track.m4a")).unwrap(), b"old");
        assert_eq!(std::fs::read(result).unwrap(), b"new");
        assert!(!part.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
