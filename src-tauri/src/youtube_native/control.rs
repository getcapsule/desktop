use crate::player::{EngineCommand, PlayerState, Status};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Ticket {
    pub generation: u64,
    pub track_id: String,
    pub video_id: String,
}
#[derive(Clone, Default)]
pub(super) struct Intent {
    pub paused: bool,
    pub seek: Option<u64>,
}
struct Pending {
    ticket: Ticket,
    intent: Intent,
    resolving: bool,
    refreshed: bool,
    task: Option<tokio::task::AbortHandle>,
}
#[derive(Default)]
pub(super) struct Control {
    generation: u64,
    current: Option<Pending>,
}
impl Control {
    pub fn begin(&mut self, track_id: String, video_id: String) -> Ticket {
        self.cancel();
        let ticket = Ticket {
            generation: self.generation,
            track_id,
            video_id,
        };
        self.current = Some(Pending {
            ticket: ticket.clone(),
            intent: Intent::default(),
            resolving: true,
            refreshed: false,
            task: None,
        });
        ticket
    }
    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(task) = self.current.take().and_then(|pending| pending.task) {
            task.abort();
        }
    }
    pub fn accepts(&self, ticket: &Ticket) -> bool {
        self.current
            .as_ref()
            .is_some_and(|pending| &pending.ticket == ticket)
    }
    pub fn resolving(&self) -> bool {
        self.current.as_ref().is_some_and(|p| p.resolving)
    }
    pub fn current_video(&self) -> Option<&str> {
        self.current.as_ref().map(|p| p.ticket.video_id.as_str())
    }
    pub fn current_ticket(&self) -> Option<Ticket> {
        self.current.as_ref().map(|pending| pending.ticket.clone())
    }
    /// A refresh only reopens the same selected recording, once per explicit load.
    pub fn refresh(&mut self, expected: &Ticket) -> Option<Ticket> {
        if !self.accepts(expected) || self.current.as_ref()?.refreshed {
            return None;
        }
        let intent = self.current.as_ref()?.intent.clone();
        let ticket = self.begin(expected.track_id.clone(), expected.video_id.clone());
        let pending = self.current.as_mut()?;
        pending.refreshed = true;
        pending.intent = intent;
        Some(ticket)
    }
    pub fn command(&mut self, command: &EngineCommand) {
        if let Some(pending) = self.current.as_mut() {
            match command {
                EngineCommand::Play => pending.intent.paused = false,
                EngineCommand::Pause => pending.intent.paused = true,
                EngineCommand::Seek { ms } => pending.intent.seek = Some(*ms),
                _ => {}
            }
        }
    }
    pub fn set_task(&mut self, ticket: &Ticket, task: tokio::task::AbortHandle) {
        if self.accepts(ticket) {
            self.current.as_mut().unwrap().task = Some(task);
        } else {
            task.abort();
        }
    }
    pub fn finish(&mut self, ticket: &Ticket) -> Option<Intent> {
        if !self.accepts(ticket) {
            return None;
        }
        let pending = self.current.as_mut()?;
        pending.resolving = false;
        Some(pending.intent.clone())
    }
    pub fn reconcile(&mut self, state: &PlayerState, commands: &[EngineCommand]) -> bool {
        if state.status == Status::Ended && commands.is_empty() {
            self.cancel();
            true
        } else {
            false
        }
    }
    pub fn complete_for(&mut self, ticket: &Ticket, state: &PlayerState) -> Option<Intent> {
        if !self.accepts(ticket) {
            return None;
        }
        if state
            .current()
            .is_none_or(|track| track.id != ticket.track_id)
            || matches!(state.status, Status::Ended | Status::Idle)
        {
            self.cancel();
            return None;
        }
        self.finish(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::EngineCommand;

    #[test]
    fn automatic_refresh_is_once_per_load_and_rejects_stale_tickets() {
        let mut control = Control::default();
        let first = control.begin("canonical".into(), "M7lc1UVf-VE".into());
        let retry = control.refresh(&first).expect("first refresh");
        assert!(!control.accepts(&first));
        assert!(
            control.refresh(&retry).is_none(),
            "never loop on an expired stream"
        );
        let next = control.begin("other".into(), "jNQXAC9IVRw".into());
        assert!(
            control.refresh(&retry).is_none(),
            "wrong-track refresh is discarded"
        );
        assert!(control.refresh(&next).is_some());
    }

    #[test]
    fn refreshed_stream_keeps_pause_and_seek_and_cannot_revive_a_new_track() {
        use crate::player::{Player, Track};
        let mut player = Player::new();
        player.play_queue(
            vec![Track {
                id: "other".into(),
                title: String::new(),
                artist: String::new(),
                album: String::new(),
                duration_ms: 1000,
            }],
            0,
        );
        let mut control = Control::default();
        let first = control.begin("canonical".into(), "M7lc1UVf-VE".into());
        control.command(&EngineCommand::Pause);
        control.command(&EngineCommand::Seek { ms: 12_000 });
        let retry = control.refresh(&first).unwrap();
        let intent = control.finish(&retry).unwrap();
        assert!(intent.paused);
        assert_eq!(intent.seek, Some(12_000));
        assert!(control.complete_for(&retry, player.state()).is_none());
        assert!(control.current_video().is_none());
    }

    #[test]
    fn rapid_skip_and_repeated_recordings_reject_stale_completions() {
        let mut control = Control::default();
        let first = control.begin("canonical".into(), "M7lc1UVf-VE".into());
        let second = control.begin("canonical".into(), "M7lc1UVf-VE".into());
        assert!(!control.accepts(&first));
        assert!(control.accepts(&second));
        assert!(control.finish(&first).is_none());
        assert!(control.finish(&second).is_some());
    }

    #[test]
    fn pause_and_latest_seek_survive_async_resolution() {
        let mut control = Control::default();
        let ticket = control.begin("canonical".into(), "M7lc1UVf-VE".into());
        control.command(&EngineCommand::Seek { ms: 3000 });
        control.command(&EngineCommand::Pause);
        control.command(&EngineCommand::Seek { ms: 7000 });
        let intent = control.finish(&ticket).unwrap();
        assert!(intent.paused);
        assert_eq!(intent.seek, Some(7000));
        assert!(!control.resolving());
    }

    #[test]
    fn provider_switch_or_disable_cancels_resolution() {
        let mut control = Control::default();
        let ticket = control.begin("canonical".into(), "M7lc1UVf-VE".into());
        control.cancel();
        assert!(!control.accepts(&ticket));
        assert!(!control.resolving());
        assert!(control.finish(&ticket).is_none());
    }

    #[test]
    fn skipping_past_the_last_entry_cancels_pending_and_active_playback() {
        use crate::player::{Player, Status, Track};
        for completed in [false, true] {
            let mut player = Player::new();
            player.play_queue(
                vec![Track {
                    id: "canonical".into(),
                    title: String::new(),
                    artist: String::new(),
                    album: String::new(),
                    duration_ms: 1000,
                }],
                0,
            );
            let mut control = Control::default();
            let ticket = control.begin("canonical".into(), "M7lc1UVf-VE".into());
            if completed {
                control.finish(&ticket);
                player.set_status(Status::Playing);
            }
            let commands = player.next_track();
            assert!(control.reconcile(player.state(), &commands));
            assert!(control.finish(&ticket).is_none());
            assert!(control.current_video().is_none());
        }
    }

    #[test]
    fn reset_queue_cannot_be_resurrected_by_an_async_completion() {
        use crate::player::{Player, Track};
        let mut player = Player::new();
        player.play_queue(
            vec![Track {
                id: "canonical".into(),
                title: String::new(),
                artist: String::new(),
                album: String::new(),
                duration_ms: 1000,
            }],
            0,
        );
        let mut control = Control::default();
        let ticket = control.begin("canonical".into(), "M7lc1UVf-VE".into());
        player.reset_queue();
        assert!(control.complete_for(&ticket, player.state()).is_none());
        assert!(control.current_video().is_none());
    }
}
