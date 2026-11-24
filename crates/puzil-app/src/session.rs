//! One signed-in account: the live domain model, its write-behind store, and everything the
//! views ask of the server. Views read `Session` directly and subscribe to [`Updated`].
//!
//! The server is authoritative for what a conversation shows. The store only paints a
//! conversation before the server answers: every opened conversation is re-fetched once
//! per queue, and cached messages the server no longer returns in that range are dropped.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::*;
use puzil_store::Store;
use puzil_zulip::{
    ApiError, Client, State,
    client::{Anchor, TopicInfo},
    events::Event,
    model::{Changes, Conv, DmKey, conv_of},
    queue::{self, Update},
    types::{Message, MessageId, StreamId, UserId},
};

use crate::{
    accounts::Account,
    runtime::on_tokio,
    typing::{Notice, Typing},
};

/// Messages held in memory before [`Session::trim`] lets some go, and how many it keeps.
const TRIM_AT: usize = 5000;
const TRIM_TO: usize = 3000;

/// How long message times wait after a failed request before they're asked for again.
const TIMES_RETRY: Duration = Duration::from_secs(60);

/// How long a channel whose topics failed to load waits before the next try.
const TOPICS_RETRY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Showing the cached model; register has not returned yet.
    Connecting,
    Live,
    Retrying(String),
    /// The server refused the credentials.
    SignedOut(String),
}

/// What changed, for views deciding what to re-render.
#[derive(Debug, Default)]
pub struct Updated {
    pub convs: HashSet<Conv>,
    pub messages: HashSet<MessageId>,
    pub unreads: bool,
    pub sidebar: bool,
    pub all: bool,
}

impl Updated {
    fn from_changes(ch: &Changes) -> Self {
        Self {
            convs: ch.convs.clone(),
            messages: ch.messages.clone(),
            unreads: ch.unreads,
            sidebar: ch.unreads || ch.streams || ch.user_topics || ch.users || ch.all || !ch.convs.is_empty(),
            all: ch.all,
        }
    }
}

/// Who's typing in this DM changed. Separate from [`Updated`] so it redraws only the
/// conversation showing it.
pub struct TypingChanged(pub Conv);

#[derive(Debug, Default)]
pub struct ConvData {
    /// Validated against the server since the current queue registered.
    pub fresh: bool,
    pub loading: bool,
    pub loading_older: bool,
    pub found_oldest: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PendingState {
    Sending,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Pending {
    pub local_id: u64,
    pub conv: Conv,
    pub topic: String,
    pub content: String,
    pub state: PendingState,
    /// The server id, once the send succeeded. The pending row goes away when the
    /// message itself arrives.
    pub sent_id: Option<MessageId>,
}

#[derive(Debug, Clone)]
pub struct EmojiName {
    pub name: String,
    pub glyph: String,
}

pub struct Session {
    pub account: Account,
    pub client: Client,
    pub state: State,
    pub store: Arc<Store>,
    pub status: Status,
    convs: HashMap<Conv, ConvData>,
    /// Message ids per conversation, over every message held in memory.
    by_conv: HashMap<Conv, BTreeSet<MessageId>>,
    conv_of_id: HashMap<MessageId, Conv>,
    pub topics: HashMap<StreamId, Vec<TopicInfo>>,
    topics_loading: HashSet<StreamId>,
    /// When fetching a channel's topics last failed: retried after [`TOPICS_RETRY`], not on
    /// every render (the sidebar asks each time it draws the channel).
    topics_failed: HashMap<StreamId, Instant>,
    /// Send times of messages we don't hold (a topic's newest, for "active" topics). Zero
    /// means unknown for good (deleted or inaccessible).
    message_times: HashMap<MessageId, i64>,
    /// Message times being fetched, and when fetching them last failed.
    times_inflight: HashSet<MessageId>,
    times_failed: Option<Instant>,
    times_pending: HashSet<MessageId>,
    times_scheduled: bool,
    /// Conversations being paged back to reach a message (a search result), with the pages
    /// fetched so far.
    reveal: HashMap<Conv, (MessageId, u32)>,
    /// When each conversation was last opened, for choosing what to let go (see `trim`).
    opened: HashMap<Conv, Instant>,
    pub outbox: Vec<Pending>,
    next_local: u64,
    read_queue: BTreeSet<MessageId>,
    read_flush: Option<Task<()>>,
    pub emoji: Rc<Vec<EmojiName>>,
    pub images: Entity<crate::media::Images>,
    /// Iliad Overlay, on for the configured realms.
    pub overlay: Option<Entity<crate::overlay::Overlay>>,
    registered_once: bool,
    /// Our Zulip settings as the server last sent them (`user_settings` from register),
    /// updated in place when we change one.
    pub user_settings: serde_json::Value,
    typing: Typing,
    /// Subscriber lists fetched for Channel settings, kept current from peer events.
    members: HashMap<StreamId, Vec<UserId>>,
    members_loading: HashSet<StreamId>,
    /// Channels whose member list failed to load (asked again when their settings reopen).
    members_failed: HashSet<StreamId>,
    /// Bumped on each new event queue: a member list fetched before it may miss peer events.
    members_gen: u64,
    /// Clears typing that lapsed without a stop; armed for the next lapse.
    typing_lapse: Option<Task<()>>,
    /// Sends stop once we pause typing.
    typing_idle: Option<Task<()>>,
    /// Typing notifications to send, one request at a time so a stop can't overtake its start.
    typing_out: Option<tokio::sync::mpsc::UnboundedSender<(Vec<UserId>, bool)>>,
    _sync: Task<()>,
}

impl EventEmitter<Rc<Updated>> for Session {}
impl EventEmitter<TypingChanged> for Session {}

impl Session {
    /// Opens the store, shows its cached model at once, and starts the event queue.
    pub fn new(account: Account, client: Client, store: Arc<Store>, cx: &mut Context<Self>) -> Self {
        let t = std::time::Instant::now();
        let state = match store.load_state() {
            Ok(Some(s)) => s,
            Ok(None) => State { me: account.user_id, ..Default::default() },
            Err(e) => {
                tracing::warn!("cached model unreadable, starting empty: {e:#}");
                State { me: account.user_id, ..Default::default() }
            }
        };
        tracing::info!("cached model loaded in {:.1?}", t.elapsed());
        crate::bench::record("cached_model_ms", t.elapsed().as_secs_f64() * 1000.);
        // Benchmark mode: the cache alone, no queue, no overlay, no server.
        let offline = puzil_zulip::client::is_offline();
        let images = cx.new(|_| crate::media::Images::new(client.clone(), crate::accounts::data_dir().join("cache/media")));
        let overlay = (puzil_overlay::enabled_for(&account.realm) && !offline)
            .then(|| cx.new(|cx| crate::overlay::Overlay::new(client.clone(), account.user_id, cx)));

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        if offline {
            drop(tx);
        } else {
            crate::runtime::rt().spawn(queue::run(client.clone(), tx));
        }
        let sync = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.recv().await {
                // Coalesce whatever else is already queued into one model update.
                let mut batch = vec![first];
                while let Ok(u) = rx.try_recv() {
                    batch.push(u);
                }
                if this.update(cx, |s, cx| s.apply_updates(batch, cx)).is_err() {
                    break;
                }
            }
        });
        let typing_out = (!offline).then(|| typing_sender(client.clone()));
        Self {
            account,
            client,
            state,
            store,
            status: Status::Connecting,
            convs: HashMap::new(),
            by_conv: HashMap::new(),
            conv_of_id: HashMap::new(),
            topics: HashMap::new(),
            topics_loading: HashSet::new(),
            topics_failed: HashMap::new(),
            message_times: HashMap::new(),
            times_inflight: HashSet::new(),
            times_failed: None,
            times_pending: HashSet::new(),
            times_scheduled: false,
            reveal: HashMap::new(),
            opened: HashMap::new(),
            outbox: Vec::new(),
            next_local: 1,
            read_queue: BTreeSet::new(),
            read_flush: None,
            emoji: Rc::new(Vec::new()),
            images,
            overlay,
            registered_once: false,
            user_settings: serde_json::Value::Null,
            typing: Typing::default(),
            members: HashMap::new(),
            members_loading: HashSet::new(),
            members_failed: HashSet::new(),
            members_gen: 0,
            typing_lapse: None,
            typing_idle: None,
            typing_out,
            _sync: sync,
        }
    }

    fn apply_updates(&mut self, batch: Vec<Update>, cx: &mut Context<Self>) {
        let mut up = Updated::default();
        let mut moved_streams = HashSet::new();
        // Whether anything shown could have changed: a batch of heartbeats (one a minute,
        // idle or not) changes nothing and mustn't redraw.
        let mut changed = false;
        let mut typing = HashSet::new();
        for u in batch {
            match u {
                Update::Registered { register, raw } => {
                    self.user_settings = raw.get("user_settings").cloned().unwrap_or_default();
                    self.typing.set_periods(&raw);
                    typing.extend(self.typing.clear());
                    let first = !self.registered_once;
                    self.registered_once = true;
                    let ch = self.state.reset_from_register(&register);
                    self.store.write(puzil_store::writes_for(&self.state, &ch));
                    self.status = Status::Live;
                    // A new queue: everything cached must be re-validated before it is trusted.
                    for d in self.convs.values_mut() {
                        d.fresh = false;
                    }
                    self.topics.clear();
                    // Peer events were lost with the old queue.
                    self.members.clear();
                    self.members_loading.clear();
                    self.members_gen += 1;
                    // Topics that failed while we were cut off are asked for again now.
                    self.topics_failed.clear();
                    if first {
                        self.load_emoji(cx);
                    }
                    up.all = true;
                    up.sidebar = true;
                    up.unreads = true;
                    changed = true;
                }
                Update::Events { events, .. } => {
                    // Events flowing again after a failed poll on the same queue: back to live.
                    if matches!(self.status, Status::Retrying(_)) {
                        self.status = Status::Live;
                        self.topics_failed.clear();
                        up.sidebar = true;
                        changed = true;
                    }
                    for e in &events {
                        if let Event::UpdateMessage(u) = e
                            && u.is_move()
                        {
                            moved_streams.extend(u.stream_id);
                            moved_streams.extend(u.new_stream_id);
                        }
                        // A deletion may empty a topic (Delete topic does): its list is refetched.
                        if let Event::DeleteMessage { stream_id: Some(sid), .. } = e {
                            moved_streams.insert(*sid);
                        }
                        typing.extend(self.note_typing(e));
                        changed |= self.note_members(e);
                    }
                    let ch = self.state.apply_all(&events);
                    self.store.write(puzil_store::writes_for(&self.state, &ch));
                    self.reindex(&ch.messages);
                    self.note_topics(&ch);
                    let u = Updated::from_changes(&ch);
                    up.convs.extend(u.convs);
                    up.messages.extend(u.messages);
                    up.unreads |= u.unreads;
                    up.sidebar |= u.sidebar;
                    let outbox = self.outbox.len();
                    self.outbox.retain(|p| p.sent_id.is_none_or(|id| !self.state.messages.contains_key(&id)));
                    changed |= ch != Changes::default() || self.outbox.len() != outbox;
                }
                Update::Retrying { error, after } => {
                    self.status = Status::Retrying(format!("{error} (retrying in {:.0?})", after));
                    up.sidebar = true;
                    changed = true;
                }
                Update::Unauthorized(m) => {
                    self.status = Status::SignedOut(m);
                    up.sidebar = true;
                    changed = true;
                }
            }
        }
        for sid in moved_streams {
            if self.topics.remove(&sid).is_some() {
                self.load_topics(sid, cx);
            }
        }
        if !typing.is_empty() {
            for c in typing {
                cx.emit(TypingChanged(c));
            }
            self.arm_typing_lapse(cx);
        }
        if !changed {
            return;
        }
        // Live events grow the messages held too: a window left on one conversation in a busy
        // organization would otherwise never let go of any.
        if self.state.messages.len() > TRIM_AT {
            let current = self.opened.iter().max_by_key(|(_, t)| **t).map(|(c, _)| c.clone());
            self.trim(current.as_ref());
        }
        cx.emit(Rc::new(up));
        cx.notify();
    }

    // ---- Channel members ----

    /// Who's subscribed to `sid`, once [`Self::load_members`] fetched it.
    pub fn members(&self, sid: StreamId) -> Option<&Vec<UserId>> {
        self.members.get(&sid)
    }

    pub fn members_failed(&self, sid: StreamId) -> bool {
        self.members_failed.contains(&sid)
    }

    pub fn load_members(&mut self, sid: StreamId, cx: &mut Context<Self>) {
        if self.members.contains_key(&sid) || !self.members_loading.insert(sid) {
            return;
        }
        self.members_failed.remove(&sid);
        let (client, generation) = (self.client.clone(), self.members_gen);
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { client.channel_members(sid).await }).await;
            let _ = this.update(cx, |s, cx| {
                if s.members_gen != generation {
                    return;
                }
                s.members_loading.remove(&sid);
                match r {
                    Ok(mut ids) => {
                        ids.sort_unstable();
                        s.members.insert(sid, ids);
                        cx.notify();
                    }
                    Err(e) => {
                        tracing::warn!("members of {sid}: {e}");
                        s.members_failed.insert(sid);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Keeps fetched member lists current. Returns whether one changed.
    fn note_members(&mut self, e: &Event) -> bool {
        use puzil_zulip::events::SubscriptionEvent as S;
        let me = self.state.me;
        let (add, sids, users): (bool, Vec<StreamId>, Vec<UserId>) = match e {
            Event::Subscription(S::Peers { add, stream_ids, user_ids }) => (*add, stream_ids.clone(), user_ids.clone()),
            Event::Subscription(S::Add(pairs)) => (true, pairs.iter().map(|(st, _)| st.stream_id).collect(), vec![me]),
            Event::Subscription(S::Remove(ids)) => (false, ids.clone(), vec![me]),
            _ => return false,
        };
        let mut changed = false;
        for sid in sids {
            let Some(list) = self.members.get_mut(&sid) else { continue };
            for u in &users {
                match (add, list.binary_search(u)) {
                    (true, Err(i)) => list.insert(i, *u),
                    (false, Ok(i)) => {
                        list.remove(i);
                    }
                    _ => continue,
                }
                changed = true;
            }
        }
        changed
    }

    // ---- Typing ----

    /// Updates who's typing for one event; returns the DM it changed.
    fn note_typing(&mut self, e: &Event) -> Option<Conv> {
        let now = Instant::now();
        let me = self.state.me;
        match e {
            Event::Typing { start, sender, stream_id: None, recipients, .. }
                if *sender != me && !self.state.muted_users.contains(sender) =>
            {
                let c = Conv::Dm(DmKey::new(me, recipients.iter().copied()));
                self.typing.event(c.clone(), *sender, *start, now).then_some(c)
            }
            Event::Message { message, .. } if self.typing.anyone() => {
                let c = conv_of(me, message);
                self.typing.message_from(&c, message.sender_id, now).then_some(c)
            }
            _ => None,
        }
    }

    /// Who is typing in `c` (a DM) besides us.
    pub fn typing_in(&self, c: &Conv) -> Vec<UserId> {
        self.typing.who(c, Instant::now())
    }

    fn arm_typing_lapse(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self.typing.next_lapse() else {
            self.typing_lapse = None;
            return;
        };
        self.typing_lapse = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(at.saturating_duration_since(Instant::now()) + Duration::from_millis(20)).await;
            let _ = this.update(cx, |s, cx| {
                for c in s.typing.prune(Instant::now()) {
                    cx.emit(TypingChanged(c));
                }
                s.arm_typing_lapse(cx);
            });
        }));
    }

    /// The composer of `c` changed; tells the other side of a DM that we're typing (or
    /// stopped), as the Zulip setting allows.
    pub fn composing(&mut self, c: &Conv, empty: bool, cx: &mut Context<Self>) {
        // Off until register says (the setting may be off); on for a server without it.
        let allowed = self.user_settings.is_object()
            && self.user_settings.get("send_private_typing_notifications").and_then(|v| v.as_bool()).unwrap_or(true);
        // Channel typing isn't sent: we don't show it either. A DM with ourselves has no one to tell.
        let to_others = matches!(c, Conv::Dm(k) if k.0.len() > 1);
        let empty = empty || !allowed || !to_others;
        let notices = self.typing.keystroke(c, empty, Instant::now());
        for n in notices {
            self.tell_typing(n, cx);
        }
        if empty {
            self.typing_idle = None;
        } else {
            self.arm_typing_idle(cx);
        }
    }

    /// We left the composer of `except` (or every composer): no longer typing there.
    pub fn stop_composing(&mut self, except: Option<&Conv>, cx: &mut Context<Self>) {
        if except.is_some_and(|c| self.typing.typing_to() == Some(c)) {
            return;
        }
        if let Some(n) = self.typing.sent_message() {
            self.tell_typing(n, cx);
        }
        self.typing_idle = None;
    }

    fn arm_typing_idle(&mut self, cx: &mut Context<Self>) {
        let due = match self.typing.idle(Instant::now()) {
            Ok(n) => {
                self.typing_idle = None;
                if let Some(n) = n {
                    self.tell_typing(n, cx);
                }
                return;
            }
            Err(due) => due,
        };
        // Re-armed only when no timer is pending; one fires, then checks the latest keystroke.
        if self.typing_idle.is_some() {
            return;
        }
        self.typing_idle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(due.saturating_duration_since(Instant::now())).await;
            let _ = this.update(cx, |s, cx| {
                s.typing_idle = None;
                s.arm_typing_idle(cx);
            });
        }));
    }

    fn tell_typing(&self, n: Notice, _: &mut Context<Self>) {
        let (c, start) = match n {
            Notice::Start(c) => (c, true),
            Notice::Stop(c) => (c, false),
        };
        let (Conv::Dm(k), Some(out)) = (c, &self.typing_out) else { return };
        let _ = out.send((k.others(self.state.me).collect(), start));
    }

    /// Keeps `by_conv` in line with `state.messages` for the given ids.
    fn reindex(&mut self, ids: &HashSet<MessageId>) {
        for id in ids {
            if let Some(old) = self.conv_of_id.remove(id)
                && let Some(set) = self.by_conv.get_mut(&old)
            {
                set.remove(id);
            }
            if let Some(m) = self.state.messages.get(id) {
                let c = conv_of(self.state.me, m);
                self.by_conv.entry(c.clone()).or_default().insert(*id);
                self.conv_of_id.insert(*id, c);
            }
        }
    }

    fn note_topics(&mut self, ch: &Changes) {
        for c in &ch.convs {
            let Conv::Topic(k) = c else { continue };
            let Some(list) = self.topics.get_mut(&k.stream_id) else { continue };
            let Some(max) = self.by_conv.get(c).and_then(|s| s.last().copied()) else { continue };
            // The server's name (`""` for general chat), as the server's own list has it.
            let name = self.state.topic_name(k);
            match list.iter_mut().find(|t| t.name.to_lowercase() == k.topic) {
                Some(t) => t.max_id = t.max_id.max(max),
                None => list.push(TopicInfo { name, max_id: max }),
            }
            list.sort_by(|a, b| b.max_id.cmp(&a.max_id));
        }
    }

    fn insert_messages(&mut self, msgs: Vec<Message>) -> HashSet<MessageId> {
        let mut ids = HashSet::new();
        for m in msgs {
            ids.insert(m.id);
            self.state.messages.insert(m.id, m);
        }
        self.reindex(&ids);
        ids
    }

    // ---- Conversations ----

    pub fn conv_data(&self, c: &Conv) -> Option<&ConvData> {
        self.convs.get(c)
    }

    /// Message ids of `c` held in memory, ascending.
    pub fn conv_ids(&self, c: &Conv) -> Vec<MessageId> {
        self.by_conv.get(c).map(|s| s.iter().copied().collect()).unwrap_or_default()
    }

    /// Shows the cached messages of `c` and validates them against the server.
    pub fn open(&mut self, c: &Conv, cx: &mut Context<Self>) {
        self.opened.insert(c.clone(), Instant::now());
        self.stop_composing(Some(c), cx);
        self.trim(Some(c));
        // Never opened (or let go by `trim`): its cached history may hold more than the live
        // messages that arrived meanwhile.
        let read_store = !self.convs.contains_key(c) || !self.by_conv.contains_key(c);
        let d = self.convs.entry(c.clone()).or_default();
        if d.fresh || d.loading {
            return;
        }
        d.loading = true;
        // Cached messages: read on a background thread, but waited for up to a few ms, so a
        // warm read (well under a millisecond) shows them in the frame that opens the
        // conversation. A read from a cold disk cache (~10 ms) arrives a frame or two later
        // instead of stalling the main thread. The server's first page is asked for after
        // the cached messages are in, so they can't land on top of it.
        let pending = if !read_store {
            None
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            let (store, conv) = (self.store.clone(), c.clone());
            cx.background_spawn(async move {
                let _ = tx.send(store.messages(&conv, None, 200).unwrap_or_default());
            })
            .detach();
            match rx.recv_timeout(Duration::from_millis(4)) {
                Ok(cached) => {
                    self.show_cached(c, cached, cx);
                    None
                }
                Err(_) => Some(rx),
            }
        };
        let conv = c.clone();
        let has_unread = self.state.unread.values().any(|u| u == c);
        cx.spawn(async move |this, cx| {
            if let Some(rx) = pending {
                let cached = cx.background_spawn(async move { rx.recv().unwrap_or_default() }).await;
                let _ = this.update(cx, |s, cx| s.show_cached(&conv, cached, cx));
            }
            if puzil_zulip::client::is_offline() {
                let _ = this.update(cx, |s, _| {
                    if let Some(d) = s.convs.get_mut(&conv) {
                        d.loading = false;
                    }
                });
                return;
            }
            let Ok((client, narrow)) = this.read_with(cx, |s, _| (s.client.clone(), s.narrow(&conv))) else { return };
            let anchor = if has_unread { Anchor::FirstUnread } else { Anchor::Newest };
            let page = on_tokio(async move { client.messages(&narrow, anchor, 100, 1000).await }).await;
            let _ = this.update(cx, |s, cx| s.absorb_page(&conv, page, false, cx));
        })
        .detach();
    }

    /// Lets go of messages held in memory once there are more than [`TRIM_AT`]: whole
    /// conversations, those never opened (live events) or opened longest ago first, down to
    /// [`TRIM_TO`]. They stay in the store; opening one again reads it back and re-validates
    /// it with the server, as after a launch. Without this a session left running for days in a
    /// busy organization keeps every message it ever saw.
    fn trim(&mut self, keep: Option<&Conv>) {
        if self.state.messages.len() <= TRIM_AT {
            return;
        }
        let mut convs: Vec<(Option<Instant>, Conv)> = self
            .by_conv
            .keys()
            .filter(|c| Some(*c) != keep)
            .filter(|c| !self.convs.get(*c).is_some_and(|d| d.loading || d.loading_older))
            .map(|c| (self.opened.get(c).copied(), c.clone()))
            .collect();
        convs.sort_by_key(|(t, _)| *t);
        for (_, c) in convs {
            if self.state.messages.len() <= TRIM_TO {
                break;
            }
            for id in self.by_conv.remove(&c).unwrap_or_default() {
                self.state.messages.remove(&id);
                self.conv_of_id.remove(&id);
            }
            self.convs.remove(&c);
            self.reveal.remove(&c);
        }
    }

    fn show_cached(&mut self, c: &Conv, cached: Vec<Message>, cx: &mut Context<Self>) {
        let fresh: Vec<_> = cached.into_iter().filter(|m| !self.state.messages.contains_key(&m.id)).collect();
        if !fresh.is_empty() {
            let ids = self.insert_messages(fresh);
            cx.emit(Rc::new(Updated { convs: [c.clone()].into(), messages: ids, ..Default::default() }));
            cx.notify();
        }
    }

    /// Pages `c` back until `id` is held (or the start is reached, or 30 pages: 3000
    /// messages). Pages stay contiguous, so the list never shows a hidden gap.
    pub fn load_back_to(&mut self, c: &Conv, id: MessageId, cx: &mut Context<Self>) {
        self.reveal.insert(c.clone(), (id, 0));
        self.continue_reveal(c, cx);
    }

    fn continue_reveal(&mut self, c: &Conv, cx: &mut Context<Self>) {
        let Some((id, pages)) = self.reveal.get(c).copied() else { return };
        let oldest = self.by_conv.get(c).and_then(|s| s.first().copied());
        let d = self.convs.get(c);
        if oldest.is_some_and(|o| o <= id) || d.is_some_and(|d| d.found_oldest) || pages >= 30 {
            self.reveal.remove(c);
            return;
        }
        // Wait for the open's first page; absorbing it calls back here.
        if d.is_some_and(|d| d.fresh && !d.loading_older) {
            self.reveal.insert(c.clone(), (id, pages + 1));
            self.load_older(c, cx);
        }
    }

    /// Fetches the page before the oldest message held for `c`.
    pub fn load_older(&mut self, c: &Conv, cx: &mut Context<Self>) {
        let oldest = self.by_conv.get(c).and_then(|s| s.first().copied());
        let Some(d) = self.convs.get_mut(c) else { return };
        if d.loading || d.loading_older || d.found_oldest || !d.fresh {
            return;
        }
        let Some(oldest) = oldest else { return };
        d.loading_older = true;
        let conv = c.clone();
        let (client, narrow) = (self.client.clone(), self.narrow(c));
        cx.spawn(async move |this, cx| {
            let page = on_tokio(async move { client.messages(&narrow, Anchor::Id(oldest), 100, 0).await }).await;
            let _ = this.update(cx, |s, cx| s.absorb_page(&conv, page, true, cx));
        })
        .detach();
        cx.notify();
    }

    fn absorb_page(
        &mut self,
        conv: &Conv,
        page: Result<puzil_zulip::client::MessagesPage, ApiError>,
        older: bool,
        cx: &mut Context<Self>,
    ) {
        let d = self.convs.entry(conv.clone()).or_default();
        if older {
            d.loading_older = false;
        } else {
            d.loading = false;
        }
        let page = match page {
            Ok(p) => p,
            Err(e) => {
                d.error = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        d.error = None;
        if !older {
            d.fresh = true;
        }
        d.found_oldest |= page.found_oldest;
        let returned: HashSet<MessageId> = page.messages.iter().map(|m| m.id).collect();
        let lo = if page.found_oldest { 0 } else { page.messages.first().map(|m| m.id).unwrap_or(0) };
        let hi = if page.found_newest && !older { u64::MAX >> 1 } else { page.messages.last().map(|m| m.id).unwrap_or(0) };
        // Cached messages in the covered range that the server no longer returns were
        // moved or deleted while we were away.
        let stale: Vec<MessageId> = self
            .by_conv
            .get(conv)
            .map(|s| s.range(lo..=hi).filter(|id| !returned.contains(id)).copied().collect())
            .unwrap_or_default();
        let mut ch = Changes::default();
        for id in &stale {
            self.state.messages.remove(id);
            ch.messages.insert(*id);
        }
        // Flags from the page are authoritative for these messages too.
        // (Ids we marked read locally and haven't sent yet stay read.)
        for m in &page.messages {
            if m.has_flag("read") {
                if self.state.unread.remove(&m.id).is_some() {
                    ch.unreads = true;
                    ch.unread_ids.insert(m.id);
                }
                self.state.mentions.remove(&m.id);
            } else if !self.read_queue.contains(&m.id) {
                if self.state.unread.insert(m.id, conv_of(self.state.me, m)).is_none() {
                    ch.unreads = true;
                    ch.unread_ids.insert(m.id);
                }
                if m.is_mention() && self.state.mentions.insert(m.id) {
                    ch.unreads = true;
                    ch.unread_ids.insert(m.id);
                }
            }
        }
        let ids = self.insert_messages(page.messages);
        ch.messages.extend(ids.iter().copied());
        self.reindex(&stale.iter().copied().collect());
        ch.convs.insert(conv.clone());
        let mut w = puzil_store::writes_for(&self.state, &ch);
        if lo <= hi && !returned.is_empty() {
            w.push(puzil_store::Write::FetchedRange { conv: conv.clone(), lo, hi });
        }
        self.store.write(w);
        self.note_topics(&ch);
        cx.emit(Rc::new(Updated { convs: ch.convs, messages: ch.messages, unreads: true, sidebar: true, all: false }));
        cx.notify();
        self.continue_reveal(conv, cx);
    }

    pub fn narrow(&self, c: &Conv) -> serde_json::Value {
        let topic = match c {
            Conv::Topic(k) => self.state.topic_display(k),
            Conv::Dm(_) => String::new(),
        };
        let topic = if topic == self.state.empty_topic_name { String::new() } else { topic };
        c.narrow(self.state.me, &topic)
    }

    /// The topic string to send to (the display name, or `""` for general chat).
    pub fn send_topic(&self, c: &Conv) -> String {
        match c {
            Conv::Topic(k) if k.topic.is_empty() => String::new(),
            Conv::Topic(k) => self.state.topic_names.get(k).cloned().unwrap_or_else(|| k.topic.clone()),
            Conv::Dm(_) => String::new(),
        }
    }

    /// When a message was sent, if known; otherwise fetches it (batched with other requests
    /// in the next 100 ms) and announces the answer as a sidebar update.
    pub fn message_time(&mut self, id: MessageId, cx: &mut Context<Self>) -> Option<i64> {
        if let Some(m) = self.state.messages.get(&id) {
            return Some(m.timestamp);
        }
        if let Some(t) = self.message_times.get(&id) {
            return Some(*t);
        }
        if id == 0 {
            return None;
        }
        // Not while it's being asked for, nor right after a request failed (offline, rate
        // limited): the sidebar asks on every redraw.
        if self.times_inflight.contains(&id) || self.times_failed.is_some_and(|t| t.elapsed() < TIMES_RETRY) {
            return None;
        }
        if self.times_pending.insert(id) && !self.times_scheduled {
            self.times_scheduled = true;
            let client = self.client.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                let Ok(ids) = this.update(cx, |this, _| {
                    this.times_scheduled = false;
                    let ids = this.times_pending.drain().collect::<Vec<_>>();
                    this.times_inflight.extend(ids.iter().copied());
                    ids
                }) else {
                    return;
                };
                let asked = ids.clone();
                let r = on_tokio(async move {
                    let mut out = Vec::new();
                    for chunk in ids.chunks(100) {
                        out.extend(client.messages_by_id_raw(chunk).await?.into_iter().map(|m| (m.id, m.timestamp)));
                    }
                    Ok::<_, ApiError>(out)
                })
                .await;
                let _ = this.update(cx, |this, cx| {
                    for id in &asked {
                        this.times_inflight.remove(id);
                    }
                    match r {
                        Ok(v) => {
                            this.message_times.extend(v);
                            // Whatever didn't come back (deleted) won't on a retry either.
                            for id in asked {
                                this.message_times.entry(id).or_insert(0);
                            }
                        }
                        Err(e) => {
                            // Asked again after a while, with a redraw to do the asking.
                            tracing::warn!("message times: {e}");
                            this.times_failed = Some(Instant::now());
                            cx.spawn(async move |this, cx| {
                                cx.background_executor().timer(TIMES_RETRY + Duration::from_millis(250)).await;
                                let _ = this.update(cx, |_, cx| cx.emit(Rc::new(Updated { sidebar: true, ..Default::default() })));
                            })
                            .detach();
                        }
                    }
                    cx.emit(Rc::new(Updated { sidebar: true, ..Default::default() }));
                });
            })
            .detach();
        }
        None
    }

    /// Changes a Zulip setting: shown at once, sent to the server, put back if refused.
    pub fn set_zulip_setting(&mut self, name: &str, value: serde_json::Value, cx: &mut Context<Self>) {
        let old = self.user_settings.get(name).cloned();
        if let Some(o) = self.user_settings.as_object_mut() {
            o.insert(name.to_string(), value.clone());
        }
        cx.notify();
        let n = name.to_string();
        self.call(
            cx,
            move |c| async move { c.set_setting(&n, &value).await },
            {
                let name = name.to_string();
                move |s, e, cx| {
                    tracing::warn!("setting {name}: {e}");
                    if let (Some(o), Some(old)) = (s.user_settings.as_object_mut(), old) {
                        o.insert(name, old);
                    }
                    cx.notify();
                }
            },
        );
    }

    /// Whether the last try to load `sid`'s topics failed (it's retried after a while).
    pub fn topics_failed(&self, sid: StreamId) -> bool {
        self.topics_failed.contains_key(&sid)
    }

    pub fn load_topics(&mut self, sid: StreamId, cx: &mut Context<Self>) {
        if self.topics.contains_key(&sid) || self.topics_failed.get(&sid).is_some_and(|t| t.elapsed() < TOPICS_RETRY) {
            return;
        }
        if !self.topics_loading.insert(sid) {
            return;
        }
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { client.topics(sid).await }).await;
            let _ = this.update(cx, |s, cx| {
                s.topics_loading.remove(&sid);
                match r {
                    Ok(t) => {
                        s.topics_failed.remove(&sid);
                        // The server's spelling: requests about a topic must send it, not the lowercased key.
                        for info in &t {
                            s.state.topic_names.entry(puzil_zulip::model::TopicKey::new(sid, &info.name)).or_insert_with(|| info.name.clone());
                        }
                        s.topics.insert(sid, t);
                        cx.emit(Rc::new(Updated { sidebar: true, ..Default::default() }));
                        cx.notify();
                    }
                    Err(e) => {
                        tracing::warn!("topics for {sid}: {e}");
                        s.topics_failed.insert(sid, Instant::now());
                        // Show the failure, and redraw when a retry is due: the sidebar asks
                        // again as it draws (and only then, being drawn from cache otherwise).
                        cx.emit(Rc::new(Updated { sidebar: true, ..Default::default() }));
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(TOPICS_RETRY + Duration::from_millis(250)).await;
                            let _ = this.update(cx, |_, cx| cx.emit(Rc::new(Updated { sidebar: true, ..Default::default() })));
                        })
                        .detach();
                    }
                }
            });
        })
        .detach();
    }

    // ---- Read state ----

    /// Marks messages read now and tells the server shortly after, in one batch.
    pub fn mark_read(&mut self, ids: impl IntoIterator<Item = MessageId>, cx: &mut Context<Self>) {
        let mut ch = Changes::default();
        for id in ids {
            if let Some(c) = self.state.unread.remove(&id) {
                self.state.mentions.remove(&id);
                ch.convs.insert(c);
                ch.unreads = true;
                ch.unread_ids.insert(id);
                self.read_queue.insert(id);
            }
        }
        if !ch.unreads {
            return;
        }
        self.store.write(puzil_store::writes_for(
            &self.state,
            &Changes { unreads: true, unread_ids: std::mem::take(&mut ch.unread_ids), ..Default::default() },
        ));
        cx.emit(Rc::new(Updated { convs: ch.convs, unreads: true, sidebar: true, ..Default::default() }));
        cx.notify();
        if self.read_flush.is_none() {
            self.read_flush = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let _ = this.update(cx, |s, cx| s.flush_reads(cx));
            }));
        }
    }

    fn flush_reads(&mut self, cx: &mut Context<Self>) {
        self.read_flush = None;
        let ids: Vec<MessageId> = std::mem::take(&mut self.read_queue).into_iter().collect();
        if ids.is_empty() {
            return;
        }
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let r = on_tokio({
                let ids = ids.clone();
                async move {
                    for chunk in ids.chunks(1000) {
                        client.update_flags(chunk, true, "read").await?;
                    }
                    Ok::<_, ApiError>(())
                }
            })
            .await;
            if let Err(e) = r {
                tracing::warn!("marking {} messages read failed: {e}; will retry", ids.len());
                let _ = this.update(cx, |s, cx| {
                    s.read_queue.extend(ids);
                    s.read_flush = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_secs(5)).await;
                        let _ = this.update(cx, |s, cx| s.flush_reads(cx));
                    }));
                });
            }
        })
        .detach();
    }

    pub fn unread_in(&self, c: &Conv) -> Vec<MessageId> {
        let mut v: Vec<_> = self.state.unread.iter().filter(|(_, x)| *x == c).map(|(id, _)| *id).collect();
        v.sort_unstable();
        v
    }

    // ---- Sending ----

    pub fn send(&mut self, conv: Conv, topic: String, content: String, cx: &mut Context<Self>) {
        let local_id = self.next_local;
        self.next_local += 1;
        self.outbox.push(Pending { local_id, conv: conv.clone(), topic, content, state: PendingState::Sending, sent_id: None });
        self.store.write(vec![puzil_store::Write::Draft { conv: conv.clone(), body: String::new() }]);
        self.stop_composing(None, cx);
        self.dispatch(local_id, cx);
        cx.emit(Rc::new(Updated { convs: [conv].into(), ..Default::default() }));
        cx.notify();
    }

    pub fn retry(&mut self, local_id: u64, cx: &mut Context<Self>) {
        if let Some(p) = self.outbox.iter_mut().find(|p| p.local_id == local_id) {
            p.state = PendingState::Sending;
            self.dispatch(local_id, cx);
            cx.notify();
        }
    }

    pub fn discard(&mut self, local_id: u64, cx: &mut Context<Self>) {
        self.outbox.retain(|p| p.local_id != local_id);
        cx.notify();
    }

    fn dispatch(&mut self, local_id: u64, cx: &mut Context<Self>) {
        let Some(p) = self.outbox.iter().find(|p| p.local_id == local_id) else { return };
        let dest = p.conv.dest(self.state.me, &p.topic);
        let content = p.content.clone();
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { client.send_message(&dest, &content).await }).await;
            let _ = this.update(cx, |s, cx| {
                match r {
                    Ok(id) => {
                        if s.state.messages.contains_key(&id) {
                            s.outbox.retain(|p| p.local_id != local_id);
                        } else if let Some(p) = s.outbox.iter_mut().find(|p| p.local_id == local_id) {
                            p.sent_id = Some(id);
                        }
                    }
                    Err(e) => {
                        if let Some(p) = s.outbox.iter_mut().find(|p| p.local_id == local_id) {
                            p.state = PendingState::Failed(e.to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn save_draft(&self, conv: &Conv, body: String) {
        self.store.write(vec![puzil_store::Write::Draft { conv: conv.clone(), body }]);
    }

    pub fn draft(&self, conv: &Conv) -> Option<String> {
        self.store.draft(conv).ok().flatten()
    }

    /// Runs a fire-and-forget server call and reports failure to `on_err`.
    pub fn call<F, Fut>(&self, cx: &mut Context<Self>, f: F, on_err: impl FnOnce(&mut Self, String, &mut Context<Self>) + 'static)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            if let Err(e) = on_tokio(async move { f(client).await }).await {
                let _ = this.update(cx, |s, cx| {
                    on_err(s, e.to_string(), cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn load_emoji(&mut self, cx: &mut Context<Self>) {
        let Some(url) = self.state.realm.emoji_data_url.clone() else { return };
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let Ok(v) = on_tokio(async move { client.public_json(&url).await }).await else { return };
            let mut out = Vec::new();
            if let Some(map) = v["code_to_names"].as_object() {
                for (code, names) in map {
                    let Some(glyph) = puzil_render::ast::emoji_glyph(code) else { continue };
                    for n in names.as_array().into_iter().flatten().filter_map(|n| n.as_str()) {
                        out.push(EmojiName { name: n.to_string(), glyph: glyph.clone() });
                    }
                }
            }
            out.sort_by(|a, b| a.name.cmp(&b.name));
            let _ = this.update(cx, |s, _| s.emoji = Rc::new(out));
        })
        .detach();
    }
}

/// Sends typing notifications in order on the network runtime. A start still queued when
/// its stop arrives is dropped with it: the other side never needs to see either.
fn typing_sender(client: Client) -> tokio::sync::mpsc::UnboundedSender<(Vec<UserId>, bool)> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<UserId>, bool)>();
    crate::runtime::rt().spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut batch = vec![first];
            while let Ok(n) = rx.try_recv() {
                if !n.1 && batch.last().is_some_and(|(to, start)| *start && *to == n.0) {
                    batch.pop();
                } else {
                    batch.push(n);
                }
            }
            for (to, start) in batch {
                if let Err(e) = client.set_typing(&to, start).await {
                    tracing::debug!("typing notification failed: {e}");
                }
            }
        }
    });
    tx
}
