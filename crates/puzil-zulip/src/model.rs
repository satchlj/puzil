//! The in-memory domain model for one account: built from a register response, then kept
//! current by applying events. Pure and synchronous, so it can be replayed in tests.
//!
//! Keys (see PLAN §5):
//! - A DM conversation is the sorted set of all participants' ids, the current user
//!   included; a self-DM is `{me}`. Never `recipient_id`.
//! - A topic is `(stream_id, lowercase(topic))`; the display name is kept separately.
//! - Unreads are a map from message id to conversation, seeded from `unread_msgs`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    events::{Event, RealmUserEvent, StreamEvent, SubscriptionEvent, UpdateMessage},
    types::*,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TopicKey {
    pub stream_id: StreamId,
    /// Lowercased. `""` is the "general chat" topic.
    pub topic: String,
}

impl TopicKey {
    pub fn new(stream_id: StreamId, topic: &str) -> Self {
        Self { stream_id, topic: topic.to_lowercase() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DmKey(pub Vec<UserId>);

impl DmKey {
    /// From any list of participants, with or without the current user.
    pub fn new(me: UserId, others: impl IntoIterator<Item = UserId>) -> Self {
        let mut v: Vec<UserId> = others.into_iter().chain([me]).collect();
        v.sort_unstable();
        v.dedup();
        Self(v)
    }
    pub fn others(&self, me: UserId) -> impl Iterator<Item = UserId> + '_ {
        let solo = self.0.len() == 1;
        self.0.iter().copied().filter(move |&u| solo || u != me)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Conv {
    Topic(TopicKey),
    Dm(DmKey),
}

/// What an [`State::apply`] call touched, so views can refresh selectively.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    pub convs: HashSet<Conv>,
    pub messages: HashSet<MessageId>,
    pub unreads: bool,
    /// The messages whose unread or mention state changed, so the store can update just
    /// those rows. Empty with `unreads` set (or with `unreads_all`) means rewrite them all.
    pub unread_ids: HashSet<MessageId>,
    /// Unread state changed wholesale (everything marked read).
    pub unreads_all: bool,
    pub streams: bool,
    pub users: bool,
    /// The users that changed (realm_user events), so the store can write just those.
    pub user_ids: HashSet<UserId>,
    /// The muted-users list changed.
    pub muted_users: bool,
    pub user_topics: bool,
    /// Everything may have changed (re-register).
    pub all: bool,
}

/// Realm facts the UI needs that no event updates often enough to model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RealmInfo {
    pub name: String,
    pub url: String,
    pub icon_url: Option<String>,
    pub emoji_data_url: Option<String>,
    pub max_message_length: u32,
    pub max_upload_mib: u32,
    pub empty_topic_name: String,
    pub feature_level: u32,
    /// Channel folders, in the organization's order.
    #[serde(default)]
    pub channel_folders: Vec<crate::types::ChannelFolder>,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub me: UserId,
    pub feature_level: u32,
    pub realm_name: String,
    /// Display name for the `""` topic.
    pub empty_topic_name: String,
    pub users: HashMap<UserId, User>,
    pub streams: HashMap<StreamId, Stream>,
    pub subs: HashMap<StreamId, Subscription>,
    pub user_topics: HashMap<TopicKey, Visibility>,
    pub muted_users: HashSet<UserId>,
    /// Newest message id per DM conversation, for the DM list.
    pub recent_dms: HashMap<DmKey, MessageId>,
    pub realm: RealmInfo,
    pub realm_emoji: HashMap<String, RealmEmoji>,
    pub topic_names: HashMap<TopicKey, String>,
    pub unread: HashMap<MessageId, Conv>,
    pub mentions: HashSet<MessageId>,
    pub old_unreads_missing: bool,
    /// Messages we have seen. Bounded by the caller (the store holds the rest).
    pub messages: BTreeMap<MessageId, Message>,
    /// Events that could not be modeled, by kind. Diagnostics only.
    pub unmodeled: BTreeMap<String, usize>,
}

pub fn conv_of(me: UserId, m: &Message) -> Conv {
    match (&m.display_recipient, m.stream_id) {
        (DisplayRecipient::Dm(rs), _) => Conv::Dm(DmKey::new(me, rs.iter().map(|r| r.id))),
        (DisplayRecipient::Stream(_), Some(s)) => Conv::Topic(TopicKey::new(s, &m.subject)),
        // Malformed: a stream message without a stream id. File it under stream 0.
        (DisplayRecipient::Stream(_), None) => Conv::Topic(TopicKey::new(0, &m.subject)),
    }
}

impl State {
    pub fn from_register(r: &Register) -> Self {
        let mut s = State { me: r.user_id, ..Default::default() };
        s.reset_from_register(r);
        s
    }

    /// Replaces everything the register covers. Keeps the message cache: the caller must
    /// re-validate it (`GET /messages?message_ids=`) after a lost queue.
    pub fn reset_from_register(&mut self, r: &Register) -> Changes {
        self.me = r.user_id;
        self.feature_level = r.zulip_feature_level;
        self.realm_name = r.realm_name.clone();
        self.empty_topic_name = r.realm_empty_topic_display_name.clone().unwrap_or_else(|| "general chat".into());
        self.users.clear();
        for u in r.realm_users.iter().chain(&r.cross_realm_bots) {
            self.users.insert(u.user_id, u.clone());
        }
        for u in &r.realm_non_active_users {
            let mut u = u.clone();
            u.deactivated = true;
            self.users.insert(u.user_id, u);
        }
        self.streams.clear();
        self.subs.clear();
        for st in r.streams.iter().chain(&r.unsubscribed).chain(&r.never_subscribed) {
            self.streams.insert(st.stream_id, st.clone());
        }
        for v in &r.subscriptions {
            match (Stream::deserialize(v), Subscription::deserialize(v)) {
                (Ok(st), Ok(sub)) => {
                    self.streams.insert(st.stream_id, st);
                    self.subs.insert(sub.stream_id, sub);
                }
                (Err(e), _) | (_, Err(e)) => tracing::warn!("bad subscription: {e}"),
            }
        }
        self.user_topics =
            r.user_topics.iter().map(|t| (TopicKey::new(t.stream_id, &t.topic_name), t.visibility_policy)).collect();
        self.muted_users = r.muted_users.iter().map(|m| m.id).collect();
        self.realm_emoji = r.realm_emoji.clone();
        self.realm = RealmInfo {
            name: r.realm_name.clone(),
            url: r.realm_url.clone().unwrap_or_default(),
            icon_url: r.realm_icon_url.clone(),
            emoji_data_url: r.server_emoji_data_url.clone(),
            max_message_length: r.max_message_length.unwrap_or(10_000),
            max_upload_mib: r.max_file_upload_size_mib.unwrap_or(25),
            empty_topic_name: self.empty_topic_name.clone(),
            feature_level: r.zulip_feature_level,
            channel_folders: {
                let mut f = r.channel_folders.clone();
                f.sort_by_key(|f| f.order);
                f
            },
        };
        // The server's list is authoritative after a register.
        self.recent_dms.clear();
        for d in &r.recent_private_conversations {
            let k = DmKey::new(self.me, d.user_ids.iter().copied());
            let e = self.recent_dms.entry(k).or_default();
            *e = (*e).max(d.max_message_id);
        }

        let u = &r.unread_msgs;
        self.unread.clear();
        for dm in &u.pms {
            let other = dm.other_user_id.or(dm.sender_id).unwrap_or(self.me);
            let k = Conv::Dm(DmKey::new(self.me, [other]));
            for &id in &dm.unread_message_ids {
                self.unread.insert(id, k.clone());
            }
        }
        for h in &u.huddles {
            let ids = h.user_ids_string.split(',').filter_map(|x| x.trim().parse().ok());
            let k = Conv::Dm(DmKey::new(self.me, ids));
            for &id in &h.unread_message_ids {
                self.unread.insert(id, k.clone());
            }
        }
        for st in &u.streams {
            let k = TopicKey::new(st.stream_id, &st.topic);
            self.topic_names.entry(k.clone()).or_insert_with(|| st.topic.clone());
            for &id in &st.unread_message_ids {
                self.unread.insert(id, Conv::Topic(k.clone()));
            }
        }
        self.mentions = u.mentions.iter().copied().collect();
        self.old_unreads_missing = u.old_unreads_missing;
        Changes { all: true, unreads: true, streams: true, users: true, ..Default::default() }
    }

    pub fn apply_all<'a>(&mut self, events: impl IntoIterator<Item = &'a Event>) -> Changes {
        let mut ch = Changes::default();
        for e in events {
            self.apply_into(e, &mut ch);
        }
        ch
    }

    pub fn apply(&mut self, e: &Event) -> Changes {
        let mut ch = Changes::default();
        self.apply_into(e, &mut ch);
        ch
    }

    fn mark_read(&mut self, id: MessageId, ch: &mut Changes) {
        if let Some(c) = self.unread.remove(&id) {
            ch.convs.insert(c);
            ch.unreads = true;
            ch.unread_ids.insert(id);
        }
        self.mentions.remove(&id);
    }

    fn apply_into(&mut self, e: &Event, ch: &mut Changes) {
        match e {
            Event::Heartbeat | Event::Typing { .. } => {}
            Event::Message { message, flags } => {
                let mut m = (**message).clone();
                m.flags = flags.clone();
                let conv = conv_of(self.me, &m);
                match &conv {
                    Conv::Topic(k) => {
                        self.topic_names.insert(k.clone(), m.subject.clone());
                    }
                    Conv::Dm(k) => {
                        let e = self.recent_dms.entry(k.clone()).or_default();
                        *e = (*e).max(m.id);
                    }
                }
                if !m.has_flag("read") {
                    self.unread.insert(m.id, conv.clone());
                    if m.is_mention() {
                        self.mentions.insert(m.id);
                    }
                    ch.unreads = true;
                    ch.unread_ids.insert(m.id);
                }
                ch.convs.insert(conv);
                ch.messages.insert(m.id);
                self.messages.insert(m.id, m);
            }
            Event::UpdateMessage(u) => self.update_message(u, ch),
            Event::DeleteMessage { message_ids } => {
                for id in message_ids {
                    self.mark_read(*id, ch);
                    if let Some(m) = self.messages.remove(id) {
                        let c = conv_of(self.me, &m);
                        // The DM list orders by newest message: a deleted newest message
                        // (an Overlay sign-in code, say) shouldn't keep its DM on top.
                        if let Conv::Dm(k) = &c
                            && self.recent_dms.get(k) == Some(id)
                        {
                            match self.messages.values().filter(|o| conv_of(self.me, o) == c).map(|o| o.id).max() {
                                Some(prev) => self.recent_dms.insert(k.clone(), prev),
                                None => self.recent_dms.remove(k),
                            };
                        }
                        ch.convs.insert(c);
                    }
                    ch.messages.insert(*id);
                }
            }
            Event::UpdateMessageFlags(f) => {
                for id in &f.messages {
                    if let Some(m) = self.messages.get_mut(id) {
                        m.flags.retain(|x| x != &f.flag);
                        if f.add() {
                            m.flags.push(f.flag.clone());
                        }
                        ch.messages.insert(*id);
                    }
                }
                match (f.flag.as_str(), f.add()) {
                    ("read", true) if f.all => {
                        ch.convs.extend(self.unread.drain().map(|(_, c)| c));
                        self.mentions.clear();
                        ch.unreads = true;
                        ch.unreads_all = true;
                    }
                    ("read", true) => {
                        for id in &f.messages {
                            self.mark_read(*id, ch);
                        }
                    }
                    ("read", false) => {
                        for id in &f.messages {
                            let conv = match f.message_details.get(&id.to_string()) {
                                Some(d) if d.kind == "private" => Conv::Dm(DmKey::new(self.me, d.user_ids.clone().unwrap_or_default())),
                                Some(d) => Conv::Topic(TopicKey::new(d.stream_id.unwrap_or(0), d.topic.as_deref().unwrap_or(""))),
                                None => match self.messages.get(id) {
                                    Some(m) => conv_of(self.me, m),
                                    None => continue,
                                },
                            };
                            let mentioned = f.message_details.get(&id.to_string()).map(|d| d.mentioned).unwrap_or_else(|| {
                                self.messages.get(id).is_some_and(|m| m.is_mention())
                            });
                            if mentioned {
                                self.mentions.insert(*id);
                            }
                            if let (Conv::Topic(k), Some(d)) = (&conv, f.message_details.get(&id.to_string())) {
                                if let Some(t) = &d.topic {
                                    self.topic_names.entry(k.clone()).or_insert_with(|| t.clone());
                                }
                            }
                            self.unread.insert(*id, conv.clone());
                            ch.convs.insert(conv);
                            ch.unreads = true;
                            ch.unread_ids.insert(*id);
                        }
                    }
                    _ => {}
                }
            }
            Event::Reaction { add, message_id, reaction } => {
                if let Some(m) = self.messages.get_mut(message_id) {
                    m.reactions.retain(|r| {
                        !(r.user_id == reaction.user_id && r.emoji_code == reaction.emoji_code && r.reaction_type == reaction.reaction_type)
                    });
                    if *add {
                        m.reactions.push(reaction.clone());
                    }
                    ch.messages.insert(*message_id);
                }
            }
            Event::Subscription(s) => {
                // peer_add / peer_remove (someone else joined or left) are the common ones, and
                // nothing modeled changes: no rewrite of the streams tables, no redraw.
                ch.streams = !matches!(s, SubscriptionEvent::Other(_));
                match s {
                    SubscriptionEvent::Add(pairs) => {
                        for (st, sub) in pairs {
                            self.streams.insert(st.stream_id, st.clone());
                            self.subs.insert(sub.stream_id, sub.clone());
                        }
                    }
                    SubscriptionEvent::Remove(ids) => {
                        for id in ids {
                            self.subs.remove(id);
                        }
                    }
                    SubscriptionEvent::Update { stream_id, property, value } => {
                        if let Some(sub) = self.subs.get_mut(stream_id) {
                            set_field(sub, property, value);
                        }
                    }
                    SubscriptionEvent::Other(o) => *self.unmodeled.entry(format!("subscription/{o}")).or_default() += 1,
                }
            }
            Event::ChannelFolder(v) => {
                ch.streams = true;
                let folders = &mut self.realm.channel_folders;
                match v["op"].as_str() {
                    Some("add") => {
                        if let Ok(f) = serde_json::from_value::<crate::types::ChannelFolder>(v["channel_folder"].clone()) {
                            folders.retain(|o| o.id != f.id);
                            folders.push(f);
                        }
                    }
                    Some("update") => {
                        let id = v["channel_folder_id"].as_i64();
                        if let Some(f) = folders.iter_mut().find(|f| Some(f.id) == id)
                            && let Some(data) = v["data"].as_object()
                        {
                            for (k, val) in data {
                                set_field(f, k, val);
                            }
                        }
                    }
                    Some("reorder") => {
                        if let Some(order) = v["order"].as_array() {
                            for (i, id) in order.iter().filter_map(|x| x.as_i64()).enumerate() {
                                if let Some(f) = folders.iter_mut().find(|f| f.id == id) {
                                    f.order = i as i64;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                folders.sort_by_key(|f| f.order);
            }
            Event::Stream(s) => {
                ch.streams = true;
                match s {
                    StreamEvent::Create(sts) => {
                        for st in sts {
                            self.streams.insert(st.stream_id, st.clone());
                        }
                    }
                    StreamEvent::Delete(ids) => {
                        // Deletion now archives; the stream stays readable but inactive.
                        for id in ids {
                            if let Some(st) = self.streams.get_mut(id) {
                                st.is_archived = true;
                            }
                        }
                    }
                    StreamEvent::Update { stream_id, property, value } => {
                        if let Some(st) = self.streams.get_mut(stream_id) {
                            set_field(st, property, value);
                        }
                    }
                    StreamEvent::Other(o) => *self.unmodeled.entry(format!("stream/{o}")).or_default() += 1,
                }
            }
            Event::RealmUser(u) => {
                ch.users = !matches!(u, RealmUserEvent::Other(_));
                match u {
                    RealmUserEvent::Add(u) => {
                        self.users.insert(u.user_id, u.clone());
                        ch.user_ids.insert(u.user_id);
                    }
                    RealmUserEvent::Remove(id) => {
                        if let Some(u) = self.users.get_mut(id) {
                            u.deactivated = true;
                            ch.user_ids.insert(*id);
                        }
                    }
                    RealmUserEvent::Update(v) => {
                        if let Some(id) = v["user_id"].as_u64() {
                            ch.user_ids.insert(id);
                        }
                        if let Some(u) = v["user_id"].as_u64().and_then(|id| self.users.get_mut(&id)) {
                            if let Some(o) = v.as_object() {
                                for (k, val) in o {
                                    if k != "user_id" {
                                        set_field(u, k, val);
                                    }
                                }
                            }
                        }
                    }
                    RealmUserEvent::Other(o) => *self.unmodeled.entry(format!("realm_user/{o}")).or_default() += 1,
                }
            }
            Event::UserTopic(t) => {
                let k = TopicKey::new(t.stream_id, &t.topic_name);
                if t.visibility_policy == Visibility::Inherit {
                    self.user_topics.remove(&k);
                } else {
                    self.user_topics.insert(k.clone(), t.visibility_policy);
                }
                ch.user_topics = true;
                ch.convs.insert(Conv::Topic(k));
            }
            Event::MutedUsers(m) => {
                self.muted_users = m.iter().map(|m| m.id).collect();
                ch.users = true;
                ch.muted_users = true;
            }
            Event::RealmEmoji(e) => self.realm_emoji = e.clone(),
            Event::Other { kind, .. } => *self.unmodeled.entry(kind.clone()).or_default() += 1,
        }
    }

    fn update_message(&mut self, u: &UpdateMessage, ch: &mut Changes) {
        let ids: Vec<MessageId> = if u.message_ids.is_empty() { u.message_id.into_iter().collect() } else { u.message_ids.clone() };
        // Content edit: only the primary message.
        if let (Some(id), Some(html)) = (u.message_id, &u.rendered_content) {
            if let Some(m) = self.messages.get_mut(&id) {
                m.content = html.clone();
                if !u.rendering_only {
                    m.last_edit_timestamp = u.edit_timestamp;
                }
                m.flags = if u.flags.is_empty() { m.flags.clone() } else { u.flags.clone() };
                ch.messages.insert(id);
            }
            // An edit can add or remove a mention.
            if self.unread.contains_key(&id) && !u.flags.is_empty() {
                if u.flags.iter().any(|f| is_mention_flag(f)) {
                    self.mentions.insert(id);
                } else {
                    self.mentions.remove(&id);
                }
                ch.unreads = true;
                ch.unread_ids.insert(id);
            }
        }
        if !u.is_move() {
            return;
        }
        let Some(old_stream) = u.stream_id else { return };
        let new_stream = u.new_stream_id.unwrap_or(old_stream);
        let new_topic = u.subject.clone().or_else(|| u.orig_subject.clone()).unwrap_or_default();
        let new_key = TopicKey::new(new_stream, &new_topic);
        self.topic_names.insert(new_key.clone(), new_topic.clone());
        for id in ids {
            if let Some(c) = self.unread.get_mut(&id) {
                ch.convs.insert(std::mem::replace(c, Conv::Topic(new_key.clone())));
                ch.unreads = true;
                ch.unread_ids.insert(id);
            }
            if let Some(m) = self.messages.get_mut(&id) {
                ch.convs.insert(conv_of(self.me, m));
                m.stream_id = Some(new_stream);
                m.subject = new_topic.clone();
                if let (Some(st), DisplayRecipient::Stream(name)) = (self.streams.get(&new_stream), &mut m.display_recipient) {
                    *name = st.name.clone();
                }
                m.last_moved_timestamp = u.edit_timestamp.or(m.last_moved_timestamp);
            }
            ch.messages.insert(id);
        }
        ch.convs.insert(Conv::Topic(new_key));
    }

    // ---- Queries ----

    pub fn is_subscribed(&self, s: StreamId) -> bool {
        self.subs.contains_key(&s)
    }

    /// Whether unread messages here count toward totals, following web's rules: unsubscribed
    /// channels never count; muted channels count only in unmuted or followed topics; muted
    /// topics never count.
    pub fn counts_unread(&self, c: &Conv) -> bool {
        match c {
            Conv::Dm(k) => !k.others(self.me).any(|u| self.muted_users.contains(&u)) || k.0.len() > 2,
            Conv::Topic(k) => {
                let Some(sub) = self.subs.get(&k.stream_id) else { return false };
                match self.user_topics.get(k) {
                    Some(Visibility::Muted) => false,
                    Some(Visibility::Unmuted | Visibility::Followed) => true,
                    _ => !sub.is_muted,
                }
            }
        }
    }

    pub fn topic_display(&self, k: &TopicKey) -> String {
        match self.topic_names.get(k) {
            Some(t) if t.is_empty() => self.empty_topic_name.clone(),
            Some(t) => t.clone(),
            None if k.topic.is_empty() => self.empty_topic_name.clone(),
            None => k.topic.clone(),
        }
    }

    pub fn user_name(&self, u: UserId) -> String {
        self.users.get(&u).map(|u| u.full_name.clone()).unwrap_or_else(|| format!("user {u}"))
    }

    pub fn dm_display(&self, k: &DmKey) -> String {
        let names: Vec<_> = k.others(self.me).map(|u| self.user_name(u)).collect();
        names.join(", ")
    }

    /// Unread counts per conversation, ignoring muting.
    pub fn unread_by_conv(&self) -> BTreeMap<Conv, Vec<MessageId>> {
        let mut m: BTreeMap<Conv, Vec<MessageId>> = BTreeMap::new();
        for (id, c) in &self.unread {
            m.entry(c.clone()).or_default().push(*id);
        }
        for v in m.values_mut() {
            v.sort_unstable();
        }
        m
    }
}

/// Applies a `property`/`value` update to a serde struct by round-tripping through JSON.
/// Unknown properties are ignored.
fn set_field<T: Serialize + serde::de::DeserializeOwned>(t: &mut T, property: &str, value: &serde_json::Value) {
    let Ok(mut v) = serde_json::to_value(&*t) else { return };
    let Some(o) = v.as_object_mut() else { return };
    if !o.contains_key(property) {
        return;
    }
    o.insert(property.to_string(), value.clone());
    if let Ok(n) = serde_json::from_value(v) {
        *t = n;
    }
}

impl Conv {
    /// Zulip's narrow for this conversation. `topic` is the display name (the server
    /// matches topics case-insensitively, but the original case is the canonical form).
    pub fn narrow(&self, me: UserId, topic: &str) -> serde_json::Value {
        match self {
            Conv::Topic(k) => serde_json::json!([
                {"operator": "channel", "operand": k.stream_id},
                {"operator": "topic", "operand": topic},
            ]),
            Conv::Dm(k) => serde_json::json!([{"operator": "dm", "operand": k.others(me).collect::<Vec<_>>()}]),
        }
    }

    pub fn dest(&self, me: UserId, topic: &str) -> crate::client::Dest {
        match self {
            Conv::Topic(k) => crate::client::Dest::Topic { stream_id: k.stream_id, topic: topic.to_string() },
            Conv::Dm(k) => crate::client::Dest::Dm(k.others(me).collect()),
        }
    }
}
