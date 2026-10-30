use serde::Deserialize;

use crate::db::{AlbumUpsert, ArtistUpsert, PlaylistUpsert, SongUpsert};

pub const API_VERSION: &str = "1.16.1";
pub const CLIENT_NAME: &str = "capsule";

#[derive(Debug, thiserror::Error)]
pub enum SubsonicError {
    #[error("{0}")]
    Http(String),
    #[error("wrong username or password")]
    Unauthorized,
    #[error("server url is not usable: {0}")]
    InvalidUrl(String),
    #[error("server returned error {code}: {message}")]
    Api { code: i32, message: String },
    #[error("server response was not valid subsonic json: {0}")]
    Malformed(String),
}

impl From<reqwest::Error> for SubsonicError {
    fn from(e: reqwest::Error) -> Self {
        let what = if e.is_timeout() {
            "the server took too long to respond"
        } else if e.is_connect() {
            "could not reach the server"
        } else if e.is_decode() {
            "the server sent a malformed response"
        } else if e.is_body() {
            "the connection dropped mid-response"
        } else {
            "the request failed"
        };
        SubsonicError::Http(what.to_string())
    }
}

pub fn auth_token(password: &str, salt: &str) -> String {
    format!("{:x}", md5::compute(format!("{password}{salt}")))
}

pub fn auth_query(user: &str, password: &str, salt: &str) -> Vec<(String, String)> {
    vec![
        ("u".into(), user.to_string()),
        ("t".into(), auth_token(password, salt)),
        ("s".into(), salt.to_string()),
        ("v".into(), API_VERSION.to_string()),
        ("c".into(), CLIENT_NAME.to_string()),
        ("f".into(), "json".to_string()),
    ]
}

pub fn normalize_base_url(raw: &str) -> Result<String, SubsonicError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(SubsonicError::InvalidUrl("empty".into()));
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    Ok(with_scheme.trim_end_matches('/').to_string())
}

pub fn is_insecure(base_url: &str) -> bool {
    base_url.trim().starts_with("http://")
}

pub fn random_salt() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let pid = u128::from(std::process::id());
    format!("{:x}", md5::compute(format!("{nanos}{pid}")))[..12].to_string()
}

pub const ARTWORK_PREFIX: &str = "subsonic:";

const ALBUM_PAGE: u32 = 500;
const MAX_ALBUM_PAGES: u32 = 500;

pub fn cover_art_ref(id: Option<String>) -> Option<String> {
    id.filter(|s| !s.trim().is_empty()).map(|s| format!("{ARTWORK_PREFIX}{s}"))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ArtistIndex {
    pub index: Vec<ArtistIndexEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ArtistIndexEntry {
    pub artist: Vec<WireArtist>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WireArtist {
    pub id: String,
    pub name: String,
    #[serde(rename = "coverArt")]
    pub cover_art: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AlbumList {
    pub album: Vec<WireAlbum>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WireAlbum {
    pub id: String,
    pub name: String,
    pub artist: String,
    #[serde(rename = "songCount")]
    pub song_count: u32,
    pub year: Option<i64>,
    #[serde(rename = "coverArt")]
    pub cover_art: Option<String>,
    pub created: Option<String>,
}

pub fn artists_from(index: ArtistIndex) -> Vec<ArtistUpsert> {
    index
        .index
        .into_iter()
        .flat_map(|e| e.artist)
        .map(|a| ArtistUpsert {
            id: a.id,
            name: a.name,
            artwork_url: cover_art_ref(a.cover_art),
        })
        .collect()
}

pub fn album_from(a: WireAlbum) -> AlbumUpsert {
    AlbumUpsert {
        id: a.id,
        catalog_id: None,
        name: a.name,
        artist_name: a.artist,
        artwork_url: cover_art_ref(a.cover_art),
        release_date: a.year.map(|y| y.to_string()),
        track_count: a.song_count,
        added_at: a.created,
        artwork_width: None,
        artwork_height: None,
    }
}

const ALBUM_CONCURRENCY: usize = 8;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SongSyncReport {
    pub skipped_albums: u32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WireAlbumDetail {
    pub id: String,
    pub song: Vec<WireSong>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WireSong {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Option<u64>,
    pub track: Option<i64>,
    #[serde(rename = "discNumber")]
    pub disc_number: Option<i64>,
    #[serde(rename = "coverArt")]
    pub cover_art: Option<String>,
    pub created: Option<String>,
}

pub fn song_from(s: WireSong, album_id: &str) -> SongUpsert {
    SongUpsert {
        id: s.id,
        catalog_id: None,
        name: s.title,
        artist_name: s.artist,
        album_name: s.album,
        album_id: Some(album_id.to_string()),
        duration_ms: s.duration.unwrap_or(0).saturating_mul(1000),
        artwork_url: cover_art_ref(s.cover_art),
        track_number: s.track,
        disc_number: s.disc_number,
        added_at: s.created,
        artwork_width: None,
        artwork_height: None,
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PlaylistList {
    pub playlist: Vec<WirePlaylist>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WirePlaylist {
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    #[serde(rename = "coverArt")]
    pub cover_art: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WirePlaylistDetail {
    pub id: String,
    pub entry: Vec<WirePlaylistEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WirePlaylistEntry {
    pub id: String,
}

pub fn playlist_from(p: WirePlaylist) -> PlaylistUpsert {
    PlaylistUpsert {
        id: p.id,
        name: p.name,
        description: p.comment.filter(|c| !c.trim().is_empty()),
        artwork_url: cover_art_ref(p.cover_art),
        can_edit: false,
        artwork_width: None,
        artwork_height: None,
    }
}

#[derive(Debug, Clone)]
pub struct Credentials {
    pub base_url: String,
    pub username: String,
    pub password: String,
}

pub struct Client {
    http: reqwest::Client,
    creds: Credentials,
}

impl Client {
    pub fn new(creds: Credentials) -> Result<Self, SubsonicError> {
        let base_url = normalize_base_url(&creds.base_url)?;
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("capsule/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(60))
                .build()?,
            creds: Credentials { base_url, ..creds },
        })
    }

    pub fn base_url(&self) -> &str {
        &self.creds.base_url
    }

    pub fn clone_for_artwork(&self) -> Self {
        Self { http: self.http.clone(), creds: self.creds.clone() }
    }

    pub fn signed_url(&self, method: &str, extra: &[(&str, String)]) -> String {
        let mut params = auth_query(&self.creds.username, &self.creds.password, &random_salt());
        for (k, v) in extra {
            params.push(((*k).to_string(), v.clone()));
        }
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect();
        format!("{}/rest/{}.view?{}", self.creds.base_url, method, query.join("&"))
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        key: &str,
        extra: &[(&str, String)],
    ) -> Result<T, SubsonicError> {
        let url = self.signed_url(method, extra);
        let body = self.http.get(&url).send().await?.text().await?;
        parse_envelope(&body, key)
    }

    pub async fn ping(&self) -> Result<(), SubsonicError> {
        #[derive(Deserialize)]
        struct Empty {}
        let _: Empty = self.call("ping", "", &[]).await?;
        Ok(())
    }

    pub async fn library_artists(
        &self,
        mut sink: impl FnMut(Vec<ArtistUpsert>),
    ) -> Result<(), SubsonicError> {
        let index: ArtistIndex = self.call("getArtists", "artists", &[]).await?;
        sink(artists_from(index));
        Ok(())
    }

    pub async fn library_albums(
        &self,
        mut sink: impl FnMut(Vec<AlbumUpsert>),
    ) -> Result<(), SubsonicError> {
        let mut offset = 0u32;
        for _ in 0..MAX_ALBUM_PAGES {
            let list: AlbumList = self
                .call(
                    "getAlbumList2",
                    "albumList2",
                    &[
                        ("type", "alphabeticalByName".to_string()),
                        ("size", ALBUM_PAGE.to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            let n = list.album.len() as u32;
            sink(list.album.into_iter().map(album_from).collect());
            if n < ALBUM_PAGE {
                return Ok(());
            }
            offset += n;
        }
        tracing::warn!("album pagination hit the page cap; library may be truncated");
        Ok(())
    }

    pub async fn library_playlists(
        &self,
        mut sink: impl FnMut(Vec<PlaylistUpsert>),
    ) -> Result<(), SubsonicError> {
        let list: PlaylistList = self.call("getPlaylists", "playlists", &[]).await?;
        sink(list.playlist.into_iter().map(playlist_from).collect());
        Ok(())
    }

    pub fn stream_url(&self, id: &str) -> String {
        self.signed_url("stream", &[("id", id.to_string())])
    }

    pub fn cover_art_url(&self, id: &str, size: u32) -> String {
        self.signed_url("getCoverArt", &[("id", id.to_string()), ("size", size.to_string())])
    }

    pub async fn scrobble(&self, id: &str, submission: bool) -> Result<(), SubsonicError> {
        #[derive(Deserialize)]
        struct Empty {}
        let _: Empty = self
            .call(
                "scrobble",
                "",
                &[("id", id.to_string()), ("submission", submission.to_string())],
            )
            .await?;
        Ok(())
    }

    pub async fn playlist_track_ids(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<String>, SubsonicError> {
        let detail: WirePlaylistDetail = self
            .call("getPlaylist", "playlist", &[("id", playlist_id.to_string())])
            .await?;
        Ok(detail.entry.into_iter().map(|e| e.id).collect())
    }

    fn spawn_album(
        &self,
        set: &mut tokio::task::JoinSet<(String, Result<WireAlbumDetail, SubsonicError>)>,
        id: String,
    ) {
        let url = self.signed_url("getAlbum", &[("id", id.clone())]);
        let http = self.http.clone();
        set.spawn(async move {
            let res: Result<WireAlbumDetail, SubsonicError> = async {
                let body = http.get(&url).send().await?.text().await?;
                parse_envelope::<WireAlbumDetail>(&body, "album")
            }
            .await;
            (id, res)
        });
    }

    pub async fn library_songs(
        &self,
        mut sink: impl FnMut(Vec<SongUpsert>),
    ) -> Result<SongSyncReport, SubsonicError> {
        let mut album_ids: Vec<String> = Vec::new();
        self.library_albums(|rows| album_ids.extend(rows.into_iter().map(|a| a.id))).await?;

        let mut report = SongSyncReport::default();
        let mut queue = album_ids.into_iter();
        let mut set = tokio::task::JoinSet::new();

        for _ in 0..ALBUM_CONCURRENCY {
            match queue.next() {
                Some(id) => self.spawn_album(&mut set, id),
                None => break,
            }
        }

        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((album_id, Ok(detail))) => {
                    let rows: Vec<SongUpsert> =
                        detail.song.into_iter().map(|s| song_from(s, &album_id)).collect();
                    if !rows.is_empty() {
                        sink(rows);
                    }
                }
                Ok((album_id, Err(SubsonicError::Unauthorized))) => {
                    tracing::warn!(%album_id, "unauthorized during album fetch; aborting sync");
                    set.abort_all();
                    return Err(SubsonicError::Unauthorized);
                }
                Ok((album_id, Err(e))) => {
                    tracing::warn!(%album_id, error = %e, "album fetch failed; skipping");
                    report.skipped_albums += 1;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "album task failed; skipping");
                    report.skipped_albums += 1;
                }
            }
            if let Some(id) = queue.next() {
                self.spawn_album(&mut set, id);
            }
        }

        Ok(report)
    }
}

pub fn parse_envelope<T: serde::de::DeserializeOwned>(
    body: &str,
    key: &str,
) -> Result<T, SubsonicError> {
    let root: serde_json::Value =
        serde_json::from_str(body).map_err(|e| SubsonicError::Malformed(e.to_string()))?;

    let resp = root
        .get("subsonic-response")
        .ok_or_else(|| SubsonicError::Malformed("missing subsonic-response".into()))?;

    if resp.get("status").and_then(|s| s.as_str()) == Some("failed") {
        let err = resp.get("error");
        let code = err.and_then(|e| e.get("code")).and_then(|c| c.as_i64()).unwrap_or(0) as i32;
        let message = err
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error")
            .to_string();
        return match code {
            40 | 41 => Err(SubsonicError::Unauthorized),
            _ => Err(SubsonicError::Api { code, message }),
        };
    }

    let empty = || serde_json::Value::Object(serde_json::Map::new());
    let payload = if key.is_empty() {
        empty()
    } else {
        match resp.get(key) {
            None | Some(serde_json::Value::Null) => empty(),
            Some(v) => v.clone(),
        }
    };

    serde_json::from_value(payload).map_err(|e| SubsonicError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct Pong {}

    #[derive(Debug, Default, serde::Deserialize, PartialEq)]
    #[serde(default)]
    struct Artists {
        index: Vec<serde_json::Value>,
    }

    #[test]
    fn parses_an_ok_envelope() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
        let _: Pong = parse_envelope(body, "").unwrap();
    }

    #[test]
    fn extracts_a_keyed_payload() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
                       "artists":{"index":[]}}}"#;
        let a: Artists = parse_envelope(body, "artists").unwrap();
        assert_eq!(a.index.len(), 0);
    }

    #[test]
    fn wrong_password_maps_to_unauthorized() {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
                       "error":{"code":40,"message":"Wrong username or password"}}}"#;
        let err = parse_envelope::<Pong>(body, "").unwrap_err();
        assert!(matches!(err, SubsonicError::Unauthorized), "got {err:?}");
    }

    #[test]
    fn ldap_token_rejection_also_maps_to_unauthorized() {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
                       "error":{"code":41,"message":"Token auth not supported"}}}"#;
        assert!(matches!(
            parse_envelope::<Pong>(body, "").unwrap_err(),
            SubsonicError::Unauthorized
        ));
    }

    #[test]
    fn other_error_codes_keep_code_and_message() {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
                       "error":{"code":70,"message":"Data not found"}}}"#;
        match parse_envelope::<Pong>(body, "").unwrap_err() {
            SubsonicError::Api { code, message } => {
                assert_eq!(code, 70);
                assert_eq!(message, "Data not found");
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn garbage_is_malformed_not_a_panic() {
        assert!(matches!(
            parse_envelope::<Pong>("not json", "").unwrap_err(),
            SubsonicError::Malformed(_)
        ));
        assert!(matches!(
            parse_envelope::<Pong>("{}", "").unwrap_err(),
            SubsonicError::Malformed(_)
        ));
    }

    #[test]
    fn maps_an_artist_index_to_upserts() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","artists":{
            "index":[
              {"name":"B","artist":[{"id":"ar-1","name":"Bladee","albumCount":3}]},
              {"name":"E","artist":[{"id":"ar-2","name":"Ecco2k","coverArt":"ar-2"}]}
            ]}}}"#;
        let idx: ArtistIndex = parse_envelope(body, "artists").unwrap();
        let rows = artists_from(idx);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "ar-1");
        assert_eq!(rows[0].name, "Bladee");
        assert_eq!(rows[0].artwork_url, None, "no coverArt means no artwork");
        assert_eq!(rows[1].artwork_url.as_deref(), Some("subsonic:ar-2"));
    }

    #[test]
    fn maps_an_album_list_to_upserts() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","albumList2":{
            "album":[{"id":"al-1","name":"333","artist":"Bladee","songCount":12,
                      "year":2020,"coverArt":"al-1","created":"2024-01-05T10:00:00Z"}]}}}"#;
        let list: AlbumList = parse_envelope(body, "albumList2").unwrap();
        let rows: Vec<_> = list.album.into_iter().map(album_from).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "al-1");
        assert_eq!(rows[0].name, "333");
        assert_eq!(rows[0].artist_name, "Bladee");
        assert_eq!(rows[0].track_count, 12);
        assert_eq!(rows[0].release_date.as_deref(), Some("2020"));
        assert_eq!(rows[0].artwork_url.as_deref(), Some("subsonic:al-1"));
        assert_eq!(rows[0].added_at.as_deref(), Some("2024-01-05T10:00:00Z"));
        assert_eq!(rows[0].catalog_id, None, "catalog_id is an Apple concept");
        assert_eq!(rows[0].artwork_width, None, "subsonic does not report dimensions");
    }

    #[test]
    fn album_missing_optional_fields_still_maps() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","albumList2":{
            "album":[{"id":"al-2","name":"Untitled"}]}}}"#;
        let list: AlbumList = parse_envelope(body, "albumList2").unwrap();
        let rows: Vec<_> = list.album.into_iter().map(album_from).collect();
        assert_eq!(rows[0].artist_name, "");
        assert_eq!(rows[0].track_count, 0);
        assert_eq!(rows[0].release_date, None);
        assert_eq!(rows[0].artwork_url, None);
    }

    #[test]
    fn maps_album_songs_to_upserts() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","album":{
            "id":"al-1","name":"333","artist":"Bladee",
            "song":[{"id":"tr-1","title":"Be Nice 2 Me","artist":"Bladee","album":"333",
                     "duration":184,"track":3,"discNumber":1,"coverArt":"al-1",
                     "created":"2024-01-05T10:00:00Z"}]}}}"#;
        let album: WireAlbumDetail = parse_envelope(body, "album").unwrap();
        let rows: Vec<_> = album.song.into_iter().map(|s| song_from(s, "al-1")).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "tr-1");
        assert_eq!(rows[0].name, "Be Nice 2 Me");
        assert_eq!(rows[0].album_id.as_deref(), Some("al-1"));
        assert_eq!(rows[0].duration_ms, 184_000, "subsonic reports seconds");
        assert_eq!(rows[0].track_number, Some(3));
        assert_eq!(rows[0].disc_number, Some(1));
        assert_eq!(rows[0].artwork_url.as_deref(), Some("subsonic:al-1"));
    }

    #[test]
    fn song_without_duration_is_zero_not_an_error() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","album":{
            "song":[{"id":"tr-2","title":"Untitled"}]}}}"#;
        let album: WireAlbumDetail = parse_envelope(body, "album").unwrap();
        let rows: Vec<_> = album.song.into_iter().map(|s| song_from(s, "al-9")).collect();
        assert_eq!(rows[0].duration_ms, 0);
        assert_eq!(rows[0].track_number, None);
    }

    #[test]
    fn cover_art_ref_is_a_scheme_not_a_url() {
        assert_eq!(cover_art_ref(Some("al-1".into())).as_deref(), Some("subsonic:al-1"));
        assert_eq!(cover_art_ref(None), None);
        assert_eq!(cover_art_ref(Some("  ".into())), None);
    }

    #[test]
    fn maps_playlists_to_upserts() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlists":{
            "playlist":[{"id":"pl-1","name":"night","comment":"for driving",
                         "songCount":9,"coverArt":"pl-1"}]}}}"#;
        let list: PlaylistList = parse_envelope(body, "playlists").unwrap();
        let rows: Vec<_> = list.playlist.into_iter().map(playlist_from).collect();
        assert_eq!(rows[0].id, "pl-1");
        assert_eq!(rows[0].name, "night");
        assert_eq!(rows[0].description.as_deref(), Some("for driving"));
        assert_eq!(rows[0].artwork_url.as_deref(), Some("subsonic:pl-1"));
        assert!(!rows[0].can_edit, "milestone 1 is read-only");
    }

    #[test]
    fn extracts_playlist_track_ids_in_order() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlist":{
            "id":"pl-1","entry":[{"id":"tr-3"},{"id":"tr-1"},{"id":"tr-2"}]}}}"#;
        let detail: WirePlaylistDetail = parse_envelope(body, "playlist").unwrap();
        let ids: Vec<String> = detail.entry.into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec!["tr-3", "tr-1", "tr-2"], "playlist order is meaningful");
    }

    #[test]
    fn empty_playlist_has_no_entries() {
        let body =
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlist":{"id":"pl-2"}}}"#;
        let detail: WirePlaylistDetail = parse_envelope(body, "playlist").unwrap();
        assert!(detail.entry.is_empty());
    }

    #[test]
    fn signed_url_encodes_and_targets_rest_endpoint() {
        let c = Client::new(Credentials {
            base_url: "https://m.example.com/".into(),
            username: "de niz".into(),
            password: "sesame".into(),
        })
        .unwrap();
        let url = c.signed_url("getAlbum", &[("id", "al 1".into())]);
        assert!(url.starts_with("https://m.example.com/rest/getAlbum.view?"), "{url}");
        assert!(url.contains("u=de%20niz"), "{url}");
        assert!(url.contains("id=al%201"), "{url}");
        assert!(!url.contains("sesame"), "password must never appear in a url");
    }

    #[test]
    fn stream_url_points_at_the_stream_endpoint_and_hides_the_password() {
        let c = Client::new(Credentials {
            base_url: "https://m.example.com".into(),
            username: "deniz".into(),
            password: "sesame".into(),
        })
        .unwrap();
        let url = c.stream_url("tr-1");
        assert!(url.starts_with("https://m.example.com/rest/stream.view?"), "{url}");
        assert!(url.contains("id=tr-1"), "{url}");
        assert!(!url.contains("sesame"));
    }

    #[test]
    fn client_rejects_an_unusable_url_before_touching_the_network() {
        assert!(Client::new(Credentials {
            base_url: "   ".into(),
            username: "deniz".into(),
            password: "sesame".into(),
        })
        .is_err());
    }

    #[test]
    fn a_missing_payload_key_is_an_empty_default() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
        let a: Artists = parse_envelope(body, "artists").unwrap();
        assert_eq!(a.index.len(), 0);
    }

    #[test]
    fn auth_token_matches_the_subsonic_reference_fixture() {
        assert_eq!(auth_token("sesame", "c19b2d"), "26719a1196d2a940705a59634eb18eab");
    }

    #[test]
    fn auth_query_carries_the_required_parameters() {
        let q = auth_query("deniz", "sesame", "c19b2d");
        let get = |k: &str| q.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        assert_eq!(get("u").as_deref(), Some("deniz"));
        assert_eq!(get("s").as_deref(), Some("c19b2d"));
        assert_eq!(get("t").as_deref(), Some("26719a1196d2a940705a59634eb18eab"));
        assert_eq!(get("v").as_deref(), Some("1.16.1"));
        assert_eq!(get("c").as_deref(), Some("capsule"));
        assert_eq!(get("f").as_deref(), Some("json"));
        assert!(get("p").is_none(), "plaintext password must never be sent");
    }

    #[test]
    fn base_url_normalisation() {
        assert_eq!(normalize_base_url("https://m.example.com/").unwrap(), "https://m.example.com");
        assert_eq!(normalize_base_url("https://m.example.com").unwrap(), "https://m.example.com");
        assert_eq!(normalize_base_url("  https://m.example.com  ").unwrap(), "https://m.example.com");
        assert_eq!(normalize_base_url("https://ex.com/music/").unwrap(), "https://ex.com/music");
        assert_eq!(normalize_base_url("m.example.com").unwrap(), "https://m.example.com");
    }

    #[test]
    fn base_url_rejects_empty() {
        assert!(matches!(normalize_base_url("   "), Err(SubsonicError::InvalidUrl(_))));
    }

    #[test]
    fn is_insecure_detects_plain_http() {
        assert!(is_insecure("http://m.example.com"));
        assert!(!is_insecure("https://m.example.com"));
    }

    #[test]
    fn salts_differ_between_calls() {
        assert_ne!(random_salt(), random_salt());
    }
}
