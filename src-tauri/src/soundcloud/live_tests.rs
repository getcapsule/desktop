use super::*;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires public SoundCloud access; verifies a temporary full offline file"]
async fn original_soundcloud_download_decodes_offline() {
    let client = Client::default();
    let cancel = Cancellation::default();
    let results = client
        .search(SearchKind::Tracks, "Tycho Awake", 5, 0, &cancel)
        .await
        .unwrap();
    let mut selected = None;
    for track in results.tracks {
        if let Ok(stream) = client.playable(&track.id, &cancel).await {
            selected = Some(stream);
            break;
        }
    }
    let stream = selected.expect("a public recording resolves");
    let directory =
        std::env::temp_dir().join(format!("capsule-sc-download-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("original.part");
    let shared = std::sync::Arc::new((
        std::sync::Mutex::new(crate::stream::CacheState::new()),
        std::sync::Condvar::new(),
    ));
    let transfer = crate::stream::Cancel::default();
    crate::stream::spawn_fetch(
        stream.url,
        path.clone(),
        shared.clone(),
        0,
        transfer.clone(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            {
                let progress = shared.0.lock().unwrap();
                assert!(progress.error().is_none(), "{:?}", progress.error());
                if progress.is_complete() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("full SoundCloud recording downloads");
    let decoder =
        rodio::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
    assert_eq!(decoder.take(48_000).count(), 48_000);
    println!(
        "Full SoundCloud original download and offline decoding passed ({} bytes).",
        std::fs::metadata(&path).unwrap().len()
    );
    transfer.stop();
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires public SoundCloud access and a real audio output device"]
async fn public_search_resolves_and_native_audio_advances() {
    let client = Client::default();
    let cancel = Cancellation::default();
    let started = Instant::now();
    let results = client
        .search(SearchKind::Tracks, "Tycho Awake", 5, 0, &cancel)
        .await
        .unwrap();
    assert!(!results.tracks.is_empty());
    let mut selected = None;
    for track in results.tracks {
        match client.playable(&track.id, &cancel).await {
            Ok(stream) => {
                selected = Some(stream);
                break;
            }
            Err(error) => {
                eprintln!("SoundCloud public track {}: {}", track.id, error);
                let metadata = client.track_value(&track.id, false, &cancel).await.unwrap();
                for media in metadata["media"]["transcodings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    eprintln!("SoundCloud format protocol={} mime={} snipped={} quality={} endpoint_host={}",
                        media["format"]["protocol"].as_str().unwrap_or("missing"), media["format"]["mime_type"].as_str().unwrap_or("missing"), media["snipped"].as_bool().unwrap_or(false), media["quality"].as_str().unwrap_or("missing"),
                        media["url"].as_str().and_then(|raw|url::Url::parse(raw).ok()).and_then(|u|u.host_str().map(str::to_owned)).unwrap_or_default());
                }
            }
        }
    }
    let stream = selected.expect("a public full recording must resolve");
    println!(
        "SoundCloud search_and_resolution_ms={} quality={} hls={}",
        started.elapsed().as_millis(),
        stream.quality,
        hls::is_manifest_url(&stream.url)
    );
    let directory = std::env::temp_dir().join(format!("capsule-sc-live-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let engine = crate::audio::Engine::new().unwrap();
    engine.set_volume(0);
    let first_audio = Instant::now();
    engine
        .load(stream.url, directory.join("audio.dat"))
        .unwrap();
    engine.play();
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            assert!(
                engine.current_error().is_none(),
                "{:?}",
                engine.current_error()
            );
            if engine.position_ms() >= 250 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("native public SoundCloud playback starts");
    println!(
        "SoundCloud resolved_url_to_250ms_audio_ms={}",
        first_audio.elapsed().as_millis()
    );
    engine.stop();
    let _ = std::fs::remove_dir_all(directory);
}
