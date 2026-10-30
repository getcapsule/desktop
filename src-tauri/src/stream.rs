use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

#[derive(Debug, Default)]
pub struct CacheState {
    available: u64,
    total: Option<u64>,
    error: Option<String>,
}

impl CacheState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn available(&self) -> u64 {
        self.available
    }

    pub fn total(&self) -> Option<u64> {
        self.total
    }

    pub fn is_complete(&self) -> bool {
        matches!(self.total, Some(t) if self.available >= t)
    }

    pub fn note_written(&mut self, n: u64) {
        self.available += n;
    }

    pub fn set_total(&mut self, total: Option<u64>) {
        self.total = total;
    }

    pub fn fail(&mut self, reason: String) {
        if self.error.is_none() {
            self.error = Some(reason);
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

type CachePair = (Mutex<CacheState>, Condvar);
pub type Shared = Arc<CachePair>;

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("{0}")]
    Fetch(String),
    #[error("timed out waiting for audio data")]
    Timeout,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub fn wait_for(shared: &Shared, want: u64, timeout: Duration) -> Result<u64, StreamError> {
    let (lock, cvar) = &**shared;
    let deadline = Instant::now() + timeout;
    let mut guard = lock.lock().expect("cache state mutex");

    loop {
        if let Some(e) = guard.error() {
            return Err(StreamError::Fetch(e.to_string()));
        }
        if guard.available() >= want || guard.is_complete() {
            return Ok(guard.available());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(StreamError::Timeout);
        }
        let (g, res) = cvar.wait_timeout(guard, left).expect("cache state mutex");
        guard = g;
        if res.timed_out() && guard.available() < want && !guard.is_complete() {
            if guard.error().is_some() {
                continue;
            }
            return Err(StreamError::Timeout);
        }
    }
}

const READ_TIMEOUT: Duration = Duration::from_secs(20);

pub const CACHE_CAP_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub fn cache_dir(app_data: Option<PathBuf>) -> Option<PathBuf> {
    Some(app_data?.join("audio"))
}

pub fn cache_path(dir: &Path, key: &str) -> PathBuf {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    dir.join(format!("{:x}.dat", h.finalize()))
}

pub fn prune(dir: &Path, cap_bytes: u64, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((e.path(), meta.len(), meta.modified().ok()?))
        })
        .collect();

    let mut total: u64 = files.iter().map(|(_, n, _)| n).sum();
    if total <= cap_bytes {
        return;
    }

    files.sort_by_key(|(_, _, t)| *t);
    for (path, size, _) in files {
        if total <= cap_bytes {
            break;
        }
        if keep.is_some_and(|k| k == path) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
            tracing::debug!(path = %path.display(), "pruned audio cache file");
        }
    }
}

fn cached_len(path: &Path) -> Option<u64> {
    let marker = path.with_extension("done");
    let want: u64 = std::fs::read_to_string(&marker).ok()?.trim().parse().ok()?;
    let have = std::fs::metadata(path).ok()?.len();
    (have == want && want > 0).then_some(want)
}

fn mark_complete(path: &Path, len: u64) {
    let marker = path.with_extension("done");
    if let Err(e) = std::fs::write(&marker, len.to_string()) {
        tracing::debug!(error = %e, "could not mark cache entry complete");
    }
}

#[derive(Default)]
struct CancelRoot {
    stopped: AtomicBool,
    wake: Mutex<Option<Weak<CachePair>>>,
    write: Mutex<()>,
}
#[derive(Clone, Default)]
pub struct Cancel {
    root: Arc<CancelRoot>,
    local: Arc<AtomicBool>,
}

impl Cancel {
    pub fn stop(&self) {
        {
            let _write = self.root.write.lock().expect("stream write");
            self.root.stopped.store(true, Ordering::SeqCst);
        }
        if let Some(shared) = self
            .root
            .wake
            .lock()
            .expect("stream cancellation")
            .as_ref()
            .and_then(Weak::upgrade)
        {
            let mut state = shared.0.lock().expect("cache state mutex");
            if !state.is_complete() {
                state.fail("Playback superseded.".into());
            }
            shared.1.notify_all();
        }
    }
    pub(crate) fn stopped(&self) -> bool {
        self.root.stopped.load(Ordering::SeqCst) || self.local.load(Ordering::SeqCst)
    }
    pub(crate) fn write_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.root.write.lock().expect("stream write")
    }
    fn stop_fetch(&self) {
        let _write = self.root.write.lock().expect("stream write");
        self.local.store(true, Ordering::SeqCst);
    }
    fn next_fetch(&self) -> Self {
        self.stop_fetch();
        Self {
            root: self.root.clone(),
            local: Arc::default(),
        }
    }
    fn watch(&self, shared: &Shared) {
        *self.root.wake.lock().expect("stream cancellation") = Some(Arc::downgrade(shared));
        if self.root.stopped.load(Ordering::SeqCst) {
            self.stop();
        }
    }
}

pub fn spawn_fetch(
    url: String,
    path: PathBuf,
    shared: Shared,
    from: u64,
    cancel: Cancel,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if from == 0 {
        if let Some(len) = cached_len(&path) {
            tracing::debug!(path = %path.display(), len, "audio cache hit");
            let mut g = shared.0.lock().expect("cache state mutex");
            g.set_total(Some(len));
            g.note_written(len);
            drop(g);
            shared.1.notify_all();
            return Ok(());
        }
    }

    File::create(&path)?;
    cancel.watch(&shared);

    tauri::async_runtime::spawn(async move {
        if crate::soundcloud::hls::is_manifest_url(&url) {
            let result = crate::soundcloud::hls::download(&url, &path, &shared, &cancel).await;
            if let Err(message) = result {
                shared.0.lock().expect("cache state mutex").fail(message);
                shared.1.notify_all();
            } else if !cancel.stopped() {
                let state = shared.0.lock().expect("cache state mutex");
                if state.is_complete() {
                    mark_complete(&path, state.available());
                }
            }
            return;
        }
        let bounded = crate::youtube_native::resolver::valid_media_url(&url);
        let result = match if crate::soundcloud::validate_audio_url(&url).is_ok() {
            soundcloud_http_client()
        } else {
            http_client(bounded)
        } {
            Ok(client) => {
                transfer(
                    client,
                    &url,
                    &path,
                    &shared,
                    from,
                    &cancel,
                    bounded.then_some(1024 * 1024),
                )
                .await
            }
            Err(()) => Err("could not start the transfer".into()),
        };
        if let Err(message) = result {
            if cancel.stopped() {
                return;
            }
            shared.0.lock().expect("cache state mutex").fail(message);
            shared.1.notify_all();
        } else if from == 0 && !cancel.stopped() {
            let state = shared.0.lock().expect("cache state mutex");
            if state.is_complete() {
                mark_complete(&path, state.available());
            }
        }
    });
    Ok(())
}

fn soundcloud_http_client() -> Result<&'static reqwest::Client, ()> {
    static CLIENT: OnceLock<Result<reqwest::Client, ()>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() < 5
                        && crate::soundcloud::validate_audio_url(attempt.url().as_str()).is_ok()
                    {
                        attempt.follow()
                    } else {
                        attempt.stop()
                    }
                }))
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| ())
}

fn http_client(google_media: bool) -> Result<&'static reqwest::Client, ()> {
    static STANDARD: OnceLock<Result<reqwest::Client, ()>> = OnceLock::new();
    static GOOGLE: OnceLock<Result<reqwest::Client, ()>> = OnceLock::new();
    let slot = if google_media { &GOOGLE } else { &STANDARD };
    slot.get_or_init(|| {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30));
        if google_media {
            builder = builder.redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() < 5
                    && crate::youtube_native::resolver::valid_media_url(attempt.url().as_str())
                {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }));
        }
        builder.build().map_err(|_| ())
    })
    .as_ref()
    .map_err(|_| ())
}

fn content_range(raw: &str, expected_start: u64) -> Option<(u64, u64)> {
    let (range, total) = raw.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total): (u64, u64, u64) =
        (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    (start == expected_start && end >= start && total > end).then_some((end, total))
}

async fn transfer(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    shared: &Shared,
    from: u64,
    cancel: &Cancel,
    chunk_size: Option<u64>,
) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|_| "Could not open the audio buffer.")?;
    let mut offset = from;
    loop {
        if cancel.stopped() {
            return Ok(());
        }
        let mut request = client.get(url);
        if let Some(size) = chunk_size {
            request = request.header(
                reqwest::header::RANGE,
                format!("bytes={offset}-{}", offset.saturating_add(size - 1)),
            );
        } else if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let mut response = request.send().await.map_err(|_| "server unreachable")?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(if chunk_size.is_some() {
                "YouTube denied this audio stream. Retry the recording or choose another source."
            } else {
                "server rejected the login"
            }
            .into());
        }
        if !response.status().is_success() {
            return Err(format!("server returned {}", response.status()));
        }
        let range = if chunk_size.is_some() || offset > 0 {
            if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                return Err("The media server ignored the requested byte range.".into());
            }
            let raw = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .ok_or("Missing media byte range.")?;
            Some(content_range(raw, offset).ok_or("Invalid media byte range.")?)
        } else {
            None
        };
        let expected = range
            .map(|(end, _)| end + 1 - offset)
            .or(response.content_length());
        let total = range
            .map(|(_, total)| total - from)
            .or(response.content_length());
        shared.0.lock().expect("cache state mutex").set_total(total);
        shared.1.notify_all();
        let mut received = 0u64;
        while let Some(bytes) = response.chunk().await.map_err(|_| "connection lost")? {
            if cancel.stopped() {
                return Ok(());
            }
            received += bytes.len() as u64;
            if expected.is_some_and(|expected| received > expected) {
                return Err("The media server returned an invalid range length.".into());
            }
            let _write = cancel.root.write.lock().expect("stream write");
            if cancel.stopped() {
                return Ok(());
            }
            file.write_all(&bytes)
                .map_err(|_| "Could not write the audio buffer.")?;
            shared
                .0
                .lock()
                .expect("cache state mutex")
                .note_written(bytes.len() as u64);
            shared.1.notify_all();
        }
        if expected.is_some_and(|expected| received != expected) {
            return Err("The audio transfer ended early.".into());
        }
        if let Some((end, total)) = range {
            offset = end + 1;
            if chunk_size.is_some() && offset < total {
                continue;
            }
        }
        let mut state = shared.0.lock().expect("cache state mutex");
        if state.total().is_none() {
            let available = state.available();
            state.set_total(Some(available));
        }
        shared.1.notify_all();
        return Ok(());
    }
}

pub struct StreamingSource {
    file: File,
    shared: Shared,
    observer: Shared,
    path: PathBuf,
    url: String,
    base: u64,
    total: Option<u64>,
    pos: u64,
    abandoned: bool,
    cancel: Cancel,
}

impl StreamingSource {
    pub fn new(
        path: PathBuf,
        shared: Shared,
        url: String,
        cancel: Cancel,
    ) -> std::io::Result<Self> {
        Ok(Self {
            file: File::open(&path)?,
            observer: shared.clone(),
            shared,
            path,
            url,
            base: 0,
            total: None,
            pos: 0,
            abandoned: false,
            cancel,
        })
    }

    pub fn caching_abandoned(&self) -> bool {
        self.abandoned
    }

    fn absolute_available(&self) -> u64 {
        self.base + self.shared.0.lock().expect("cache state mutex").available()
    }

    fn known_total(&mut self) -> Option<u64> {
        if self.total.is_none() {
            let base = self.base;
            self.total = self
                .shared
                .0
                .lock()
                .expect("cache state mutex")
                .total()
                .map(|t| base + t);
        }
        self.total
    }

    fn refetch(&mut self, from: u64) -> std::io::Result<()> {
        tracing::debug!(from, "seek outside the buffer; refetching");
        let total = self.known_total();
        let shared: Shared = Arc::new((Mutex::new(CacheState::new()), Condvar::new()));
        let _ = std::fs::remove_file(self.path.with_extension("done"));
        let cancel = self.cancel.next_fetch();
        spawn_fetch(
            self.url.clone(),
            self.path.clone(),
            shared.clone(),
            from,
            cancel.clone(),
        )?;
        self.cancel = cancel;
        self.file = File::open(&self.path)?;
        self.shared = shared;
        self.base = from;
        self.total = total;
        self.abandoned = true;
        Ok(())
    }
}

impl Read for StreamingSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let local_pos = self.pos.saturating_sub(self.base);
        let want = local_pos + buf.len() as u64;
        let available = wait_for(&self.shared, want, READ_TIMEOUT).map_err(|e| {
            let message = e.to_string();
            self.observer
                .0
                .lock()
                .expect("cache state mutex")
                .fail(message.clone());
            self.observer.1.notify_all();
            std::io::Error::other(message)
        })?;

        if available <= local_pos {
            return Ok(0); // EOF
        }
        let can = (available - local_pos).min(buf.len() as u64) as usize;
        self.file.seek(SeekFrom::Start(local_pos))?;
        let n = self.file.read(&mut buf[..can])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Drop for StreamingSource {
    fn drop(&mut self) {
        self.cancel.stop_fetch();
    }
}

impl Seek for StreamingSource {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let total = self.known_total();
        let target = match from {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => self.pos.saturating_add_signed(d),
            SeekFrom::End(d) => {
                let total = total.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "cannot seek from end without a known length",
                    )
                })?;
                total.saturating_add_signed(d)
            }
        };

        if target < self.base || target > self.absolute_available() {
            let past_end = total.is_some_and(|t| target >= t);
            if !past_end && !crate::soundcloud::hls::is_manifest_url(&self.url) {
                self.refetch(target)?;
            }
        }
        self.pos = target;
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("capsule-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        let mut f = File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    fn shared() -> Shared {
        Arc::new((Mutex::new(CacheState::new()), Condvar::new()))
    }

    #[test]
    fn starts_empty_and_incomplete() {
        let s = CacheState::new();
        assert_eq!(s.available(), 0);
        assert_eq!(s.total(), None);
        assert!(!s.is_complete());
        assert_eq!(s.error(), None);
    }

    #[test]
    fn validates_content_ranges_before_advancing_the_stream() {
        assert_eq!(content_range("bytes 4-7/10", 4), Some((7, 10)));
        for raw in [
            "bytes 5-7/10",
            "bytes 4-11/10",
            "bytes 7-4/10",
            "bytes 4-7/*",
            "garbage",
        ] {
            assert_eq!(content_range(raw, 4), None, "{raw}");
        }
    }

    #[test]
    fn dropping_a_stream_cancels_its_latest_transfer() {
        let cancel = Cancel::default();
        let src = StreamingSource::new(
            temp_file("cancel-on-drop.dat", b""),
            shared(),
            String::new(),
            cancel.clone(),
        )
        .unwrap();
        drop(src);
        assert!(cancel.stopped());
    }

    #[test]
    fn engine_cancellation_reaches_seek_refetch_and_wakes_its_reader() {
        let parent = Cancel::default();
        let child = parent.next_fetch();
        let pending = shared();
        child.watch(&pending);
        let waiting = pending.clone();
        let reader = std::thread::spawn(move || wait_for(&waiting, 1, Duration::from_secs(2)));
        parent.stop();
        assert!(child.stopped());
        assert!(matches!(reader.join().unwrap(), Err(StreamError::Fetch(_))));
    }

    #[test]
    fn stop_finishes_an_in_flight_write_before_the_cache_path_is_reused() {
        let cancel = Cancel::default();
        let write = cancel.root.write.lock().unwrap();
        let (done, observed) = std::sync::mpsc::channel();
        let stop = cancel.clone();
        let worker = std::thread::spawn(move || {
            stop.stop();
            done.send(()).unwrap();
        });
        assert!(
            observed.recv_timeout(Duration::from_millis(50)).is_err(),
            "stop must wait until the old cache write finishes"
        );
        drop(write);
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn bounded_ranges_append_until_the_real_end_and_support_seek_offsets() {
        for from in [0, 4] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/audio", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                for start in (from..10).step_by(4) {
                    let (mut socket, _) = listener.accept().unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        socket.read_exact(&mut byte).unwrap();
                        request.push(byte[0]);
                    }
                    let end = (start + 3).min(9);
                    assert!(String::from_utf8_lossy(&request)
                        .to_ascii_lowercase()
                        .contains(&format!("range: bytes={start}-{}", start + 3)));
                    let body = &b"0123456789"[start as usize..=end as usize];
                    write!(socket, "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/10\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    socket.write_all(body).unwrap();
                }
            });
            let path = temp_file(&format!("ranges-{from}.dat"), b"");
            let shared = shared();
            let client = reqwest::Client::new();
            tokio::time::timeout(
                Duration::from_secs(5),
                transfer(
                    &client,
                    &url,
                    &path,
                    &shared,
                    from,
                    &Cancel::default(),
                    Some(4),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            server.join().unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                &b"0123456789"[from as usize..]
            );
            assert_eq!(shared.0.lock().unwrap().total(), Some(10 - from));
            assert!(shared.0.lock().unwrap().is_complete());
        }
    }

    #[test]
    fn written_bytes_accumulate() {
        let mut s = CacheState::new();
        s.note_written(1024);
        s.note_written(512);
        assert_eq!(s.available(), 1536);
    }

    #[test]
    fn complete_only_when_available_reaches_total() {
        let mut s = CacheState::new();
        s.set_total(Some(1000));
        s.note_written(999);
        assert!(!s.is_complete(), "one byte short is not complete");
        s.note_written(1);
        assert!(s.is_complete());
    }

    #[test]
    fn unknown_total_is_never_complete() {
        let mut s = CacheState::new();
        s.note_written(10_000);
        assert!(!s.is_complete());
    }

    #[test]
    fn failure_is_recorded_and_sticky() {
        let mut s = CacheState::new();
        s.fail("server unreachable".into());
        assert_eq!(s.error(), Some("server unreachable"));
        s.note_written(10);
        assert_eq!(
            s.error(),
            Some("server unreachable"),
            "a late write must not clear the error"
        );
    }

    #[test]
    fn returns_immediately_when_bytes_are_already_there() {
        let s = shared();
        s.0.lock().unwrap().note_written(500);
        let got = wait_for(&s, 100, Duration::from_millis(50)).unwrap();
        assert_eq!(got, 500);
    }

    #[test]
    fn wakes_when_bytes_arrive() {
        let s = shared();
        let writer = s.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            writer.0.lock().unwrap().note_written(2048);
            writer.1.notify_all();
        });
        let got = wait_for(&s, 1024, Duration::from_secs(2)).unwrap();
        assert_eq!(got, 2048, "must observe the bytes the writer added");
    }

    #[test]
    fn returns_short_at_end_of_a_complete_stream() {
        let s = shared();
        {
            let mut g = s.0.lock().unwrap();
            g.set_total(Some(100));
            g.note_written(100);
        }
        let got = wait_for(&s, 500, Duration::from_millis(50)).unwrap();
        assert_eq!(got, 100);
    }

    #[test]
    fn surfaces_a_fetch_failure_rather_than_hanging() {
        let s = shared();
        let writer = s.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            writer.0.lock().unwrap().fail("server unreachable".into());
            writer.1.notify_all();
        });
        match wait_for(&s, 1024, Duration::from_secs(2)) {
            Err(StreamError::Fetch(m)) => assert_eq!(m, "server unreachable"),
            other => panic!("expected a fetch error, got {other:?}"),
        }
    }

    #[test]
    fn times_out_instead_of_blocking_forever() {
        let s = shared();
        assert!(matches!(
            wait_for(&s, 1024, Duration::from_millis(60)),
            Err(StreamError::Timeout)
        ));
    }

    fn complete(s: &Shared, n: u64) {
        let mut g = s.0.lock().unwrap();
        g.set_total(Some(n));
        g.note_written(n);
    }

    #[test]
    fn reads_bytes_that_have_landed() {
        let p = temp_file("read.dat", b"0123456789");
        let s = shared();
        complete(&s, 10);
        let mut src = StreamingSource::new(p, s, String::new(), Cancel::default()).unwrap();
        let mut buf = [0u8; 4];
        src.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"0123");
    }

    #[test]
    fn seek_inside_the_prefix_does_not_abandon_caching() {
        let p = temp_file("seek-in.dat", b"0123456789");
        let s = shared();
        complete(&s, 10);
        let mut src = StreamingSource::new(p, s, String::new(), Cancel::default()).unwrap();
        assert_eq!(src.seek(SeekFrom::Start(6)).unwrap(), 6);
        let mut buf = [0u8; 2];
        src.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"67");
        assert!(
            !src.caching_abandoned(),
            "a seek within cached bytes is free"
        );
    }

    #[test]
    fn seek_beyond_the_prefix_abandons_caching() {
        let p = temp_file("seek-out.dat", b"01234");
        let s = shared();
        {
            let mut g = s.0.lock().unwrap();
            g.set_total(Some(1_000_000));
            g.note_written(5);
        }
        let mut src = StreamingSource::new(p, s, String::new(), Cancel::default()).unwrap();
        src.seek(SeekFrom::Start(900_000)).unwrap();
        assert!(src.caching_abandoned());
    }

    #[test]
    fn seek_from_end_uses_the_known_total() {
        let p = temp_file("seek-end.dat", b"0123456789");
        let s = shared();
        complete(&s, 10);
        let mut src = StreamingSource::new(p, s, String::new(), Cancel::default()).unwrap();
        assert_eq!(src.seek(SeekFrom::End(-2)).unwrap(), 8);
    }

    #[test]
    fn seek_from_end_without_a_total_is_an_error_not_a_guess() {
        let p = temp_file("seek-end-none.dat", b"0123456789");
        let s = shared();
        s.0.lock().unwrap().note_written(10);
        let mut src = StreamingSource::new(p, s, String::new(), Cancel::default()).unwrap();
        assert!(src.seek(SeekFrom::End(-2)).is_err());
    }

    #[test]
    fn a_total_learned_after_a_refetch_counts_from_the_start_of_the_file() {
        let p = temp_file("seek-end-refetch.dat", b"01234");
        let s = shared();
        s.0.lock().unwrap().note_written(5);
        let mut src = StreamingSource::new(p, s, String::new(), Cancel::default()).unwrap();

        src.seek(SeekFrom::Start(900_000)).unwrap();
        assert_eq!(src.base, 900_000);
        src.shared.0.lock().unwrap().set_total(Some(100_000));

        assert_eq!(src.seek(SeekFrom::End(-2)).unwrap(), 999_998);
    }

    #[test]
    fn cache_path_is_stable_and_distinct_per_track() {
        let dir = Path::new(r"C:\cache");
        assert_eq!(cache_path(dir, "tr-1"), cache_path(dir, "tr-1"));
        assert_ne!(cache_path(dir, "tr-1"), cache_path(dir, "tr-2"));
    }

    #[test]
    fn prune_evicts_oldest_first_and_spares_the_playing_file() {
        let dir = std::env::temp_dir().join(format!("capsule-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let old = dir.join("old.dat");
        let mid = dir.join("mid.dat");
        let playing = dir.join("playing.dat");
        for p in [&old, &mid, &playing] {
            std::fs::write(p, vec![0u8; 400]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }

        prune(&dir, 500, Some(&playing));

        assert!(playing.exists(), "the in-use file must never be evicted");
        assert!(!old.exists(), "oldest goes first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_completed_download_is_reusable() {
        let p = temp_file("cached.dat", b"0123456789");
        mark_complete(&p, 10);
        assert_eq!(cached_len(&p), Some(10));
    }

    #[test]
    fn a_partial_download_is_not_reusable() {
        let p = temp_file("partial.dat", b"01234");
        assert_eq!(cached_len(&p), None);
    }

    #[test]
    fn a_truncated_file_is_rejected_even_with_a_marker() {
        let p = temp_file("short.dat", b"012");
        mark_complete(&p, 10);
        assert_eq!(cached_len(&p), None, "length must match what was recorded");
    }

    #[test]
    fn prune_does_nothing_below_the_cap() {
        let dir = std::env::temp_dir().join(format!("capsule-prune2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.dat");
        std::fs::write(&f, vec![0u8; 100]).unwrap();
        prune(&dir, 10_000, None);
        assert!(f.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fetch_failure_surfaces_as_an_io_error() {
        let p = temp_file("fail.dat", b"");
        let s = shared();
        s.0.lock().unwrap().fail("server unreachable".into());
        let mut src = StreamingSource::new(p, s, String::new(), Cancel::default()).unwrap();
        let mut buf = [0u8; 4];
        assert!(src.read(&mut buf).is_err());
    }
}
