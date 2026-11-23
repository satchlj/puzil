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
    time::Duration,
};

use gpui_kit::*;
use puzil_store::Store;
use puzil_zulip::{
    ApiError, Client, State,
    client::{Anchor, TopicInfo},
    events::Event,
    model::{Changes, Conv, conv_of},
    queue::{self, Update},
    types::{Message, MessageId, StreamId},
};

use crate::{accounts::Account, runtime::on_tokio};

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
    /// Send times of messages we don't hold (a topic's newest, for "active" topics). Zero
    /// means unknown for good (deleted or inaccessible).
    message_times: HashMap<MessageId, i64>,
    times_pending: HashSet<MessageId>,
    times_scheduled: bool,
    /// Conversations being paged back to reach a message (a search result), with the pages
    /// fetched so far.
    reveal: HashMap<Conv, (MessageId, u32)>,
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
    _sync: Task<()>,
}

impl EventEmitter<Rc<Updated>> for Session {}

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
        let images = cx.new(|_| crate::media::Images::new(client.clone(), crate::accounts::data_dir().join("cache/media")));
        let overlay = puzil_overlay::enabled_for(&account.realm)
            .then(|| cx.new(|cx| crate::overlay::Overlay::new(client.clone(), account.user_id, cx)));

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        crate::runtime::rt().spawn(queue::run(client.clone(), tx));
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
            message_times: HashMap::new(),
            times_pending: HashSet::new(),
            times_scheduled: false,
            reveal: HashMap::new(),
            outbox: Vec::new(),
            next_local: 1,
            read_queue: BTreeSet::new(),
            read_flush: None,
            emoji: Rc::new(Vec::new()),
            images,
            overlay,
            registered_once: false,
            user_settings: serde_json::Value::Null,
            
            _sync: sync,
        }
    }

    fn apply_updates(&mut self, batch: Vec<Update>, cx: &mut Context<Self>) {
        let mut up = Updated::default();
        let mut moved_streams = HashSet::new();
        for u in batch {
            match u {
                Update::Registered { register, raw } => {
                    self.user_settings = raw.get("user_settings").cloned().unwrap_or_default();
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
                    if first {
                        self.load_emoji(cx);
                    }
                    up.all = true;
                    up.sidebar = true;
                    up.unreads = true;
                }
                Update::Events { events, .. } => {
                    for e in &events {
                        if let Event::UpdateMessage(u) = e
                            && u.is_move()
                        {
                            moved_streams.extend(u.stream_id);
                            moved_streams.extend(u.new_stream_id);
                        }
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
                    self.outbox.retain(|p| p.sent_id.is_none_or(|id| !self.state.messages.contains_key(&id)));
                }
                Update::Retrying { error, after } => {
                    self.status = Status::Retrying(format!("{error} (retrying in {:.0?})", after));
                    up.sidebar = true;
                }
                Update::Unauthorized(m) => {
                    self.status = Status::SignedOut(m);
                    up.sidebar = true;
                }
            }
        }
        for sid in moved_streams {
            if self.topics.remove(&sid).is_some() {
                self.load_topics(sid, cx);
            }
        }
        cx.emit(Rc::new(up));
        cx.notify();
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
            let name = self.state.topic_display(k);
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
        let d = self.convs.entry(c.clone()).or_default();
        if d.fresh || d.loading {
            return;
        }
        d.loading = true;
        let first_load = !self.by_conv.contains_key(c);
        let store = self.store.clone();
        let conv = c.clone();
        let has_unread = self.state.unread.values().any(|u| u == c);
        cx.spawn(async move |this, cx| {
            if first_load {
                let cached = cx.background_spawn({
                    let conv = conv.clone();
                    async move { store.messages(&conv, None, 200).unwrap_or_default() }
                });
                let cached = cached.await;
                let _ = this.update(cx, |s, cx| {
                    let fresh: Vec<_> = cached.into_iter().filter(|m| !s.state.messages.contains_key(&m.id)).collect();
                    if !fresh.is_empty() {
                        let ids = s.insert_messages(fresh);
                        cx.emit(Rc::new(Updated { convs: [conv.clone()].into(), messages: ids, ..Default::default() }));
                        cx.notify();
                    }
                });
            }
            let Ok((client, narrow)) = this.read_with(cx, |s, _| (s.client.clone(), s.narrow(&conv))) else { return };
            let anchor = if has_unread { Anchor::FirstUnread } else { Anchor::Newest };
            let page = on_tokio(async move { client.messages(&narrow, anchor, 100, 1000).await }).await;
            let _ = this.update(cx, |s, cx| s.absorb_page(&conv, page, false, cx));
        })
        .detach();
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
                self.state.unread.remove(&m.id);
                self.state.mentions.remove(&m.id);
            } else if !self.read_queue.contains(&m.id) {
                self.state.unread.insert(m.id, conv_of(self.state.me, m));
                if m.is_mention() {
                    self.state.mentions.insert(m.id);
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
        if self.times_pending.insert(id) && !self.times_scheduled {
            self.times_scheduled = true;
            let client = self.client.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                let Ok(ids) = this.update(cx, |this, _| {
                    this.times_scheduled = false;
                    this.times_pending.drain().collect::<Vec<_>>()
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
                    match r {
                        Ok(v) => this.message_times.extend(v),
                        Err(e) => tracing::warn!("message times: {e}"),
                    }
                    // Whatever didn't come back won't on a retry either.
                    for id in asked {
                        this.message_times.entry(id).or_insert(0);
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

    pub fn load_topics(&mut self, sid: StreamId, cx: &mut Context<Self>) {
        if self.topics.contains_key(&sid) || !self.topics_loading.insert(sid) {
            return;
        }
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { client.topics(sid).await }).await;
            let _ = this.update(cx, |s, cx| {
                s.topics_loading.remove(&sid);
                match r {
                    Ok(t) => {
                        s.topics.insert(sid, t);
                    }
                    Err(e) => tracing::warn!("topics for {sid}: {e}"),
                }
                cx.emit(Rc::new(Updated { sidebar: true, ..Default::default() }));
                cx.notify();
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
                self.read_queue.insert(id);
            }
        }
        if !ch.unreads {
            return;
        }
        self.store.write(puzil_store::writes_for(&self.state, &Changes { unreads: true, ..Default::default() }));
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
