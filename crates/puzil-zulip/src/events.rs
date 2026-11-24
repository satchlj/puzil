//! Typed events from `GET /events`. Each event is parsed on its own: an event we do not
//! model, or one whose shape surprises us, becomes [`Event::Other`] instead of failing the
//! whole batch.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::types::*;

#[derive(Debug, Clone)]
pub enum Event {
    Message { message: Box<Message>, flags: Vec<String> },
    UpdateMessage(Box<UpdateMessage>),
    /// `stream_id` is set for channel messages (their topic list may have lost a topic).
    DeleteMessage { message_ids: Vec<MessageId>, stream_id: Option<StreamId> },
    UpdateMessageFlags(UpdateFlags),
    Reaction { add: bool, message_id: MessageId, reaction: Reaction },
    Subscription(SubscriptionEvent),
    Stream(StreamEvent),
    RealmUser(RealmUserEvent),
    UserTopic(UserTopic),
    MutedUsers(Vec<MutedUser>),
    RealmEmoji(HashMap<String, RealmEmoji>),
    /// `channel_folder` add, update or reorder, applied from the raw event.
    ChannelFolder(Value),
    /// `realm` update or update_dict: organization settings, applied from the raw event.
    Realm(Value),
    /// `user_group` add, update, remove, and member and subgroup changes, from the raw event.
    UserGroup(Value),
    /// `default_streams`: the channels new users join.
    DefaultStreams(Vec<Value>),
    Typing { start: bool, sender: UserId, stream_id: Option<StreamId>, topic: Option<String>, recipients: Vec<UserId> },
    Heartbeat,
    /// Unmodeled or malformed. `kind` is the event's `type`/`op`.
    Other { kind: String, raw: Value },
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateMessage {
    #[serde(default)]
    pub user_id: Option<UserId>,
    pub message_id: Option<MessageId>,
    #[serde(default)]
    pub message_ids: Vec<MessageId>,
    #[serde(default)]
    pub rendering_only: bool,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub edit_timestamp: Option<i64>,
    #[serde(default)]
    pub rendered_content: Option<String>,
    #[serde(default)]
    pub propagate_mode: Option<String>,
    #[serde(default)]
    pub stream_id: Option<StreamId>,
    #[serde(default)]
    pub new_stream_id: Option<StreamId>,
    #[serde(default)]
    pub orig_subject: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
}

impl UpdateMessage {
    /// A topic or channel move (including resolve/unresolve, which is a rename).
    pub fn is_move(&self) -> bool {
        self.new_stream_id.is_some() || (self.subject.is_some() && self.orig_subject.is_some())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFlags {
    pub op: String,
    pub flag: String,
    #[serde(default)]
    pub messages: Vec<MessageId>,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub message_details: HashMap<String, MessageDetails>,
}

impl UpdateFlags {
    pub fn add(&self) -> bool {
        self.op == "add"
    }
}

#[derive(Debug, Clone)]
pub enum SubscriptionEvent {
    Add(Vec<(Stream, Subscription)>),
    Remove(Vec<StreamId>),
    Update { stream_id: StreamId, property: String, value: Value },
    /// Other people joined (or left) channels: `peer_add` / `peer_remove`.
    Peers { add: bool, stream_ids: Vec<StreamId>, user_ids: Vec<UserId> },
    Other(String),
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Create(Vec<Stream>),
    Delete(Vec<StreamId>),
    /// `raw` is the whole event: some updates carry related fields alongside (a privacy change
    /// sends the history and web-public flags, a description its rendered form).
    Update { stream_id: StreamId, property: String, value: Value, raw: Value },
    Other(String),
}

#[derive(Debug, Clone)]
pub enum RealmUserEvent {
    Add(User),
    Remove(UserId),
    /// A partial user: `user_id` plus whichever fields changed.
    Update(Value),
    Other(String),
}

pub fn parse_batch(events: &[Value]) -> Vec<Event> {
    events.iter().map(parse).collect()
}

pub fn parse(v: &Value) -> Event {
    let ty = v["type"].as_str().unwrap_or("");
    let op = v["op"].as_str().unwrap_or("");
    match try_parse(ty, op, v) {
        Ok(e) => e,
        Err(err) => {
            let kind = if op.is_empty() { ty.to_string() } else { format!("{ty}/{op}") };
            if !matches!(err, Unmodeled) {
                tracing::warn!("cannot parse {kind} event: {err:?}");
            }
            Event::Other { kind, raw: v.clone() }
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // the message is read through Debug in the warning
enum ParseErr {
    Unmodeled,
    Bad(String),
}
use ParseErr::Unmodeled;

impl From<serde_json::Error> for ParseErr {
    fn from(e: serde_json::Error) -> Self {
        Self::Bad(e.to_string())
    }
}

fn field<T: serde::de::DeserializeOwned>(v: &Value, k: &str) -> Result<T, ParseErr> {
    T::deserialize(&v[k]).map_err(|e| ParseErr::Bad(format!("{k}: {e}")))
}

fn try_parse(ty: &str, op: &str, v: &Value) -> Result<Event, ParseErr> {
    Ok(match ty {
        "heartbeat" => Event::Heartbeat,
        "channel_folder" => Event::ChannelFolder(v.clone()),
        "message" => Event::Message { message: Box::new(field(v, "message")?), flags: field(v, "flags").unwrap_or_default() },
        "update_message" => Event::UpdateMessage(Box::new(UpdateMessage::deserialize(v)?)),
        "delete_message" => {
            // Bulk form (with the `bulk_message_deletion` capability) has `message_ids`.
            let ids = if v.get("message_ids").is_some() { field(v, "message_ids")? } else { vec![field(v, "message_id")?] };
            Event::DeleteMessage { message_ids: ids, stream_id: v["stream_id"].as_u64() }
        }
        "update_message_flags" => {
            // Older servers use `operation` instead of `op`.
            let mut f = v.clone();
            if f.get("op").is_none() {
                f["op"] = f["operation"].clone();
            }
            Event::UpdateMessageFlags(UpdateFlags::deserialize(&f)?)
        }
        "reaction" => Event::Reaction {
            add: op == "add",
            message_id: field(v, "message_id")?,
            reaction: Reaction::deserialize(v)?,
        },
        "subscription" => Event::Subscription(match op {
            "add" => {
                let subs: Vec<Value> = field(v, "subscriptions")?;
                let pairs = subs
                    .iter()
                    .map(|s| Ok((Stream::deserialize(s)?, Subscription::deserialize(s)?)))
                    .collect::<Result<_, serde_json::Error>>()?;
                SubscriptionEvent::Add(pairs)
            }
            "remove" => {
                let subs: Vec<Value> = field(v, "subscriptions")?;
                SubscriptionEvent::Remove(subs.iter().filter_map(|s| s["stream_id"].as_u64()).collect())
            }
            "update" => SubscriptionEvent::Update {
                stream_id: field(v, "stream_id")?,
                property: field(v, "property")?,
                value: v["value"].clone(),
            },
            "peer_add" | "peer_remove" => SubscriptionEvent::Peers {
                add: op == "peer_add",
                stream_ids: field(v, "stream_ids")?,
                user_ids: field(v, "user_ids")?,
            },
            o => SubscriptionEvent::Other(o.into()),
        }),
        "stream" => Event::Stream(match op {
            "create" => StreamEvent::Create(field(v, "streams")?),
            "delete" => {
                // Feature level 343+ sends `stream_ids`; older servers send `streams`.
                let ids = match v.get("stream_ids") {
                    Some(_) => field(v, "stream_ids")?,
                    None => field::<Vec<Value>>(v, "streams")?.iter().filter_map(|s| s["stream_id"].as_u64()).collect(),
                };
                StreamEvent::Delete(ids)
            }
            "update" => StreamEvent::Update {
                stream_id: field(v, "stream_id")?,
                property: field(v, "property")?,
                value: v["value"].clone(),
                raw: v.clone(),
            },
            o => StreamEvent::Other(o.into()),
        }),
        "realm_user" => Event::RealmUser(match op {
            "add" => RealmUserEvent::Add(field(v, "person")?),
            "remove" => RealmUserEvent::Remove(field::<Value>(v, "person")?["user_id"].as_u64().ok_or(ParseErr::Bad("user_id".into()))?),
            "update" => RealmUserEvent::Update(v["person"].clone()),
            o => RealmUserEvent::Other(o.into()),
        }),
        "user_topic" => Event::UserTopic(UserTopic::deserialize(v)?),
        "muted_users" => Event::MutedUsers(field(v, "muted_users")?),
        "realm_emoji" if op == "update" => Event::RealmEmoji(field(v, "realm_emoji")?),
        "realm" if op == "update" || op == "update_dict" => Event::Realm(v.clone()),
        "user_group" => Event::UserGroup(v.clone()),
        "default_streams" => Event::DefaultStreams(field(v, "default_streams")?),
        "typing" => Event::Typing {
            start: op == "start",
            sender: field::<Value>(v, "sender")?["user_id"].as_u64().ok_or(ParseErr::Bad("sender".into()))?,
            stream_id: v["stream_id"].as_u64(),
            topic: v["topic"].as_str().map(Into::into),
            recipients: v["recipients"].as_array().map(|r| r.iter().filter_map(|x| x["user_id"].as_u64()).collect()).unwrap_or_default(),
        },
        _ => return Err(Unmodeled),
    })
}
