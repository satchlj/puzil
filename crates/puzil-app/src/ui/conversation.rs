//! One conversation (a topic or a DM): header, message list, composer.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use gpui_kit::{prelude::FluentBuilder as _, *};
use puzil_render::{
    MessageAst,
    ast::{Media, MediaKind},
};
use puzil_zulip::{
    model::{Conv, TopicKey},
    types::{Message, MessageId, StreamId},
};

use super::{
    blocks::{self, BuiltMsg, RenderCtx},
    composer::{Composer, ComposerEvent},
    fmt,
    rich::{LinkHandler, Selection},
};
use crate::{
    icons,
    math::MathReady,
    media::{Images, ImagesReady, ImgState},
    session::{PendingState, Session, TypingChanged, Updated},
    theme::Theme,
};

actions!(conversation, [CopySelection, SelectPrev, SelectNext, EditSelected, DeleteSelected, ClearSelection]);

/// Keys for the message list once it has focus (↑ in an empty composer gives it focus):
/// ↑/↓ move the selection, Enter edits our message, ⌫ deletes it, Esc goes back.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("MessageList");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("enter", EditSelected, ctx),
        KeyBinding::new("backspace", DeleteSelected, ctx),
        KeyBinding::new("delete", DeleteSelected, ctx),
        KeyBinding::new("escape", ClearSelection, ctx),
    ]);
}

/// What the pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Conv(Conv),
    /// Composing the first message of a new topic.
    NewTopic(StreamId),
}

pub enum ConversationEvent {
    Navigate(Target),
    /// Show an image in the lightbox.
    OpenImage(Media),
    /// Open `target` with the composer pre-filled (an Overlay thread: a new topic whose
    /// first message links back to where it started).
    Compose {
        target: Target,
        topic: Option<String>,
        body: String,
    },
}

mod menu;
mod profile;
mod overlay_ui;

#[derive(Debug, Clone, PartialEq)]
enum Item {
    Top,
    Day(i64),
    NewMarker,
    Msg {
        id: MessageId,
        head: bool,
    },
    Pending(u64),
    /// Overlay's time-gap divider: this many seconds passed since the previous message.
    Gap(i64),
}


pub struct Conversation {
    session: Entity<Session>,
    pub target: Target,
    theme: Rc<Theme>,
    items: Rc<RefCell<Vec<Item>>>,
    list: ListState,
    built: RefCell<HashMap<MessageId, Rc<BuiltMsg>>>,
    selection: Rc<RefCell<Selection>>,
    expanded: Rc<HashSet<(u64, usize)>>,
    /// The first unread message when the conversation was opened; the "new" line sits above it.
    new_marker: Option<MessageId>,
    scrolled_to_marker: bool,
    composer: Entity<Composer>,
    focus: FocusHandle,
    hovered: Option<MessageId>,
    /// The message picked with the arrow keys (shown while the list has focus).
    kb_selected: Option<MessageId>,
    /// Whether the message list had focus when this frame was rendered.
    list_focused: bool,
    /// The open right-click menu: its message and where it was opened.
    menu: Option<(MessageId, Point<Pixels>)>,
    /// The open profile card: whose, and where it was opened.
    profile: Option<(u64, Point<Pixels>)>,
    /// The profile card's message box, and whether a message went out from it.
    profile_input: Option<(Entity<gpui_kit::component::input::InputState>, Subscription)>,
    profile_sent: bool,
    /// The reaction picker opened from a reaction row's add button.
    picker: Option<(MessageId, Point<Pixels>)>,
    /// Set by Mark unread: stop marking messages read until the conversation is reopened.
    read_paused: bool,
    /// The message whose toolbar the pointer is on. The toolbar sticks out above its
    /// message, so leaving the message's box mustn't hide it.
    bar_hovered: Option<MessageId>,
    /// Bounds of unread rows painted this frame, for marking them read.
    painted: Rc<RefCell<Vec<(MessageId, Bounds<Pixels>)>>>,
    math: Arc<puzil_math::Engine>,
    /// Messages whose formulas were already queued for typesetting.
    prefetched: HashSet<MessageId>,
    images: Entity<Images>,
    downloading: Rc<HashSet<String>>,
    overlay: Option<Entity<crate::overlay::Overlay>>,
    /// Each message's reply header, if it has one (parsed once).
    replies: HashMap<MessageId, Option<puzil_render::ast::ReplyMeta>>,
    reply_counts: HashMap<MessageId, usize>,
    /// The thread name box, while a thread is being started from a message.
    thread_ask: Option<(MessageId, Entity<gpui_kit::component::input::InputState>, Subscription)>,
    /// A message briefly highlighted after jumping to it.
    flashing: Option<MessageId>,
    /// How many times something was flashed, so each flash restarts its fade.
    flash_count: usize,
    /// A message to scroll to and highlight once it's loaded (a search result).
    pending_reveal: Option<MessageId>,
    /// Messages that arrived while the conversation was open, and when (they rise in).
    arrived: HashMap<MessageId, std::time::Instant>,
    /// Reactions we just added, and when (they pop in).
    reacted: HashMap<(MessageId, String), std::time::Instant>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ConversationEvent> for Conversation {}

enum Title {
    Topic { channel: String, private: bool, topic: String, description: String },
    Dm { name: String, user: u64, group: bool, avatar: Option<String> },
}

impl Conversation {
    pub fn new(session: Entity<Session>, target: Target, theme: Rc<Theme>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| Composer::new(session.clone(), target.clone(), theme.clone(), window, cx));
        let mut subs = vec![cx.subscribe(&session, |this, _, up: &Rc<Updated>, cx| this.on_update(up, cx))];
        subs.push(cx.subscribe(&session, |this, _, e: &TypingChanged, cx| {
            if this.conv() == Some(&e.0) {
                cx.notify();
            }
        }));
        subs.push(cx.subscribe_in(&composer, window, |this, _, e: &ComposerEvent, window, cx| match e {
            ComposerEvent::Sent(t) => {
                if *t != this.target {
                    cx.emit(ConversationEvent::Navigate(t.clone()));
                } else {
                    this.list.scroll_to_end();
                }
            }
            ComposerEvent::SelectLast => this.select_last(window, cx),
        }));
        // Messages on screen count as read only while the window is active, so re-check
        // when it becomes active.
        subs.push(cx.observe_window_activation(window, |_, _, cx| cx.notify()));
        // Settings (grouping, time format) can change how the list reads.
        subs.push(cx.observe(&crate::settings::handle(cx), |this, _, cx| {
            this.refresh_items(cx);
            this.list.remeasure();
            cx.notify();
        }));
        let hub = crate::math::hub(cx);
        let math = hub.read(cx).engine.clone();
        subs.push(cx.subscribe(&hub, |this, _, e: &MathReady, cx| this.on_math(&e.0, cx)));
        let images = session.read(cx).images.clone();
        subs.push(cx.subscribe(&images, |this, _, e: &ImagesReady, cx| this.on_images(&e.0, cx)));
        let overlay = session.read(cx).overlay.clone();
        if let Some(o) = &overlay {
            subs.push(cx.subscribe(o, |this, _, ch: &Rc<crate::overlay::OverlayChanged>, cx| this.on_overlay(ch, cx)));
        }
        let list = ListState::new(0, ListAlignment::Bottom, px(1000.));
        let new_marker = match &target {
            Target::Conv(c) => session.read(cx).unread_in(c).first().copied(),
            Target::NewTopic(_) => None,
        };
        let mut this = Self {
            session: session.clone(),
            target: target.clone(),
            theme,
            items: Rc::default(),
            list,
            built: RefCell::default(),
            selection: Rc::default(),
            expanded: Rc::default(),
            new_marker,
            scrolled_to_marker: false,
            composer,
            focus: cx.focus_handle(),
            hovered: None,
            kb_selected: None,
            list_focused: false,
            menu: None,
            profile: None,
            profile_input: None,
            profile_sent: false,
            picker: None,
            read_paused: false,
            bar_hovered: None,
            painted: Rc::default(),
            math,
            prefetched: HashSet::new(),
            images,
            downloading: Rc::default(),
            overlay,
            replies: HashMap::new(),
            reply_counts: HashMap::new(),
            thread_ask: None,
            flashing: None,
            flash_count: 0,
            pending_reveal: None,
            arrived: HashMap::new(),
            reacted: HashMap::new(),
            _subs: subs,
        };
        let weak = cx.entity().downgrade();
        this.list.set_scroll_handler(move |e, _, cx| {
            if e.visible_range.start <= 2 {
                let _ = weak.update(cx, |this, cx| this.load_older(cx));
            }
        });
        if let Target::Conv(c) = &target {
            session.update(cx, |s, cx| s.open(c, cx));
        }
        this.refresh_items(cx);
        this.overlay_on_open(cx);
        this
    }

    pub fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    pub fn set_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme.clone();
        self.built.borrow_mut().clear();
        self.list.remeasure();
        self.composer.update(cx, |c, cx| c.set_theme(theme, cx));
        cx.notify();
    }

    fn conv(&self) -> Option<&Conv> {
        match &self.target {
            Target::Conv(c) => Some(c),
            Target::NewTopic(_) => None,
        }
    }

    fn load_older(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.conv().cloned() {
            self.session.update(cx, |s, cx| s.load_older(&c, cx));
        }
    }

    fn on_update(&mut self, up: &Updated, cx: &mut Context<Self>) {
        {
            let mut built = self.built.borrow_mut();
            for id in &up.messages {
                built.remove(id);
            }
        }
        let mine = match self.conv() {
            Some(c) => up.all || up.convs.contains(c) || up.messages.iter().any(|id| self.built.borrow().contains_key(id)),
            None => false,
        };
        if mine || !up.messages.is_empty() {
            self.refresh_items(cx);
        }
        if up.sidebar {
            self.look_for_threads(cx);
        }
        cx.notify();
    }

    /// Rebuilds messages that were waiting on any of these formulas.
    fn on_math(&mut self, keys: &HashSet<puzil_math::Key>, cx: &mut Context<Self>) {
        let stale: Vec<MessageId> =
            self.built.borrow().iter().filter(|(_, b)| !b.pending.is_disjoint(keys)).map(|(id, _)| *id).collect();
        if stale.is_empty() {
            return;
        }
        let mut built = self.built.borrow_mut();
        for (ix, it) in self.items.borrow().iter().enumerate() {
            if let Item::Msg { id, .. } = it
                && stale.contains(id)
            {
                built.remove(id);
                self.list.remeasure_items(ix..ix + 1);
            }
        }
        cx.notify();
    }

    /// Queues the formulas of messages not seen before, parsing off the main thread, so
    /// they are typeset by the time they scroll into view.
    fn prefetch_math(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.conv().cloned() else {
            return;
        };
        let s = self.session.read(cx);
        let html: Vec<String> = s
            .conv_ids(&c)
            .into_iter()
            .rev()
            .filter(|id| !self.prefetched.contains(id))
            .filter_map(|id| s.state.messages.get(&id))
            .filter(|m| m.content.contains("katex"))
            .map(|m| m.content.clone())
            .collect();
        self.prefetched.extend(s.conv_ids(&c));
        if html.is_empty() {
            return;
        }
        let math = self.math.clone();
        cx.background_spawn(async move {
            for h in html {
                for (tex, mathml, display) in MessageAst::from_html(&h).math() {
                    math.request(&crate::math::source(tex, mathml, display));
                }
            }
        })
        .detach();
    }

    fn refresh_items(&mut self, cx: &mut Context<Self>) {
        self.prefetch_math(cx);
        self.count_replies(cx);
        let group_secs = crate::settings::effective(cx, Some(&self.session.read(cx).account.realm.clone())).group_minutes as i64 * 60;
        let s = self.session.read(cx);
        let mut items = Vec::new();
        if let Some(c) = self.conv() {
            let d = s.conv_data(c);
            if d.is_some_and(|d| d.found_oldest) {
                items.push(Item::Top);
            }
            let mut prev: Option<&Message> = None;
            for id in s.conv_ids(c) {
                let Some(m) = s.state.messages.get(&id) else {
                    continue;
                };
                if prev.is_none_or(|p| !fmt::same_day(p.timestamp, m.timestamp)) {
                    items.push(Item::Day(m.timestamp));
                    prev = None;
                }
                if self.new_marker == Some(id) {
                    items.push(Item::NewMarker);
                    prev = None;
                }
                if let Some(g) = prev.and_then(|p| overlay_ui::gap_after(p.timestamp, m.timestamp))
                    && self.overlay.is_some()
                {
                    items.push(Item::Gap(g));
                    prev = None;
                }
                // A reply always starts a group, so its "replying to" line sits above a name.
                let head = prev.is_none_or(|p| p.sender_id != m.sender_id || m.timestamp - p.timestamp > group_secs)
                    || self.replies.get(&id).is_some_and(|r| r.as_ref().is_some_and(|r| !r.is_thread));
                items.push(Item::Msg { id, head });
                prev = Some(m);
            }
            for p in s.outbox.iter().filter(|p| &p.conv == c) {
                items.push(Item::Pending(p.local_id));
            }
        }
        let changed: Vec<usize> = {
            let built = self.built.borrow();
            items
                .iter()
                .enumerate()
                .filter(|(_, it)| matches!(it, Item::Msg { id, .. } if !built.contains_key(id)))
                .map(|(i, _)| i)
                .collect()
        };
        let old = std::mem::replace(&mut *self.items.borrow_mut(), items);
        let new = self.items.borrow();
        // Messages newer than everything already shown arrived live (older pages loaded by
        // scrolling up don't count, nor does the first fill).
        let newest_old = old.iter().filter_map(|i| if let Item::Msg { id, .. } = i { Some(*id) } else { None }).max();
        if let Some(top) = newest_old {
            let now = std::time::Instant::now();
            self.arrived.retain(|_, t| now.duration_since(*t).as_millis() < 1000);
            for i in new.iter() {
                if let Item::Msg { id, .. } = i
                    && *id > top
                {
                    self.arrived.entry(*id).or_insert(now);
                }
            }
        }
        // Splice only the differing middle, so prepends and appends keep the scroll anchor.
        let prefix = old.iter().zip(new.iter()).take_while(|(a, b)| a == b).count();
        let max_suffix = old.len().min(new.len()) - prefix;
        let suffix = old.iter().rev().zip(new.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
        if old.len() != new.len() || prefix != old.len() {
            self.list.splice(prefix..old.len() - suffix, new.len() - suffix - prefix);
        }
        for i in changed {
            if i < prefix || i >= new.len() - suffix {
                self.list.remeasure_items(i..i + 1);
            }
        }
        let fresh = self.conv().and_then(|c| s.conv_data(c)).is_some_and(|d| d.fresh);
        if fresh && !self.scrolled_to_marker {
            self.scrolled_to_marker = true;
            if let Some(ix) = new.iter().position(|i| *i == Item::NewMarker) {
                self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.) });
            }
        }
        drop(new);
        self.try_reveal(cx);
    }

    fn built(&self, m: &Message, me: u64, cx: &App) -> Rc<BuiltMsg> {
        if let Some(b) = self.built.borrow().get(&m.id) {
            return b.clone();
        }
        let images = self.images.read(cx);
        let lookup = |u: &str| images.peek(u);
        // Messages that can carry inline reactions get their Overlay text index.
        let covered = match (&self.overlay, m.stream_id) {
            (Some(o), Some(sid)) => {
                let s = self.session.read(cx);
                o.read(cx).covers(sid, s.state.streams.get(&sid).map(|st| st.invite_only))
            }
            _ => false,
        };
        let reactions = self.overlay.as_ref().and_then(|o| o.read(cx).inline.get(&m.id).cloned()).unwrap_or_default();
        let inline = covered.then(|| blocks::Inline { html: &m.content, reactions: &reactions });
        let b =
            Rc::new(blocks::build(&MessageAst::from_html(&m.content), &self.theme, me, Some(&self.math), Some(&lookup), inline));
        self.built.borrow_mut().insert(m.id, b.clone());
        b
    }

    /// Rebuilds messages whose layout depends on images that just arrived; repaints the rest.
    fn on_images(&mut self, urls: &HashSet<String>, cx: &mut Context<Self>) {
        let stale: Vec<MessageId> =
            self.built.borrow().iter().filter(|(_, b)| !b.relayout_images.is_disjoint(urls)).map(|(id, _)| *id).collect();
        if !stale.is_empty() {
            let mut built = self.built.borrow_mut();
            for (ix, it) in self.items.borrow().iter().enumerate() {
                if let Item::Msg { id, .. } = it
                    && stale.contains(id)
                {
                    built.remove(id);
                    self.list.remeasure_items(ix..ix + 1);
                }
            }
        }
        // Thumbnails and avatars only need a repaint.
        cx.notify();
    }

    fn open_media(&mut self, m: Media, cx: &mut Context<Self>) {
        match m.kind {
            MediaKind::Image => cx.emit(ConversationEvent::OpenImage(m)),
            _ => {
                let client = self.session.read(cx).client.clone();
                if let Some(h) = m.href.or(m.src) {
                    cx.open_url(&client.absolute(&h));
                }
            }
        }
    }

    /// Downloads an uploaded file (with our credentials) and opens it with the system.
    fn open_attachment(&mut self, name: String, href: String, cx: &mut Context<Self>) {
        let client = self.session.read(cx).client.clone();
        if !href.contains("/user_uploads/") || !href.starts_with('/') && !href.starts_with(&client.creds.realm) {
            cx.open_url(&client.absolute(&href));
            return;
        }
        if self.downloading.contains(&href) {
            return;
        }
        let mut d = (*self.downloading).clone();
        d.insert(href.clone());
        self.downloading = Rc::new(d);
        cx.notify();
        let file = href.rsplit('/').next().filter(|f| !f.is_empty()).map(str::to_string).unwrap_or(name);
        let dir = crate::accounts::data_dir().join("cache/files").join(&blake3::hash(href.as_bytes()).to_hex()[..16]);
        let path = dir.join(sanitize(&file));
        cx.spawn(async move |this, cx| {
            let r = if path.exists() {
                Ok(())
            } else {
                let (h, path) = (href.clone(), path.clone());
                crate::runtime::on_tokio(async move {
                    let bytes = client.get_bytes(&h).await.map_err(|e| e.to_string())?;
                    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                    std::fs::write(&path, bytes).map_err(|e| e.to_string())
                })
                .await
            };
            let _ = this.update(cx, |this, cx| {
                let mut d = (*this.downloading).clone();
                d.remove(&href);
                this.downloading = Rc::new(d);
                match r {
                    Ok(()) => cx.open_with_system(&path),
                    Err(e) => tracing::warn!("download {href}: {e}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_selection(&mut self, _: &CopySelection, _: &mut Window, cx: &mut Context<Self>) {
        let sel = self.selection.borrow();
        let Some((a, b)) = sel.ordered() else { return };
        let built = self.built.borrow();
        let mut ids: Vec<_> = built.keys().copied().filter(|id| *id >= a.msg && *id <= b.msg).collect();
        ids.sort_unstable();
        let text: Vec<String> = ids.iter().map(|id| built[id].copy(*id, &sel)).filter(|t| !t.is_empty()).collect();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
        }
    }

    /// Message ids in display order.
    fn shown_ids(&self) -> Vec<MessageId> {
        self.items.borrow().iter().filter_map(|i| if let Item::Msg { id, .. } = i { Some(*id) } else { None }).collect()
    }

    fn select(&mut self, id: Option<MessageId>, window: &mut Window, cx: &mut Context<Self>) {
        self.kb_selected = id;
        match id {
            Some(id) => {
                if let Some(ix) = self.items.borrow().iter().position(|i| matches!(i, Item::Msg { id: m, .. } if *m == id)) {
                    self.list.scroll_to_reveal_item(ix);
                }
                self.focus.focus(window, cx);
            }
            None => self.composer.update(cx, |c, cx| c.focus(window, cx)),
        }
        cx.notify();
    }

    /// Scrolls to `id` and highlights it, paging back to it first if it isn't loaded.
    pub fn reveal(&mut self, id: MessageId, cx: &mut Context<Self>) {
        self.pending_reveal = Some(id);
        if let Some(c) = self.conv().cloned() {
            self.session.update(cx, |s, cx| s.load_back_to(&c, id, cx));
        }
        self.try_reveal(cx);
    }

    fn try_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.pending_reveal else { return };
        if self.items.borrow().iter().any(|i| matches!(i, Item::Msg { id: m, .. } if *m == id)) {
            self.pending_reveal = None;
            self.flash(id, cx);
        }
    }

    fn select_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.shown_ids().last().copied();
        if last.is_some() {
            self.select(last, window, cx);
        }
    }

    fn select_prev(&mut self, _: &SelectPrev, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.shown_ids();
        let next = match self.kb_selected.and_then(|s| ids.iter().position(|i| *i == s)) {
            Some(0) => ids.first().copied(),
            Some(p) => ids.get(p - 1).copied(),
            None => ids.last().copied(),
        };
        self.select(next, window, cx);
    }

    fn select_next(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.shown_ids();
        // Past the newest message, back to the composer.
        let next = self.kb_selected.and_then(|s| ids.iter().position(|i| *i == s)).and_then(|p| ids.get(p + 1).copied());
        self.select(next, window, cx);
    }

    fn selected_mine(&self, cx: &App) -> Option<MessageId> {
        let s = self.session.read(cx);
        self.kb_selected.filter(|id| s.state.messages.get(id).is_some_and(|m| m.sender_id == s.state.me))
    }

    /// Enter on the selected message: edit it if it's ours, otherwise reply to it.
    fn edit_selected(&mut self, _: &EditSelected, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.kb_selected else { return };
        self.kb_selected = None;
        if self.selected_mine_id(id, cx) {
            self.composer.update(cx, |comp, cx| comp.start_edit(id, window, cx));
        } else {
            let info = {
                let s = self.session.read(cx);
                s.state.messages.get(&id).map(|m| self.info(m, cx))
            };
            if let Some(info) = info {
                self.start_reply(&info, window, cx);
            }
        }
        cx.notify();
    }

    fn selected_mine_id(&self, id: MessageId, cx: &App) -> bool {
        let s = self.session.read(cx);
        s.state.messages.get(&id).is_some_and(|m| m.sender_id == s.state.me)
    }

    fn delete_selected(&mut self, _: &DeleteSelected, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_mine(cx) {
            self.delete(id, window, cx);
        }
    }

    fn clear_selection(&mut self, _: &ClearSelection, window: &mut Window, cx: &mut Context<Self>) {
        self.select(None, window, cx);
    }

    fn delete(&mut self, id: MessageId, window: &mut Window, cx: &mut Context<Self>) {
        let answer =
            window.prompt(PromptLevel::Warning, "Delete this message?", Some("This can't be undone."), &["Delete", "Cancel"], cx);
        let session = self.session.clone();
        cx.spawn(async move |_, cx| {
            let a = answer.await.ok();
            if a == Some(0) {
                let _ = session.update(cx, |s, cx| {
                    s.call(cx, move |c| async move { c.delete_message(id).await }, |_, e, _| tracing::warn!("delete failed: {e}"))
                });
            }
        })
        .detach();
    }

    fn toggle_reaction(&mut self, id: MessageId, name: String, code: String, kind: String, mine: bool, cx: &mut Context<Self>) {
        if !mine {
            self.reacted.insert((id, code.clone()), std::time::Instant::now());
        }
        self.session.update(cx, |s, cx| {
            s.call(
                cx,
                move |c| async move { c.react(id, !mine, &name, &code, &kind).await },
                |_, e, _| tracing::warn!("reaction failed: {e}"),
            )
        });
    }

    fn link_handler(&self, cx: &Context<Self>) -> LinkHandler {
        let weak = cx.entity().downgrade();
        let realm = self.session.read(cx).account.realm.clone();
        Rc::new(move |href: &str, window, cx| {
            let href = href.trim();
            if let Some(id) = href.strip_prefix(super::rich::USER_LINK).and_then(|i| i.parse::<u64>().ok()) {
                let at = window.mouse_position();
                let _ = weak.update(cx, |this, cx| this.open_profile(id, at, window, cx));
                return;
            }
            let local = href.strip_prefix(realm.as_str()).unwrap_or(href);
            if let Some(narrow) = local.strip_prefix("/#narrow/").or(local.strip_prefix("#narrow/")) {
                let _ = weak.update(cx, |this, cx| match this.parse_narrow(narrow, cx) {
                    Some(t) => cx.emit(ConversationEvent::Navigate(t)),
                    None => cx.open_url(&format!("{realm}/#narrow/{narrow}")),
                });
            } else if local.starts_with('/') {
                cx.open_url(&format!("{realm}{local}"));
            } else {
                cx.open_url(href);
            }
        })
    }

    /// `channel/12-name/topic/a.20b[/near/5]` or `dm/3,4-group` → a conversation.
    fn parse_narrow(&self, narrow: &str, cx: &App) -> Option<Target> {
        let parts: Vec<&str> = narrow.split('/').collect();
        let s = self.session.read(cx);
        match parts.as_slice() {
            [kind, ch, "topic", topic, ..] if matches!(*kind, "channel" | "stream") => {
                let sid: u64 = ch.split('-').next()?.parse().ok()?;
                Some(Target::Conv(Conv::Topic(TopicKey::new(sid, &decode_hash(topic)?))))
            }
            ["dm" | "pm-with", who, ..] => {
                let ids: Vec<u64> = who.split('-').next()?.split(',').filter_map(|x| x.parse().ok()).collect();
                Some(Target::Conv(Conv::Dm(puzil_zulip::model::DmKey::new(s.state.me, ids))))
            }
            _ => None,
        }
    }

    /// What the header names: a channel and topic, or a DM partner.
    fn title(&self, cx: &App) -> Title {
        let s = self.session.read(cx);
        let chan = |sid: &StreamId| {
            let name = s.state.streams.get(sid).map(|x| x.name.clone()).unwrap_or_else(|| "?".into());
            let private = s.state.streams.get(sid).is_some_and(|x| x.invite_only);
            let description = s.state.streams.get(sid).map(|x| x.description.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
            (name, private, description)
        };
        match &self.target {
            Target::Conv(Conv::Topic(k)) => {
                let (channel, private, description) = chan(&k.stream_id);
                Title::Topic { channel, private, topic: s.state.topic_display(k), description }
            }
            Target::NewTopic(sid) => {
                let (channel, private, description) = chan(sid);
                Title::Topic { channel, private, topic: "New topic".into(), description }
            }
            Target::Conv(Conv::Dm(k)) => {
                let others: Vec<u64> = k.others(s.state.me).collect();
                let other = others.first().copied().unwrap_or(s.state.me);
                Title::Dm {
                    name: s.state.dm_display(k),
                    user: other,
                    group: others.len() > 1,
                    avatar: s.state.users.get(&other).and_then(|u| u.avatar_url.clone()),
                }
            }
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let votes = self.topic_votes(cx);
        let t = self.theme.clone();
        let title = self.title(cx);
        let s = self.session.read(cx);
        let error = self.conv().and_then(|c| s.conv_data(c)).and_then(|d| d.error.clone());
        // Only when there's nothing to show yet: a refresh behind cached messages is silent.
        let loading = self.conv().and_then(|c| s.conv_data(c)).is_some_and(|d| d.loading)
            && !self.items.borrow().iter().any(|i| matches!(i, Item::Msg { .. }));
        let lead: AnyElement = match &title {
            Title::Topic { channel, private, topic, description } => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .child(icons::channel(*private, 22., t.faint, t.bg))
                .child(div().flex_none().text_color(t.muted).font_weight(FontWeight::SEMIBOLD).child(channel.clone()))
                .child(icons::icon("chevron-right", 16., t.faint))
                .child(
                    div()
                        .min_w_0()
                        .text_color(t.fg_strong)
                        .font_weight(FontWeight::SEMIBOLD)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(topic.clone()),
                )
                // Discord's "· description" after the name.
                .when(!description.is_empty(), |d| {
                    d.child(div().flex_none().mx_1().size(px(4.)).rounded_full().bg(t.faint)).child(
                        div()
                            .min_w_0()
                            .text_size(px(14.))
                            .text_color(t.muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(description.clone()),
                    )
                })
                .into_any_element(),
            Title::Dm { name, user, group, avatar } => {
                let img =
                    if *group { None } else { self.images.update(cx, |i, cx| i.avatar(*user, avatar.as_deref(), cx)) };
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(if *group {
                        icons::icon("users", 22., t.icon).into_any_element()
                    } else {
                        super::avatar::avatar(img, 24., t.user_color(*user), name).into_any_element()
                    })
                    .child(
                        div()
                            .min_w_0()
                            .text_color(t.fg_strong)
                            .font_weight(FontWeight::SEMIBOLD)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(name.clone()),
                    )
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .h(px(48.))
            .px_4()
            .flex_none()
            // Opaque, so the shadow below shows only past its bottom edge.
            .bg(t.bg)
            .border_b_1()
            .border_color(t.border)
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., if t.dark { 0.2 } else { 0.06 }),
                offset: point(px(0.), px(1.)),
                blur_radius: px(2.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(lead)
            .when(loading, |d| d.child(div().flex_none().text_color(t.faint).text_size(px(13.)).child("Loading…")))
            .when_some(error, |d, e| d.child(div().flex_none().text_color(t.danger).text_size(px(13.)).child(e)))
            .child(crate::theme::drag_region(div().id("header-drag").flex_1().h_full()))
            .children(votes)
    }

    /// The top of a conversation's history: Discord's big welcome.
    fn welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme.clone();
        let (badge, heading, sub): (AnyElement, String, String) = match self.title(cx) {
            Title::Topic { channel, private, topic, .. } => (
                div()
                    .size(px(68.))
                    .rounded_full()
                    .bg(t.bg_selected)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icons::channel(private, 40., t.fg_strong, t.bg_selected))
                    .into_any_element(),
                topic.clone(),
                format!("This is the start of the {topic} topic in #{channel}."),
            ),
            Title::Dm { name, user, group, avatar } => {
                let img = if group { None } else { self.images.update(cx, |i, cx| i.avatar(user, avatar.as_deref(), cx)) };
                (
                    super::avatar::avatar(img, 80., t.user_color(user), &name).into_any_element(),
                    name.clone(),
                    format!("This is the beginning of your direct message history with {name}."),
                )
            }
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pt_8()
            .pb_4()
            .child(badge)
            .child(
                div()
                    .pt_2()
                    .font_family(super::rich::DISPLAY_FONT)
                    .text_size(px(26.))
                    .line_height(px(34.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(t.fg_strong)
                    .child(heading),
            )
            .child(div().text_color(t.muted).child(sub))
            .into_any_element()
    }

    fn render_item(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.items.borrow().get(ix).cloned() else {
            return div().into_any_element();
        };
        let t = self.theme.clone();
        match item {
            Item::Top => self.welcome(cx),
            Item::Day(ts) => div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .px_4()
                .pt_6()
                .pb_2()
                .child(div().h(px(1.)).flex_1().bg(t.divider))
                .child(div().px_1().text_size(px(12.)).text_color(t.faint).font_weight(FontWeight::SEMIBOLD).child(fmt::day(ts)))
                .child(div().h(px(1.)).flex_1().bg(t.divider))
                .into_any_element(),
            Item::NewMarker => div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .pl_4()
                .pt_2()
                .child(div().h(px(1.)).flex_1().bg(t.new_marker))
                .child(
                    div()
                        .h(px(13.))
                        .pl_1()
                        .pr(px(6.))
                        .flex()
                        .items_center()
                        .rounded_l(px(3.))
                        .bg(t.new_marker)
                        .text_color(white())
                        .text_size(px(10.))
                        .font_weight(FontWeight::BOLD)
                        .child("NEW"),
                )
                .into_any_element(),
            Item::Pending(local) => self.render_pending(local, cx),
            Item::Gap(secs) => div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .pl(px(72.))
                .pr_4()
                .pt_3()
                .child(div().text_size(px(11.)).text_color(t.faint).child(overlay_ui::describe_gap(secs)))
                .child(div().h(px(1.)).flex_1().bg(t.divider.opacity(0.6)))
                .into_any_element(),
            Item::Msg { id, head } => self.render_msg(id, head, cx),
        }
    }

    fn render_pending(&self, local: u64, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme.clone();
        let s = self.session.read(cx);
        let Some(p) = s.outbox.iter().find(|p| p.local_id == local) else {
            return div().into_any_element();
        };
        let failed = match &p.state {
            PendingState::Failed(e) => Some(e.clone()),
            PendingState::Sending => None,
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .px_4()
            .pl(px(72.))
            .py_0p5()
            .child(div().text_color(t.muted).child(p.content.clone()))
            .child(match failed {
                None => div().text_xs().text_color(t.faint).child("Sending…"),
                Some(e) => {
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .text_xs()
                        .child(div().text_color(t.danger).child(format!("Not sent: {e}")))
                        .child(
                            div()
                                .id(("retry", local as usize))
                                .text_color(t.link)
                                .cursor_pointer()
                                .child("Retry")
                                .on_click(cx.listener(move |this, _, _, cx| this.session.update(cx, |s, cx| s.retry(local, cx)))),
                        )
                        .child(
                            div().id(("discard", local as usize)).text_color(t.link).cursor_pointer().child("Discard").on_click(
                                cx.listener(move |this, _, _, cx| this.session.update(cx, |s, cx| s.discard(local, cx))),
                            ),
                        )
                }
            })
            .into_any_element()
    }

    fn render_msg(&self, id: MessageId, head: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme.clone();
        let (built, avatar_url, sender) = {
            let s = self.session.read(cx);
            let Some(m) = s.state.messages.get(&id) else {
                return div().into_any_element();
            };
            let avatar = s.state.users.get(&m.sender_id).and_then(|u| u.avatar_url.clone()).or_else(|| m.avatar_url.clone());
            (self.built(m, s.state.me, cx), avatar, m.sender_id)
        };
        let avatar = if head { self.images.update(cx, |i, cx| i.avatar(sender, avatar_url.as_deref(), cx)) } else { None };
        // Start loading this message's images; pass along the ones already in memory.
        let images: HashMap<String, Arc<RenderImage>> = built
            .images
            .iter()
            .filter_map(|u| match self.images.update(cx, |i, cx| i.get(u, cx)) {
                ImgState::Ready(img) => Some((u.clone(), img)),
                _ => None,
            })
            .collect();
        let s = self.session.read(cx);
        let Some(m) = s.state.messages.get(&id) else {
            return div().into_any_element();
        };
        let me = s.state.me;
        let unread = s.state.unread.contains_key(&id);
        let mentioned = s.state.mentions.contains(&id);
        let sender = s.state.users.get(&m.sender_id);
        let name = sender.map(|u| u.full_name.clone()).unwrap_or_else(|| m.sender_full_name.clone());
        let is_bot = sender.is_some_and(|u| u.is_bot);
        let mine = m.sender_id == me;
        let edited = m.last_edit_timestamp.is_some();
        let ts = m.timestamp;
        let reactions = reaction_chips(m, me);
        let hovered = self.hovered == Some(id) || self.bar_hovered == Some(id);
        let kb_selected = self.kb_selected == Some(id) && self.list_focused;

        let weak = cx.entity().downgrade();
        let rc = RenderCtx {
            msg: id,
            theme: t.clone(),
            selection: self.selection.clone(),
            on_link: self.link_handler(cx),
            expanded: self.expanded.clone(),
            on_toggle: Rc::new(move |msg, key, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    let mut e = (*this.expanded).clone();
                    if !e.remove(&(msg, key)) {
                        e.insert((msg, key));
                    }
                    this.expanded = Rc::new(e);
                    this.built.borrow_mut().remove(&msg);
                    let ix = this.items.borrow().iter().position(|i| matches!(i, Item::Msg { id, .. } if *id == msg));
                    if let Some(ix) = ix {
                        this.list.remeasure_items(ix..ix + 1);
                    }
                    cx.notify();
                });
            }),
            images,
            on_media: {
                let weak = cx.entity().downgrade();
                Rc::new(move |m: &Media, _, cx| {
                    let m = m.clone();
                    let _ = weak.update(cx, |this, cx| this.open_media(m, cx));
                })
            },
            on_attachment: {
                let weak = cx.entity().downgrade();
                Rc::new(move |name: &str, href: &str, _, cx| {
                    let (name, href) = (name.to_string(), href.to_string());
                    let _ = weak.update(cx, |this, cx| this.open_attachment(name, href, cx));
                })
            },
            downloading: self.downloading.clone(),
            on_badge: self.overlay.as_ref().map(|_| {
                let weak = cx.entity().downgrade();
                Rc::new(move |msg, badge, _: &mut Window, cx: &mut App| {
                    let _ = weak.update(cx, |this, cx| this.on_badge(msg, badge, cx));
                }) as Rc<dyn Fn(u64, usize, &mut Window, &mut App)>
            }),
        };

        let sender_id = m.sender_id;
        let gutter = if head {
            super::avatar::avatar(avatar, 40., t.user_color(m.sender_id), &name)
                .mt_0p5()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_profile(sender_id, e.position, window, cx)
                }))
        } else {
            div()
                .w(px(40.))
                .h(px(22.))
                .text_size(px(11.))
                .text_center()
                .whitespace_nowrap()
                .line_height(px(22.))
                .text_color(if hovered { t.faint } else { transparent_black() })
                .child(fmt::time(ts))
        };

        let _ = mine;
        let info = self.info(m, cx);
        if info.covered
            && let Some(o) = &self.overlay
        {
            o.update(cx, |o, cx| o.want_message(id, cx));
        }
        let actions = hovered.then(|| self.hover_bar(&info, head, cx));
        let chip = built.reply.as_ref().map(|r| {
            self.note_thread_origin(&info, r, &built, cx);
            self.reply_chip(&info, r, cx)
        });
        // A reply heading a group gets Discord's spine from the avatar up to the parent.
        let (chip_above, chip) =
            if head && built.reply.as_ref().is_some_and(|r| !r.is_thread) { (chip, None) } else { (None, chip) };
        let chip_above = chip_above.map(|c| {
            div()
                .flex()
                .flex_row()
                .items_end()
                .pl(px(36.))
                .h(px(22.))
                .pt_1()
                .child(
                    div()
                        .w(px(30.))
                        .h(px(10.))
                        .mb(px(-2.))
                        .flex_none()
                        .border_l_2()
                        .border_t_2()
                        .rounded_tl(px(6.))
                        .border_color(t.quote_bar),
                )
                .child(div().flex_1().min_w_0().pl_1().child(c))
        });
        let meta = self.meta_rows(&info, cx);
        let selection_bar = self.selection_bar(&info, &built, cx);
        let flashing = self.flashing == Some(id);

        let row = div()
            .id(("msg", id as usize))
            .w_full()
            .relative()
            .flex()
            .flex_col()
            .when(head, |d| d.pt(px(17.)))
            .pb_0p5()
            .when(mentioned, |d| d.bg(t.mention_me_bg.opacity(0.35)).border_l_2().border_color(t.mention_me_fg))
            .when(!mentioned && hovered, |d| d.bg(t.bg_hover))
            .when(kb_selected, |d| {
                d.bg(t.bg_selected.opacity(0.45)).child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(t.accent))
            })
            .when(flashing, |d| {
                d.child(super::motion::fade_out(
                    ElementId::named_usize(format!("flash-{id}"), self.flash_count),
                    div().absolute().inset_0().bg(t.passage),
                    1400,
                ))
            })
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| this.open_menu(id, e.position, cx)))
            // A click selects the message, as the arrow keys do (Enter then edits or replies).
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                if this.kb_selected != Some(id) {
                    this.kb_selected = Some(id);
                    cx.notify();
                }
                this.focus.focus(window, cx);
            }))
            .on_hover(cx.listener(move |this, h: &bool, _, cx| {
                let next = if *h {
                    Some(id)
                } else if this.hovered == Some(id) {
                    None
                } else {
                    this.hovered
                };
                if next != this.hovered {
                    this.hovered = next;
                    cx.notify();
                }
            }))
            .children(chip_above)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_4()
                    .px_4()
                    .when(mentioned, |d| d.pl(px(14.)))
                    .child(div().flex_none().child(gutter))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .flex_basis(px(0.))
                            .min_w_0()
                            .overflow_hidden()
                            .when(head, |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_baseline()
                                        .gap_2()
                                        .child(
                                        div()
                                            .text_color(t.fg_strong)
                                            .font_weight(FontWeight::MEDIUM)
                                            .cursor_pointer()
                                            .hover(|s| s.underline())
                                            .child(name)
                                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                                cx.stop_propagation();
                                                this.open_profile(sender_id, e.position, window, cx)
                                            })),
                                    )
                                        .when(is_bot, |d| {
                                            d.child(
                                                div()
                                                    .flex()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap_0p5()
                                                    .h(px(15.))
                                                    .px(px(4.))
                                                    .rounded(px(3.))
                                                    .bg(t.accent)
                                                    .text_color(gpui_kit::white())
                                                    .text_size(px(10.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(icons::icon("check", 10., gpui_kit::white()))
                                                    .child("BOT"),
                                            )
                                        })
                                        .child(div().text_size(px(12.)).text_color(t.faint).child(fmt::datetime(ts))),
                                )
                            })
                            .children(chip)
                            .child(built.render(&rc))
                            .when(edited, |d| {
                                d.child(div().text_size(px(10.)).line_height(px(14.)).text_color(t.faint).child("(edited)"))
                            })
                            .when(!reactions.is_empty(), |d| {
                                d.child(div().flex().flex_row().flex_wrap().gap_1().pt(px(3.)).children(
                                    reactions.into_iter().enumerate().map(|(i, r)| {
                                        let (name, code, kind, mine) = (r.name.clone(), r.code.clone(), r.kind.clone(), r.mine);
                                        let popped = self.reacted.get(&(id, r.code.clone())).is_some_and(|t| t.elapsed().as_millis() < 1500);
                                        let chip = div()
                                            .id(ElementId::named_usize(format!("react-{id}"), i))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap_1p5()
                                            .h(px(26.))
                                            .px(px(7.))
                                            .rounded(px(8.))
                                            .border_1()
                                            .border_color(if r.mine { t.reaction_mine_border } else { t.reaction_bg })
                                            .bg(if r.mine { t.reaction_mine_bg } else { t.reaction_bg })
                                            .hover(|s| s.border_color(if r.mine { t.reaction_mine_border } else { t.faint }))
                                            .cursor_pointer()
                                            .text_size(px(15.))
                                            .child(r.label)
                                            // A lone reaction needs no count.
                                            .when(r.count > 1, |d| {
                                                d.child(
                                                    div()
                                                        .text_size(px(13.))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(if r.mine { t.mention_fg } else { t.muted })
                                                        .child(r.count.to_string()),
                                                )
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.toggle_reaction(id, name.clone(), code.clone(), kind.clone(), mine, cx)
                                            }));
                                        if popped {
                                            super::motion::arrive(ElementId::named_usize(format!("pop-{id}"), i), chip, 6.).into_any_element()
                                        } else {
                                            chip.into_any_element()
                                        }
                                    }),
                                )
                                // Discord's add-reaction button closes the row.
                                .child(
                                    div()
                                        .id(("add-reaction", id as usize))
                                        .h(px(26.))
                                        .w(px(32.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(8.))
                                        .bg(t.reaction_bg)
                                        .border_1()
                                        .border_color(t.reaction_bg)
                                        .hover(|s| s.border_color(t.faint))
                                        .cursor_pointer()
                                        .child(icons::icon("face-slightly-smiling-plus", 18., t.icon))
                                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.open_picker(id, e.position, cx)
                                        })),
                                ))
                            })
                            .children(meta),
                    ),
            )
            .children(actions)
            .children(selection_bar)
            .when(unread, |d| {
                let painted = self.painted.clone();
                d.child(canvas(|_, _, _| {}, move |b, _, _, _| painted.borrow_mut().push((id, b))).absolute().inset_0())
            });
        // A message arriving while we watch rises in; one already there just appears.
        if self.arrived.get(&id).is_some_and(|t| t.elapsed().as_millis() < 400) {
            super::motion::arrive(("arrive", id as usize), row, 6.).into_any_element()
        } else {
            row.into_any_element()
        }
    }
}

struct Chip {
    label: String,
    name: String,
    code: String,
    kind: String,
    count: usize,
    mine: bool,
}

fn reaction_chips(m: &Message, me: u64) -> Vec<Chip> {
    let mut out: Vec<Chip> = Vec::new();
    for r in &m.reactions {
        match out.iter_mut().find(|c| c.code == r.emoji_code && c.kind == r.reaction_type) {
            Some(c) => {
                c.count += 1;
                c.mine |= r.user_id == me;
            }
            None => out.push(Chip {
                label: match r.reaction_type.as_str() {
                    "unicode_emoji" => {
                        puzil_render::ast::emoji_glyph(&r.emoji_code).unwrap_or_else(|| format!(":{}:", r.emoji_name))
                    }
                    _ => format!(":{}:", r.emoji_name),
                },
                name: r.emoji_name.clone(),
                code: r.emoji_code.clone(),
                kind: r.reaction_type.clone(),
                count: 1,
                mine: r.user_id == me,
            }),
        }
    }
    out
}

/// A file name safe to write: no path separators or leading dots.
fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect();
    let s = s.trim_start_matches('.').to_string();
    if s.is_empty() { "file".into() } else { s }
}

/// Zulip's URL fragment encoding: `encodeURIComponent` with `%` replaced by `.`.
fn decode_hash(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'.'
            && i + 2 < b.len() + 0
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl Conversation {
    /// Benchmark hooks: how many messages are laid out, and scrolling by hand.
    pub fn bench_message_count(&self) -> usize {
        self.items.borrow().iter().filter(|i| matches!(i, Item::Msg { .. })).count()
    }

    pub fn bench_scroll_top(&self) -> (usize, f32) {
        let o = self.list.logical_scroll_top();
        (o.item_ix, f32::from(o.offset_in_item))
    }

    /// "Ada is typing…" under the messages of a DM, in the list's bottom padding (taller in
    /// DMs) so the list doesn't move when it comes and goes.
    fn typing_line(&self, cx: &App) -> Option<Div> {
        let c = self.conv()?;
        let s = self.session.read(cx);
        let who = s.typing_in(c);
        let text = match who.as_slice() {
            [] => return None,
            [a] => format!("{} is typing…", s.state.user_name(*a)),
            [a, b] => format!("{} and {} are typing…", s.state.user_name(*a), s.state.user_name(*b)),
            _ => "Several people are typing…".to_string(),
        };
        Some(
            div()
                .absolute()
                .bottom(px(3.))
                .left(px(72.))
                .right_4()
                .h(px(16.))
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(self.theme.muted)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(text),
        )
    }

    pub fn bench_scroll_to_end(&self) {
        self.list.scroll_to_end();
    }
}

impl Render for Conversation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _timer = crate::bench::Timer::start("conversation_render_us");
        let menu = self.render_menu(window, cx);
        let profile = self.render_profile(cx);
        let picker = self.render_picker(cx);
        self.list_focused = self.focus.is_focused(window);
        let t = self.theme.clone();
        let this = cx.entity();
        // Mark-as-read probe: after layout, any unread message whose box is on screen
        // while the window is active counts as read.
        let probe = {
            let painted = self.painted.clone();
            let list = self.list.clone();
            let session = self.session.clone();
            let auto_read = crate::settings::effective(cx, Some(&self.session.read(cx).account.realm.clone())).auto_read && !self.read_paused;
            canvas(
                |_, _, _| {},
                move |_, _, window, cx| {
                    // Unread rows record their bounds as they paint (GPUI's `bounds_for_item`
                    // is unusable on a bottom-anchored list); this runs after them.
                    let rows = std::mem::take(&mut *painted.borrow_mut());
                    if !auto_read || rows.is_empty() || !window.is_window_active() {
                        return;
                    }
                    let view = list.viewport_bounds();
                    let seen: Vec<MessageId> = rows
                        .into_iter()
                        .filter(|(_, b)| b.bottom() > view.top() && b.top() < view.bottom())
                        .map(|(id, _)| id)
                        .collect();
                    if !seen.is_empty() {
                        cx.defer(move |cx| session.update(cx, |s, cx| s.mark_read(seen, cx)));
                    }
                },
            )
            .absolute()
            .size_0()
        };
        let _ = window;
        let typing = self.typing_line(cx);
        let dm = matches!(self.conv(), Some(Conv::Dm(_)));
        div()
            .key_context("Conversation")
            .on_action(cx.listener(Self::copy_selection))
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .text_size(px(super::rich::FONT_SIZE))
            .child(self.header(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .map(|d| if dm { d.pb(px(22.)) } else { d.pb_3() })
                    .key_context("MessageList")
                    .track_focus(&self.focus)
                    .on_action(cx.listener(Self::select_prev))
                    .on_action(cx.listener(Self::select_next))
                    .on_action(cx.listener(Self::edit_selected))
                    .on_action(cx.listener(Self::delete_selected))
                    .on_action(cx.listener(Self::clear_selection))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus.focus(window, cx)))
                    .child(
                        list(self.list.clone(), move |ix, _, cx| {
                            let el = this.update(cx, |this, cx| this.render_item(ix, cx));
                            // PUZIL_DEBUG_ITEMS=1 outlines every list item (layout debugging).
                            static DEBUG: std::sync::LazyLock<bool> =
                                std::sync::LazyLock::new(|| std::env::var_os("PUZIL_DEBUG_ITEMS").is_some());
                            if *DEBUG {
                                div().w_full().border_1().border_color(hsla(0., 1., 0.5, 0.6)).child(el).into_any_element()
                            } else {
                                el
                            }
                        })
                        .size_full(),
                    )
                    .children(typing),
            )
            .child(self.composer.clone())
            .child(probe)
            .children(menu)
            .children(profile)
            .children(picker)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn decodes_zulip_hash() {
        assert_eq!(super::decode_hash("a.20b.2Ec").as_deref(), Some("a b.c"));
        assert_eq!(super::decode_hash("caf.C3.A9").as_deref(), Some("café"));
        assert_eq!(super::decode_hash("v1.2").as_deref(), Some("v1.2"));
    }
}
