//! Recordings of a live session, for replay tests.
//!
//! A recording is JSON lines: one `register`, then `events` batches exactly as the server
//! sent them, and optionally a `final_register` fetched at the end. Replaying the events on
//! top of the first register must reproduce the final register's unread state.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    events::parse_batch,
    model::{Conv, State},
    types::{MessageId, Register},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Line {
    Register(Value),
    Events(Vec<Value>),
    FinalRegister(Value),
}

pub struct Recording {
    pub register: Register,
    pub batches: Vec<Vec<Value>>,
    pub final_register: Option<Register>,
}

impl Recording {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let (mut register, mut batches, mut final_register) = (None, vec![], None);
        for (i, l) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            match serde_json::from_str::<Line>(l).map_err(|e| anyhow::anyhow!("line {}: {e}", i + 1))? {
                Line::Register(v) => register = Some(serde_json::from_value(v)?),
                Line::Events(e) => batches.push(e),
                Line::FinalRegister(v) => final_register = Some(serde_json::from_value(v)?),
            }
        }
        Ok(Self { register: register.ok_or_else(|| anyhow::anyhow!("no register line"))?, batches, final_register })
    }

    pub fn replay(&self) -> State {
        let mut s = State::from_register(&self.register);
        for b in &self.batches {
            s.apply_all(&parse_batch(b));
        }
        s
    }

    pub fn event_count(&self) -> usize {
        self.batches.iter().map(Vec::len).sum()
    }
}

/// Differences in unread state between a replayed model and a fresh register.
#[derive(Debug, Default)]
pub struct UnreadDiff {
    /// Unread in the replay but not on the server.
    pub extra: Vec<(MessageId, Conv)>,
    /// Unread on the server but not in the replay.
    pub missing: Vec<(MessageId, Conv)>,
    /// Unread in both, filed under different conversations.
    pub misfiled: Vec<(MessageId, Conv, Conv)>,
    pub mentions_extra: BTreeSet<MessageId>,
    pub mentions_missing: BTreeSet<MessageId>,
}

impl UnreadDiff {
    pub fn is_empty(&self) -> bool {
        self.extra.is_empty()
            && self.missing.is_empty()
            && self.misfiled.is_empty()
            && self.mentions_extra.is_empty()
            && self.mentions_missing.is_empty()
    }
}

/// Compares unreads. The server's `unread_msgs` leaves out messages it does not consider
/// unread for the user (unsubscribed channels, muted senders), so an id unread in the replay
/// counts as `extra` only where the server would have reported it.
pub fn diff_unreads(replayed: &State, fresh: &State) -> UnreadDiff {
    let mut d = UnreadDiff::default();
    let all: BTreeMap<_, _> = replayed.unread.iter().map(|(k, v)| (*k, v.clone())).collect();
    for (id, c) in &all {
        match fresh.unread.get(id) {
            None => {
                let reportable = match c {
                    Conv::Topic(k) => fresh.is_subscribed(k.stream_id),
                    Conv::Dm(_) => true,
                };
                if reportable {
                    d.extra.push((*id, c.clone()));
                }
            }
            Some(f) if f != c => d.misfiled.push((*id, c.clone(), f.clone())),
            _ => {}
        }
    }
    for (id, c) in &fresh.unread {
        if !replayed.unread.contains_key(id) {
            d.missing.push((*id, c.clone()));
        }
    }
    d.missing.sort();
    let (a, b): (BTreeSet<_>, BTreeSet<_>) = (replayed.mentions.iter().copied().collect(), fresh.mentions.iter().copied().collect());
    d.mentions_extra = a.difference(&b).copied().filter(|id| fresh.unread.contains_key(id)).collect();
    d.mentions_missing = b.difference(&a).copied().collect();
    d
}
