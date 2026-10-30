use rodio::Source;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub const CHANNELS: u16 = 2;
pub const RATE: u32 = 48_000;
const SAMPLES_PER_MS: u64 = RATE as u64 * CHANNELS as u64 / 1000;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Key {
    pub revision: u64,
    pub index: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub generation: u64,
    pub serial: u64,
    pub key: Key,
}
pub enum Chunk {
    Samples(Vec<f32>),
    End,
    Error(String),
}
pub struct Prepared {
    pub first: Vec<f32>,
    pub rest: Receiver<Chunk>,
    pub offset_ms: u64,
    pub space: Option<Arc<(Mutex<()>, Condvar)>>,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub key: Option<Key>,
    pub position_ms: u64,
    pub loading: bool,
    pub ended: bool,
    pub error: Option<String>,
}
pub type Cursor = Arc<Mutex<Snapshot>>;
struct Slot {
    ticket: Ticket,
    ready: Option<Result<Prepared, String>>,
}
#[derive(Default)]
struct Control {
    serial: u64,
    current: Option<Slot>,
    next: Option<Slot>,
    active: Option<Ticket>,
}
pub struct Pipeline {
    generation: AtomicU64,
    control: Mutex<Control>,
    cursor: Cursor,
}
struct Active {
    ticket: Ticket,
    samples: std::vec::IntoIter<f32>,
    rest: Receiver<Chunk>,
    count: u64,
    offset_ms: u64,
    space: Option<Arc<(Mutex<()>, Condvar)>>,
}
pub struct Output {
    pipeline: Arc<Pipeline>,
    active: Option<Active>,
    idle: u64,
    generation: u64,
}
impl Pipeline {
    pub fn new() -> (Arc<Self>, Output) {
        let p = Arc::new(Self {
            generation: AtomicU64::new(0),
            control: Mutex::default(),
            cursor: Arc::default(),
        });
        let out = Output {
            pipeline: p.clone(),
            active: None,
            idle: 0,
            generation: 0,
        };
        (p, out)
    }
    pub fn cursor(&self) -> Cursor {
        self.cursor.clone()
    }
    pub fn begin(&self, key: Key) -> Ticket {
        let mut c = self.control.lock().expect("audio control");
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        c.serial += 1;
        let ticket = Ticket {
            generation,
            serial: c.serial,
            key,
        };
        c.current = Some(Slot {
            ticket,
            ready: None,
        });
        c.next = None;
        c.active = None;
        *self.cursor.lock().expect("audio cursor") = Snapshot {
            loading: true,
            ..Snapshot::default()
        };
        ticket
    }
    /// Replace only the upcoming track. A plan computed before an audio handoff
    /// cannot overwrite the new current track's plan.
    pub fn plan(&self, expected: Key, key: Option<Key>) -> Option<Ticket> {
        let mut c = self.control.lock().expect("audio control");
        let current = c.current.as_ref().map(|s| s.ticket).or(c.active)?;
        if current.key != expected || c.next.as_ref().map(|s| s.ticket.key) == key {
            return None;
        }
        c.next = None;
        let key = key?;
        c.serial += 1;
        let ticket = Ticket {
            generation: current.generation,
            serial: c.serial,
            key,
        };
        c.next = Some(Slot {
            ticket,
            ready: None,
        });
        Some(ticket)
    }
    pub fn complete(&self, ticket: Ticket, result: Result<Prepared, String>) -> bool {
        let mut c = self.control.lock().expect("audio control");
        if self.generation.load(Ordering::SeqCst) != ticket.generation {
            return false;
        }
        let initial = c.current.as_ref().is_some_and(|s| s.ticket == ticket);
        let slot = if initial {
            c.current.as_mut()
        } else {
            c.next.as_mut()
        };
        let Some(slot) = slot.filter(|s| s.ticket == ticket && s.ready.is_none()) else {
            return false;
        };
        if initial {
            let mut cursor = self.cursor.lock().expect("audio cursor");
            cursor.loading = result.is_ok();
            cursor.error = result.as_ref().err().cloned();
        }
        slot.ready = Some(result);
        true
    }
    pub fn valid(&self, ticket: Ticket) -> bool {
        if self.generation.load(Ordering::SeqCst) != ticket.generation {
            return false;
        }
        let c = self.control.lock().expect("audio control");
        c.active == Some(ticket)
            || c.current.as_ref().is_some_and(|s| s.ticket == ticket)
            || c.next.as_ref().is_some_and(|s| s.ticket == ticket)
    }
    fn update_active(&self, ticket: Ticket, update: impl FnOnce(&mut Snapshot)) {
        let c = self.control.lock().expect("audio control");
        if self.generation.load(Ordering::SeqCst) == ticket.generation && c.active == Some(ticket) {
            update(&mut self.cursor.lock().expect("audio cursor"));
        }
    }
    fn fail_active(&self, ticket: Ticket, error: String) {
        let mut c = self.control.lock().expect("audio control");
        if self.generation.load(Ordering::SeqCst) == ticket.generation && c.active == Some(ticket) {
            c.next = None;
            let mut cursor = self.cursor.lock().expect("audio cursor");
            cursor.error = Some(error);
            cursor.loading = false;
        }
    }
    pub fn stop(&self) {
        let mut c = self.control.lock().expect("audio control");
        self.generation.fetch_add(1, Ordering::SeqCst);
        c.current = None;
        c.next = None;
        c.active = None;
        *self.cursor.lock().expect("audio cursor") = Snapshot::default();
    }
    pub fn snapshot(&self) -> Snapshot {
        self.cursor.lock().expect("audio cursor").clone()
    }
}
impl Output {
    fn take_ready(&mut self) -> bool {
        let mut c = self.pipeline.control.lock().expect("audio control");
        let slot = if c.current.is_some() {
            &mut c.current
        } else {
            &mut c.next
        };
        if let Some(s) = slot {
            if let Some(result) = s.ready.take() {
                let ticket = s.ticket;
                *slot = None;
                let mut cursor = self.pipeline.cursor.lock().expect("audio cursor");
                match result {
                    Ok(p) => {
                        c.active = Some(ticket);
                        self.generation = ticket.generation;
                        *cursor = Snapshot {
                            key: Some(ticket.key),
                            position_ms: p.offset_ms,
                            ..Snapshot::default()
                        };
                        self.active = Some(Active {
                            ticket,
                            samples: p.first.into_iter(),
                            rest: p.rest,
                            count: 0,
                            offset_ms: p.offset_ms,
                            space: p.space,
                        });
                        return true;
                    }
                    Err(error) => {
                        c.active = Some(ticket);
                        cursor.key = Some(ticket.key);
                        cursor.position_ms = 0;
                        cursor.error = Some(error);
                        cursor.loading = false;
                    }
                }
            } else {
                self.pipeline.cursor.lock().expect("audio cursor").loading = true;
            }
        } else {
            let mut cursor = self.pipeline.cursor.lock().expect("audio cursor");
            cursor.ended = cursor.key.is_some();
            cursor.loading = false;
        }
        false
    }
}
impl Iterator for Output {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let generation = self.pipeline.generation.load(Ordering::SeqCst);
        if self.generation != generation {
            self.active = None;
            self.idle = 0;
            self.generation = generation;
        }
        loop {
            if let Some(a) = &mut self.active {
                if let Some(sample) = a.samples.next() {
                    a.count += 1;
                    if a.count % SAMPLES_PER_MS == 0 {
                        self.pipeline.update_active(a.ticket, |cursor| {
                            cursor.position_ms = a.offset_ms + a.count / SAMPLES_PER_MS;
                        });
                    }
                    return Some(sample);
                }
                let received = {
                    let _space = a
                        .space
                        .as_ref()
                        .map(|space| space.0.lock().expect("PCM capacity"));
                    let received = a.rest.try_recv();
                    if let Some(space) = &a.space {
                        space.1.notify_one();
                    }
                    received
                };
                match received {
                    Ok(Chunk::Samples(samples)) => {
                        a.samples = samples.into_iter();
                        self.pipeline
                            .update_active(a.ticket, |cursor| cursor.loading = false);
                        continue;
                    }
                    Ok(Chunk::End) => {
                        self.active = None;
                    }
                    Ok(Chunk::Error(error)) => {
                        self.pipeline.fail_active(a.ticket, error);
                        self.active = None;
                        return Some(0.0);
                    }
                    Err(TryRecvError::Empty) => {
                        self.pipeline
                            .update_active(a.ticket, |cursor| cursor.loading = true);
                        return Some(0.0);
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.pipeline
                            .fail_active(a.ticket, "audio decoder stopped unexpectedly".into());
                        self.active = None;
                        return Some(0.0);
                    }
                }
            }
            if self.idle > 0 {
                self.idle -= 1;
                return Some(0.0);
            }
            if !self.take_ready() {
                self.idle = SAMPLES_PER_MS - 1;
                return Some(0.0);
            }
        }
    }
}
impl Source for Output {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        CHANNELS
    }
    fn sample_rate(&self) -> u32 {
        RATE
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    fn key(index: usize) -> Key {
        Key { revision: 1, index }
    }
    fn pcm(samples: &[f32]) -> Prepared {
        let (tx, rest) = mpsc::channel();
        tx.send(Chunk::End).unwrap();
        Prepared {
            first: samples.to_vec(),
            rest,
            offset_ms: 0,
            space: None,
        }
    }
    #[test]
    fn consecutive_tracks_have_no_inserted_or_lost_samples_and_mark_first_sample() {
        let (p, mut out) = Pipeline::new();
        let first = p.begin(key(0));
        assert!(p.complete(first, Ok(pcm(&[1., 2., 3., 4.]))));
        let next = p.plan(key(0), Some(key(1))).unwrap();
        assert!(p.complete(next, Ok(pcm(&[5., 6., 7., 8.]))));
        assert_eq!(
            p.snapshot().key,
            None,
            "preparation must not announce playback"
        );
        assert_eq!(
            out.by_ref().take(4).collect::<Vec<_>>(),
            vec![1., 2., 3., 4.]
        );
        assert_eq!(p.snapshot().key, Some(key(0)));
        assert_eq!(out.next(), Some(5.));
        assert_eq!(p.snapshot().key, Some(key(1)));
        assert_eq!(out.by_ref().take(3).collect::<Vec<_>>(), vec![6., 7., 8.]);
        assert_eq!(out.next(), Some(0.));
        assert!(p.snapshot().ended);
    }
    #[test]
    fn replacing_upcoming_preserves_current_and_rejects_old_worker() {
        let (p, mut out) = Pipeline::new();
        let a = p.begin(key(0));
        p.complete(a, Ok(pcm(&[1., 2.])));
        assert_eq!(out.next(), Some(1.));
        let stale = p.plan(key(0), Some(key(1))).unwrap();
        let next = p.plan(key(0), Some(key(2))).unwrap();
        assert!(!p.complete(stale, Ok(pcm(&[99., 99.]))));
        assert!(p.complete(next, Ok(pcm(&[3., 4.]))));
        assert_eq!(out.take(3).collect::<Vec<_>>(), vec![2., 3., 4.]);
    }
    #[test]
    fn stop_invalidates_workers_and_old_completion_cannot_clear_new_loading() {
        let (p, mut out) = Pipeline::new();
        let old = p.begin(key(0));
        p.stop();
        assert!(!p.valid(old));
        assert!(!p.complete(old, Ok(pcm(&[9., 9.]))));
        let new = p.begin(key(1));
        assert!(!p.complete(old, Err("old failure".into())));
        assert!(p.snapshot().loading);
        assert!(p.snapshot().error.is_none());
        assert!(p.complete(new, Ok(pcm(&[1., 2.]))));
        assert_eq!(out.next(), Some(1.));
    }
    #[test]
    fn pending_decoder_can_finish_before_current_without_reordering() {
        let (p, out) = Pipeline::new();
        let first = p.begin(key(0));
        let next = p.plan(key(0), Some(key(1))).unwrap();
        p.complete(next, Ok(pcm(&[3., 4.])));
        p.complete(first, Ok(pcm(&[1., 2.])));
        assert_eq!(out.take(4).collect::<Vec<_>>(), vec![1., 2., 3., 4.]);
    }
    #[test]
    fn upcoming_error_is_reported_at_boundary_without_cutting_current() {
        let (p, mut out) = Pipeline::new();
        let first = p.begin(key(0));
        p.complete(first, Ok(pcm(&[1., 2.])));
        let next = p.plan(key(0), Some(key(1))).unwrap();
        p.complete(next, Err("decode failed".into()));
        assert!(p.snapshot().error.is_none());
        assert_eq!(out.by_ref().take(2).collect::<Vec<_>>(), vec![1., 2.]);
        out.next();
        assert_eq!(p.snapshot().error.as_deref(), Some("decode failed"));
        assert_eq!(
            p.snapshot().key,
            Some(key(1)),
            "recovery belongs to the failed successor, never the previous recording"
        );
        assert_eq!(p.snapshot().position_ms, 0);
    }
    #[test]
    fn seek_generation_replaces_audio_and_resets_track_relative_position() {
        let (p, mut out) = Pipeline::new();
        let first = p.begin(key(0));
        p.complete(first, Ok(pcm(&[9.; 192])));
        out.by_ref().take(96).for_each(drop);
        assert_eq!(p.snapshot().position_ms, 1);
        let seek = p.begin(Key {
            revision: 2,
            index: 0,
        });
        let mut ready = pcm(&[1., 2.]);
        ready.offset_ms = 5000;
        p.complete(seek, Ok(ready));
        assert_eq!(out.next(), Some(1.));
        assert_eq!(p.snapshot().position_ms, 5000);
    }
    #[test]
    fn removing_prepared_next_does_not_play_it_at_end() {
        let (p, out) = Pipeline::new();
        let a = p.begin(key(0));
        p.complete(a, Ok(pcm(&[1., 2.])));
        let b = p.plan(key(0), Some(key(1))).unwrap();
        p.complete(b, Ok(pcm(&[3., 4.])));
        p.plan(key(0), None);
        assert_eq!(out.take(3).collect::<Vec<_>>(), vec![1., 2., 0.]);
        assert!(p.snapshot().ended);
    }

    #[test]
    fn prepared_initial_track_stays_loading_until_its_first_sample() {
        let (p, mut out) = Pipeline::new();
        let first = p.begin(key(0));
        p.complete(first, Ok(pcm(&[1., 2.])));
        assert!(
            p.snapshot().loading,
            "play during decoder completion must not reload the same track"
        );
        out.next();
        assert!(!p.snapshot().loading);
    }

    #[test]
    fn old_plan_cannot_replace_upcoming_after_sample_boundary() {
        let (p, mut out) = Pipeline::new();
        let a = p.begin(key(0));
        p.complete(a, Ok(pcm(&[1., 2.])));
        let b = p.plan(key(0), Some(key(1))).unwrap();
        p.complete(b, Ok(pcm(&[3., 4.])));
        out.by_ref().take(3).for_each(drop);
        let c = p.plan(key(1), Some(key(2))).unwrap();
        p.complete(c, Ok(pcm(&[5., 6.])));
        assert!(p.plan(key(0), None).is_none());
        assert_eq!(out.take(3).collect::<Vec<_>>(), vec![4., 5., 6.]);
    }

    #[test]
    fn waiting_for_pcm_does_not_advance_position_or_finish_the_track() {
        let (p, mut out) = Pipeline::new();
        let (tx, rest) = mpsc::channel();
        let a = p.begin(key(0));
        p.complete(
            a,
            Ok(Prepared {
                first: vec![1.; 96],
                rest,
                offset_ms: 200,
                space: None,
            }),
        );
        out.by_ref().take(96).for_each(drop);
        assert_eq!(out.next(), Some(0.));
        assert_eq!(p.snapshot().position_ms, 201);
        assert!(p.snapshot().loading);
        assert!(!p.snapshot().ended);
        tx.send(Chunk::Samples(vec![2., 3.])).unwrap();
        tx.send(Chunk::End).unwrap();
        assert_eq!(out.next(), Some(2.));
        assert!(!p.snapshot().loading);
    }
}
