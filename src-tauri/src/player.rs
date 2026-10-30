use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    #[serde(default)]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Idle,
    Loading,
    Playing,
    Paused,
    Stalled,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCommand {
    SetQueue {
        ids: Vec<String>,
        start_index: usize,
    },
    Play,
    Pause,
    Seek {
        ms: u64,
    },
    SetVolume {
        percent: u8,
    },
    SkipNext,
    SkipPrevious,
    SetShuffle {
        on: bool,
    },
    SetRepeat {
        mode: u8,
    },
    Prewarm {
        id: String,
    },
}

impl Repeat {
    fn musickit_mode(self) -> u8 {
        match self {
            Repeat::Off => 0,
            Repeat::One => 1,
            Repeat::All => 2,
        }
    }

    fn cycle(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    pub status: Status,
    pub queue: Vec<Track>,
    pub index: Option<usize>,
    pub position_ms: u64,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: Repeat,
    #[serde(default)]
    pub imported_queue: bool,
    #[serde(default)]
    pub playback_source: Option<String>,
    /// Changes only when a new occurrence starts, never when queue rows move.
    #[serde(default)]
    pub playback_revision: u64,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            status: Status::Idle,
            queue: Vec::new(),
            index: None,
            position_ms: 0,
            volume: 100,
            shuffle: false,
            repeat: Repeat::Off,
            imported_queue: false,
            playback_source: None,
            playback_revision: 0,
        }
    }
}

impl PlayerState {
    pub fn current(&self) -> Option<&Track> {
        self.queue.get(self.index?)
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            Status::Playing | Status::Loading | Status::Stalled
        )
    }
}

pub fn map_musickit_state(raw: i64) -> Status {
    match raw {
        0 => Status::Idle,
        1 | 6 => Status::Loading, // loading, seeking
        2 => Status::Playing,
        3 => Status::Paused,
        4 => Status::Idle, // stopped
        5 | 10 => Status::Ended,
        8 | 9 => Status::Stalled, // waiting, stalled
        _ => Status::Loading,
    }
}

pub struct Player {
    state: PlayerState,
    undo: Option<(PlayerState, bool, bool)>,
    pending_restore: bool,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Self {
        Self {
            state: PlayerState::default(),
            undo: None,
            pending_restore: false,
        }
    }

    pub fn state(&self) -> &PlayerState {
        &self.state
    }
    pub fn set_playback_source(&mut self, source: Option<&str>, imported: bool) {
        self.state.playback_source = source.map(str::to_string);
        self.state.imported_queue = imported;
    }

    /// Reconcile provider observations without losing duplicate queue occurrences.
    pub fn observe_track(&mut self, track: Track) {
        let current = self
            .state
            .index
            .filter(|&i| self.state.queue.get(i).is_some_and(|t| t.id == track.id));
        let found = current.or_else(|| {
            let start = self.state.index.map_or(0, |i| i.saturating_add(1));
            (start..self.state.queue.len())
                .chain(0..start.min(self.state.queue.len()))
                .find(|&i| self.state.queue[i].id == track.id)
        });
        let index = match found {
            Some(i) => {
                self.state.queue[i] = track;
                i
            }
            None => {
                self.state.queue.push(track);
                self.state.queue.len() - 1
            }
        };
        if self.state.index != Some(index) {
            self.undo = None;
            self.state.playback_revision = self.state.playback_revision.wrapping_add(1);
            self.state.position_ms = 0;
        }
        self.state.index = Some(index);
    }

    pub fn play_queue(&mut self, tracks: Vec<Track>, start_index: usize) -> Vec<EngineCommand> {
        if tracks.is_empty() {
            return Vec::new();
        }
        let start = start_index.min(tracks.len() - 1);
        let ids = tracks.iter().map(|t| t.id.clone()).collect();
        self.pending_restore = false;
        self.undo = None;
        self.state.playback_revision = self.state.playback_revision.wrapping_add(1);
        self.state.queue = tracks;
        self.state.index = Some(start);
        self.state.position_ms = 0;
        self.state.status = Status::Loading;
        vec![
            EngineCommand::SetQueue {
                ids,
                start_index: start,
            },
            EngineCommand::Play,
        ]
    }

    pub fn play(&mut self) -> Vec<EngineCommand> {
        if self.state.index.is_none() {
            return Vec::new();
        }
        if self.pending_restore {
            self.pending_restore = false;
            let mut commands = self.load_current();
            if let Some(last) = commands.last_mut() {
                *last = EngineCommand::Play;
            }
            commands.push(EngineCommand::SetVolume {
                percent: self.state.volume,
            });
            commands.push(EngineCommand::SetShuffle {
                on: self.state.shuffle,
            });
            commands.push(EngineCommand::SetRepeat {
                mode: self.state.repeat.musickit_mode(),
            });
            if self.state.position_ms > 0 {
                commands.push(EngineCommand::Seek {
                    ms: self.state.position_ms,
                });
            }
            return commands;
        }
        vec![EngineCommand::Play]
    }

    pub fn pause(&mut self) -> Vec<EngineCommand> {
        if !self.state.is_active() {
            return Vec::new();
        }
        vec![EngineCommand::Pause]
    }

    pub fn toggle(&mut self) -> Vec<EngineCommand> {
        match self.state.status {
            Status::Playing | Status::Loading | Status::Stalled => self.pause(),
            Status::Paused | Status::Ended => self.play(),
            Status::Idle => self.play(),
        }
    }

    pub fn next_track(&mut self) -> Vec<EngineCommand> {
        let Some(i) = self.state.index else {
            return Vec::new();
        };
        let len = self.state.queue.len();
        if len == 0 {
            return Vec::new();
        }

        if i + 1 < len {
            self.undo = None;
            self.pending_restore = false;
            self.state.playback_revision = self.state.playback_revision.wrapping_add(1);
            self.state.index = Some(i + 1);
            self.state.position_ms = 0;
            self.state.status = Status::Loading;
            return vec![EngineCommand::SkipNext];
        }

        if self.state.repeat != Repeat::Off {
            return self.jump_to(0);
        }
        self.state.status = Status::Ended;
        self.state.position_ms = 0;
        Vec::new()
    }

    pub fn previous_track(&mut self) -> Vec<EngineCommand> {
        const RESTART_THRESHOLD_MS: u64 = 3_000;

        let Some(i) = self.state.index else {
            return Vec::new();
        };
        if self.state.queue.is_empty() {
            return Vec::new();
        }

        if self.state.position_ms > RESTART_THRESHOLD_MS {
            self.state.position_ms = 0;
            return vec![EngineCommand::Seek { ms: 0 }];
        }

        if i > 0 {
            self.undo = None;
            self.pending_restore = false;
            self.state.playback_revision = self.state.playback_revision.wrapping_add(1);
            self.state.index = Some(i - 1);
            self.state.position_ms = 0;
            self.state.status = Status::Loading;
            return vec![EngineCommand::SkipPrevious];
        }

        if self.state.repeat != Repeat::Off {
            return self.jump_to(self.state.queue.len() - 1);
        }
        self.state.position_ms = 0;
        vec![EngineCommand::Seek { ms: 0 }]
    }

    pub fn jump_to(&mut self, index: usize) -> Vec<EngineCommand> {
        if index >= self.state.queue.len() {
            return Vec::new();
        }
        self.pending_restore = false;
        self.undo = None;
        self.state.playback_revision = self.state.playback_revision.wrapping_add(1);
        self.state.index = Some(index);
        self.state.position_ms = 0;
        self.state.status = Status::Loading;
        let ids = self.state.queue.iter().map(|t| t.id.clone()).collect();
        vec![
            EngineCommand::SetQueue {
                ids,
                start_index: index,
            },
            EngineCommand::Play,
        ]
    }

    pub fn seek(&mut self, ms: u64) -> Vec<EngineCommand> {
        let Some(t) = self.state.current() else {
            return Vec::new();
        };
        let ms = if t.duration_ms > 0 {
            ms.min(t.duration_ms)
        } else {
            ms
        };
        self.state.position_ms = ms;
        if self.pending_restore {
            return Vec::new();
        }
        vec![EngineCommand::Seek { ms }]
    }

    pub fn set_volume(&mut self, percent: u8) -> Vec<EngineCommand> {
        let v = percent.min(100);
        self.state.volume = v;
        if self.pending_restore {
            return vec![];
        }
        vec![EngineCommand::SetVolume { percent: v }]
    }

    pub fn toggle_shuffle(&mut self) -> Vec<EngineCommand> {
        self.state.shuffle = !self.state.shuffle;
        if self.pending_restore {
            return vec![];
        }
        vec![EngineCommand::SetShuffle {
            on: self.state.shuffle,
        }]
    }

    /// Reorder only future occurrences; callers retain the returned original positions.
    /// Provider queues keep their own shuffle behavior and do not call this method.
    pub fn shuffle_upcoming(&mut self, mut choose: impl FnMut(usize) -> usize) -> Vec<usize> {
        let mut order: Vec<_> = (0..self.state.queue.len()).collect();
        if let Some(current) = self.state.index {
            let start = current.saturating_add(1);
            for index in (start.saturating_add(1)..order.len()).rev() {
                let bound = index - start + 1;
                order.swap(index, start + choose(bound) % bound);
            }
        }
        self.reorder_occurrences(&order);
        order
    }

    /// Restore the original order among the remaining occurrences, keeping history intact.
    pub fn restore_upcoming(&mut self, original_positions: &[usize]) -> Vec<usize> {
        let mut order: Vec<_> = (0..self.state.queue.len()).collect();
        if original_positions.len() == order.len() {
            if let Some(current) = self.state.index {
                let start = current.saturating_add(1).min(order.len());
                order[start..].sort_by_key(|&position| original_positions[position]);
            }
        }
        self.reorder_occurrences(&order);
        order
    }

    fn reorder_occurrences(&mut self, order: &[usize]) {
        self.state.queue = order
            .iter()
            .map(|&index| self.state.queue[index].clone())
            .collect();
    }

    pub fn cycle_repeat(&mut self) -> Vec<EngineCommand> {
        self.state.repeat = self.state.repeat.cycle();
        if self.pending_restore {
            return vec![];
        }
        vec![EngineCommand::SetRepeat {
            mode: self.state.repeat.musickit_mode(),
        }]
    }

    #[cfg(test)]
    pub fn set_repeat(&mut self, r: Repeat) {
        self.state.repeat = r;
    }

    pub fn on_playback_state(&mut self, raw: i64) -> Vec<EngineCommand> {
        const ITEM_ENDED: i64 = 5;
        const QUEUE_COMPLETED: i64 = 10;

        self.state.status = map_musickit_state(raw);

        match raw {
            ITEM_ENDED => {
                if self.state.repeat == Repeat::One {
                    self.state.position_ms = 0;
                    return vec![EngineCommand::Seek { ms: 0 }, EngineCommand::Play];
                }
                Vec::new()
            }
            QUEUE_COMPLETED => {
                if self.state.repeat != Repeat::Off {
                    return self.jump_to(0);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Report playback state from a backend that is not MusicKit.
    ///
    /// [`Self::on_playback_state`] speaks MusicKit's integer codes, which a
    /// native engine has no business pretending to emit. Without this the
    /// status never leaves `Loading` and the transport looks stuck.
    /// Forget the queue entirely.
    ///
    /// Track ids only mean something to the source that produced them, so a
    /// queue must not survive a source change - Apple catalog ids handed to a
    /// Subsonic backend fail every load, while the transport keeps claiming to
    /// play. Volume, shuffle and repeat are preferences and stay.
    pub fn reset_queue(&mut self) {
        self.pending_restore = false;
        self.undo = None;
        self.set_playback_source(None, false);
        self.state.queue.clear();
        self.state.index = None;
        self.state.position_ms = 0;
        self.state.status = Status::Idle;
    }

    /// Restore metadata only. The first explicit Play installs the backend queue.
    pub fn restore(&mut self, mut state: PlayerState) {
        state.volume = state.volume.min(100);
        state.index = state.index.filter(|&i| i < state.queue.len());
        if state.index.is_none() && !state.queue.is_empty() {
            state.index = Some(0);
        }
        state.status = if state.index.is_some() {
            Status::Paused
        } else {
            Status::Idle
        };
        if let Some(track) = state.current() {
            if track.duration_ms > 0 {
                state.position_ms = state.position_ms.min(track.duration_ms);
            }
        }
        self.pending_restore = state.index.is_some();
        self.undo = None;
        self.state = state;
    }

    fn load_current(&self) -> Vec<EngineCommand> {
        let Some(start_index) = self.state.index else {
            return vec![EngineCommand::Pause];
        };
        let mut commands = vec![EngineCommand::SetQueue {
            ids: self.state.queue.iter().map(|t| t.id.clone()).collect(),
            start_index,
        }];
        commands.push(if self.state.is_active() || self.pending_restore {
            EngineCommand::Play
        } else {
            EngineCommand::Pause
        });
        commands
    }

    fn remember_queue(&mut self, reload: bool) {
        self.undo = Some((self.state.clone(), reload, self.pending_restore));
    }

    pub fn queue_add(&mut self, tracks: Vec<Track>, next: bool) -> Vec<EngineCommand> {
        if tracks.is_empty() {
            return vec![];
        }
        self.remember_queue(false);
        let at = if next {
            self.state.index.map_or(self.state.queue.len(), |i| i + 1)
        } else {
            self.state.queue.len()
        };
        self.state.queue.splice(at..at, tracks);
        if self.state.index.is_none() {
            self.state.index = Some(0);
            self.state.status = Status::Paused;
            self.pending_restore = true;
        }
        vec![]
    }

    pub fn queue_move(&mut self, from: usize, to: usize) -> Vec<EngineCommand> {
        if from >= self.state.queue.len() || to >= self.state.queue.len() || from == to {
            return vec![];
        }
        self.remember_queue(false);
        let track = self.state.queue.remove(from);
        self.state.queue.insert(to, track);
        self.state.index = self.state.index.map(|i| {
            if i == from {
                to
            } else if from < i && i <= to {
                i - 1
            } else if to <= i && i < from {
                i + 1
            } else {
                i
            }
        });
        vec![]
    }

    pub fn queue_remove(&mut self, index: usize) -> Vec<EngineCommand> {
        if index >= self.state.queue.len() {
            return vec![];
        }
        let reload = self.state.index == Some(index);
        self.remember_queue(reload);
        self.state.queue.remove(index);
        if self.state.queue.is_empty() {
            self.state.index = None;
            self.state.status = Status::Idle;
            self.state.position_ms = 0;
            self.pending_restore = false;
            return vec![EngineCommand::Pause];
        }
        self.state.index = self.state.index.map(|i| {
            if i > index {
                i - 1
            } else {
                i.min(self.state.queue.len() - 1)
            }
        });
        if reload {
            self.state.playback_revision = self.state.playback_revision.wrapping_add(1);
            self.state.position_ms = 0;
            if self.pending_restore {
                return vec![];
            }
            return self.load_current();
        }
        vec![]
    }

    pub fn queue_clear(&mut self) -> Vec<EngineCommand> {
        if self.state.queue.is_empty() {
            return vec![];
        }
        self.remember_queue(true);
        self.state.queue.clear();
        self.state.index = None;
        self.state.position_ms = 0;
        self.state.status = Status::Idle;
        self.pending_restore = false;
        vec![EngineCommand::Pause]
    }

    pub fn queue_undo(&mut self) -> Vec<EngineCommand> {
        let Some((saved, reload, pending_restore)) = self.undo.take() else {
            return vec![];
        };
        let position = self.state.position_ms;
        let status = self.state.status;
        let revision = self.state.playback_revision;
        self.state = saved;
        self.state.playback_revision = revision.wrapping_add(u64::from(reload));
        self.pending_restore = pending_restore;
        if pending_restore {
            return vec![];
        }
        if reload {
            self.pending_restore = false;
            let mut commands = self.load_current();
            commands.push(EngineCommand::Seek {
                ms: self.state.position_ms,
            });
            commands
        } else {
            if self.state.index.is_some() {
                self.state.position_ms = position;
                self.state.status = status;
            }
            vec![]
        }
    }

    pub fn set_status(&mut self, status: Status) {
        self.state.status = status;
    }

    pub fn on_position(&mut self, ms: u64) {
        self.state.position_ms = ms;
    }

    pub fn on_now_playing(&mut self, id: &str, duration_ms: u64) {
        let current = self.state.index.filter(|&index| {
            self.state
                .queue
                .get(index)
                .is_some_and(|track| track.id == id)
        });
        if let Some(pos) = current.or_else(|| self.state.queue.iter().position(|t| t.id == id)) {
            if self.state.index != Some(pos) {
                self.state.playback_revision = self.state.playback_revision.wrapping_add(1);
                self.undo = None;
            }
            self.state.index = Some(pos);
        }
        if duration_ms > 0 {
            if let Some(i) = self.state.index {
                if let Some(t) = self.state.queue.get_mut(i) {
                    t.duration_ms = duration_ms;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: format!("t{id}"),
            artist: "a".into(),
            album: "b".into(),
            duration_ms: 200_000,
        }
    }

    fn loaded() -> Player {
        let mut p = Player::new();
        p.play_queue(vec![track("1"), track("2"), track("3")], 0);
        p
    }

    #[test]
    fn musickit_state_1_is_loading_not_playing() {
        assert_eq!(map_musickit_state(1), Status::Loading);
        assert_eq!(map_musickit_state(2), Status::Playing);
        assert_eq!(map_musickit_state(3), Status::Paused);
        assert_eq!(map_musickit_state(8), Status::Stalled);
        assert_eq!(map_musickit_state(10), Status::Ended);
    }

    #[test]
    fn play_queue_sets_queue_and_emits_commands() {
        let mut p = Player::new();
        let cmds = p.play_queue(vec![track("1"), track("2")], 1);
        assert_eq!(p.state().index, Some(1));
        assert_eq!(p.state().status, Status::Loading);
        assert_eq!(
            cmds,
            vec![
                EngineCommand::SetQueue {
                    ids: vec!["1".into(), "2".into()],
                    start_index: 1
                },
                EngineCommand::Play
            ]
        );
    }

    #[test]
    fn play_queue_clamps_out_of_range_start() {
        let mut p = Player::new();
        p.play_queue(vec![track("1"), track("2")], 99);
        assert_eq!(p.state().index, Some(1));
    }

    #[test]
    fn empty_queue_is_a_noop_not_a_panic() {
        let mut p = Player::new();
        assert!(p.play_queue(vec![], 0).is_empty());
        assert!(p.next_track().is_empty());
        assert!(p.previous_track().is_empty());
        assert!(p.seek(1000).is_empty());
        assert_eq!(p.state().status, Status::Idle);
    }

    #[test]
    fn next_advances_and_stops_at_end_when_repeat_off() {
        let mut p = loaded();
        assert_eq!(p.next_track(), vec![EngineCommand::SkipNext]);
        assert_eq!(p.state().index, Some(1));
        assert_eq!(p.next_track(), vec![EngineCommand::SkipNext]);
        assert_eq!(p.state().index, Some(2));

        let cmds = p.next_track();
        assert!(cmds.is_empty(), "should not wrap with repeat off");
        assert_eq!(p.state().index, Some(2));
        assert_eq!(p.state().status, Status::Ended);
    }

    #[test]
    fn next_wraps_when_repeat_all() {
        let mut p = loaded();
        p.set_repeat(Repeat::All);
        p.next_track();
        p.next_track();
        let cmds = p.next_track();
        assert_eq!(p.state().index, Some(0));
        assert!(matches!(cmds.first(), Some(EngineCommand::SetQueue { .. })));
    }

    #[test]
    fn next_still_advances_under_repeat_one() {
        let mut p = loaded();
        p.set_repeat(Repeat::One);
        p.next_track();
        assert_eq!(p.state().index, Some(1));
    }

    #[test]
    fn previous_restarts_track_when_past_threshold() {
        let mut p = loaded();
        p.next_track();
        p.on_position(9_000);
        let cmds = p.previous_track();
        assert_eq!(cmds, vec![EngineCommand::Seek { ms: 0 }]);
        assert_eq!(p.state().index, Some(1), "should not change track");
        assert_eq!(p.state().position_ms, 0);
    }

    #[test]
    fn previous_goes_back_when_early_in_track() {
        let mut p = loaded();
        p.next_track();
        p.on_position(500);
        assert_eq!(p.previous_track(), vec![EngineCommand::SkipPrevious]);
        assert_eq!(p.state().index, Some(0));
    }

    #[test]
    fn previous_at_first_track_restarts_rather_than_underflowing() {
        let mut p = loaded();
        p.on_position(100);
        let cmds = p.previous_track();
        assert_eq!(p.state().index, Some(0));
        assert_eq!(cmds, vec![EngineCommand::Seek { ms: 0 }]);
    }

    #[test]
    fn previous_wraps_to_last_when_repeat_on() {
        let mut p = loaded();
        p.set_repeat(Repeat::All);
        p.on_position(100);
        p.previous_track();
        assert_eq!(p.state().index, Some(2));
    }

    #[test]
    fn seek_clamps_to_duration() {
        let mut p = loaded();
        let cmds = p.seek(999_999);
        assert_eq!(cmds, vec![EngineCommand::Seek { ms: 200_000 }]);
        assert_eq!(p.state().position_ms, 200_000);
    }

    #[test]
    fn volume_clamps_to_100() {
        let mut p = Player::new();
        let cmds = p.set_volume(250);
        assert_eq!(cmds, vec![EngineCommand::SetVolume { percent: 100 }]);
        assert_eq!(p.state().volume, 100);
    }

    #[test]
    fn toggle_pauses_while_playing_and_resumes_while_paused() {
        let mut p = loaded();
        p.on_playback_state(2); // playing
        assert_eq!(p.toggle(), vec![EngineCommand::Pause]);
        p.on_playback_state(3); // paused
        assert_eq!(p.toggle(), vec![EngineCommand::Play]);
    }

    #[test]
    fn toggle_with_nothing_queued_does_nothing() {
        let mut p = Player::new();
        assert!(p.toggle().is_empty());
    }

    #[test]
    fn item_ended_does_not_skip_because_musickit_auto_advances() {
        let mut p = loaded();
        let cmds = p.on_playback_state(5); // item ended
        assert!(
            cmds.is_empty(),
            "must not command a skip on natural item-end"
        );
        assert_eq!(
            p.state().index,
            Some(0),
            "index follows nowPlaying, not this"
        );
    }

    #[test]
    fn item_ended_under_repeat_one_replays_same_track() {
        let mut p = loaded();
        p.set_repeat(Repeat::One);
        let cmds = p.on_playback_state(5); // item ended
        assert_eq!(p.state().index, Some(0));
        assert_eq!(
            cmds,
            vec![EngineCommand::Seek { ms: 0 }, EngineCommand::Play]
        );
    }

    #[test]
    fn queue_completed_stops_when_repeat_off() {
        let mut p = loaded();
        let cmds = p.on_playback_state(10); // whole queue done
        assert!(cmds.is_empty());
    }

    #[test]
    fn queue_completed_wraps_when_repeat_all() {
        let mut p = loaded();
        p.set_repeat(Repeat::All);
        let cmds = p.on_playback_state(10);
        assert_eq!(p.state().index, Some(0));
        assert!(matches!(cmds.first(), Some(EngineCommand::SetQueue { .. })));
    }

    #[test]
    fn now_playing_resyncs_index_when_engine_disagrees() {
        let mut p = loaded();
        p.on_now_playing("3", 123_000);
        assert_eq!(p.state().index, Some(2));
        assert_eq!(p.state().current().unwrap().duration_ms, 123_000);
    }

    #[test]
    fn now_playing_for_unknown_id_leaves_index_alone() {
        let mut p = loaded();
        p.on_now_playing("does-not-exist", 1000);
        assert_eq!(p.state().index, Some(0));
    }

    #[test]
    fn observed_track_preserves_current_duplicate_occurrence() {
        let mut p = loaded();
        let track = p.state().queue[0].clone();
        p.play_queue(vec![track.clone(), track.clone()], 1);
        p.on_position(500);
        let mut observed = track;
        observed.title = "Updated title".into();
        p.observe_track(observed);
        assert_eq!(p.state().index, Some(1));
        assert_eq!(p.state().position_ms, 500);
        assert_eq!(p.state().queue[1].title, "Updated title");
        assert_ne!(p.state().queue[0].title, "Updated title");
    }

    #[test]
    fn observing_external_track_keeps_the_remaining_queue() {
        let mut p = loaded();
        let mut track = p.state().queue[0].clone();
        track.id = "external".into();
        p.observe_track(track);
        assert_eq!(p.state().current().unwrap().id, "external");
        assert_eq!(p.state().queue.len(), 4);
    }

    #[test]
    fn shuffle_and_restore_preserve_current_duplicate_occurrence_and_played_prefix() {
        let mut p = loaded();
        let original = vec![
            p.state().queue[0].clone(),
            p.state().queue[1].clone(),
            p.state().queue[0].clone(),
            p.state().queue[2].clone(),
        ];
        p.play_queue(original.clone(), 1);
        p.on_position(1234);
        let order = p.shuffle_upcoming(|_| 0);
        assert_eq!(order, vec![0, 1, 3, 2]);
        assert_eq!(p.state().index, Some(1));
        assert_eq!(p.state().position_ms, 1234);
        assert_eq!(p.state().queue[0], original[0]);
        assert_eq!(p.state().queue[1], original[1]);
        let restored = p.restore_upcoming(&order);
        assert_eq!(restored, vec![0, 1, 3, 2]);
        assert_eq!(p.state().queue, original);
    }

    #[test]
    fn repeated_now_playing_observation_keeps_the_current_duplicate_index() {
        let mut p = loaded();
        let track = p.state().queue[0].clone();
        p.play_queue(vec![track.clone(), track], 1);
        p.on_now_playing("1", 1234);
        assert_eq!(p.state().index, Some(1));
    }
}

#[cfg(test)]
mod qol_tests {
    use super::*;
    fn t(id: &str) -> Track {
        Track {
            id: id.into(),
            title: id.into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 10000,
        }
    }
    #[test]
    fn duplicate_queue_edits_preserve_active_occurrence() {
        let mut p = Player::new();
        p.play_queue(vec![t("a"), t("a"), t("b")], 1);
        p.on_position(500);
        assert!(p.queue_move(0, 2).is_empty());
        assert_eq!(p.state().index, Some(0));
        assert_eq!(p.state().position_ms, 500);
        assert!(p.queue_remove(2).is_empty());
        assert_eq!(p.state().queue.len(), 2);
        assert!(p.queue_undo().is_empty());
        assert_eq!(p.state().queue.len(), 3);
        assert_eq!(p.state().position_ms, 500);
    }
    #[test]
    fn restore_is_paused_and_seeks_only_on_play() {
        let mut p = Player::new();
        p.play_queue(vec![t("a")], 0);
        p.on_position(500);
        let saved = p.state().clone();
        let mut restored = Player::new();
        restored.restore(saved);
        assert_eq!(restored.state().status, Status::Paused);
        assert!(restored.play().contains(&EngineCommand::Seek { ms: 500 }));
        assert_eq!(restored.play(), vec![EngineCommand::Play]);
    }
    #[test]
    fn play_next_preserves_order_and_duplicate_occurrences() {
        let mut p = Player::new();
        p.play_queue(vec![t("a"), t("b")], 0);
        p.on_position(800);
        assert!(p.queue_add(vec![t("a"), t("c")], true).is_empty());
        assert_eq!(
            p.state()
                .queue
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "a", "c", "b"]
        );
        assert_eq!(p.state().position_ms, 800);
    }
    #[test]
    fn restored_preferences_never_contact_backend_before_explicit_play() {
        let mut p = Player::new();
        p.play_queue(vec![t("a")], 0);
        let saved = p.state().clone();
        p.restore(saved);
        assert!(p.set_volume(42).is_empty());
        assert!(p.toggle_shuffle().is_empty());
        assert!(p.cycle_repeat().is_empty());
        assert_eq!(p.state().status, Status::Paused);
        let commands = p.play();
        assert!(commands.contains(&EngineCommand::SetVolume { percent: 42 }));
        assert!(commands.contains(&EngineCommand::SetShuffle { on: true }));
    }
    #[test]
    fn restored_seek_does_not_contact_backend_before_play() {
        let mut p = Player::new();
        p.play_queue(vec![t("a")], 0);
        let saved = p.state().clone();
        p.restore(saved);
        assert!(p.seek(900).is_empty());
        assert_eq!(p.state().status, Status::Paused);
        assert!(p.play().contains(&EngineCommand::Seek { ms: 900 }));
    }
    #[test]
    fn editing_or_undoing_restored_queue_never_loads_audio() {
        let mut p = Player::new();
        p.play_queue(vec![t("a"), t("b")], 0);
        let saved = p.state().clone();
        p.restore(saved);
        assert!(p.queue_remove(0).is_empty());
        assert_eq!(p.state().status, Status::Paused);
        assert!(p.queue_undo().is_empty());
        assert_eq!(p.queue_clear(), vec![EngineCommand::Pause]);
        assert!(p.queue_undo().is_empty());
        assert!(p
            .play()
            .iter()
            .any(|c| matches!(c, EngineCommand::SetQueue { .. })));
    }
    #[test]
    fn history_identity_survives_row_edits_but_changes_for_duplicate_playback() {
        let mut p = Player::new();
        p.play_queue(vec![t("a"), t("a"), t("b")], 1);
        let revision = p.state().playback_revision;
        p.queue_move(0, 2);
        p.queue_remove(2);
        assert_eq!(p.state().playback_revision, revision);
        p.jump_to(0);
        assert_ne!(p.state().playback_revision, revision);
    }
    #[test]
    fn undo_expires_after_changing_current_occurrence() {
        let mut p = Player::new();
        p.play_queue(vec![t("a"), t("b"), t("c")], 0);
        p.queue_move(1, 2);
        p.next_track();
        assert!(p.queue_undo().is_empty());
        assert_eq!(p.state().current().unwrap().id, "c");
    }
    #[test]
    fn removing_current_loads_successor_and_clear_can_undo() {
        let mut p = Player::new();
        p.play_queue(vec![t("a"), t("b")], 0);
        assert!(p
            .queue_remove(0)
            .iter()
            .any(|c| matches!(c, EngineCommand::SetQueue { .. })));
        assert_eq!(p.state().current().unwrap().id, "b");
        assert_eq!(p.queue_clear(), vec![EngineCommand::Pause]);
        assert!(p.state().queue.is_empty());
        assert!(!p.queue_undo().is_empty());
        assert_eq!(p.state().current().unwrap().id, "b");
    }
}
