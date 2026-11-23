//! Iliad Overlay for one account: sign-in, scores, inline reactions, topic anchors and
//! threads. Votes and inline reactions live in the Overlay service; replies and threads
//! live in Zulip (see `puzil-overlay`).
//!
//! Privacy: only ids of messages and topics in channels the service covers (public ones,
//! plus `/health.extra_streams`) are ever sent to it. Callers check [`Overlay::covers`].

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::*;
use puzil_overlay::{CallError, Health, InlineReaction, Kind, Score, Service, SignIn, VoteTarget};
use puzil_zulip::{
    Client,
    model::TopicKey,
    types::{MessageId, StreamId},
};

use crate::runtime::on_tokio;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    SigningIn,
    Ready,
    /// The service can't be reached; retried every ten seconds.
    Unreachable,
    /// The service refused us. Not retried until the user asks (overlay.js `signInFailed`).
    Refused(String),
}

/// A topic's identity for votes: its first message.
#[derive(Debug, Clone)]
pub struct Anchor {
    pub id: MessageId,
    pub sender_id: u64,
    pub stream_id: StreamId,
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub stream_id: StreamId,
    pub topic: String,
    pub started_by: String,
    pub preview: String,
}

/// What changed, for views.
#[derive(Default)]
pub struct OverlayChanged {
    /// Messages whose inline reactions changed (their layout changes).
    pub inline: HashSet<MessageId>,
    pub threads: bool,
    pub status: bool,
}

pub struct Overlay {
    service: Service,
    zulip: Client,
    user_id: u64,
    pub status: Status,
    session: Option<puzil_overlay::Session>,
    health: Health,
    pub message_scores: HashMap<MessageId, Score>,
    pub topic_scores: HashMap<MessageId, Score>,
    pub inline: HashMap<MessageId, Vec<InlineReaction>>,
    anchors: HashMap<TopicKey, Option<Anchor>>,
    anchors_loading: HashSet<TopicKey>,
    /// Threads started from a message, by that message's id.
    pub threads: HashMap<MessageId, Vec<Thread>>,
    thread_checks: HashMap<TopicKey, Instant>,
    /// Plain-text previews of messages a reply points at, fetched when not in memory.
    pub snippets: HashMap<MessageId, Option<(StreamId, String, String)>>,
    snippets_loading: HashSet<MessageId>,
    queried: HashSet<MessageId>,
    pending_messages: HashSet<MessageId>,
    pending_topics: HashSet<MessageId>,
    /// A batch of score and inline queries is scheduled (the task is detached: clearing a
    /// held `Task` from inside itself would cancel it at its next await).
    flush_scheduled: bool,
    times: HashMap<MessageId, i64>,
    times_pending: HashSet<MessageId>,
    times_scheduled: bool,
    _retry: Option<Task<()>>,
}

impl EventEmitter<Rc<OverlayChanged>> for Overlay {}

fn token_key(service: &str, user_id: u64) -> String {
    format!("overlay {user_id} @ {service}")
}

impl Overlay {
    pub fn new(zulip: Client, user_id: u64, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            service: Service::new(puzil_overlay::service_url()),
            zulip,
            user_id,
            status: Status::SigningIn,
            session: None,
            health: Health::default(),
            message_scores: HashMap::new(),
            topic_scores: HashMap::new(),
            inline: HashMap::new(),
            anchors: HashMap::new(),
            anchors_loading: HashSet::new(),
            threads: HashMap::new(),
            thread_checks: HashMap::new(),
            snippets: HashMap::new(),
            snippets_loading: HashSet::new(),
            queried: HashSet::new(),
            pending_messages: HashSet::new(),
            pending_topics: HashSet::new(),
            flush_scheduled: false,
            times: HashMap::new(),
            times_pending: HashSet::new(),
            times_scheduled: false,
            _retry: None,
        };
        this.sign_in(cx);
        this
    }

    pub fn ready(&self) -> bool {
        self.status == Status::Ready
    }

    /// Whether votes and inline reactions may be offered (and ids sent) for a channel.
    pub fn covers(&self, stream_id: StreamId, invite_only: Option<bool>) -> bool {
        self.ready() && puzil_overlay::covers(invite_only, stream_id, &self.health)
    }

    fn emit(&self, ch: OverlayChanged, cx: &mut Context<Self>) {
        cx.emit(Rc::new(ch));
        cx.notify();
    }

    /// Single-flight: one sign-in at a time; a refusal stops it for good.
    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Refused(_)) {
            return;
        }
        self.status = Status::SigningIn;
        let (service, zulip, user_id) = (self.service.clone(), self.zulip.clone(), self.user_id);
        let key = token_key(&puzil_overlay::service_url(), user_id);
        cx.spawn(async move |this, cx| {
            let k = key.clone();
            let outcome = on_tokio(async move {
                let Some(health) = service.health().await else { return (None, SignIn::Unreachable) };
                let stored = crate::accounts::secrets().get(&k).ok().flatten();
                let r = service.sign_in(&zulip, user_id, stored.as_deref(), &health).await;
                match &r {
                    SignIn::Token(t) if stored.as_deref() != Some(t.as_str()) => {
                        let _ = crate::accounts::secrets().set(&k, t);
                    }
                    SignIn::Refused(_) if stored.is_some() => {
                        let _ = crate::accounts::secrets().delete(&k);
                    }
                    _ => {}
                }
                (Some(health), r)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let (health, r) = outcome;
                if let Some(h) = health {
                    this.health = h;
                }
                match r {
                    SignIn::Token(token) => {
                        tracing::info!("signed in to the overlay service");
                        this.session = Some(puzil_overlay::Session { service: this.service.clone(), token });
                        this.status = Status::Ready;
                        this._retry = None;
                        // Anything asked for while signing in.
                        this.queried.clear();
                        this.schedule_flush(cx);
                    }
                    SignIn::Unreachable => {
                        this.status = Status::Unreachable;
                        this.retry_later(cx);
                    }
                    SignIn::Refused(why) => {
                        tracing::warn!("overlay sign-in refused: {why}");
                        this.status = Status::Refused(why);
                    }
                }
                this.emit(OverlayChanged { status: true, ..Default::default() }, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn retry_later(&mut self, cx: &mut Context<Self>) {
        self._retry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(10)).await;
            let _ = this.update(cx, |this, cx| this.sign_in(cx));
        }));
    }

    /// A call failed: sign in again on 401, back off when unreachable.
    fn on_error(&mut self, e: CallError, cx: &mut Context<Self>) {
        match e {
            CallError::Unauthorized => {
                self.session = None;
                let k = token_key(&puzil_overlay::service_url(), self.user_id);
                let _ = crate::accounts::secrets().delete(&k);
                self.sign_in(cx);
            }
            CallError::Unreachable => {
                self.session = None;
                self.status = Status::Unreachable;
                self.retry_later(cx);
                self.emit(OverlayChanged { status: true, ..Default::default() }, cx);
            }
            other => tracing::warn!("overlay: {other}"),
        }
    }

    // ---- Scores and inline reactions ----

    /// Asks for a covered message's score and inline reactions (once per view).
    pub fn want_message(&mut self, id: MessageId, cx: &mut Context<Self>) {
        if self.queried.insert(id) {
            self.pending_messages.insert(id);
            self.schedule_flush(cx);
        }
    }

    fn want_topic(&mut self, anchor: MessageId, cx: &mut Context<Self>) {
        if self.queried.insert(anchor) {
            self.pending_topics.insert(anchor);
            self.schedule_flush(cx);
        }
    }

    /// Votes and reactions are refreshed when a conversation is viewed again.
    pub fn refresh(&mut self, ids: impl IntoIterator<Item = MessageId>) {
        for id in ids {
            self.queried.remove(&id);
        }
    }

    fn schedule_flush(&mut self, cx: &mut Context<Self>) {
        if self.flush_scheduled || !self.ready() {
            return;
        }
        self.flush_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let Ok(Some((session, messages, topics))) = this.update(cx, |this, _| {
                this.flush_scheduled = false;
                let s = this.session.clone()?;
                let m: Vec<_> = this.pending_messages.drain().collect();
                let t: Vec<_> = this.pending_topics.drain().collect();
                (!m.is_empty() || !t.is_empty()).then_some((s, m, t))
            }) else {
                return;
            };
            let (m2, t2) = (messages.clone(), topics.clone());
            let r = on_tokio(async move {
                let scores = session.scores(&t2, &m2).await?;
                let inline = if m2.is_empty() { HashMap::new() } else { session.inline_query(&m2).await? };
                Ok::<_, CallError>((scores, inline))
            })
            .await;
            let _ = this.update(cx, |this, cx| match r {
                Ok((scores, inline)) => {
                    for (id, s) in scores.messages {
                        if let Ok(id) = id.parse() {
                            this.message_scores.insert(id, s);
                        }
                    }
                    for (id, s) in scores.topics {
                        if let Ok(id) = id.parse() {
                            this.topic_scores.insert(id, s);
                        }
                    }
                    let mut changed = HashSet::new();
                    for id in &messages {
                        let new = inline.get(id).cloned().unwrap_or_default();
                        if this.inline.get(id).map(Vec::as_slice).unwrap_or_default() != new.as_slice() {
                            changed.insert(*id);
                        }
                        if new.is_empty() {
                            this.inline.remove(id);
                        } else {
                            this.inline.insert(*id, new);
                        }
                    }
                    this.emit(OverlayChanged { inline: changed, ..Default::default() }, cx);
                }
                Err(e) => {
                    for id in messages.iter().chain(&topics) {
                        this.queried.remove(id);
                    }
                    this.on_error(e, cx);
                }
            });
        })
        .detach();
    }

    /// Up (1) or down (-1); voting the same way again takes the vote back.
    pub fn vote(&mut self, kind: Kind, target: VoteTarget, direction: i64, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else { return };
        let scores = match kind {
            Kind::Message => &self.message_scores,
            Kind::Topic => &self.topic_scores,
        };
        let current = scores.get(&target.id).map(|s| s.my_vote).unwrap_or(0);
        let value = if current == direction { 0 } else { direction };
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { session.vote(kind, target, value).await }).await;
            let _ = this.update(cx, |this, cx| match r {
                Ok(s) => {
                    match kind {
                        Kind::Message => this.message_scores.insert(target.id, s),
                        Kind::Topic => this.topic_scores.insert(target.id, s),
                    };
                    this.emit(OverlayChanged::default(), cx);
                }
                Err(e) => this.on_error(e, cx),
            });
        })
        .detach();
    }

    pub fn put_inline(&mut self, message: MessageId, start: usize, end: usize, hash: String, emoji: String, on: bool, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else { return };
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { session.put_inline(message, start, end, &hash, &emoji, on).await }).await;
            let _ = this.update(cx, |this, cx| match r {
                Ok(reactions) => {
                    this.inline.insert(message, reactions);
                    this.emit(OverlayChanged { inline: [message].into(), ..Default::default() }, cx);
                }
                Err(e) => this.on_error(e, cx),
            });
        })
        .detach();
    }

    // ---- Topics: anchors and threads (from Zulip) ----

    /// The topic's first message, loading it if needed. Asks for the topic's score too.
    pub fn topic_anchor(&mut self, key: &TopicKey, topic: &str, cx: &mut Context<Self>) -> Option<Anchor> {
        if let Some(a) = self.anchors.get(key).cloned() {
            if let Some(a) = &a {
                self.want_topic(a.id, cx);
            }
            return a;
        }
        if !self.anchors_loading.insert(key.clone()) {
            return None;
        }
        let narrow = serde_json::json!([
            {"operator": "channel", "operand": key.stream_id},
            {"operator": "topic", "operand": topic},
        ]);
        let (zulip, key) = (self.zulip.clone(), key.clone());
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { zulip.first_message_raw(&narrow).await }).await;
            let _ = this.update(cx, |this, cx| {
                this.anchors_loading.remove(&key);
                match r {
                    Ok(m) => {
                        let a = m.map(|m| Anchor { id: m.id, sender_id: m.sender_id, stream_id: m.stream_id.unwrap_or(key.stream_id) });
                        if let Some(a) = &a {
                            this.want_topic(a.id, cx);
                        }
                        this.anchors.insert(key, a);
                        this.emit(OverlayChanged::default(), cx);
                    }
                    Err(e) => tracing::debug!("topic anchor: {e}"),
                }
            });
        })
        .detach();
        None
    }

    /// Finds threads branching from messages of `root` (a topic): topics named
    /// `root / …` whose first message links back with `thread from this message`.
    /// `topics` is the channel's topic list.
    pub fn find_threads(&mut self, stream_id: StreamId, root: &str, topics: Vec<String>, cx: &mut Context<Self>) {
        let root = puzil_overlay::unresolved(root).to_string();
        let key = TopicKey::new(stream_id, &root);
        if self.thread_checks.get(&key).is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return;
        }
        self.thread_checks.insert(key, Instant::now());
        let prefix = format!("{root}{}", puzil_overlay::SEPARATOR);
        let names: Vec<String> = topics.into_iter().filter(|t| puzil_overlay::unresolved(t).starts_with(&prefix)).collect();
        if names.is_empty() {
            return;
        }
        let zulip = self.zulip.clone();
        cx.spawn(async move |this, cx| {
            let found = on_tokio(async move {
                let mut out = Vec::new();
                for name in names {
                    let narrow = serde_json::json!([
                        {"operator": "channel", "operand": stream_id},
                        {"operator": "topic", "operand": name},
                    ]);
                    if let Ok(Some(m)) = zulip.first_message_raw(&narrow).await
                        && let Some(src) = puzil_overlay::thread_source(&m.content)
                    {
                        out.push((
                            src,
                            Thread {
                                stream_id,
                                topic: name,
                                started_by: m.sender_full_name.clone(),
                                preview: puzil_overlay::shorten(&puzil_overlay::plain_text(&m.content), 160),
                            },
                        ));
                    }
                }
                out
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                for (src, t) in found {
                    let list = this.threads.entry(src).or_default();
                    if !list.iter().any(|x| x.topic == t.topic) {
                        list.push(t);
                    }
                }
                this.emit(OverlayChanged { threads: true, ..Default::default() }, cx);
            });
        })
        .detach();
    }

    /// A thread's first message is on screen: record it under its source right away.
    pub fn note_thread(&mut self, source: MessageId, t: Thread, cx: &mut Context<Self>) {
        let list = self.threads.entry(source).or_default();
        if !list.iter().any(|x| x.topic == t.topic) {
            list.push(t);
            self.emit(OverlayChanged { threads: true, ..Default::default() }, cx);
        }
    }

    /// Forget the throttle so a just-started thread shows up on the next look.
    pub fn recheck_threads(&mut self, stream_id: StreamId, root: &str) {
        self.thread_checks.remove(&TopicKey::new(stream_id, puzil_overlay::unresolved(root)));
    }

    /// `(stream, topic, plain text)` of a message not in memory, for a reply chip.
    pub fn snippet(&mut self, id: MessageId, cx: &mut Context<Self>) -> Option<(StreamId, String, String)> {
        if let Some(s) = self.snippets.get(&id) {
            return s.clone();
        }
        if self.snippets_loading.insert(id) {
            let zulip = self.zulip.clone();
            cx.spawn(async move |this, cx| {
                let r = on_tokio(async move { zulip.messages_by_id_raw(&[id]).await }).await;
                let _ = this.update(cx, |this, cx| {
                    this.snippets_loading.remove(&id);
                    let v = r.ok().and_then(|v| v.into_iter().next()).and_then(|m| {
                        Some((m.stream_id?, m.subject.clone(), puzil_overlay::plain_text(&m.content)))
                    });
                    this.snippets.insert(id, v);
                    this.emit(OverlayChanged { threads: true, ..Default::default() }, cx);
                });
            })
            .detach();
        }
        None
    }
}

impl Overlay {
    /// When a topic's latest message was sent, for "Hot" sorting; fetched in batches.
    pub fn message_time(&mut self, id: MessageId, cx: &mut Context<Self>) -> Option<i64> {
        if let Some(t) = self.times.get(&id) {
            return Some(*t);
        }
        if self.times_pending.insert(id) && !self.times_scheduled {
            self.times_scheduled = true;
            let zulip = self.zulip.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let Ok(ids) = this.update(cx, |this, _| {
                    this.times_scheduled = false;
                    this.times_pending.drain().collect::<Vec<_>>()
                }) else {
                    return;
                };
                let r = on_tokio(async move {
                    let mut out = Vec::new();
                    for chunk in ids.chunks(100) {
                        out.extend(zulip.messages_by_id_raw(chunk).await?.into_iter().map(|m| (m.id, m.timestamp)));
                    }
                    Ok::<_, puzil_zulip::ApiError>(out)
                })
                .await;
                let _ = this.update(cx, |this, cx| {
                    if let Ok(v) = r {
                        this.times.extend(v);
                        this.emit(OverlayChanged::default(), cx);
                    }
                });
            })
            .detach();
        }
        None
    }
}
