//! Keeping track of how far an observer has got.

use pirs_protocol::Event;

/// The last `seq` a client has seen, so it can resume where it left off.
///
/// Every event except a streaming delta carries a monotonic per-loop `seq`:
/// the index of its entry in the loop's session log, which *is* the event
/// stream (D-06). A client that remembers the highest `seq` it has processed
/// can reconnect and call
/// [`subscribe_with_replay`](crate::Client::subscribe_with_replay) with
/// `since = last_seq` to receive everything it missed and nothing it already
/// has. Deltas are not logged and never replayed, so they are not tracked.
///
/// One tracker follows one loop. A client watching several loops keeps one per
/// loop id.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeqTracker {
    last: Option<u64>,
}

impl SeqTracker {
    /// A tracker that has seen nothing; [`since`](Self::since) is `Some(0)`,
    /// which replays the whole log.
    pub fn new() -> Self {
        SeqTracker { last: None }
    }

    /// A tracker resuming from a known point, such as the `seq` in
    /// [`LoopAttachResult`](pirs_protocol::LoopAttachResult).
    pub fn resuming_from(seq: u64) -> Self {
        SeqTracker { last: Some(seq) }
    }

    /// The highest `seq` seen so far, or `None` before the first sequenced
    /// event.
    pub fn last_seq(&self) -> Option<u64> {
        self.last
    }

    /// What to pass as `subscribe`'s `since`: the last `seq` seen, or `0` to
    /// replay everything.
    pub fn since(&self) -> u64 {
        self.last.unwrap_or(0)
    }

    /// Record an event and say whether it is new.
    ///
    /// `false` means the event has already been seen — a replay after a
    /// reconnect can overlap — and the caller should skip it. Deltas carry no
    /// `seq`, are never replayed, and are always new.
    pub fn observe(&mut self, event: &Event) -> bool {
        match event.seq() {
            None => true,
            Some(seq) => match self.last {
                Some(last) if seq <= last => false,
                _ => {
                    self.last = Some(seq);
                    true
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pirs_protocol::{
        Delta, LoopMessageBody, LoopMessageEvent, LoopState, LoopStatusEvent, Role,
    };

    fn status(seq: u64) -> Event {
        Event::LoopStatus(LoopStatusEvent {
            loop_id: "l1".to_owned(),
            seq,
            state: LoopState::Idle,
            since: 0,
            detail: None,
        })
    }

    fn delta() -> Event {
        Event::LoopMessage(LoopMessageEvent {
            loop_id: "l1".to_owned(),
            seq: None,
            role: Role::Assistant,
            body: LoopMessageBody::Delta {
                delta: Delta::Text {
                    index: 0,
                    text: "x".to_owned(),
                },
            },
        })
    }

    #[test]
    fn tracks_the_highest_seq_and_skips_replays() {
        let mut tracker = SeqTracker::new();
        assert_eq!(tracker.since(), 0);
        assert_eq!(tracker.last_seq(), None);

        assert!(tracker.observe(&status(3)));
        assert_eq!(tracker.last_seq(), Some(3));
        assert_eq!(tracker.since(), 3);

        // A replay of what we already have.
        assert!(!tracker.observe(&status(3)));
        assert!(!tracker.observe(&status(1)));
        assert_eq!(tracker.last_seq(), Some(3));

        // Deltas are never sequenced and never suppressed.
        assert!(tracker.observe(&delta()));
        assert_eq!(tracker.last_seq(), Some(3));

        assert!(tracker.observe(&status(4)));
        assert_eq!(tracker.since(), 4);
    }

    #[test]
    fn resuming_skips_everything_up_to_the_mark() {
        let mut tracker = SeqTracker::resuming_from(10);
        assert!(!tracker.observe(&status(10)));
        assert!(tracker.observe(&status(11)));
    }
}
