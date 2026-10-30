use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Spotify,
    AppleMusic,
    Youtube,
    Soundcloud,
    Navidrome,
    Local,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spotify => "spotify",
            Self::AppleMusic => "apple_music",
            Self::Youtube => "youtube",
            Self::Soundcloud => "soundcloud",
            Self::Navidrome => "navidrome",
            Self::Local => "local",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub platform: Platform,
    pub external_id: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    pub artwork_url: Option<String>,
    pub isrc: Option<String>,
    pub explicit: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedTrack {
    pub origin: Origin,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapsuleTrack {
    pub id: String,
    #[serde(flatten)]
    pub metadata: Metadata,
    pub origins: Vec<Origin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedCollection {
    pub source_url: String,
    pub provider: Platform,
    pub kind: super::urls::ContentKind,
    pub title: String,
    pub artwork_url: Option<String>,
    pub tracks: Vec<ImportedTrack>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub entry_id: String,
    pub position: u32,
    pub track: CapsuleTrack,
    pub resolution: String,
    pub source_preference: Option<Platform>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackCandidate {
    pub provider: Platform,
    pub external_id: String,
    #[serde(flatten)]
    pub metadata: Metadata,
    pub confidence: f64,
    pub compatible: bool,
}

#[cfg(test)]
pub(crate) fn fixture(id: &str, title: &str) -> ImportedTrack {
    ImportedTrack {
        origin: Origin {
            platform: Platform::Spotify,
            external_id: id.into(),
            url: None,
        },
        metadata: Metadata {
            title: title.into(),
            artists: vec!["Artist".into()],
            album: Some("Album".into()),
            duration_ms: Some(200_000),
            artwork_url: None,
            isrc: None,
            explicit: None,
        },
    }
}
