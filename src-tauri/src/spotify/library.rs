//! Spotify library mapping. IDs stay raw; playback is responsible for Spotify URIs.
use super::{Client, Error};
use crate::db::{AlbumUpsert, ArtistUpsert, PlaylistUpsert, SongUpsert};
use serde_json::Value;
use std::collections::HashSet;
use url::Url;

// At 50 items/page this allows 100,000 entries without an unbounded walk.
// Exceeding the bound is an error, so callers do not mistake truncation for success.
const MAX_PAGES: usize = 2000;

impl Client {
    pub async fn library_songs(
        &self,
        mut sink: impl FnMut(Vec<SongUpsert>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.library_pages("/me/tracks?limit=50", false, |items| {
            sink(items.iter().filter_map(saved_song).collect())
        })
        .await
    }

    pub async fn library_albums(
        &self,
        mut sink: impl FnMut(Vec<AlbumUpsert>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.library_pages("/me/albums?limit=50", false, |items| {
            sink(items.iter().filter_map(saved_album).collect())
        })
        .await
    }

    pub async fn library_playlists(
        &self,
        mut sink: impl FnMut(Vec<PlaylistUpsert>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.library_pages("/me/playlists?limit=50", false, |items| {
            sink(items.iter().filter_map(playlist_from).collect())
        })
        .await
    }

    pub async fn library_artists(
        &self,
        mut sink: impl FnMut(Vec<ArtistUpsert>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.library_pages("/me/following?type=artist&limit=50", true, |items| {
            sink(items.iter().filter_map(artist_from).collect())
        })
        .await
    }

    pub async fn album_songs(&self, id: &str) -> Result<Vec<SongUpsert>, Error> {
        validate_id(id)?;
        // Simplified album tracks lack album name/art. Fetch the album once,
        // then apply that metadata to every track, including subsequent pages.
        let album = self.get(&format!("/albums/{id}")).await?;
        if raw_id(&album) != Some(id) {
            return Err(Error::Protocol(
                "Spotify returned an unexpected album".into(),
            ));
        }
        let mut songs = Vec::new();
        self.library_pages(&format!("/albums/{id}/tracks?limit=50"), false, |items| {
            songs.extend(
                items
                    .iter()
                    .filter_map(|track| song_from(track, Some(&album), None)),
            );
            Ok(())
        })
        .await?;
        Ok(songs)
    }

    pub async fn playlist_songs(&self, id: &str) -> Result<Vec<SongUpsert>, Error> {
        validate_id(id)?;
        let mut songs = Vec::new();
        // February 2026 API: /items and entry.item replace /tracks and entry.track.
        self.library_pages(&format!("/playlists/{id}/items?limit=50"), false, |items| {
            // Keep order and duplicates: playlist membership is positional.
            songs.extend(items.iter().filter_map(playlist_song));
            Ok(())
        })
        .await?;
        Ok(songs)
    }

    async fn library_pages(
        &self,
        first: &str,
        nested_artists: bool,
        mut sink: impl FnMut(&[Value]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut pages = Pagination::new(first)?;
        let mut next = Some(first.to_owned());
        while let Some(path) = next {
            let url = pages.visit(&path)?;
            let response = self.get(&url).await?;
            let page = library_page(&response, nested_artists)?;
            let items = page_items(page)?;
            next = next_page(page)?.map(str::to_owned);
            sink(items)?;
        }
        Ok(())
    }
}

/// Validates the origin and endpoint before any request and detects URL cycles.
/// Query sorting also catches repeated pages whose query parameters are reordered.
struct Pagination {
    endpoint: String,
    seen: HashSet<String>,
}

impl Pagination {
    fn new(first: &str) -> Result<Self, Error> {
        let url = page_url(first)?;
        Ok(Self {
            endpoint: url.path().to_owned(),
            seen: HashSet::new(),
        })
    }

    fn visit(&mut self, path: &str) -> Result<String, Error> {
        let mut url = page_url(path)?;
        if url.path() != self.endpoint {
            return Err(Error::Protocol(
                "Spotify pagination changed endpoint".into(),
            ));
        }
        let mut query: Vec<_> = url.query_pairs().into_owned().collect();
        query.sort();
        url.set_query(None);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let key = url.to_string();
        if self.seen.len() >= MAX_PAGES {
            return Err(Error::Protocol(
                "Spotify pagination exceeded the page limit".into(),
            ));
        }
        if !self.seen.insert(key.clone()) {
            return Err(Error::Protocol("Spotify pagination repeated a page".into()));
        }
        Ok(key)
    }
}

fn page_url(path: &str) -> Result<Url, Error> {
    // Reject characters URL parsers would silently strip or reinterpret.
    if path.is_empty()
        || path
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(Error::Protocol("Invalid Spotify pagination URL".into()));
    }
    let absolute = if path.starts_with("/v1/") {
        format!("https://api.spotify.com{path}")
    } else if path.starts_with('/') && !path.starts_with("//") {
        format!("https://api.spotify.com/v1{path}")
    } else {
        path.to_owned()
    };
    let url = Url::parse(&absolute)
        .map_err(|_| Error::Protocol("Invalid Spotify pagination URL".into()))?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.spotify.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with("/v1/")
    {
        return Err(Error::Protocol("Untrusted Spotify pagination URL".into()));
    }
    Ok(url)
}

fn library_page(response: &Value, nested_artists: bool) -> Result<&Value, Error> {
    let page = if nested_artists {
        &response["artists"]
    } else {
        response
    };
    if !page.is_object() {
        return Err(Error::Protocol("Missing Spotify library page".into()));
    }
    Ok(page)
}

fn page_items(page: &Value) -> Result<&Vec<Value>, Error> {
    page["items"]
        .as_array()
        .ok_or_else(|| Error::Protocol("Missing Spotify page items".into()))
}

fn next_page(page: &Value) -> Result<Option<&str>, Error> {
    match page.get("next") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(next)) if !next.is_empty() => Ok(Some(next)),
        _ => Err(Error::Protocol("Invalid Spotify next page".into())),
    }
}

fn validate_id(id: &str) -> Result<(), Error> {
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return Err(Error::Protocol("Expected a raw Spotify ID".into()));
    }
    Ok(())
}

fn raw_id(value: &Value) -> Option<&str> {
    value["id"].as_str().filter(|id| validate_id(id).is_ok())
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str().filter(|s| !s.trim().is_empty())
}

fn artist_names(value: &Value) -> String {
    value["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|artist| text(artist, "name"))
        .collect::<Vec<_>>()
        .join(", ")
}

// Spotify orders image arrays by descending size. Choose the first usable image
// and keep dimensions from that same image, including unknown/null dimensions.
fn artwork(value: &Value) -> Option<&Value> {
    value["images"]
        .as_array()?
        .iter()
        .find(|image| text(image, "url").is_some())
}

fn dimension(image: Option<&Value>, key: &str) -> Option<i64> {
    image?[key].as_i64().filter(|n| *n > 0)
}

fn saved_song(entry: &Value) -> Option<SongUpsert> {
    song_from(entry.get("track")?, None, text(entry, "added_at"))
}

fn playlist_song(entry: &Value) -> Option<SongUpsert> {
    if entry["is_local"].as_bool() == Some(true) {
        return None;
    }
    // An explicit null item is unavailable; never resurrect it via legacy track.
    let track = entry.get("item").or_else(|| entry.get("track"))?;
    song_from(track, None, text(entry, "added_at"))
}

fn song_from(
    track: &Value,
    parent_album: Option<&Value>,
    added_at: Option<&str>,
) -> Option<SongUpsert> {
    if track["is_local"].as_bool() == Some(true)
        || track["is_playable"].as_bool() == Some(false)
        || track["type"].as_str().is_some_and(|kind| kind != "track")
        || track["uri"].as_str().is_some_and(|uri| {
            uri.starts_with("spotify:local:") || uri.starts_with("spotify:episode:")
        })
    {
        return None;
    }
    let id = raw_id(track)?.to_owned();
    let album = parent_album.or_else(|| track.get("album").filter(|a| a.is_object()));
    let image = album.and_then(artwork);
    let mut artists = artist_names(track);
    if artists.is_empty() {
        artists = album.map(artist_names).unwrap_or_default();
    }
    Some(SongUpsert {
        catalog_id: Some(id.clone()),
        id,
        name: text(track, "name").unwrap_or_default().to_owned(),
        artist_name: artists,
        album_name: album
            .and_then(|a| text(a, "name"))
            .unwrap_or_default()
            .to_owned(),
        album_id: album.and_then(raw_id).map(str::to_owned),
        duration_ms: track["duration_ms"].as_u64().unwrap_or_default(),
        artwork_url: image.and_then(|i| text(i, "url")).map(str::to_owned),
        artwork_width: dimension(image, "width"),
        artwork_height: dimension(image, "height"),
        track_number: track["track_number"].as_i64().filter(|n| *n > 0),
        disc_number: track["disc_number"].as_i64().filter(|n| *n > 0),
        added_at: added_at.map(str::to_owned),
    })
}

fn saved_album(entry: &Value) -> Option<AlbumUpsert> {
    album_from(entry.get("album")?, text(entry, "added_at"))
}

fn album_from(album: &Value, added_at: Option<&str>) -> Option<AlbumUpsert> {
    let id = raw_id(album)?.to_owned();
    let image = artwork(album);
    Some(AlbumUpsert {
        catalog_id: Some(id.clone()),
        id,
        name: text(album, "name").unwrap_or_default().to_owned(),
        artist_name: artist_names(album),
        artwork_url: image.and_then(|i| text(i, "url")).map(str::to_owned),
        artwork_width: dimension(image, "width"),
        artwork_height: dimension(image, "height"),
        release_date: text(album, "release_date").map(str::to_owned),
        track_count: album["total_tracks"]
            .as_u64()
            .and_then(|n| n.try_into().ok())
            .unwrap_or_default(),
        added_at: added_at.map(str::to_owned),
    })
}

fn playlist_from(playlist: &Value) -> Option<PlaylistUpsert> {
    let id = raw_id(playlist)?.to_owned();
    let image = artwork(playlist);
    Some(PlaylistUpsert {
        id,
        name: text(playlist, "name").unwrap_or_default().to_owned(),
        description: text(playlist, "description").map(str::to_owned),
        artwork_url: image.and_then(|i| text(i, "url")).map(str::to_owned),
        artwork_width: dimension(image, "width"),
        artwork_height: dimension(image, "height"),
        // Listing a playlist (even collaborative) does not establish ownership.
        can_edit: false,
    })
}

fn artist_from(artist: &Value) -> Option<ArtistUpsert> {
    Some(ArtistUpsert {
        id: raw_id(artist)?.to_owned(),
        name: text(artist, "name").unwrap_or_default().to_owned(),
        artwork_url: artwork(artist)
            .and_then(|i| text(i, "url"))
            .map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TRACK: &str = r#"{
        "id":"4iV5W9uYEdYUVa79Axb7Rh","type":"track","name":"Track",
        "duration_ms":191234,"track_number":3,"disc_number":2,
        "artists":[{"id":"artist1","name":"First"},{"name":"Second"}],
        "album":{"id":"2up3OPMp9Tb4dAKM2erWXQ","name":"Album",
            "images":[{"url":"https://i.scdn.co/image/cover","width":640,"height":640}]}
    }"#;

    fn track() -> Value {
        serde_json::from_str(TRACK).unwrap()
    }

    #[test]
    fn saved_track_preserves_raw_ids_album_art_and_added_date() {
        let song = saved_song(&json!({"added_at":"2026-02-23T10:00:00Z","track":track()}));
        assert!(song.is_some(), "a saved Spotify track must be imported");
        let song = song.unwrap();
        assert_eq!(song.id, "4iV5W9uYEdYUVa79Axb7Rh");
        assert_eq!(song.catalog_id.as_deref(), Some("4iV5W9uYEdYUVa79Axb7Rh"));
        assert_eq!(song.name, "Track");
        assert_eq!(song.artist_name, "First, Second");
        assert_eq!(song.album_name, "Album");
        assert_eq!(song.album_id.as_deref(), Some("2up3OPMp9Tb4dAKM2erWXQ"));
        assert_eq!(song.duration_ms, 191234);
        assert_eq!(song.track_number, Some(3));
        assert_eq!(song.disc_number, Some(2));
        assert_eq!(song.added_at.as_deref(), Some("2026-02-23T10:00:00Z"));
        assert_eq!(
            song.artwork_url.as_deref(),
            Some("https://i.scdn.co/image/cover")
        );
        assert_eq!(
            (song.artwork_width, song.artwork_height),
            (Some(640), Some(640))
        );
    }

    #[test]
    fn playlist_new_items_skip_unavailable_episodes_and_locals_without_reordering() {
        let mut second = track();
        second["id"] = json!("second");
        let mut episode = track();
        episode["type"] = json!("episode");
        let mut local = track();
        local["is_local"] = json!(true);
        let mut unavailable = track();
        unavailable["is_playable"] = json!(false);
        let page = json!({"items":[
            {"item":track(),"added_at":"2026-02-24"},
            {"item":null}, null, {"item":episode}, {"item":local},
            {"item":track(),"is_local":true}, {"item":unavailable},
            {"track":second},
            {"item":null,"track":track()},
            {"item":track()}
        ],"next":null});
        let songs: Vec<_> = page_items(&page)
            .unwrap()
            .iter()
            .filter_map(playlist_song)
            .collect();
        assert_eq!(
            songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["4iV5W9uYEdYUVa79Axb7Rh", "second", "4iV5W9uYEdYUVa79Axb7Rh"]
        );
        assert_eq!(songs[0].added_at.as_deref(), Some("2026-02-24"));
    }

    #[test]
    fn playlist_item_takes_precedence_over_legacy_track() {
        let mut preferred = track();
        preferred["id"] = json!("new");
        let song = playlist_song(&json!({"item":preferred,"track":track()})).unwrap();
        assert_eq!(song.id, "new");
    }

    #[test]
    fn missing_optional_fields_default_but_missing_identity_is_skipped() {
        let song = song_from(&json!({"id":"minimal"}), None, None);
        assert!(song.is_some());
        let song = song.unwrap();
        assert_eq!(song.name, "");
        assert_eq!(song.artist_name, "");
        assert_eq!(song.duration_ms, 0);
        assert_eq!(song.album_id, None);
        assert_eq!(song.artwork_url, None);
        assert_eq!(song.track_number, None);
        assert_eq!(song.disc_number, None);
        for value in [
            json!(null),
            json!({}),
            json!({"id":""}),
            json!({"id":"spotify:track:bad"}),
            json!({"id":"a/b"}),
            json!({"id":42}),
        ] {
            assert!(song_from(&value, None, None).is_none());
            assert!(album_from(&value, None).is_none());
            assert!(playlist_from(&value).is_none());
            assert!(artist_from(&value).is_none());
        }
    }

    #[test]
    fn album_tracks_inherit_album_art_name_and_artist_without_overwriting_track_artist() {
        let album = json!({"id":"album1","name":"Parent album",
            "artists":[{"name":"Album artist"}],
            "images":[{"url":"https://i.scdn.co/image/album","width":300,"height":300}]});
        let song = song_from(
            &json!({"id":"song1","track_number":1,"disc_number":2}),
            Some(&album),
            None,
        )
        .unwrap();
        assert_eq!(song.album_id.as_deref(), Some("album1"));
        assert_eq!(song.album_name, "Parent album");
        assert_eq!(song.artist_name, "Album artist");
        assert_eq!(
            song.artwork_url.as_deref(),
            Some("https://i.scdn.co/image/album")
        );
        assert_eq!(
            (song.artwork_width, song.artwork_height),
            (Some(300), Some(300))
        );
        let song = song_from(
            &json!({"id":"song2","artists":[{"name":"Guest"}]}),
            Some(&album),
            None,
        )
        .unwrap();
        assert_eq!(song.artist_name, "Guest");
    }

    #[test]
    fn saved_album_preserves_partial_release_date_count_and_art() {
        let value = json!({"added_at":"2026-02-01","album":{
            "id":"album1","name":"Album","artists":[{"name":"Artist"}],
            "release_date":"1981-12","total_tracks":9,
            "images":[{"url":"https://i.scdn.co/image/a","width":640,"height":640}]
        }});
        let album = saved_album(&value).unwrap();
        assert_eq!(album.id, "album1");
        assert_eq!(album.catalog_id.as_deref(), Some("album1"));
        assert_eq!(album.name, "Album");
        assert_eq!(album.artist_name, "Artist");
        assert_eq!(album.release_date.as_deref(), Some("1981-12"));
        assert_eq!(album.track_count, 9);
        assert_eq!(album.added_at.as_deref(), Some("2026-02-01"));
        assert_eq!(
            album.artwork_url.as_deref(),
            Some("https://i.scdn.co/image/a")
        );
        assert_eq!(
            (album.artwork_width, album.artwork_height),
            (Some(640), Some(640))
        );
        assert!(saved_album(&json!({"album":null})).is_none());
        assert!(saved_song(&json!({"track":null})).is_none());
    }

    #[test]
    fn playlist_metadata_does_not_infer_edit_permission_from_owner_or_collaboration() {
        let playlist = playlist_from(
            &json!({"id":"playlist1","name":"Mix","description":"Description",
            "owner":{"id":"someone"},"collaborative":true,"public":false,
            "images":[{"url":"https://i.scdn.co/image/p","width":null,"height":null}]}),
        )
        .unwrap();
        assert_eq!(playlist.name, "Mix");
        assert_eq!(playlist.description.as_deref(), Some("Description"));
        assert!(!playlist.can_edit);
        assert_eq!(
            playlist.artwork_url.as_deref(),
            Some("https://i.scdn.co/image/p")
        );
        assert_eq!(
            (playlist.artwork_width, playlist.artwork_height),
            (None, None)
        );
        assert!(!playlist_from(&json!({"id":"minimal"})).unwrap().can_edit);
    }

    #[test]
    fn artist_maps_supplied_art_without_requiring_extra_metadata() {
        let artist = artist_from(&json!({"id":"artist1","name":"Artist",
            "images":[{},{"url":""},{"url":"https://i.scdn.co/image/artist","width":320,"height":320}]})).unwrap();
        assert_eq!(artist.id, "artist1");
        assert_eq!(artist.name, "Artist");
        assert_eq!(
            artist.artwork_url.as_deref(),
            Some("https://i.scdn.co/image/artist")
        );
        assert_eq!(
            artist_from(&json!({"id":"minimal"})).unwrap().artwork_url,
            None
        );
    }

    #[test]
    fn malformed_page_is_an_error_instead_of_a_successful_empty_library() {
        for page in [
            json!(null),
            json!({}),
            json!({"items":null}),
            json!({"items":{}}),
        ] {
            assert!(matches!(page_items(&page), Err(Error::Protocol(_))));
        }
        assert!(page_items(&json!({"items":[]})).unwrap().is_empty());
        assert!(matches!(
            next_page(&json!({"items":[],"next":42})),
            Err(Error::Protocol(_))
        ));
        assert_eq!(next_page(&json!({"items":[]})).unwrap(), None);
        assert_eq!(next_page(&json!({"items":[],"next":null})).unwrap(), None);
    }

    #[test]
    fn followed_artists_use_nested_cursor_page() {
        let response = json!({"artists":{"items":[{"id":"artist1","name":"Artist"}],
            "cursors":{"after":"artist1"},
            "next":"https://api.spotify.com/v1/me/following?type=artist&after=artist1&limit=50"}});
        let page = library_page(&response, true).unwrap();
        assert_eq!(page_items(page).unwrap()[0]["id"], "artist1");
        assert_eq!(
            next_page(page).unwrap(),
            Some("https://api.spotify.com/v1/me/following?type=artist&after=artist1&limit=50")
        );
        assert!(library_page(&json!({"items":[]}), true).is_err());
        assert!(library_page(&json!({"artists":null}), true).is_err());
    }

    #[test]
    fn pagination_accepts_spotify_links_and_detects_normalized_cycles() {
        let mut pages = Pagination::new("/me/tracks?limit=50&offset=0").unwrap();
        assert!(pages.visit("/me/tracks?limit=50&offset=0").is_ok());
        assert!(pages
            .visit("https://api.spotify.com/v1/me/tracks?offset=50&limit=50")
            .is_ok());
        assert!(matches!(
            pages.visit("https://api.spotify.com:443/v1/me/tracks?offset=0&limit=50"),
            Err(Error::Protocol(_))
        ));
    }

    #[test]
    fn pagination_rejects_other_origins_credentials_fragments_and_endpoints() {
        for next in [
            "https://evil.example/v1/me/tracks",
            "https://api.spotify.com.evil.example/v1/me/tracks",
            "http://api.spotify.com/v1/me/tracks",
            "https://api.spotify.com:444/v1/me/tracks",
            "https://user:password@api.spotify.com/v1/me/tracks",
            "https://api.spotify.com/v1/me/tracks#fragment",
            "//evil.example/v1/me/tracks",
            "/me/albums?offset=50",
            "https://api.spotify.com/v1/me/tracks/../albums",
            "https://api.spotify.com/v1/me/tracks?offset=50\n",
        ] {
            let mut pages = Pagination::new("/me/tracks?limit=50").unwrap();
            assert!(
                matches!(pages.visit(next), Err(Error::Protocol(_))),
                "{next}"
            );
        }
    }

    #[test]
    fn pagination_has_a_hard_bound_even_when_every_url_is_unique() {
        let mut pages = Pagination::new("/me/tracks").unwrap();
        for offset in 0..MAX_PAGES {
            assert!(pages.visit(&format!("/me/tracks?offset={offset}")).is_ok());
        }
        assert!(matches!(
            pages.visit(&format!("/me/tracks?offset={MAX_PAGES}")),
            Err(Error::Protocol(_))
        ));
    }

    #[test]
    fn resource_paths_reject_uris_and_path_injection() {
        assert!(validate_id("4iV5W9uYEdYUVa79Axb7Rh").is_ok());
        for id in [
            "",
            "../me",
            "a/b",
            "a?offset=3",
            "spotify:album:abc",
            " a ",
            "abc%2fdef",
        ] {
            assert!(matches!(validate_id(id), Err(Error::Protocol(_))), "{id}");
        }
    }
}
