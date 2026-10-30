use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::stream::{self, CacheState, Shared, StreamingSource};
use rodio::cpal::traits::{DeviceTrait, HostTrait};

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct AudioInfo {
    pub generation: u64,
    pub codec: Option<String>,
    pub container: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u16>,
}

impl AudioInfo {
    fn decoded(&mut self, source: &dyn rodio::Source<Item = f32>) {
        self.sample_rate_hz = Some(source.sample_rate()).filter(|v| (1..=768_000).contains(v));
        self.channels = Some(source.channels()).filter(|v| (1..=64).contains(v));
    }

    fn merge_hint(&mut self, hint: Self) {
        self.codec = hint.codec.or(self.codec.take());
        self.container = hint.container.or(self.container.take());
        self.bitrate_kbps = hint
            .bitrate_kbps
            .filter(|v| (1..=100_000).contains(v))
            .or(self.bitrate_kbps);
        self.sample_rate_hz = self
            .sample_rate_hz
            .or(hint.sample_rate_hz.filter(|v| (1..=768_000).contains(v)));
        self.channels = self
            .channels
            .or(hint.channels.filter(|v| (1..=64).contains(v)));
    }
}

fn file_audio_info(path: &std::path::Path, properties: bool) -> AudioInfo {
    use lofty::{
        config::ParseOptions,
        file::{AudioFile, FileType},
        probe::Probe,
    };
    let mut info = AudioInfo::default();
    // Inspect content, never the cache extension or remote URL.
    let Ok(probe) = Probe::open(path) else {
        return info;
    };
    let Ok(probe) = probe.guess_file_type() else {
        return info;
    };
    let kind = probe.file_type();
    let (codec, container) = match kind {
        Some(FileType::Aac) => (Some("AAC"), Some("ADTS")),
        Some(FileType::Flac) => (Some("FLAC"), Some("FLAC")),
        Some(FileType::Mpeg) => (Some("MP3"), Some("MPEG audio")),
        Some(FileType::Mp4) => (None, Some("MP4/M4A")),
        Some(FileType::Vorbis) => (Some("Vorbis"), Some("Ogg")),
        Some(FileType::Opus) => (Some("Opus"), Some("Ogg")),
        Some(FileType::Wav) => (None, Some("WAV")),
        Some(FileType::Aiff) => (None, Some("AIFF")),
        Some(FileType::Ape) => (Some("APE"), Some("APE")),
        Some(FileType::WavPack) => (Some("WavPack"), Some("WavPack")),
        _ => (None, None),
    };
    info.codec = codec.map(str::to_owned);
    info.container = container.map(str::to_owned);
    let options = ParseOptions::new().read_tags(false).read_cover_art(false);
    if properties {
        if kind == Some(FileType::Mp4) {
            if let Ok(mut file) = std::fs::File::open(path) {
                if let Ok(mp4) = lofty::mp4::Mp4File::read_from(&mut file, options) {
                    use lofty::mp4::Mp4Codec;
                    info.codec = match mp4.properties().codec() {
                        Mp4Codec::AAC => Some("AAC"),
                        Mp4Codec::ALAC => Some("ALAC"),
                        Mp4Codec::MP3 => Some("MP3"),
                        Mp4Codec::FLAC => Some("FLAC"),
                        _ => None,
                    }
                    .map(str::to_owned);
                    info.bitrate_kbps = Some(mp4.properties().audio_bitrate()).filter(|v| *v > 0);
                }
            }
        } else if let Ok(file) = probe.options(options).read() {
            info.bitrate_kbps = file.properties().audio_bitrate().filter(|v| *v > 0);
        }
    }
    info
}

/// Names are persisted because CPAL does not expose portable stable device IDs.
pub fn output_devices() -> Result<Vec<String>, AudioError> {
    let devices = rodio::cpal::default_host()
        .output_devices()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    let mut names: Vec<_> = devices.filter_map(|device| device.name().ok()).collect();
    names.sort();
    names.dedup();
    Ok(names)
}

#[derive(Clone)]
enum LoadedSource {
    File(PathBuf),
    Stream { url: String, cache: PathBuf },
}

struct PcmTransfer {
    cancel: stream::Cancel,
    shared: Option<Shared>,
    space: Arc<(Mutex<()>, Condvar)>,
    info: Arc<Mutex<AudioInfo>>,
}

impl PcmTransfer {
    fn stop(&self) {
        self.cancel.stop();
        let _capacity = self.space.0.lock().expect("PCM capacity");
        self.space.1.notify_all();
    }
}

/// Decode ahead on a bounded producer; only normalized PCM reaches the mixer.
fn prepare_pcm(
    pipeline: Arc<crate::audio_queue::Pipeline>,
    ticket: crate::audio_queue::Ticket,
    source: LoadedSource,
    offset_ms: u64,
) -> PcmTransfer {
    use crate::audio_queue::{Chunk, Prepared, CHANNELS, RATE};
    use rodio::Source;
    let cancel = stream::Cancel::default();
    let shared = matches!(source, LoadedSource::Stream { .. })
        .then(|| Arc::new((Mutex::new(CacheState::new()), Condvar::new())));
    let space = Arc::new((Mutex::new(()), Condvar::new()));
    let info = Arc::new(Mutex::new(AudioInfo::default()));
    let work = PcmTransfer {
        cancel: cancel.clone(),
        shared: shared.clone(),
        space: space.clone(),
        info: info.clone(),
    };
    std::thread::spawn(move || {
        let decode = || -> Result<Box<dyn Source<Item = f32> + Send>, String> {
            let metadata_path = match &source {
                LoadedSource::File(path) => path.clone(),
                LoadedSource::Stream { cache, .. } => cache.clone(),
            };
            let local = matches!(&source, LoadedSource::File(_));
            let mut decoder: Box<dyn Source<Item = f32> + Send> = match source {
                LoadedSource::File(path) => Box::new(
                    rodio::Decoder::new(std::io::BufReader::new(
                        std::fs::File::open(path).map_err(|_| "Could not open the audio file.")?,
                    ))
                    .map_err(|_| "Could not decode the audio file.")?,
                ),
                LoadedSource::Stream { url, cache } => {
                    let shared = shared.as_ref().unwrap().clone();
                    stream::spawn_fetch(
                        url.clone(),
                        cache.clone(),
                        shared.clone(),
                        0,
                        cancel.clone(),
                    )
                    .map_err(|_| "Could not start the native audio buffer.")?;
                    let source = StreamingSource::new(cache, shared, url, cancel.clone())
                        .map_err(|_| "Could not open the native audio buffer.")?;
                    Box::new(
                        rodio::Decoder::new(source)
                            .map_err(|_| "Could not decode the audio stream.")?,
                    )
                }
            };
            {
                let mut format = info.lock().expect("audio format");
                let detected = file_audio_info(&metadata_path, local);
                // Resolver hints may have arrived while decoding the header.
                if format.codec.is_none() {
                    format.codec = detected.codec;
                }
                if format.container.is_none() {
                    format.container = detected.container;
                }
                if format.bitrate_kbps.is_none() {
                    format.bitrate_kbps = detected.bitrate_kbps;
                }
                format.decoded(decoder.as_ref());
            }
            // Fragmented MP4 can advertise zero frames in its initial header.
            // Rodio then clamps every seek to zero but returns success. Decode
            // and discard the prefix on the producer thread in that case.
            let skip_prefix = offset_ms > 0 && decoder.total_duration() == Some(Duration::ZERO);
            if offset_ms > 0 && !skip_prefix {
                decoder
                    .try_seek(Duration::from_millis(offset_ms))
                    .map_err(|_| "Could not restore the playback position.")?;
            }
            let mut pcm = rodio::source::UniformSourceIterator::new(
                decoder, CHANNELS, RATE,
            );
            if skip_prefix {
                let samples = u128::from(offset_ms) * u128::from(RATE)
                    * u128::from(CHANNELS) / 1000;
                for sample in 0..samples {
                    if sample % 4096 == 0 && (cancel.stopped() || !pipeline.valid(ticket)) {
                        return Err("Playback superseded.".into());
                    }
                    if pcm.next().is_none() {
                        return Err("Could not restore the playback position.".into());
                    }
                }
            }
            Ok(Box::new(pcm))
        };
        let mut decoder = match decode() {
            Ok(decoder) => decoder,
            Err(message) => {
                let message = shared
                    .as_ref()
                    .and_then(|s| s.0.lock().ok()?.error().map(str::to_owned))
                    .unwrap_or(message);
                pipeline.complete(ticket, Err(message));
                cancel.stop();
                return;
            }
        };
        let first = decoder.by_ref().take(8192).collect();
        let (tx, rest) = std::sync::mpsc::sync_channel(8);
        if !pipeline.complete(
            ticket,
            Ok(Prepared {
                first,
                rest,
                offset_ms,
                space: Some(space.clone()),
            }),
        ) {
            cancel.stop();
            return;
        }
        loop {
            if !pipeline.valid(ticket) {
                cancel.stop();
                return;
            }
            let samples: Vec<_> = decoder.by_ref().take(8192).collect();
            let ended = samples.is_empty();
            let mut chunk = if ended {
                match shared
                    .as_ref()
                    .and_then(|s| s.0.lock().ok()?.error().map(str::to_owned))
                {
                    Some(error) => Chunk::Error(error),
                    None => Chunk::End,
                }
            } else {
                Chunk::Samples(samples)
            };
            if !pipeline.valid(ticket) {
                cancel.stop();
                return;
            }
            match tx.try_send(chunk) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(pending)) => {
                    chunk = pending;
                    let mut capacity = space.0.lock().expect("PCM capacity");
                    loop {
                        if !pipeline.valid(ticket) {
                            cancel.stop();
                            return;
                        }
                        match tx.try_send(chunk) {
                            Ok(()) => break,
                            Err(std::sync::mpsc::TrySendError::Full(pending)) => {
                                chunk = pending;
                                capacity = space.1.wait(capacity).expect("PCM capacity");
                            }
                            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                                cancel.stop();
                                return;
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    cancel.stop();
                    return;
                }
            }
            if ended {
                return;
            }
        }
    });
    work
}

struct PcmSlot {
    ticket: crate::audio_queue::Ticket,
    source: LoadedSource,
    work: PcmTransfer,
}
struct Gapless {
    pipeline: Arc<crate::audio_queue::Pipeline>,
    slots: Vec<PcmSlot>,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no audio output device available: {0}")]
    Device(String),
    #[error("this track's format could not be decoded: {0}")]
    Decode(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

fn to_gain(percent: u8) -> f32 {
    (percent.min(100) as f32) / 100.0
}

struct ClearOnDrop {
    loading: Arc<AtomicU64>,
    ticket: u64,
}

fn seek_worker(mut run: impl FnMut() + Send + 'static) -> std::sync::mpsc::SyncSender<()> {
    let (wake, pending) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        while pending.recv().is_ok() {
            run();
        }
    });
    wake
}

impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        let _ = self
            .loading
            .compare_exchange(self.ticket, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

pub struct Engine {
    _stream: rodio::OutputStream,
    sink: Arc<rodio::Sink>,
    current: Mutex<Option<Shared>>,
    loading: Arc<AtomicU64>,
    pending_seek: Arc<Mutex<Option<u64>>>,
    generation: Arc<AtomicU64>,
    format: Arc<Mutex<AudioInfo>>,
    lifecycle: Arc<Mutex<()>>,
    cancel: Mutex<Option<stream::Cancel>>,
    seek_wake: std::sync::mpsc::SyncSender<()>,
    shutdown: Arc<AtomicBool>,
    source: Mutex<Option<LoadedSource>>,
    gapless: Mutex<Option<Gapless>>,
}

impl Engine {
    pub fn new() -> Result<Self, AudioError> {
        Self::new_for_device(None)
    }

    pub fn new_for_device(name: Option<&str>) -> Result<Self, AudioError> {
        let stream = match name {
            None => rodio::OutputStreamBuilder::open_default_stream(),
            Some(name) => {
                let device = rodio::cpal::default_host()
                    .output_devices()
                    .map_err(|e| AudioError::Device(e.to_string()))?
                    .find(|device| device.name().ok().as_deref() == Some(name))
                    .ok_or_else(|| AudioError::Device(format!("{name} is disconnected")))?;
                rodio::OutputStreamBuilder::from_device(device)
                    .and_then(|builder| builder.open_stream_or_fallback())
            }
        }
        .map_err(|e| AudioError::Device(e.to_string()))?;
        let sink = Arc::new(rodio::Sink::connect_new(stream.mixer()));
        let loading = Arc::new(AtomicU64::new(0));
        let pending_seek = Arc::new(Mutex::new(None));
        let lifecycle = Arc::new(Mutex::new(()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let seek_wake = {
            let sink = Arc::downgrade(&sink);
            let loading = loading.clone();
            let pending = pending_seek.clone();
            let lifecycle = lifecycle.clone();
            let shutdown = shutdown.clone();
            seek_worker(move || {
                let _lifecycle = lifecycle.lock().expect("audio lifecycle");
                if shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if loading.load(Ordering::SeqCst) != 0 {
                    return;
                }
                let seek = pending.lock().expect("pending seek mutex").take();
                if let (Some(sink), Some(ms)) = (sink.upgrade(), seek) {
                    if let Err(error) = sink.try_seek(Duration::from_millis(ms)) {
                        tracing::warn!(?error, "seek not supported for this stream");
                    }
                }
            })
        };
        static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);
        let initial_generation = NEXT_ENGINE.fetch_add(1, Ordering::SeqCst) << 32;
        Ok(Self {
            _stream: stream,
            sink,
            current: Mutex::new(None),
            loading,
            pending_seek,
            generation: Arc::new(AtomicU64::new(initial_generation)),
            format: Arc::new(Mutex::new(AudioInfo::default())),
            lifecycle,
            cancel: Mutex::new(None),
            seek_wake,
            shutdown,
            source: Mutex::new(None),
            gapless: Mutex::new(None),
        })
    }

    pub fn load_gapless(&self, url: String, cache: PathBuf, key: crate::audio_queue::Key) {
        self.cancel_fetch();
        let _lifecycle = self.lifecycle.lock().expect("audio lifecycle");
        self.reset();
        let (pipeline, output) = crate::audio_queue::Pipeline::new();
        let ticket = pipeline.begin(key);
        let source = LoadedSource::Stream { url, cache };
        let work = prepare_pcm(pipeline.clone(), ticket, source.clone(), 0);
        *self.source.lock().expect("audio source") = Some(source.clone());
        *self.gapless.lock().expect("gapless audio") = Some(Gapless {
            pipeline,
            slots: vec![PcmSlot {
                ticket,
                source,
                work,
            }],
        });
        self.sink.append(output);
    }

    pub fn plan_gapless(
        &self,
        expected: crate::audio_queue::Key,
        next: Option<(crate::audio_queue::Key, String, PathBuf)>,
    ) {
        let mut guard = self.gapless.lock().expect("gapless audio");
        let Some(gapless) = guard.as_mut() else {
            return;
        };
        let ticket = gapless.pipeline.plan(expected, next.as_ref().map(|n| n.0));
        gapless.slots.retain(|slot| {
            if gapless.pipeline.valid(slot.ticket) {
                true
            } else {
                slot.work.stop();
                false
            }
        });
        if let (Some(ticket), Some((_, url, cache))) = (ticket, next) {
            let source = LoadedSource::Stream { url, cache };
            let work = prepare_pcm(gapless.pipeline.clone(), ticket, source.clone(), 0);
            gapless.slots.push(PcmSlot {
                ticket,
                source,
                work,
            });
        }
    }

    pub fn gapless_snapshot(&self) -> Option<crate::audio_queue::Snapshot> {
        self.gapless
            .lock()
            .expect("gapless audio")
            .as_ref()
            .map(|g| g.pipeline.snapshot())
    }

    pub fn adopt_gapless(&self, key: crate::audio_queue::Key) {
        let guard = self.gapless.lock().expect("gapless audio");
        if let Some(slot) = guard
            .as_ref()
            .and_then(|g| g.slots.iter().find(|s| s.ticket.key == key))
        {
            *self.source.lock().expect("audio source") = Some(slot.source.clone());
        }
    }

    pub fn audio_info(&self) -> AudioInfo {
        let _lifecycle = self.lifecycle.lock().expect("audio lifecycle");
        let guard = self.gapless.lock().expect("gapless audio");
        let mut info = if let Some(gapless) = guard.as_ref() {
            let key = gapless.pipeline.snapshot().key;
            gapless
                .slots
                .iter()
                .find(|slot| Some(slot.ticket.key) == key)
                .map(|slot| slot.work.info.lock().expect("audio format").clone())
                .unwrap_or_default()
        } else {
            self.format.lock().expect("audio format").clone()
        };
        info.generation = self.generation();
        info
    }

    /// A resolver's selected format belongs only to this prepared queue entry.
    pub fn set_gapless_format(&self, key: crate::audio_queue::Key, info: AudioInfo) {
        if let Some(slot) = self
            .gapless
            .lock()
            .expect("gapless audio")
            .as_ref()
            .and_then(|gapless| gapless.slots.iter().find(|slot| slot.ticket.key == key))
        {
            slot.work
                .info
                .lock()
                .expect("audio format")
                .merge_hint(info);
        }
    }

    /// The generation guard prevents a late resolver from labeling a replacement track.
    pub fn set_format(&self, generation: u64, info: AudioInfo) {
        let _lifecycle = self.lifecycle.lock().expect("audio lifecycle");
        if self.generation() == generation {
            self.format.lock().expect("audio format").merge_hint(info);
        }
    }

    pub fn load(&self, url: String, cache: PathBuf) -> Result<(), AudioError> {
        self.cancel_fetch();
        let _lifecycle = self.lifecycle.lock().expect("audio lifecycle");
        let mine = self.reset();
        let url_for_seek = url.clone();

        let shared: Shared = Arc::new((Mutex::new(CacheState::new()), Condvar::new()));
        let cancel = stream::Cancel::default();
        stream::spawn_fetch(url, cache.clone(), shared.clone(), 0, cancel.clone())?;
        *self.source.lock().expect("audio source") = Some(LoadedSource::Stream {
            url: url_for_seek.clone(),
            cache: cache.clone(),
        });
        *self.cancel.lock().expect("audio cancellation") = Some(cancel.clone());
        *self.current.lock().expect("current mutex") = Some(shared.clone());
        self.loading.store(mine, Ordering::SeqCst);

        let sink = self.sink.clone();
        let loading = self.loading.clone();
        let pending = self.pending_seek.clone();
        let generation = self.generation.clone();
        let lifecycle = self.lifecycle.clone();
        let refetch_url = url_for_seek;
        let format = self.format.clone();
        let metadata_path = cache.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let _guard = ClearOnDrop {
                loading,
                ticket: mine,
            };
            let current = || generation.load(Ordering::SeqCst) == mine;

            let source = match StreamingSource::new(cache, shared.clone(), refetch_url, cancel) {
                Ok(s) => s,
                Err(e) => {
                    shared
                        .0
                        .lock()
                        .expect("cache state mutex")
                        .fail(e.to_string());
                    shared.1.notify_all();
                    return;
                }
            };
            if !current() {
                return;
            }
            match rodio::Decoder::new(source) {
                Ok(d) => {
                    let detected = file_audio_info(&metadata_path, false);
                    let _lifecycle = lifecycle.lock().expect("audio lifecycle");
                    if !current() {
                        return;
                    }
                    {
                        let mut info = format.lock().expect("audio format");
                        if info.codec.is_none() {
                            info.codec = detected.codec;
                        }
                        if info.container.is_none() {
                            info.container = detected.container;
                        }
                        info.decoded(&d);
                    }
                    sink.append(d);
                    let seek = pending.lock().expect("pending seek mutex").take();
                    if let Some(ms) = seek {
                        if let Err(e) = sink.try_seek(Duration::from_millis(ms)) {
                            tracing::warn!(error = ?e, "deferred seek failed");
                        }
                    }
                    let _ = _guard.loading.compare_exchange(
                        mine,
                        0,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                }
                Err(e) => {
                    if !current() {
                        return;
                    }
                    shared
                        .0
                        .lock()
                        .expect("cache state mutex")
                        .fail(format!("could not decode this track: {e}"));
                    shared.1.notify_all();
                }
            }
        });
        Ok(())
    }

    pub fn load_file(&self, path: PathBuf) -> Result<(), AudioError> {
        // Validate before replacing the currently playing source.
        std::fs::File::open(&path)?;
        self.cancel_fetch();
        let _lifecycle = self.lifecycle.lock().expect("audio lifecycle");
        let mine = self.reset();
        *self.source.lock().expect("audio source") = Some(LoadedSource::File(path.clone()));
        self.loading.store(mine, Ordering::SeqCst);

        let sink = self.sink.clone();
        let loading = self.loading.clone();
        let pending = self.pending_seek.clone();
        let generation = self.generation.clone();
        let lifecycle = self.lifecycle.clone();
        let format = self.format.clone();
        std::thread::spawn(move || {
            let _guard = ClearOnDrop {
                loading,
                ticket: mine,
            };
            let current = || generation.load(Ordering::SeqCst) == mine;

            let file = match std::fs::File::open(&path) {
                Ok(f) => std::io::BufReader::new(f),
                Err(e) => {
                    tracing::error!(error = %e, path = %path.display(), "could not open track");
                    return;
                }
            };
            if !current() {
                return;
            }
            match rodio::Decoder::new(file) {
                Ok(d) => {
                    let detected = file_audio_info(&path, true);
                    let _lifecycle = lifecycle.lock().expect("audio lifecycle");
                    if !current() {
                        return;
                    }
                    {
                        let mut info = format.lock().expect("audio format");
                        *info = detected;
                        info.decoded(&d);
                    }
                    sink.append(d);
                    let seek = pending.lock().expect("pending seek mutex").take();
                    if let Some(ms) = seek {
                        if let Err(e) = sink.try_seek(Duration::from_millis(ms)) {
                            tracing::warn!(error = ?e, "deferred seek failed");
                        }
                    }
                    let _ = _guard.loading.compare_exchange(
                        mine,
                        0,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                }
                Err(e) => tracing::error!(error = %e, path = %path.display(), "could not decode"),
            }
        });
        Ok(())
    }

    pub fn is_loading(&self) -> bool {
        if let Some(snapshot) = self.gapless_snapshot() {
            return snapshot.loading;
        }
        self.loading.load(Ordering::SeqCst) != 0
    }

    pub fn play(&self) {
        self.sink.play();
    }

    pub fn pause(&self) {
        self.sink.pause();
    }

    pub fn seek(&self, ms: u64) {
        {
            let mut guard = self.gapless.lock().expect("gapless audio");
            if let Some(gapless) = guard.as_mut() {
                let key = gapless
                    .pipeline
                    .snapshot()
                    .key
                    .or_else(|| gapless.slots.first().map(|s| s.ticket.key));
                if let Some((key, source)) = key.and_then(|key| {
                    gapless
                        .slots
                        .iter()
                        .find(|s| s.ticket.key == key)
                        .map(|s| (key, s.source.clone()))
                }) {
                    let ticket = gapless.pipeline.begin(key);
                    for slot in gapless.slots.drain(..) {
                        slot.work.stop();
                    }
                    let work = prepare_pcm(gapless.pipeline.clone(), ticket, source.clone(), ms);
                    gapless.slots.push(PcmSlot {
                        ticket,
                        source,
                        work,
                    });
                }
                return;
            }
        }
        *self.pending_seek.lock().expect("pending seek mutex") = Some(ms);
        let _ = self.seek_wake.try_send(());
    }

    pub fn set_volume(&self, percent: u8) {
        self.sink.set_volume(to_gain(percent));
    }

    // Caller holds lifecycle: an old decoder cannot append after the reset.
    fn reset(&self) -> u64 {
        if let Some(gapless) = self.gapless.lock().expect("gapless audio").take() {
            gapless.pipeline.stop();
            for slot in gapless.slots {
                slot.work.stop();
            }
        }
        self.cancel_fetch();
        *self.source.lock().expect("audio source") = None;
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        self.generation.store(generation, Ordering::SeqCst);
        *self.format.lock().expect("audio format") = AudioInfo {
            generation,
            ..AudioInfo::default()
        };
        self.sink.stop();
        self.loading.store(0, Ordering::SeqCst);
        *self.pending_seek.lock().expect("pending seek mutex") = None;
        generation
    }

    // Cancel I/O before waiting for lifecycle: a seek may be waiting for uncached data.
    fn cancel_fetch(&self) {
        let previous = self.current.lock().expect("current mutex").take();
        if let Some(cancel) = self.cancel.lock().expect("audio cancellation").take() {
            cancel.stop();
        }
        if let Some(shared) = previous {
            shared
                .0
                .lock()
                .expect("cache state mutex")
                .fail("Playback superseded.".into());
            shared.1.notify_all();
        }
    }

    pub fn stop(&self) {
        self.cancel_fetch();
        let _lifecycle = self.lifecycle.lock().expect("audio lifecycle");
        self.reset();
    }

    /// Move the exact resolved source; provider routing and queue stay untouched.
    /// The caller opens the replacement first so unavailable devices cannot stop playback.
    pub fn transfer_to(
        &self,
        replacement: &Self,
        ms: u64,
        paused: bool,
        volume: u8,
    ) -> Result<(), AudioError> {
        let source = self.source.lock().expect("audio source").clone();
        let key = self.gapless_snapshot().and_then(|snapshot| snapshot.key);
        if let Some(LoadedSource::File(path)) = &source {
            std::fs::File::open(path)?;
        }
        self.stop();
        if let Err(error) = replacement.restore_source(source.clone(), ms, paused, volume, key) {
            // A failed reload must leave the original output usable.
            let _ = self.restore_source(source, ms, paused, volume, key);
            return Err(error);
        }
        Ok(())
    }

    fn restore_source(
        &self,
        source: Option<LoadedSource>,
        ms: u64,
        paused: bool,
        volume: u8,
        key: Option<crate::audio_queue::Key>,
    ) -> Result<(), AudioError> {
        self.pause();
        self.set_volume(volume);
        match source {
            Some(LoadedSource::File(path)) => self.load_file(path)?,
            Some(LoadedSource::Stream { url, cache }) => match key {
                Some(key) => self.load_gapless(url, cache, key),
                None => self.load(url, cache)?,
            },
            None => return Ok(()),
        }
        self.seek(ms);
        if paused {
            self.pause();
        } else {
            self.play();
        }
        Ok(())
    }

    pub fn position_ms(&self) -> u64 {
        if let Some(snapshot) = self.gapless_snapshot() {
            return snapshot.position_ms;
        }
        self.sink.get_pos().as_millis() as u64
    }

    pub fn is_empty(&self) -> bool {
        if let Some(snapshot) = self.gapless_snapshot() {
            return snapshot.ended || (snapshot.key.is_none() && !snapshot.loading);
        }
        self.sink.empty()
    }

    pub fn is_paused(&self) -> bool {
        self.sink.is_paused()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn current_error(&self) -> Option<String> {
        if let Some(snapshot) = self.gapless_snapshot() {
            if snapshot.error.is_some() {
                return snapshot.error;
            }
            let guard = self.gapless.lock().expect("gapless audio");
            return guard
                .as_ref()
                .and_then(|g| g.slots.iter().find(|s| Some(s.ticket.key) == snapshot.key))
                .and_then(|s| s.work.shared.as_ref())
                .and_then(|s| s.0.lock().ok()?.error().map(str::to_owned));
        }
        let guard = self.current.lock().expect("current mutex");
        let shared = guard.as_ref()?;
        let state = shared.0.lock().expect("cache state mutex");
        state.error().map(|s| s.to_string())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.cancel_fetch();
        // Keep OutputStream alive until any active seek receives callback feedback.
        let _lifecycle = self.lifecycle.lock().expect("audio lifecycle");
        self.reset();
    }
}

fn track_ended(was_playing: bool, sink_empty: bool) -> bool {
    was_playing && sink_empty
}

#[derive(Default)]
struct EndDetector {
    generation: u64,
    was_playing: bool,
}
impl EndDetector {
    fn observe(&mut self, generation: u64, empty: bool, loading: bool) -> bool {
        let ended =
            !loading && self.generation == generation && track_ended(self.was_playing, empty);
        self.generation = generation;
        self.was_playing = !loading && !empty;
        ended
    }
}

pub fn start_ticker(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tauri::{Emitter, Manager};

        let mut detector = EndDetector::default();
        let mut published_format: Option<AudioInfo> = None;
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            crate::youtube_native::reconcile_handoff(&app);
            crate::youtube_native::queue_changed(&app);

            if crate::import::playback::owns_current(&app)
                && !crate::import::playback::owns_native(&app)
            {
                detector = EndDetector::default();
                continue;
            }

            let engine = {
                let state = app.state::<crate::AppState>();
                let guard = state.audio.lock().expect("audio mutex");
                guard.clone()
            };
            let Some(engine) = engine else {
                detector = EndDetector::default();
                continue;
            };

            let format = engine.audio_info();
            if published_format.as_ref() != Some(&format) {
                let _ = app.emit("audio://format", &format);
                published_format = Some(format);
            }

            if let Some(err) = engine.current_error() {
                if crate::import::playback::owns_current(&app) {
                    let state = app.state::<crate::AppState>();
                    crate::import::playback::report_native_error(&app, &state, &err);
                    crate::commands::publish(&app, &state);
                    detector = EndDetector::default();
                    continue;
                }
                if crate::youtube_native::refresh_current(&app, &err) {
                    detector = EndDetector::default();
                    continue;
                }
                tracing::warn!(%err, "playback stream failed");
                crate::youtube_native::invalidate_current(&app);
                let _ = app.emit("playback://error", err);
                engine.stop();
                detector = EndDetector::default();

                let state = app.state::<crate::AppState>();
                state
                    .player
                    .lock()
                    .expect("player mutex")
                    .set_status(crate::player::Status::Idle);
                crate::commands::publish(&app, &state);
                continue;
            }

            if engine.is_loading() || crate::youtube_native::is_resolving(&app) {
                detector = EndDetector::default();
                continue;
            }

            let generation = engine.generation();
            let empty = engine.is_empty();
            if !empty {
                let ms = engine.position_ms();
                let state = app.state::<crate::AppState>();
                {
                    let mut p = state.player.lock().expect("player mutex");
                    if engine.generation() != generation {
                        continue;
                    }

                    p.set_status(if engine.is_paused() {
                        crate::player::Status::Paused
                    } else {
                        crate::player::Status::Playing
                    });
                    p.on_position(ms);
                }

                crate::commands::publish(&app, &state);
            }

            if detector.observe(
                generation,
                empty,
                engine.is_loading() || crate::youtube_native::is_resolving(&app),
            ) {
                crate::commands::apply(&app, |p| {
                    if engine.generation() != generation
                        || p.state().status == crate::player::Status::Loading
                    {
                        return Vec::new();
                    }
                    if p.state().repeat == crate::player::Repeat::One {
                        let mut commands = p.seek(0);
                        commands.extend(p.play());
                        commands
                    } else {
                        p.next_track()
                    }
                });

                let ended = {
                    let state = app.state::<crate::AppState>();
                    let status = state.player.lock().expect("player mutex").state().status;
                    status == crate::player::Status::Ended
                };
                if ended {
                    engine.stop();
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_aac_pcm_starts_at_the_saved_audio_not_just_the_saved_clock() {
        for name in ["resume-two-tones.m4a", "resume-two-tones-fragmented.m4a"] {
            check_restored_aac_pcm(name, false);
            check_restored_aac_pcm(name, true);
        }
    }

    fn check_restored_aac_pcm(name: &str, streamed: bool) {
        use crate::audio_queue::{Key, Pipeline};
        // First two seconds are 440 Hz; last two are 880 Hz. Generated by
        // ffmpeg's aevalsrc, so the fixture contains no recorded music.
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata").join(name);
        let (pipeline, mut output) = Pipeline::new();
        let ticket = pipeline.begin(Key { revision: 1, index: 0 });
        let source = if streamed {
            LoadedSource::Stream { url: "http://127.0.0.1:9/cached-fixture".into(), cache: fixture }
        } else { LoadedSource::File(fixture) };
        let work = prepare_pcm(pipeline.clone(), ticket, source, 2500);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while pipeline.snapshot().key.is_none() && std::time::Instant::now() < deadline {
            output.next();
            std::thread::sleep(Duration::from_millis(1));
        }
        let state = pipeline.snapshot();
        assert!(state.error.is_none(), "{:?}", state.error);
        assert_eq!(state.key, Some(ticket.key));
        assert!((2500..2510).contains(&state.position_ms));
        let samples: Vec<_> = output.by_ref().take(9600).step_by(2).collect();
        let crossings = samples.windows(2).filter(|s| s[0] <= 0.0 && s[1] > 0.0).count();
        pipeline.stop();
        work.stop();
        assert!((75..100).contains(&crossings),
            "expected the 880 Hz audio at 2.5s, got {crossings} crossings in 100ms");
    }

    #[test]
    fn prepared_decoders_handoff_without_silence_or_lost_pcm() {
        use crate::audio_queue::{Key, Pipeline};
        use std::io::Write;
        let directory = std::env::temp_dir().join(format!("capsule-pcm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let write_wav = |name: &str, sample: i16| {
            let path = directory.join(name);
            let mut file = std::fs::File::create(&path).unwrap();
            let size = 1920u32 * 2;
            file.write_all(b"RIFF").unwrap();
            file.write_all(&(size + 36).to_le_bytes()).unwrap();
            file.write_all(b"WAVEfmt \x10\0\0\0\x01\0\x02\0").unwrap();
            file.write_all(&48000u32.to_le_bytes()).unwrap();
            file.write_all(&192000u32.to_le_bytes()).unwrap();
            file.write_all(b"\x04\0\x10\0data").unwrap();
            file.write_all(&size.to_le_bytes()).unwrap();
            for _ in 0..1920 {
                file.write_all(&sample.to_le_bytes()).unwrap();
            }
            path
        };
        let a = write_wav("a.wav", 8192);
        let b = write_wav("b.wav", 16384);
        let (pipeline, mut output) = Pipeline::new();
        let first = pipeline.begin(Key {
            revision: 1,
            index: 0,
        });
        let second = pipeline
            .plan(
                first.key,
                Some(Key {
                    revision: 2,
                    index: 1,
                }),
            )
            .unwrap();
        prepare_pcm(pipeline.clone(), first, LoadedSource::File(a), 0);
        prepare_pcm(pipeline.clone(), second, LoadedSource::File(b), 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut samples = Vec::new();
        while samples.len() < 3840 && std::time::Instant::now() < deadline {
            let sample = output.next().unwrap();
            if samples.is_empty() && sample == 0.0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            samples.push(sample);
        }
        assert_eq!(samples.len(), 3840);
        assert!(samples[..1920].iter().all(|sample| *sample == 0.25));
        assert!(samples[1920..].iter().all(|sample| *sample == 0.5));
        assert_eq!(pipeline.snapshot().key, Some(second.key));
        pipeline.stop();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn volume_maps_percent_to_unit_and_clamps() {
        assert_eq!(to_gain(0), 0.0);
        assert_eq!(to_gain(100), 1.0);
        assert_eq!(to_gain(50), 0.5);
        assert_eq!(to_gain(150), 1.0, "over 100 must clamp, not distort");
    }

    #[test]
    fn end_of_track_is_only_detected_on_the_playing_to_empty_edge() {
        assert!(track_ended(true, true), "was playing, sink now empty");
        assert!(!track_ended(false, true), "already idle; not an end");
        assert!(!track_ended(true, false), "still has audio queued");
        assert!(!track_ended(false, false));
    }

    #[test]
    fn superseded_decoder_cannot_clear_the_current_loading_state() {
        let loading = Arc::new(AtomicU64::new(2));
        let old = ClearOnDrop {
            loading: loading.clone(),
            ticket: 1,
        };
        drop(old);
        assert_eq!(loading.load(Ordering::SeqCst), 2);
        let current = ClearOnDrop {
            loading: loading.clone(),
            ticket: 2,
        };
        drop(current);
        assert_eq!(loading.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn resolving_and_generation_changes_do_not_end_the_new_queue_entry() {
        let mut detector = EndDetector::default();
        assert!(!detector.observe(1, false, false));
        assert!(
            !detector.observe(2, true, true),
            "empty while resolving is not an end"
        );
        assert!(!detector.observe(2, false, false));
        assert!(
            detector.observe(2, true, false),
            "same generation actually finished"
        );
        assert!(
            !detector.observe(3, true, false),
            "superseded sink is not a track end"
        );
    }

    #[test]
    fn seek_work_is_bounded_and_never_blocks_the_command_caller() {
        use std::sync::mpsc;
        let latest = Arc::new(AtomicU64::new(0));
        let (entered, started) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (result, observed) = mpsc::channel();
        let current = latest.clone();
        let mut first = true;
        let worker = seek_worker(move || {
            if first {
                first = false;
                entered.send(()).unwrap();
                blocked.recv().unwrap();
            }
            result.send(current.load(Ordering::SeqCst)).unwrap();
        });
        worker.try_send(()).unwrap();
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        let started = std::time::Instant::now();
        for ms in 1..=100 {
            latest.store(ms, Ordering::SeqCst);
            let _ = worker.try_send(());
        }
        assert!(started.elapsed() < Duration::from_millis(100));
        release.send(()).unwrap();
        assert_eq!(observed.recv_timeout(Duration::from_secs(1)).unwrap(), 100);
        drop(worker);
    }

    #[test]
    #[ignore]
    fn engine_opens_the_default_device() {
        let e = Engine::new().expect("default output device");
        assert!(e.is_empty());
        assert_eq!(e.position_ms(), 0);
        assert!(!e.is_loading());
    }

    #[test]
    #[ignore = "requires a physical audio device; uses silent PCM only"]
    fn output_transfer_preserves_position_volume_and_paused_or_playing_state() {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("capsule-output-{}.wav", uuid::Uuid::new_v4()));
        let data_size = 44100u32 * 2 * 4;
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(data_size + 36).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&44100u32.to_le_bytes()).unwrap();
        file.write_all(&88200u32.to_le_bytes()).unwrap();
        file.write_all(&2u16.to_le_bytes()).unwrap();
        file.write_all(&16u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&data_size.to_le_bytes()).unwrap();
        file.write_all(&vec![0; data_size as usize]).unwrap();
        drop(file);
        let wait_loaded = |engine: &Engine| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while engine.is_loading() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(!engine.is_loading(), "decoder completed");
            assert!(!engine.is_empty(), "source exists");
        };
        for paused in [true, false] {
            let original = Engine::new().unwrap();
            original.pause();
            original.load_file(path.clone()).unwrap();
            wait_loaded(&original);
            let name = rodio::cpal::default_host()
                .default_output_device()
                .unwrap()
                .name()
                .unwrap();
            assert!(output_devices().unwrap().contains(&name));
            let replacement = Engine::new_for_device(Some(&name)).unwrap();
            original.transfer_to(&replacement, 500, paused, 43).unwrap();
            wait_loaded(&replacement);
            assert_eq!(replacement.is_paused(), paused);
            assert_eq!(replacement.sink.volume(), 0.43);
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while replacement.position_ms() < 450 && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                replacement.position_ms() >= 450,
                "position survived output change"
            );
            assert!(original.is_empty(), "previous sink is stopped");
            replacement.stop();
        }
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires public YouTube access and physical native output; silent"]
    async fn native_youtube_aac_seeks_forward_and_backward() {
        let directory =
            std::env::temp_dir().join(format!("capsule-aac-seek-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let resolver = crate::youtube_native::resolver::Resolver::public(&directory).unwrap();
        let resolved = resolver.resolve("k13FUGmRRHc").await.unwrap();
        assert!(resolved.duration_ms > 65000);
        let engine = Engine::new().unwrap();
        engine.set_volume(0);
        engine.load_gapless(
            resolved.url,
            directory.join("audio.dat"),
            crate::audio_queue::Key {
                revision: 1,
                index: 0,
            },
        );
        engine.play();
        tokio::time::timeout(Duration::from_secs(30), async {
            while engine.is_loading() || engine.position_ms() == 0 {
                assert!(
                    engine.current_error().is_none(),
                    "{:?}",
                    engine.current_error()
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("native AAC starts streaming");
        println!("initial_native_position_ms={}", engine.position_ms());
        engine.pause();
        engine.seek(60_000);
        engine.play();
        let forward = tokio::time::timeout(Duration::from_secs(10), async {
            while engine.position_ms() < 59_000 {
                assert!(
                    engine.current_error().is_none(),
                    "{:?}",
                    engine.current_error()
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        assert!(
            forward.is_ok(),
            "forward seek stayed at {}ms; direct sink diagnostic: {:?}",
            engine.position_ms(),
            engine.sink.try_seek(Duration::from_secs(60))
        );
        let at = engine.position_ms();
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(
            engine.position_ms() > at,
            "playback advances after forward seek"
        );
        println!("forward_native_position_ms={}", engine.position_ms());
        engine.pause();
        engine.seek(10_000);
        engine.play();
        let backward = tokio::time::timeout(Duration::from_secs(10), async {
            while !(9_000..12_000).contains(&engine.position_ms()) {
                assert!(
                    engine.current_error().is_none(),
                    "{:?}",
                    engine.current_error()
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        assert!(
            backward.is_ok(),
            "backward seek stayed at {}ms; direct sink diagnostic: {:?}",
            engine.position_ms(),
            engine.sink.try_seek(Duration::from_secs(10))
        );
        println!("backward_native_position_ms={}", engine.position_ms());
        engine.pause();
        let replacement = Engine::new().unwrap();
        engine.transfer_to(&replacement, 10_000, true, 0).unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            while replacement.is_loading() || !(9_000..12_000).contains(&replacement.position_ms())
            {
                assert!(
                    replacement.current_error().is_none(),
                    "{:?}",
                    replacement.current_error()
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("AAC output switch restores the saved seek");
        assert!(
            replacement.is_paused(),
            "paused AAC must stay paused after output change"
        );
        let at = replacement.position_ms();
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(
            replacement.position_ms(),
            at,
            "paused AAC position cannot advance"
        );
        println!("transferred_paused_native_position_ms={at}");
        replacement.stop();
        drop(replacement);
        engine.stop();
        drop(engine);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
