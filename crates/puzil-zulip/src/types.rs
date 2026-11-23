//! Typed subsets of Zulip's register response, messages and events. Unknown fields are
//! ignored; fields that old servers may omit are `#[serde(default)]`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type UserId = u64;
pub type StreamId = u64;
pub type MessageId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub user_id: UserId,
    pub full_name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub delivery_email: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub avatar_version: u32,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub is_guest: bool,
    #[serde(default)]
    pub role: u32,
    #[serde(default)]
    pub timezone: String,
    /// Not sent by the server; set when the user comes from `realm_non_active_users`.
    #[serde(default, skip_deserializing)]
    pub deactivated: bool,
}

/// A channel. Built from `streams`, `subscriptions`, `unsubscribed` and `never_subscribed`;
/// subscription-only fields are in [`Subscription`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stream {
    pub stream_id: StreamId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub rendered_description: String,
    #[serde(default)]
    pub invite_only: bool,
    #[serde(default)]
    pub is_web_public: bool,
    #[serde(default)]
    pub is_archived: bool,
    #[serde(default)]
    pub history_public_to_subscribers: bool,
    #[serde(default)]
    pub first_message_id: Option<MessageId>,
    #[serde(default)]
    pub date_created: i64,
    /// The channel folder it's filed in (Zulip 11+).
    #[serde(default)]
    pub folder_id: Option<i64>,
}

/// An organization's channel folder (Zulip 11+).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelFolder {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub is_archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub stream_id: StreamId,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub is_muted: bool,
    #[serde(default)]
    pub pin_to_top: bool,
    #[serde(default)]
    pub desktop_notifications: Option<bool>,
    #[serde(default)]
    pub audible_notifications: Option<bool>,
    #[serde(default)]
    pub push_notifications: Option<bool>,
    #[serde(default)]
    pub email_notifications: Option<bool>,
    #[serde(default)]
    pub wildcard_mentions_notify: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "u8", into = "u8")]
pub enum Visibility {
    Inherit,
    Muted,
    Unmuted,
    Followed,
}

impl From<u8> for Visibility {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::Muted,
            2 => Self::Unmuted,
            3 => Self::Followed,
            _ => Self::Inherit,
        }
    }
}
impl From<Visibility> for u8 {
    fn from(v: Visibility) -> u8 {
        v as u8
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTopic {
    pub stream_id: StreamId,
    pub topic_name: String,
    pub visibility_policy: Visibility,
    #[serde(default)]
    pub last_updated: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealmEmoji {
    pub id: String,
    pub name: String,
    pub source_url: String,
    #[serde(default)]
    pub still_url: Option<String>,
    #[serde(default)]
    pub deactivated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnreadMsgs {
    #[serde(default)]
    pub pms: Vec<UnreadDm>,
    #[serde(default)]
    pub streams: Vec<UnreadStream>,
    #[serde(default)]
    pub huddles: Vec<UnreadHuddle>,
    #[serde(default)]
    pub mentions: Vec<MessageId>,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub old_unreads_missing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreadDm {
    /// Feature level 119+. Older servers send only `sender_id`.
    #[serde(default)]
    pub other_user_id: Option<UserId>,
    #[serde(default)]
    pub sender_id: Option<UserId>,
    pub unread_message_ids: Vec<MessageId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreadStream {
    pub stream_id: StreamId,
    pub topic: String,
    pub unread_message_ids: Vec<MessageId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreadHuddle {
    /// Comma-separated, sorted, including the current user.
    pub user_ids_string: String,
    pub unread_message_ids: Vec<MessageId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutedUser {
    pub id: UserId,
    #[serde(default)]
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Register {
    pub queue_id: Option<String>,
    pub last_event_id: i64,
    pub zulip_feature_level: u32,
    #[serde(default)]
    pub zulip_version: String,
    pub user_id: UserId,
    #[serde(default)]
    pub max_message_id: MessageId,
    #[serde(default)]
    pub realm_users: Vec<User>,
    #[serde(default)]
    pub realm_non_active_users: Vec<User>,
    #[serde(default)]
    pub cross_realm_bots: Vec<User>,
    #[serde(default)]
    pub streams: Vec<Stream>,
    /// Same fields as [`Stream`] plus [`Subscription`]; parsed twice.
    #[serde(default)]
    pub subscriptions: Vec<Value>,
    #[serde(default)]
    pub unsubscribed: Vec<Stream>,
    #[serde(default)]
    pub never_subscribed: Vec<Stream>,
    #[serde(default)]
    pub channel_folders: Vec<ChannelFolder>,
    #[serde(default)]
    pub unread_msgs: UnreadMsgs,
    #[serde(default)]
    pub user_topics: Vec<UserTopic>,
    #[serde(default)]
    pub muted_users: Vec<MutedUser>,
    #[serde(default)]
    pub realm_emoji: std::collections::HashMap<String, RealmEmoji>,
    #[serde(default)]
    pub realm_empty_topic_display_name: Option<String>,
    #[serde(default)]
    pub realm_name: String,
    #[serde(default)]
    pub event_queue_longpoll_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub realm_url: Option<String>,
    #[serde(default)]
    pub realm_icon_url: Option<String>,
    /// Zulip's emoji names and codes, for autocomplete.
    #[serde(default)]
    pub server_emoji_data_url: Option<String>,
    #[serde(default)]
    pub max_message_length: Option<u32>,
    #[serde(default)]
    pub max_file_upload_size_mib: Option<u32>,
    #[serde(default)]
    pub recent_private_conversations: Vec<RecentDm>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentDm {
    /// The other participants; empty for the self-DM.
    pub user_ids: Vec<UserId>,
    pub max_message_id: MessageId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmRecipient {
    pub id: UserId,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub full_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DisplayRecipient {
    Stream(String),
    Dm(Vec<DmRecipient>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji_name: String,
    pub emoji_code: String,
    pub reaction_type: String,
    pub user_id: UserId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub sender_id: UserId,
    #[serde(default)]
    pub sender_full_name: String,
    #[serde(default)]
    pub sender_email: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub stream_id: Option<StreamId>,
    #[serde(default)]
    pub subject: String,
    pub display_recipient: DisplayRecipient,
    /// Rendered HTML (we register with `apply_markdown=true`).
    pub content: String,
    pub timestamp: i64,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub last_edit_timestamp: Option<i64>,
    #[serde(default)]
    pub last_moved_timestamp: Option<i64>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub submessages: Vec<Value>,
}

impl Message {
    pub fn has_flag(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }
    pub fn is_mention(&self) -> bool {
        self.flags.iter().any(|f| is_mention_flag(f))
    }
}

pub fn is_mention_flag(f: &str) -> bool {
    matches!(f, "mentioned" | "wildcard_mentioned" | "stream_wildcard_mentioned" | "topic_wildcard_mentioned")
}

/// Per-message details on a mark-unread event (`update_message_flags`, op `remove`,
/// flag `read`), enough to file the message without fetching it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDetails {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub mentioned: bool,
    #[serde(default)]
    pub user_ids: Option<Vec<UserId>>,
    #[serde(default)]
    pub stream_id: Option<StreamId>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub unmuted_stream_msg: bool,
}
