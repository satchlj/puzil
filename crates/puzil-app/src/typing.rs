//! Typing notifications in DMs: who is typing to us, and telling the server when we are.
//! Pure bookkeeping; [`crate::session::Session`] runs the timers and the requests.
//!
//! Channel typing isn't asked for (register leaves out `stream_typing_notifications`), so
//! the server sends only DM typing events: few, and only while someone writes to us.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use puzil_zulip::{model::Conv, types::UserId};

/// Zulip's periods, as register gives them (`server_typing_*_period_milliseconds`); these
/// are its defaults for a server too old to send them.
const EXPIRY: Duration = Duration::from_secs(15);
const STARTED_WAIT: Duration = Duration::from_secs(10);
const STOPPED_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq)]
pub enum Notice {
    Start(Conv),
    Stop(Conv),
}

pub struct Typing {
    /// Who is typing in each conversation, and when that lapses without a fresh start.
    seen: HashMap<Conv, Vec<(UserId, Instant)>>,
    /// How long a start event holds without being repeated.
    pub expiry: Duration,
    /// How often to repeat a start while we keep typing.
    pub started_wait: Duration,
    /// How long after our last keystroke to send stop.
    pub stopped_wait: Duration,
    /// The conversation we last told the server we're typing in, and when.
    sent: Option<(Conv, Instant)>,
    last_key: Option<Instant>,
}

impl Default for Typing {
    fn default() -> Self {
        Self { seen: HashMap::new(), expiry: EXPIRY, started_wait: STARTED_WAIT, stopped_wait: STOPPED_WAIT, sent: None, last_key: None }
    }
}

impl Typing {
    pub fn set_periods(&mut self, raw: &serde_json::Value) {
        let ms = |k: &str, d: Duration| raw[k].as_u64().map(Duration::from_millis).unwrap_or(d);
        self.expiry = ms("server_typing_started_expiry_period_milliseconds", EXPIRY);
        self.started_wait = ms("server_typing_started_wait_period_milliseconds", STARTED_WAIT);
        self.stopped_wait = ms("server_typing_stopped_wait_period_milliseconds", STOPPED_WAIT);
    }

    /// A typing event. Returns whether who's typing in `conv` changed.
    pub fn event(&mut self, conv: Conv, sender: UserId, start: bool, now: Instant) -> bool {
        let list = self.seen.entry(conv.clone()).or_default();
        let had = list.iter().position(|(u, _)| *u == sender);
        let changed = match (had, start) {
            (Some(i), true) => {
                list[i].1 = now + self.expiry;
                false
            }
            (None, true) => {
                list.push((sender, now + self.expiry));
                true
            }
            (Some(i), false) => {
                list.remove(i);
                true
            }
            (None, false) => false,
        };
        if list.is_empty() {
            self.seen.remove(&conv);
        }
        changed
    }

    /// A message from `sender` arrived in `conv`: they've stopped typing it.
    pub fn message_from(&mut self, conv: &Conv, sender: UserId, now: Instant) -> bool {
        self.seen.contains_key(conv) && self.event(conv.clone(), sender, false, now)
    }

    /// Who is typing in `conv`, in the order they started.
    pub fn who(&self, conv: &Conv, now: Instant) -> Vec<UserId> {
        self.seen.get(conv).map(|l| l.iter().filter(|(_, until)| *until > now).map(|(u, _)| *u).collect()).unwrap_or_default()
    }

    /// Whether anyone is marked as typing anywhere.
    pub fn anyone(&self) -> bool {
        !self.seen.is_empty()
    }

    /// The conversation we've told the server we're typing in.
    pub fn typing_to(&self) -> Option<&Conv> {
        self.sent.as_ref().map(|(c, _)| c)
    }

    /// Drops lapsed entries; returns the conversations that changed.
    pub fn prune(&mut self, now: Instant) -> Vec<Conv> {
        let mut changed = Vec::new();
        self.seen.retain(|c, l| {
            let n = l.len();
            l.retain(|(_, until)| *until > now);
            if l.len() != n {
                changed.push(c.clone());
            }
            !l.is_empty()
        });
        changed
    }

    pub fn next_lapse(&self) -> Option<Instant> {
        self.seen.values().flatten().map(|(_, until)| *until).min()
    }

    /// A new event queue: stop events sent meanwhile were lost, so forget everyone.
    pub fn clear(&mut self) -> Vec<Conv> {
        self.seen.drain().map(|(c, _)| c).collect()
    }

    /// We edited the composer of `conv`. `empty` is whether it's empty now.
    pub fn keystroke(&mut self, conv: &Conv, empty: bool, now: Instant) -> Vec<Notice> {
        let mut out = Vec::new();
        if self.sent.as_ref().is_some_and(|(c, _)| empty || c != conv) {
            out.push(Notice::Stop(self.sent.take().unwrap().0));
        }
        if empty {
            self.last_key = None;
            return out;
        }
        self.last_key = Some(now);
        if self.sent.as_ref().is_none_or(|(_, at)| now.duration_since(*at) >= self.started_wait) {
            self.sent = Some((conv.clone(), now));
            out.push(Notice::Start(conv.clone()));
        }
        out
    }

    /// Whether we're still marked as typing; sends stop once we've paused long enough.
    /// Returns the stop to send, or `None` (and the time to check again) while not yet due.
    pub fn idle(&mut self, now: Instant) -> Result<Option<Notice>, Instant> {
        let (Some(_), Some(last)) = (&self.sent, self.last_key) else {
            return Ok(None);
        };
        let due = last + self.stopped_wait;
        if now < due {
            return Err(due);
        }
        self.last_key = None;
        Ok(self.sent.take().map(|(c, _)| Notice::Stop(c)))
    }

    /// We sent a message: no longer typing.
    pub fn sent_message(&mut self) -> Option<Notice> {
        self.last_key = None;
        self.sent.take().map(|(c, _)| Notice::Stop(c))
    }
}

#[cfg(test)]
mod tests {
    use puzil_zulip::model::DmKey;

    use super::*;

    fn dm(u: u64) -> Conv {
        Conv::Dm(DmKey::new(1, [u]))
    }

    #[test]
    fn start_shows_until_stop_or_lapse() {
        let mut t = Typing::default();
        let now = Instant::now();
        assert!(t.event(dm(2), 2, true, now));
        assert!(!t.event(dm(2), 2, true, now + Duration::from_secs(5)), "a repeated start changes nothing shown");
        assert_eq!(t.who(&dm(2), now + Duration::from_secs(19)), vec![2], "the repeat extends it");
        assert!(t.who(&dm(2), now + Duration::from_secs(21)).is_empty());
        assert_eq!(t.prune(now + Duration::from_secs(21)), vec![dm(2)]);
        assert_eq!(t.next_lapse(), None);

        assert!(t.event(dm(2), 2, true, now));
        assert!(t.message_from(&dm(2), 2, now), "their message ends it");
        assert!(!t.message_from(&dm(3), 3, now));
        assert!(t.who(&dm(2), now).is_empty());
        assert!(!t.event(dm(2), 2, false, now), "a stop for nobody");
    }

    #[test]
    fn our_typing_starts_repeats_and_stops() {
        let mut t = Typing::default();
        let now = Instant::now();
        let s = |n| now + Duration::from_secs(n);
        assert_eq!(t.keystroke(&dm(2), false, now), vec![Notice::Start(dm(2))]);
        assert_eq!(t.keystroke(&dm(2), false, s(3)), vec![]);
        assert_eq!(t.idle(s(4)), Err(s(8)), "5 s after the last key");
        assert_eq!(t.keystroke(&dm(2), false, s(10)), vec![Notice::Start(dm(2))], "repeated every 10 s");
        assert_eq!(t.idle(s(15)), Ok(Some(Notice::Stop(dm(2)))));
        assert_eq!(t.idle(s(16)), Ok(None));

        assert_eq!(t.keystroke(&dm(2), false, s(20)), vec![Notice::Start(dm(2))]);
        assert_eq!(t.keystroke(&dm(3), false, s(21)), vec![Notice::Stop(dm(2)), Notice::Start(dm(3))], "another conversation");
        assert_eq!(t.keystroke(&dm(3), true, s(22)), vec![Notice::Stop(dm(3))], "emptied");
        assert_eq!(t.keystroke(&dm(3), true, s(23)), vec![]);
        assert_eq!(t.keystroke(&dm(3), false, s(24)), vec![Notice::Start(dm(3))]);
        assert_eq!(t.sent_message(), Some(Notice::Stop(dm(3))));
        assert_eq!(t.idle(s(40)), Ok(None));
    }
}
