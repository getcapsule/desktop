use super::*;
use crate::import::http::{Cancellation, JsonFuture, JsonTransport};
use crate::import::model::Platform;
use serde_json::json;
use std::sync::{Arc, Mutex as StdMutex};

struct Fixture {
    responses: StdMutex<Vec<Value>>,
    calls: Arc<StdMutex<Vec<String>>>,
}
impl JsonTransport for Fixture {
    fn get<'a>(
        &'a self,
        _: Platform,
        path: &'a str,
        _: Option<&'a str>,
        cancel: &'a Cancellation,
    ) -> JsonFuture<'a> {
        Box::pin(async move {
            cancel.check()?;
            self.calls.lock().unwrap().push(path.into());
            self.responses
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| "No fixture response.".into())
        })
    }
}
fn fixture(responses: Vec<Value>) -> (Client, Arc<StdMutex<Vec<String>>>) {
    let calls = Arc::new(StdMutex::new(Vec::new()));
    let client = Client {
        http: Box::new(Fixture {
            responses: StdMutex::new(responses.into_iter().rev().collect()),
            calls: calls.clone(),
        }),
        client_id: Mutex::new(Some((
            Instant::now(),
            "abcdefghijklmnopqrstuvwxyz123456".into(),
        ))),
        ..Client::default()
    };
    (client, calls)
}

#[tokio::test]
async fn search_encodes_query_clamps_limits_and_caches_public_results() {
    let (client, calls) = fixture(vec![
        json!({"collection":[{"kind":"track","id":1,"title":"Found"}]}),
    ]);
    let cancel = Cancellation::default();
    for _ in 0..2 {
        assert_eq!(
            client
                .search(SearchKind::Tracks, " a&b ", 1000, 0, &cancel)
                .await
                .unwrap()
                .tracks[0]
                .title,
            "Found"
        );
    }
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let url = url::Url::parse(&calls[0]).unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(pairs.get("q").unwrap(), "a&b");
    assert_eq!(pairs.get("limit").unwrap(), "50");
}

#[tokio::test]
async fn album_hydration_preserves_order_repeats_and_missing_count() {
    let (client, _) = fixture(vec![
        json!({"kind":"playlist","id":9,"title":"Album","is_album":true,"track_count":4,"tracks":[{"id":2},{"id":1},{"id":2},{"id":3}]}),
        json!([{"id":1,"title":"One"},{"id":2,"title":"Two"}]),
    ]);
    let album = client
        .album_tracks("9", &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(
        album
            .tracks
            .iter()
            .map(|t| t.title.as_str())
            .collect::<Vec<_>>(),
        ["Two", "One", "Two"]
    );
    assert_eq!(album.unavailable_count, 1);
    assert!(!album.truncated);
}

#[tokio::test]
async fn playback_rejects_previews_and_falls_back_to_next_full_progressive() {
    let (client, _) = fixture(vec![
        json!({"kind":"track","id":42,"title":"Song","streamable":true,"policy":"ALLOW","media":{"transcodings":[
            {"url":"https://api-v2.soundcloud.com/media/hq","quality":"hq","format":{"protocol":"progressive","mime_type":"audio/aac"}},
            {"url":"https://api-v2.soundcloud.com/media/sq","quality":"sq","format":{"protocol":"progressive","mime_type":"audio/mpeg"}}
        ]}}),
        json!({"url":"https://cf-preview-media.sndcdn.com/preview/42"}),
        json!({"url":"https://cf-media.sndcdn.com/full.mp3"}),
    ]);
    let stream = client
        .playable("42", &Cancellation::default())
        .await
        .unwrap();
    assert_eq!(stream.url, "https://cf-media.sndcdn.com/full.mp3");
    assert_eq!(stream.mime_type, "audio/mpeg");
}

#[tokio::test]
async fn cancelled_or_invalid_input_never_performs_network_requests() {
    let (client, calls) = fixture(vec![]);
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(client.playable("42", &cancel).await.is_err());
    assert!(client
        .search(SearchKind::Tracks, "song", 10, 0, &cancel)
        .await
        .is_err());
    assert!(client
        .album_tracks("../../users", &Cancellation::default())
        .await
        .is_err());
    assert!(client
        .resolve_track("https://evil.test/song", &Cancellation::default())
        .await
        .is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn cache_evicts_oldest_and_expires_without_unbounded_growth() {
    let mut cache = Cache::default();
    let now = Instant::now();
    for n in 0..130 {
        cache.put(n.to_string(), json!(n), now + Duration::from_nanos(n));
    }
    assert!(cache.get("0", now).is_none());
    assert_eq!(cache.get("129", now), Some(json!(129)));
    assert!(cache.get("129", now + Duration::from_secs(91)).is_none());
    assert!(cache.entries.len() <= 128);
}

#[test]
fn oversized_catalog_payloads_do_not_fill_the_memory_cache() {
    let mut cache = Cache::default();
    cache.put(
        "large".into(),
        json!({"data":"x".repeat(200_000)}),
        Instant::now(),
    );
    assert!(cache.get("large", Instant::now()).is_none());
}

#[test]
fn maps_public_tracks_and_urns_without_accepting_private_or_nontracks() {
    let track = parse_track(&json!({"urn":"soundcloud:tracks:42","kind":"track","title":"Song","duration":1234,"user":{"id":9,"username":"Artist"},"publisher_metadata":{"artist":"Official Artist","album_title":"Album"}})).unwrap();
    assert_eq!(track.id, "42");
    assert_eq!(track.artist, "Official Artist");
    assert_eq!(track.album.as_deref(), Some("Album"));
    assert_eq!(track.duration_ms, 1234);
    assert!(parse_track(&json!({"id":42,"title":"Private","sharing":"private"})).is_none());
    assert!(parse_track(&json!({"urn":"soundcloud:users:42","title":"Wrong"})).is_none());
}

#[test]
fn keeps_only_full_native_progressive_audio_and_ranks_quality() {
    let value = json!({"media":{"transcodings":[
        {"url":"https://api-v2.soundcloud.com/media/hls","quality":"hq","format":{"protocol":"hls","mime_type":"audio/aac"}},
        {"url":"https://api-v2.soundcloud.com/media/preview","snipped":true,"format":{"protocol":"progressive","mime_type":"audio/mpeg"}},
        {"url":"https://api-v2.soundcloud.com/media/mp3","quality":"sq","format":{"protocol":"progressive","mime_type":"audio/mpeg"}},
        {"url":"https://api-v2.soundcloud.com/media/aac","quality":"hq","format":{"protocol":"progressive","mime_type":"audio/aac"}}
    ]}});
    let candidates = progressive_candidates(&value);
    assert_eq!(candidates.len(), 2);
    assert!(candidates[0].url.ends_with("aac"));
    assert!(candidates[1].url.ends_with("mp3"));
    assert!(!public_playable(
        &json!({"sharing":"public","policy":"SNIP","streamable":true})
    ));
    assert!(!public_playable(
        &json!({"sharing":"private","policy":"ALLOW","streamable":true})
    ));
}

#[test]
fn native_candidates_include_full_mp3_and_fmp4_hls_but_exclude_encrypted_sources() {
    let track = json!({"media":{"transcodings":[
        {"url":"https://api-v2.soundcloud.com/media/mp3","quality":"sq","format":{"protocol":"progressive","mime_type":"audio/mpeg"}},
        {"url":"https://api-v2.soundcloud.com/media/hls","quality":"hq","format":{"protocol":"hls","mime_type":"audio/mpeg"}},
        {"url":"https://api-v2.soundcloud.com/media/aac","quality":"hq","format":{"protocol":"hls","mime_type":"audio/aac"}},
        {"url":"https://api-v2.soundcloud.com/media/aac160","quality":"sq","format":{"protocol":"hls","mime_type":"audio/mp4; codecs=\"mp4a.40.2\""}},
        {"url":"https://api-v2.soundcloud.com/media/aac96","quality":"lq","format":{"protocol":"hls","mime_type":"audio/mp4; codecs=\"mp4a.40.2\""}},
        {"url":"https://api-v2.soundcloud.com/media/encrypted","format":{"protocol":"hls","mime_type":"audio/x-encrypted"}}
    ]}});
    let candidates = native_candidates(&track);
    assert_eq!(candidates.len(), 4);
    assert!(candidates[0].url.ends_with("hls"));
    assert_eq!(candidates[0].mime_type, "audio/mpegurl");
    assert!(candidates[2].url.ends_with("aac160"));
    assert!(candidates[3].url.ends_with("aac96"));
}

#[test]
fn rejects_untrusted_endpoints_and_preview_audio_urls() {
    for bad in [
        "http://cf-media.sndcdn.com/x",
        "https://sndcdn.com.evil.test/x",
        "https://u@cf-media.sndcdn.com/x",
        "https://127.0.0.1/x",
        "https://cf-preview-media.sndcdn.com/x",
        "https://cf-media.sndcdn.com/preview/x",
    ] {
        assert!(validate_audio_url(bad).is_err(), "{bad}");
    }
    assert!(validate_audio_url("https://cf-media.sndcdn.com/abc.128.mp3?Policy=public").is_ok());
    assert!(validate_audio_url(
        "https://playback.media-streaming.soundcloud.cloud/a.m3u8?token=public"
    )
    .is_ok());
    assert!(validate_audio_url("https://evil.media-streaming.soundcloud.cloud/a.m3u8").is_err());
    assert!(api_url("https://evil.test/tracks", "client").is_err());
    let url = api_url("tracks?client_id=old", "fresh").unwrap();
    assert_eq!(
        url.as_str(),
        "https://api-v2.soundcloud.com/tracks?client_id=fresh"
    );
}

#[test]
fn typed_search_filters_mismatched_kinds_and_maps_albums_artists() {
    let response = json!({"collection":[
        {"kind":"user","id":3,"username":"Artist","avatar_url":"https://i1.sndcdn.com/avatar.jpg"},
        {"kind":"playlist","id":4,"title":"Album","is_album":true,"track_count":2,"user":{"id":3,"username":"Artist"}},
        {"kind":"playlist","id":5,"title":"Playlist","is_album":false}
    ]});
    assert_eq!(
        parse_search(&response, SearchKind::Artists).artists[0].name,
        "Artist"
    );
    let albums = parse_search(&response, SearchKind::Albums).albums;
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].id, "4");
    assert_eq!(albums[0].track_count, 2);
    assert!(parse_search(&response, SearchKind::Tracks)
        .tracks
        .is_empty());
}
