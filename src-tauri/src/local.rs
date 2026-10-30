use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::db::{AlbumUpsert, ArtistUpsert, SongUpsert};

const AUDIO: &[&str] = &["flac", "mp3", "m4a", "aac", "ogg", "oga", "opus", "wav", "wave"];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub const ARTWORK_PREFIX: &str = "local:";

pub fn artwork_ref(path: &Path) -> String {
    format!("{ARTWORK_PREFIX}{}", path.display())
}

fn stable_id(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.to_ascii_lowercase().as_bytes());
        h.update([0]);
    }
    format!("{:x}", h.finalize())[..16].to_string()
}

pub fn album_id(album_artist: &str, album: &str) -> String {
    stable_id(&[album_artist, album])
}

pub fn artist_id(name: &str) -> String {
    stable_id(&[name])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub duration_ms: u64,
    pub has_picture: bool,
}

pub fn title_fallback(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Unknown".into())
}

pub fn song_from(path: &Path, tags: &Tags) -> SongUpsert {
    SongUpsert {
        id: path.display().to_string(),
        catalog_id: None,
        name: tags.title.clone(),
        artist_name: tags.artist.clone(),
        album_name: tags.album.clone(),
        album_id: Some(album_id(&tags.album_artist, &tags.album)),
        duration_ms: tags.duration_ms,
        artwork_url: tags.has_picture.then(|| artwork_ref(path)),
        track_number: tags.track_number,
        disc_number: tags.disc_number,
        added_at: None,
        artwork_width: None,
        artwork_height: None,
    }
}

pub fn albums_from(songs: &[SongUpsert], tags: &HashMap<String, Tags>) -> Vec<AlbumUpsert> {
    let mut out: HashMap<String, AlbumUpsert> = HashMap::new();
    for s in songs {
        let Some(id) = s.album_id.clone() else { continue };
        let album_artist = tags
            .get(&s.id)
            .map(|t| t.album_artist.clone())
            .unwrap_or_else(|| s.artist_name.clone());
        let entry = out.entry(id.clone()).or_insert_with(|| AlbumUpsert {
            id,
            catalog_id: None,
            name: s.album_name.clone(),
            artist_name: album_artist,
            artwork_url: None,
            release_date: None,
            track_count: 0,
            added_at: None,
            artwork_width: None,
            artwork_height: None,
        });
        entry.track_count += 1;
        if entry.artwork_url.is_none() {
            entry.artwork_url.clone_from(&s.artwork_url);
        }
    }
    out.into_values().collect()
}

pub fn artists_from(songs: &[SongUpsert]) -> Vec<ArtistUpsert> {
    let mut out: HashMap<String, ArtistUpsert> = HashMap::new();
    for s in songs {
        if s.artist_name.trim().is_empty() {
            continue;
        }
        out.entry(artist_id(&s.artist_name)).or_insert_with(|| ArtistUpsert {
            id: artist_id(&s.artist_name),
            name: s.artist_name.clone(),
            artwork_url: None,
        });
    }
    out.into_values().collect()
}

/// Every audio file under `folders`, deepest-first order unspecified.
///
/// Unreadable directories are skipped rather than failing the scan: one
/// permission-denied folder should not cost the user their whole library.
pub fn walk(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in folders {
        for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok())
        {
            let path = entry.path();
            if entry.file_type().is_file() && is_audio(path) {
                found.push(path.to_path_buf());
            }
        }
    }
    found
}

/// Read what tags a file carries, defaulting anything absent.
///
/// Returns `None` only when the file cannot be parsed at all - a track with no
/// tags whatsoever is still a track, named after its file.
pub fn read_tags(path: &Path) -> Option<Tags> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::prelude::ItemKey;
    use lofty::probe::Probe;

    let tagged = Probe::open(path).ok()?.read().ok()?;
    let duration_ms = tagged.properties().duration().as_millis() as u64;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let get = |key: &ItemKey| -> Option<String> {
        tag.and_then(|t| t.get_string(key)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    let num = |key: &ItemKey| -> Option<i64> {
        get(key).and_then(|s| s.split('/').next().unwrap_or_default().trim().parse().ok())
    };

    let artist = get(&ItemKey::TrackArtist).unwrap_or_else(|| "Unknown Artist".into());
    // Falling back to the track artist keeps a normal single-artist album
    // grouped correctly when the album-artist tag is simply absent.
    let album_artist = get(&ItemKey::AlbumArtist).unwrap_or_else(|| artist.clone());

    Some(Tags {
        title: get(&ItemKey::TrackTitle).unwrap_or_else(|| title_fallback(path)),
        artist,
        album_artist,
        album: get(&ItemKey::AlbumTitle).unwrap_or_else(|| "Unknown Album".into()),
        track_number: num(&ItemKey::TrackNumber),
        disc_number: num(&ItemKey::DiscNumber),
        duration_ms,
        has_picture: tag.map(|t| t.picture_count() > 0).unwrap_or(false),
    })
}

/// The embedded cover art for a file, if it has any.
pub fn embedded_picture(path: &Path) -> Option<Vec<u8>> {
    use lofty::file::TaggedFileExt;
    use lofty::probe::Probe;

    let tagged = Probe::open(path).ok()?.read().ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    tag.pictures().first().map(|p| p.data().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(title: &str, artist: &str, album_artist: &str, album: &str) -> Tags {
        Tags {
            title: title.into(),
            artist: artist.into(),
            album_artist: album_artist.into(),
            album: album.into(),
            track_number: Some(1),
            disc_number: None,
            duration_ms: 180_000,
            has_picture: false,
        }
    }

    #[test]
    fn recognises_the_formats_symphonia_decodes() {
        assert!(is_audio(Path::new("a.flac")));
        assert!(is_audio(Path::new("a.MP3")), "extension match is case-insensitive");
        assert!(is_audio(Path::new("a.m4a")));
        assert!(!is_audio(Path::new("cover.jpg")));
        assert!(!is_audio(Path::new("notes.txt")));
        assert!(!is_audio(Path::new("no-extension")));
    }

    #[test]
    fn a_compilation_stays_one_album() {
        // Grouping on the track artist would split this into three albums.
        let a = song_from(Path::new("/m/1.flac"), &tags("One", "Bladee", "Various", "Comp"));
        let b = song_from(Path::new("/m/2.flac"), &tags("Two", "Ecco2k", "Various", "Comp"));
        let c = song_from(Path::new("/m/3.flac"), &tags("Three", "Thaiboy", "Various", "Comp"));
        assert_eq!(a.album_id, b.album_id);
        assert_eq!(b.album_id, c.album_id);
    }

    #[test]
    fn same_album_name_by_different_artists_stays_separate() {
        let a = song_from(Path::new("/m/a.flac"), &tags("x", "A", "A", "Greatest Hits"));
        let b = song_from(Path::new("/m/b.flac"), &tags("y", "B", "B", "Greatest Hits"));
        assert_ne!(a.album_id, b.album_id);
    }

    #[test]
    fn ids_are_stable_across_rescans() {
        let first = album_id("Bladee", "333");
        let second = album_id("bladee", "333");
        assert_eq!(first, second, "case should not create a second album");
    }

    #[test]
    fn the_path_is_the_track_id() {
        let s = song_from(Path::new("/m/x.flac"), &tags("t", "a", "a", "al"));
        assert_eq!(s.id, Path::new("/m/x.flac").display().to_string());
    }

    #[test]
    fn artwork_is_referenced_only_when_the_file_carries_a_picture() {
        let mut t = tags("t", "a", "a", "al");
        assert_eq!(song_from(Path::new("/m/x.flac"), &t).artwork_url, None);
        t.has_picture = true;
        assert_eq!(
            song_from(Path::new("/m/x.flac"), &t).artwork_url.as_deref(),
            Some("local:/m/x.flac")
        );
    }

    #[test]
    fn an_untagged_file_falls_back_to_its_filename() {
        assert_eq!(title_fallback(Path::new("/m/01 - Cambaz.flac")), "01 - Cambaz");
        assert_eq!(title_fallback(Path::new("/m/.flac")), ".flac");
    }

    #[test]
    fn albums_count_their_tracks_and_borrow_the_first_cover() {
        let mut with_art = tags("Two", "A", "A", "Al");
        with_art.has_picture = true;
        let songs = vec![
            song_from(Path::new("/m/1.flac"), &tags("One", "A", "A", "Al")),
            song_from(Path::new("/m/2.flac"), &with_art),
        ];
        let albums = albums_from(&songs, &HashMap::new());
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].track_count, 2);
        assert_eq!(
            albums[0].artwork_url.as_deref(),
            Some("local:/m/2.flac"),
            "an album whose art is only on a later track still gets a cover"
        );
    }

    #[test]
    fn artists_are_deduplicated_and_blanks_dropped() {
        let songs = vec![
            song_from(Path::new("/m/1.flac"), &tags("a", "Bladee", "Bladee", "X")),
            song_from(Path::new("/m/2.flac"), &tags("b", "Bladee", "Bladee", "Y")),
            song_from(Path::new("/m/3.flac"), &tags("c", "", "", "Z")),
        ];
        let artists = artists_from(&songs);
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].name, "Bladee");
    }
}
