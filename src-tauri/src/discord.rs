use std::sync::{atomic::{AtomicU64, Ordering}, Mutex};

use discord_rich_presence::activity::{Activity, Assets, Button, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

use crate::player::{Status, Track};

const REPO_URL: &str = "https://github.com/yvesdeniz/capsule";

#[derive(Debug, PartialEq, Eq)]
pub enum CoverArt {
    PublicUrl(String),
    NavidromeId(String),
}

pub fn library_artwork(
    imports: Option<&crate::db::Db>,
    provider: &crate::db::Db,
    track_id: &str,
    route: Option<&crate::import::playback::Route>,
    source: crate::settings::Source,
    allow_server: bool,
) -> Option<CoverArt> {
    use crate::import::model::Platform;
    use crate::settings::Source;

    let canonical = imports.and_then(|db| db.canonical_track(track_id).ok().flatten());
    if let Some(image) = canonical.as_ref()
        .and_then(|track| track.metadata.artwork_url.as_deref())
        .and_then(public_artwork_url)
    {
        return Some(CoverArt::PublicUrl(image));
    }

    let active_provider = route.map(|route| route.provider).unwrap_or(match source {
        Source::Navidrome => Platform::Navidrome,
        Source::Youtube => Platform::Youtube,
        Source::Soundcloud => Platform::Soundcloud,
        Source::Local => Platform::Local,
        Source::Apple => Platform::AppleMusic,
        Source::Spotify => Platform::Spotify,
    });
    let provider_matches = matches!((source, active_provider),
        (Source::Local, Platform::Local) | (Source::Navidrome, Platform::Navidrome)
        | (Source::Youtube, Platform::Youtube) | (Source::Apple, Platform::AppleMusic)
        | (Source::Spotify, Platform::Spotify) | (Source::Soundcloud, Platform::Soundcloud));
    let provider_id = route.map(|route| route.external_id.as_str()).or_else(|| {
        canonical.is_none().then_some(track_id)
    });
    if provider_matches {
        if let Some(art) = provider_id.and_then(|id| provider.artwork_for(id).ok().flatten()) {
            if let Some(image) = public_artwork_url(&art.template) {
                return Some(CoverArt::PublicUrl(image));
            }
            if allow_server && active_provider == Platform::Navidrome {
                if let Some(id) = crate::artwork::subsonic_id(&art.template).filter(|id| !id.is_empty()) {
                    return Some(CoverArt::NavidromeId(id.into()));
                }
            }
        }
    }
    if allow_server && active_provider == Platform::Navidrome {
        if let Some(id) = provider_id {
            return Some(CoverArt::NavidromeId(id.into()));
        }
    }

    // Imported IDs are UUIDs. Only verified mappings identify a public video.
    let video = if canonical.is_some() {
        imports.and_then(|db| db.mappings_for(track_id).ok()).and_then(|mappings| {
            mappings.into_iter().find(|mapping| mapping.provider == "youtube" && mapping.verified
                && crate::import::urls::video_id(&mapping.external_id)).map(|mapping| mapping.external_id)
        })
    } else if active_provider == Platform::Youtube {
        provider_id.filter(|id| crate::import::urls::video_id(id)).map(str::to_owned)
    } else {
        None
    };
    video.map(|id| CoverArt::PublicUrl(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")))
}

/// Discord fetches remote images itself; app schemes and authenticated URLs
/// cannot serve as public covers. Server credentials are handled separately.
pub fn public_artwork_url(template: &str) -> Option<String> {
    if template.len() > 8192 || template.chars().any(|c| c.is_control() || c == '\\') {
        return None;
    }
    let resolved = crate::artwork::resolve_template(template, 300);
    if resolved.chars().count() > 300 {
        return None;
    }
    let url = url::Url::parse(&resolved).ok()?;
    if !matches!(url.scheme(), "https" | "http") || !url.username().is_empty()
        || url.password().is_some() || url.fragment().is_some()
    {
        return None;
    }
    match url.host()? {
        url::Host::Domain(host) if host.contains('.') && host != "localhost"
            && !host.ends_with(".localhost") && !host.ends_with(".local") => {}
        url::Host::Ipv4(ip) if !ip.is_private() && !ip.is_loopback() && !ip.is_link_local()
            && !ip.is_unspecified() && !ip.is_broadcast() => {}
        url::Host::Ipv6(ip) if !ip.is_loopback() && !ip.is_unspecified()
            && (ip.segments()[0] & 0xfe00) != 0xfc00
            && (ip.segments()[0] & 0xffc0) != 0xfe80
            && ip.to_ipv4_mapped().is_none() => {}
        _ => return None,
    }
    if url.query_pairs().any(|(key, _)| {
        let key = key.to_ascii_lowercase();
        matches!(key.as_str(), "u" | "p" | "t" | "s" | "auth" | "authorization"
            | "token" | "access_token" | "api_key" | "apikey" | "key" | "password"
            | "signature" | "sig" | "jwt" | "policy" | "key-pair-id" | "expires")
            || key.starts_with("x-amz-") || key.starts_with("x-goog-")
    }) {
        return None;
    }
    Some(resolved)
}

pub struct Presence {
    client: Mutex<Option<DiscordIpcClient>>,
    app_id: String,
    generation: AtomicU64,
}

impl Presence {
    /// Reserve before starting artwork lookup so a slow response cannot replace
    /// a later track's activity, even when both IPC jobs are already queued.
    pub fn begin_update(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst).wrapping_add(1)
    }

    fn is_current(&self, ticket: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == ticket
    }

    pub fn new(app_id: String) -> Self {
        Self { client: Mutex::new(None), app_id, generation: AtomicU64::new(0) }
    }

    fn ensure(&self, guard: &mut Option<DiscordIpcClient>) -> bool {
        if guard.is_some() {
            return true;
        }
        let mut client = DiscordIpcClient::new(&self.app_id);
        match client.connect() {
            Ok(()) => {
                tracing::info!("discord rich presence connected");
                *guard = Some(client);
                true
            }
            Err(e) => {
                tracing::debug!(error = %e, "discord not reachable");
                false
            }
        }
    }

    pub fn show(&self, ticket: u64, track: &Track, status: Status, image: Option<&str>) {
        let mut guard = self.client.lock().expect("discord mutex");
        if !self.is_current(ticket) {
            return;
        }
        if !self.ensure(&mut guard) {
            return;
        }
        let Some(client) = guard.as_mut() else { return };

        let details = pad(&track.title);
        let state = pad(&track.artist);

        let mut assets = Assets::new();
        if let Some(url) = image.filter(|url| url.chars().count() <= 300) {
            assets = assets.large_image(url);
        }
        let album = pad(&track.album);
        if !track.album.trim().is_empty() {
            assets = assets.large_text(&album);
        }

        let mut activity = Activity::new()
            .details(&details)
            .state(&state)
            .assets(assets)
            .buttons(vec![Button::new("Get capsule", REPO_URL)]);

        let started = start_epoch(status, track.duration_ms);
        if let Some(start) = started {
            activity = activity.timestamps(Timestamps::new().start(start));
        }

        if !self.is_current(ticket) {
            return;
        }
        if let Err(e) = client.set_activity(activity) {
            tracing::debug!(error = %e, "discord set_activity failed; dropping connection");
            *guard = None;
        }
    }

    pub fn clear(&self) {
        self.clear_current(self.begin_update());
    }

    pub fn clear_current(&self, ticket: u64) {
        let mut guard = self.client.lock().expect("discord mutex");
        if !self.is_current(ticket) {
            return;
        }
        let Some(client) = guard.as_mut() else { return };
        if let Err(e) = client.clear_activity() {
            tracing::debug!(error = %e, "discord clear failed; dropping connection");
            *guard = None;
        }
    }
}

fn pad(s: &str) -> String {
    const ZWSP: char = '\u{200B}';
    if s.trim().is_empty() {
        "-".repeat(2)
    } else if s.chars().count() < 2 {
        format!("{s}{ZWSP}")
    } else {
        s.to_string()
    }
}

fn start_epoch(status: Status, _duration_ms: u64) -> Option<i64> {
    if status != Status::Playing {
        return None;
    }
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imported(art: Option<&str>) -> crate::db::Db {
        let mut db = crate::db::Db::open_in_memory().unwrap();
        db.upsert_track(&crate::db::TrackRow {
            id: "canonical-id".into(), title: "Song".into(), artists: "[\"Artist\"]".into(),
            album: None, duration_ms: Some(180000), artwork_url: art.map(str::to_owned),
            isrc: None, created_at: String::new(), updated_at: String::new(),
        }).unwrap();
        db
    }

    fn youtube_mapping(db: &mut crate::db::Db, verified: bool) {
        db.upsert_playback_mapping(&crate::db::PlaybackMappingRow {
            track_id: "canonical-id".into(), provider: "youtube".into(),
            external_id: "dQw4w9WgXcQ".into(), confidence: 1.0, verified,
            matched_at: String::new(),
        }).unwrap();
    }

    #[test]
    fn rpc_cover_uses_imported_public_art_without_an_album_or_lastfm() {
        let imports = imported(Some("https://i.scdn.co/image/album"));
        let provider = crate::db::Db::open_in_memory().unwrap();
        assert_eq!(library_artwork(Some(&imports), &provider, "canonical-id", None,
            crate::settings::Source::Local, false),
            Some(CoverArt::PublicUrl("https://i.scdn.co/image/album".into())));
    }

    #[test]
    fn rpc_cover_falls_back_to_verified_youtube_mapping() {
        let mut imports = imported(None);
        youtube_mapping(&mut imports, true);
        let provider = crate::db::Db::open_in_memory().unwrap();
        assert_eq!(library_artwork(Some(&imports), &provider, "canonical-id", None,
            crate::settings::Source::Local, false),
            Some(CoverArt::PublicUrl("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg".into())));
        youtube_mapping(&mut imports, false);
        assert_eq!(library_artwork(Some(&imports), &provider, "canonical-id", None,
            crate::settings::Source::Local, false), None);
    }

    #[test]
    fn rpc_cover_too_long_for_discord_uses_public_fallback() {
        let long = format!("https://i.scdn.co/image/{}", "a".repeat(300));
        let mut imports = imported(Some(&long));
        youtube_mapping(&mut imports, true);
        let provider = crate::db::Db::open_in_memory().unwrap();
        assert_eq!(library_artwork(Some(&imports), &provider, "canonical-id", None,
            crate::settings::Source::Local, false),
            Some(CoverArt::PublicUrl("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg".into())));
        assert!(public_artwork_url(&long).is_none());
    }

    #[test]
    fn rpc_cover_late_lookup_cannot_replace_newer_track() {
        let presence = Presence::new("unused".into());
        let old = presence.begin_update();
        let current = presence.begin_update();
        assert!(!presence.is_current(old));
        assert!(presence.is_current(current));
    }

    #[test]
    fn rpc_cover_clear_invalidates_pending_lookup_without_client() {
        let presence = Presence::new("unused".into());
        let pending = presence.begin_update();
        presence.clear();
        assert!(!presence.is_current(pending));
    }

    #[test]
    fn rpc_cover_rejects_local_custom_and_sensitive_urls() {
        let provider = crate::db::Db::open_in_memory().unwrap();
        for art in ["local:C:/Music/song.mp3", "file:///C:/cover.jpg", "artwork://localhost/track",
            "http://127.0.0.1/cover.jpg", "https://user:password@example.com/cover.jpg",
            "https://example.com/cover.jpg?u=user&t=token&s=salt", "https://example.com/cover.jpg?X-Amz-Signature=secret"] {
            let mut imports = imported(Some(art));
            youtube_mapping(&mut imports, true);
            assert_eq!(library_artwork(Some(&imports), &provider, "canonical-id", None,
                crate::settings::Source::Local, false),
                Some(CoverArt::PublicUrl("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg".into())), "{art}");
        }
    }

    #[test]
    fn rpc_cover_navidrome_requires_opt_in_and_uses_mapped_id() {
        let imports = imported(None);
        let provider = crate::db::Db::open_in_memory().unwrap();
        let route = crate::import::playback::Route {
            provider: crate::import::model::Platform::Navidrome, external_id: "server-song".into(),
        };
        assert_eq!(library_artwork(Some(&imports), &provider, "canonical-id", Some(&route),
            crate::settings::Source::Local, false), None);
        assert_eq!(library_artwork(Some(&imports), &provider, "canonical-id", Some(&route),
            crate::settings::Source::Local, true), Some(CoverArt::NavidromeId("server-song".into())));
    }

    #[test]
    fn rpc_cover_resolves_provider_templates_and_navidrome_cover_references() {
        let mut provider = crate::db::Db::open_in_memory().unwrap();
        let mut song = crate::db::SongUpsert {
            id: "provider-id".into(),
            artwork_url: Some("https://example.com/{w}x{h}bb.{f}".into()),
            ..Default::default()
        };
        provider.upsert_songs(&[song.clone()]).unwrap();
        assert_eq!(library_artwork(None, &provider, "provider-id", None,
            crate::settings::Source::Youtube, false),
            Some(CoverArt::PublicUrl("https://example.com/300x300bb.jpg".into())));
        song.artwork_url = Some("subsonic:album-cover".into());
        provider.upsert_songs(&[song]).unwrap();
        assert_eq!(library_artwork(None, &provider, "provider-id", None,
            crate::settings::Source::Navidrome, false), None);
        assert_eq!(library_artwork(None, &provider, "provider-id", None,
            crate::settings::Source::Navidrome, true), Some(CoverArt::NavidromeId("album-cover".into())));
    }

    #[test]
    fn padding_survives_trimming() {
        let padded = pad("e");
        assert_eq!(padded.chars().count(), 2);
        assert_eq!(padded.trim().chars().count(), 2, "must not collapse back to one char");
    }

    #[test]
    fn a_one_letter_album_is_padded() {
        assert_eq!(pad("E").chars().count(), 2);
    }

    #[test]
    fn short_or_empty_values_are_padded_to_discords_minimum() {
        assert_eq!(pad("").chars().count(), 2);
        assert_eq!(pad("   ").chars().count(), 2);
        assert_eq!(pad("5").chars().count(), 2);
        assert_eq!(pad("1L"), "1L");
        assert_eq!(pad("Carhartt"), "Carhartt");
    }

    #[test]
    fn only_a_playing_track_gets_a_clock() {
        assert!(start_epoch(Status::Playing, 1000).is_some());
        assert!(start_epoch(Status::Paused, 1000).is_none());
        assert!(start_epoch(Status::Idle, 1000).is_none());
    }
}
