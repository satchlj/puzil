//! The register/events actor. Registers an event queue, long-polls it, and re-registers when
//! the server drops the queue. It sends raw JSON alongside the parsed form so a recorder can
//! save exactly what the server sent.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{
    client::{ApiError, Client, json},
    events::{Event, parse_batch},
    types::Register,
};

/// Event types we subscribe to. `fetch_event_types` adds the initial-state-only keys.
pub const EVENT_TYPES: &[&str] = &[
    "message",
    "update_message",
    "delete_message",
    "update_message_flags",
    "reaction",
    "subscription",
    "stream",
    "realm_user",
    "user_topic",
    "muted_users",
    "realm_emoji",
    // DM typing only: without the `stream_typing_notifications` capability the server
    // leaves out channel typing, which would wake the app for every keystroke anywhere.
    "typing",
    "channel_folder",
];

const FETCH_ONLY: &[&str] = &["unread_msgs", "realm", "recent_private_conversations", "realm_user_groups", "user_settings", "channel_folders"];

pub enum Update {
    /// A fresh register. After the first one, this means the old queue was lost: the
    /// consumer must re-seed unreads and re-validate anything it cached.
    Registered { register: Box<Register>, raw: Value },
    Events { events: Vec<Event>, raw: Vec<Value> },
    /// The credentials were refused. The actor has stopped.
    Unauthorized(String),
    /// A transient failure; the actor retries on its own. Useful for a status indicator.
    Retrying { error: String, after: Duration },
}

pub async fn register(client: &Client) -> Result<(Register, Value), ApiError> {
    let mut fetch: Vec<&str> = EVENT_TYPES.to_vec();
    fetch.extend_from_slice(FETCH_ONLY);
    let caps = json!({
        "notification_settings_null": true,
        "bulk_message_deletion": true,
        "user_avatar_url_field_optional": true,
        "user_settings_object": true,
        "empty_topic_name": true,
        "archived_channels": true,
        "simplified_presence_events": true,
    });
    let raw = client
        .post_value(
            "register",
            &[
                ("apply_markdown", "true".into()),
                ("client_gravatar", "true".into()),
                ("include_subscribers", "false".into()),
                ("slim_presence", "true".into()),
                ("event_types", json(EVENT_TYPES)),
                ("fetch_event_types", json(&fetch)),
                ("client_capabilities", caps.to_string()),
            ],
        )
        .await?;
    let reg: Register = crate::client::decode(raw.clone())?;
    Ok((reg, raw))
}

/// Long-polls one batch. Returns the raw events (heartbeats included).
pub async fn get_events(client: &Client, queue_id: &str, last_event_id: i64, longpoll: Duration) -> Result<Vec<Value>, ApiError> {
    let v = client
        .get_value(
            "events",
            &[("queue_id", queue_id.into()), ("last_event_id", last_event_id.to_string())],
            Some(longpoll + Duration::from_secs(30)),
        )
        .await?;
    Ok(v["events"].as_array().cloned().unwrap_or_default())
}

/// Exponential backoff with full jitter: 1 s doubling to 60 s.
pub struct Backoff(u32);

impl Backoff {
    pub fn new() -> Self {
        Self(0)
    }
    pub fn reset(&mut self) {
        self.0 = 0;
    }
    pub fn next(&mut self) -> Duration {
        let cap = (1u64 << self.0.min(6)).min(60) as f64;
        self.0 += 1;
        let mut b = [0u8; 2];
        getrandom::fill(&mut b).ok();
        let frac = u16::from_le_bytes(b) as f64 / u16::MAX as f64;
        Duration::from_secs_f64((cap * (0.5 + 0.5 * frac)).max(0.5))
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs until the receiver is dropped or the credentials are refused.
pub async fn run(client: Client, tx: mpsc::UnboundedSender<Update>) {
    let mut backoff = Backoff::new();
    loop {
        let (reg, raw) = match register(&client).await {
            Ok(r) => r,
            Err(ApiError::Unauthorized(m)) => {
                let _ = tx.send(Update::Unauthorized(m));
                return;
            }
            Err(e) => {
                let after = match &e {
                    ApiError::RateLimited(d) => *d,
                    _ => backoff.next(),
                };
                if tx.send(Update::Retrying { error: e.to_string(), after }).is_err() {
                    return;
                }
                tokio::time::sleep(after).await;
                continue;
            }
        };
        backoff.reset();
        let Some(queue_id) = reg.queue_id.clone() else {
            tracing::error!("register returned no queue_id");
            return;
        };
        let mut last = reg.last_event_id;
        let longpoll = Duration::from_secs(reg.event_queue_longpoll_timeout_seconds.unwrap_or(90));
        if tx.send(Update::Registered { register: Box::new(reg), raw }).is_err() {
            return;
        }
        // Whether the last poll failed: the first one to succeed afterwards is passed on even if
        // it held only heartbeats, so the consumer knows the connection is back.
        let mut recovering = false;
        loop {
            match get_events(&client, &queue_id, last, longpoll).await {
                Ok(raw) => {
                    backoff.reset();
                    if let Some(max) = raw.iter().filter_map(|e| e["id"].as_i64()).max() {
                        last = last.max(max);
                    }
                    let raw: Vec<Value> = raw.into_iter().filter(|e| e["type"] != "heartbeat").collect();
                    if raw.is_empty() && !std::mem::take(&mut recovering) {
                        continue;
                    }
                    recovering = false;
                    let events = parse_batch(&raw);
                    if tx.send(Update::Events { events, raw }).is_err() {
                        return;
                    }
                }
                Err(ApiError::BadEventQueue) => {
                    tracing::info!("event queue expired; re-registering");
                    break;
                }
                Err(ApiError::Unauthorized(m)) => {
                    let _ = tx.send(Update::Unauthorized(m));
                    return;
                }
                Err(e) => {
                    let after = match &e {
                        ApiError::RateLimited(d) => *d,
                        _ => backoff.next(),
                    };
                    if tx.send(Update::Retrying { error: e.to_string(), after }).is_err() {
                        return;
                    }
                    recovering = true;
                    tokio::time::sleep(after).await;
                }
            }
        }
    }
}
