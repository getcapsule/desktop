use serde::Deserialize;

use crate::auth::Tokens;
use crate::db::{AlbumUpsert, ArtistUpsert, PlaylistUpsert, SongUpsert};

const BASE: &str = "https://api.music.apple.com";

const ORIGIN: &str = "https://music.apple.com";
const PAGE: u32 = 100;
const MAX_PAGES: u32 = 500;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unauthorized - token rejected")]
    Unauthorized,
    #[error("rate limited after retries")]
    RateLimited,
    #[error("apple returned {status}: {body}")]
    Status { status: u16, body: String },
}

pub struct Client {
    http: reqwest::Client,
    tokens: Tokens,
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Artwork {
    url: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayParams {
    catalog_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Item<A> {
    id: String,
    attributes: Option<A>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SongAttrs {
    name: Option<String>,
    artist_name: Option<String>,
    album_name: Option<String>,
    duration_in_millis: Option<u64>,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    artwork: Option<Artwork>,
    play_params: Option<PlayParams>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlbumAttrs {
    name: Option<String>,
    artist_name: Option<String>,
    track_count: Option<u32>,
    release_date: Option<String>,
    artwork: Option<Artwork>,
    play_params: Option<PlayParams>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaylistAttrs {
    name: Option<String>,
    description: Option<Description>,
    can_edit: Option<bool>,
    artwork: Option<Artwork>,
}

#[derive(Debug, Deserialize)]
struct Description {
    standard: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArtistAttrs {
    name: Option<String>,
    artwork: Option<Artwork>,
}

impl Client {
    pub fn new(tokens: Tokens) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .user_agent("capsule/0.1")
            .gzip(true)
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { http, tokens })
    }

    async fn get_raw(&self, path_or_url: &str) -> Result<String, ApiError> {
        let url = if path_or_url.starts_with("http") {
            path_or_url.to_string()
        } else {
            format!("{BASE}{path_or_url}")
        };

        let mut delay = std::time::Duration::from_millis(500);
        for attempt in 0..4 {
            let resp = self
                .http
                .get(&url)
                .bearer_auth(&self.tokens.developer_token)
                .header("Music-User-Token", &self.tokens.music_user_token)
                .header("Origin", ORIGIN)
                .header("Referer", format!("{ORIGIN}/"))
                .send()
                .await?;

            let status = resp.status();
            if status.is_success() {
                return Ok(resp.text().await?);
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(ApiError::Unauthorized);
            }
            if status.as_u16() == 429 || status.is_server_error() {
                if attempt == 3 {
                    return Err(ApiError::RateLimited);
                }
                tracing::warn!(%url, status = status.as_u16(), ?delay, "backing off");
                tokio::time::sleep(delay).await;
                delay *= 2;
                continue;
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Status { status: status.as_u16(), body: truncate(&body, 300) });
        }
        Err(ApiError::RateLimited)
    }

    async fn get_all<T: serde::de::DeserializeOwned>(
        &self,
        first: &str,
        mut on_page: impl FnMut(Vec<T>),
    ) -> Result<(), ApiError> {
        let mut next = Some(first.to_string());
        let mut pages = 0;
        while let Some(url) = next {
            let body = self.get_raw(&url).await?;
            let page: Page<T> = serde_json::from_str(&body)
                .map_err(|e| ApiError::Status { status: 0, body: format!("parse: {e}") })?;
            next = page.next;
            on_page(page.data);
            pages += 1;
            if pages >= MAX_PAGES {
                tracing::warn!("stopped paginating at {MAX_PAGES} pages - library truncated");
                break;
            }
        }
        Ok(())
    }

    pub async fn library_songs(
        &self,
        mut sink: impl FnMut(Vec<SongUpsert>),
    ) -> Result<(), ApiError> {
        self.get_all::<Item<SongAttrs>>(
            &format!("/v1/me/library/songs?limit={PAGE}"),
            |items| sink(items.into_iter().map(song_from).collect()),
        )
        .await
    }

    pub async fn library_albums(
        &self,
        mut sink: impl FnMut(Vec<AlbumUpsert>),
    ) -> Result<(), ApiError> {
        self.get_all::<Item<AlbumAttrs>>(
            &format!("/v1/me/library/albums?limit={PAGE}"),
            |items| sink(items.into_iter().map(album_from).collect()),
        )
        .await
    }

    pub async fn library_playlists(
        &self,
        mut sink: impl FnMut(Vec<PlaylistUpsert>),
    ) -> Result<(), ApiError> {
        self.get_all::<Item<PlaylistAttrs>>(
            &format!("/v1/me/library/playlists?limit={PAGE}"),
            |items| sink(items.into_iter().map(playlist_from).collect()),
        )
        .await
    }

    pub async fn library_artists(
        &self,
        mut sink: impl FnMut(Vec<ArtistUpsert>),
    ) -> Result<(), ApiError> {
        self.get_all::<Item<ArtistAttrs>>(
            &format!("/v1/me/library/artists?limit={PAGE}"),
            |items| sink(items.into_iter().map(artist_from).collect()),
        )
        .await
    }

    pub async fn album_tracks(&self, album_id: &str) -> Result<Vec<SongUpsert>, ApiError> {
        let mut out = Vec::new();
        self.get_all::<Item<SongAttrs>>(
            &format!("/v1/me/library/albums/{album_id}/tracks?limit={PAGE}"),
            |items| {
                for it in items {
                    let mut s = song_from(it);
                    s.album_id = Some(album_id.to_string());
                    out.push(s);
                }
            },
        )
        .await?;
        Ok(out)
    }

    pub async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<SongUpsert>, ApiError> {
        let mut out = Vec::new();
        self.get_all::<Item<SongAttrs>>(
            &format!("/v1/me/library/playlists/{playlist_id}/tracks?limit={PAGE}"),
            |items| out.extend(items.into_iter().map(song_from)),
        )
        .await?;
        Ok(out)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

fn song_from(it: Item<SongAttrs>) -> SongUpsert {
    let a = it.attributes.unwrap_or(SongAttrs {
        name: None,
        artist_name: None,
        album_name: None,
        duration_in_millis: None,
        track_number: None,
        disc_number: None,
        artwork: None,
        play_params: None,
    });
    let art = a.artwork;
    SongUpsert {
        catalog_id: a.play_params.and_then(|p| p.catalog_id),
        name: a.name.unwrap_or_default(),
        artist_name: a.artist_name.unwrap_or_default(),
        album_name: a.album_name.unwrap_or_default(),
        album_id: None,
        duration_ms: a.duration_in_millis.unwrap_or(0),
        artwork_url: art.as_ref().and_then(|x| x.url.clone()),
        artwork_width: art.as_ref().and_then(|x| x.width),
        artwork_height: art.as_ref().and_then(|x| x.height),
        track_number: a.track_number,
        disc_number: a.disc_number,
        added_at: None,
        id: it.id,
    }
}

fn album_from(it: Item<AlbumAttrs>) -> AlbumUpsert {
    let a = it.attributes.unwrap_or(AlbumAttrs {
        name: None,
        artist_name: None,
        track_count: None,
        release_date: None,
        artwork: None,
        play_params: None,
    });
    let art = a.artwork;
    AlbumUpsert {
        catalog_id: a.play_params.and_then(|p| p.catalog_id),
        name: a.name.unwrap_or_default(),
        artist_name: a.artist_name.unwrap_or_default(),
        artwork_url: art.as_ref().and_then(|x| x.url.clone()),
        artwork_width: art.as_ref().and_then(|x| x.width),
        artwork_height: art.as_ref().and_then(|x| x.height),
        release_date: a.release_date,
        track_count: a.track_count.unwrap_or(0),
        added_at: None,
        id: it.id,
    }
}

fn artist_from(it: Item<ArtistAttrs>) -> ArtistUpsert {
    let a = it.attributes.unwrap_or(ArtistAttrs { name: None, artwork: None });
    ArtistUpsert {
        name: a.name.unwrap_or_default(),
        artwork_url: a.artwork.and_then(|x| x.url),
        id: it.id,
    }
}

fn playlist_from(it: Item<PlaylistAttrs>) -> PlaylistUpsert {
    let a = it.attributes.unwrap_or(PlaylistAttrs {
        name: None,
        description: None,
        can_edit: None,
        artwork: None,
    });
    let art = a.artwork;
    PlaylistUpsert {
        name: a.name.unwrap_or_default(),
        description: a.description.and_then(|d| d.standard),
        artwork_url: art.as_ref().and_then(|x| x.url.clone()),
        artwork_width: art.as_ref().and_then(|x| x.width),
        artwork_height: art.as_ref().and_then(|x| x.height),
        can_edit: a.can_edit.unwrap_or(false),
        id: it.id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SONGS: &str = r#"{
      "next": "/v1/me/library/songs?offset=100",
      "data": [
        {
          "id": "i.abc123",
          "type": "library-songs",
          "attributes": {
            "name": "TL;DR",
            "artistName": "Bladee, Ecco2k & Thaiboy Digital",
            "albumName": "TL;DR - Single",
            "durationInMillis": 191000,
            "trackNumber": 1,
            "discNumber": 1,
            "artwork": { "url": "https://is1.mzstatic.com/image/{w}x{h}bb.jpg" },
            "playParams": { "id": "i.abc123", "kind": "song", "isLibrary": true, "catalogId": "1550924665" }
          }
        }
      ]
    }"#;

    #[test]
    fn parses_song_page_and_keeps_catalog_id() {
        let page: Page<Item<SongAttrs>> = serde_json::from_str(SONGS).unwrap();
        assert_eq!(page.next.as_deref(), Some("/v1/me/library/songs?offset=100"));
        let s = song_from(page.data.into_iter().next().unwrap());
        assert_eq!(s.id, "i.abc123");
        assert_eq!(s.catalog_id.as_deref(), Some("1550924665"));
        assert_eq!(s.duration_ms, 191_000);
        assert_eq!(s.artist_name, "Bladee, Ecco2k & Thaiboy Digital");
    }

    #[test]
    fn missing_attributes_do_not_fail_the_page() {
        let json = r#"{"data":[{"id":"i.1","type":"library-songs"}]}"#;
        let page: Page<Item<SongAttrs>> = serde_json::from_str(json).unwrap();
        let s = song_from(page.data.into_iter().next().unwrap());
        assert_eq!(s.id, "i.1");
        assert_eq!(s.name, "");
        assert_eq!(s.duration_ms, 0);
        assert_eq!(s.catalog_id, None);
    }

    #[test]
    fn absent_next_ends_pagination() {
        let json = r#"{"data":[]}"#;
        let page: Page<Item<SongAttrs>> = serde_json::from_str(json).unwrap();
        assert!(page.next.is_none());
        assert!(page.data.is_empty());
    }

    #[test]
    fn parses_album_page() {
        let json = r#"{"data":[{"id":"l.xyz","attributes":{
            "name":"333","artistName":"Bladee","trackCount":13,
            "releaseDate":"2020-10-16",
            "artwork":{"url":"https://is1.mzstatic.com/{w}x{h}.jpg"},
            "playParams":{"catalogId":"1533070849"}}}]}"#;
        let page: Page<Item<AlbumAttrs>> = serde_json::from_str(json).unwrap();
        let a = album_from(page.data.into_iter().next().unwrap());
        assert_eq!(a.name, "333");
        assert_eq!(a.track_count, 13);
        assert_eq!(a.catalog_id.as_deref(), Some("1533070849"));
    }

    #[test]
    fn parses_playlist_with_nested_description() {
        let json = r#"{"data":[{"id":"p.1","attributes":{
            "name":"drain","canEdit":true,
            "description":{"standard":"cold"}}}]}"#;
        let page: Page<Item<PlaylistAttrs>> = serde_json::from_str(json).unwrap();
        let p = playlist_from(page.data.into_iter().next().unwrap());
        assert_eq!(p.name, "drain");
        assert!(p.can_edit);
        assert_eq!(p.description.as_deref(), Some("cold"));
    }

    #[test]
    fn truncate_keeps_error_bodies_bounded() {
        let long = "x".repeat(1000);
        assert_eq!(truncate(&long, 10).chars().count(), 11); // 10 + ellipsis
        assert_eq!(truncate("short", 10), "short");
    }
}
