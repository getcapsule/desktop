use super::model::Platform;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Track,
    Album,
    Playlist,
    Artist,
    Profile,
    Likes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportUrl {
    pub provider: Platform,
    pub kind: ContentKind,
    pub id: String,
    pub storefront: Option<String>,
    pub url: String,
}

pub fn video_id(id: &str) -> bool {
    id.len() == 11 && identifier(id)
}

fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        && id != "."
        && id != ".."
}

/// Only these public provider URLs enter an adapter; raw user URLs are never fetched.
pub fn detect(raw: &str) -> Result<ImportUrl, String> {
    let raw = raw.trim();
    if raw.len() > 4096 || raw.bytes().any(|b| b.is_ascii_control() || b == b'\\') {
        return Err("Invalid music URL.".into());
    }
    let lower = raw.to_ascii_lowercase();
    if lower.contains("/../")
        || lower.contains("/./")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || lower.contains("%2e")
    {
        return Err("Invalid music URL path.".into());
    }
    let url = url::Url::parse(raw).map_err(|_| "Paste a complete HTTPS music URL.")?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Only HTTPS music links without credentials or fragments are supported.".into(),
        );
    }
    let host = url.host_str().unwrap_or_default();
    let mut parts: Vec<&str> = url
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let get_query = |key: &str| -> Result<Option<String>, String> {
        let values: Vec<_> = url
            .query_pairs()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .collect();
        if values.len() > 1 {
            return Err("Ambiguous music URL parameters.".into());
        }
        Ok(values.into_iter().next())
    };
    let (provider, kind, id, storefront) = match host {
        "open.spotify.com" => {
            if parts.first().is_some_and(|s| s.starts_with("intl-")) {
                parts.remove(0);
            }
            if parts.len() != 2 {
                return Err("Unsupported Spotify link.".into());
            }
            let kind = match parts[0] {
                "track" => ContentKind::Track,
                "album" => ContentKind::Album,
                "playlist" => ContentKind::Playlist,
                "artist" => ContentKind::Artist,
                "user" => ContentKind::Profile,
                _ => return Err("Unsupported Spotify collection.".into()),
            };
            if !identifier(parts[1]) {
                return Err("Invalid Spotify ID.".into());
            }
            (Platform::Spotify, kind, parts[1].to_string(), None)
        }
        "music.apple.com" => {
            if parts.len() < 3
                || parts.len() > 4
                || parts[0].len() != 2
                || !parts[0].bytes().all(|b| b.is_ascii_alphabetic())
            {
                return Err("Apple Music links must include a storefront and catalog ID.".into());
            }
            let mut kind = match parts[1] {
                "song" => ContentKind::Track,
                "album" => ContentKind::Album,
                "playlist" => ContentKind::Playlist,
                "artist" => ContentKind::Artist,
                "profile" => ContentKind::Profile,
                _ => return Err("Unsupported Apple Music link.".into()),
            };
            let mut id = parts.last().unwrap().to_string();
            if let Some(song) = get_query("i")? {
                if kind != ContentKind::Album {
                    return Err("Invalid Apple Music song link.".into());
                }
                kind = ContentKind::Track;
                id = song;
            }
            if !identifier(&id) {
                return Err("Invalid Apple Music ID.".into());
            }
            (
                Platform::AppleMusic,
                kind,
                id,
                Some(parts[0].to_ascii_lowercase()),
            )
        }
        "music.youtube.com" | "youtube.com" | "www.youtube.com" | "m.youtube.com" | "youtu.be" => {
            let (kind, id) = if host == "youtu.be" && parts.len() == 1 {
                (ContentKind::Track, parts[0].to_string())
            } else if parts.as_slice() == ["playlist"] {
                (
                    ContentKind::Playlist,
                    get_query("list")?.ok_or("A YouTube playlist needs a list ID.")?,
                )
            } else if parts.as_slice() == ["watch"] {
                (
                    ContentKind::Track,
                    get_query("v")?.ok_or("A YouTube track needs a video ID.")?,
                )
            } else if parts.len() == 2 && parts[0] == "channel" {
                (ContentKind::Profile, parts[1].to_string())
            } else if parts.len() == 1 && parts[0].starts_with('@') {
                (ContentKind::Profile, parts[0].to_string())
            } else {
                return Err("Unsupported YouTube link. Use a watch or playlist URL.".into());
            };
            if (kind == ContentKind::Track && !video_id(&id))
                || (kind != ContentKind::Track && !identifier(id.trim_start_matches('@')))
            {
                return Err("Invalid YouTube ID.".into());
            }
            (Platform::Youtube, kind, id, None)
        }
        "soundcloud.com" | "www.soundcloud.com" => {
            if parts.is_empty() || !parts.iter().all(|p| identifier(p)) {
                return Err("Invalid SoundCloud link.".into());
            }
            let (kind, id) = match parts.as_slice() {
                [user] => (ContentKind::Profile, (*user).into()),
                [user, "likes"] => (ContentKind::Likes, (*user).into()),
                [user, "sets", set] => (ContentKind::Playlist, format!("{user}/sets/{set}")),
                [user, track] => (ContentKind::Track, format!("{user}/{track}")),
                _ => return Err("Unsupported SoundCloud link.".into()),
            };
            (Platform::Soundcloud, kind, id, None)
        }
        _ => {
            return Err(
                "Unsupported provider. Use Spotify, Apple Music, YouTube, or SoundCloud.".into(),
            )
        }
    };
    // Canonical references exclude tracking parameters and never include an API credential.
    let canonical = match provider {
        Platform::Spotify => format!(
            "https://open.spotify.com/{}/{}",
            match kind {
                ContentKind::Track => "track",
                ContentKind::Album => "album",
                ContentKind::Playlist => "playlist",
                ContentKind::Artist => "artist",
                _ => "user",
            },
            id
        ),
        Platform::Youtube => match kind {
            ContentKind::Track => format!("https://music.youtube.com/watch?v={id}"),
            ContentKind::Playlist => format!("https://music.youtube.com/playlist?list={id}"),
            _ => format!(
                "https://www.youtube.com/{}",
                if id.starts_with('@') {
                    id.clone()
                } else {
                    format!("channel/{id}")
                }
            ),
        },
        Platform::AppleMusic => format!(
            "https://music.apple.com/{}/{}/{}",
            storefront.as_deref().unwrap(),
            match kind {
                ContentKind::Track => "song",
                ContentKind::Album => "album",
                ContentKind::Playlist => "playlist",
                ContentKind::Artist => "artist",
                _ => "profile",
            },
            id
        ),
        Platform::Soundcloud => format!(
            "https://soundcloud.com/{id}{}",
            if kind == ContentKind::Likes {
                "/likes"
            } else {
                ""
            }
        ),
        _ => unreachable!(),
    };
    Ok(ImportUrl {
        provider,
        kind,
        id,
        storefront,
        url: canonical,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsafe_and_spoofed_urls() {
        for raw in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://open.spotify.com/playlist/abc",
            "https://open.spotify.com.evil.test/playlist/abc",
            "https://user:pass@open.spotify.com/playlist/abc",
            "https://open.spotify.com:444/playlist/abc",
            "https://127.0.0.1/playlist/abc",
            "https://soundcloud.com/u/../x",
            "https://open.spotify.com/playlist/abc%2fdef",
        ] {
            assert!(detect(raw).is_err(), "{raw}");
        }
    }
    #[test]
    fn detects_collections_and_apple_track_query() {
        let cases = [
            (
                "https://open.spotify.com/intl-de/playlist/abc123?si=tracking",
                Platform::Spotify,
                ContentKind::Playlist,
                "abc123",
            ),
            (
                "https://music.apple.com/us/album/a-title/123?i=456",
                Platform::AppleMusic,
                ContentKind::Track,
                "456",
            ),
            (
                "https://music.youtube.com/playlist?list=PLabc_123",
                Platform::Youtube,
                ContentKind::Playlist,
                "PLabc_123",
            ),
            (
                "https://youtu.be/abcdefghijk",
                Platform::Youtube,
                ContentKind::Track,
                "abcdefghijk",
            ),
            (
                "https://soundcloud.com/artist/sets/an-album",
                Platform::Soundcloud,
                ContentKind::Playlist,
                "artist/sets/an-album",
            ),
            (
                "https://soundcloud.com/artist/likes",
                Platform::Soundcloud,
                ContentKind::Likes,
                "artist",
            ),
        ];
        for (raw, provider, kind, id) in cases {
            let value = detect(raw).unwrap();
            assert_eq!(
                (value.provider, value.kind, value.id.as_str()),
                (provider, kind, id)
            );
        }
    }
    #[test]
    fn rejects_ambiguous_or_invalid_provider_paths() {
        for raw in [
            "https://youtube.com/redirect?q=https://127.0.0.1",
            "https://music.youtube.com/watch?v=bad",
            "https://open.spotify.com/playlist/a?ignored=ok#fragment",
            "https://music.apple.com/us/album/a/123?i=4&i=5",
            "https://soundcloud.com/u/sets/x/extra",
        ] {
            assert!(detect(raw).is_err(), "{raw}");
        }
    }
}
