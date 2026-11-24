//! The quick switcher (⌘K): one fuzzy list over channels, topics and people. Picking a
//! channel narrows the list to its topics (`#channel › …`), since a topic is what opens.
//! Tab on a person adds them as a chip, for a direct message with several people.

use std::{collections::HashMap, rc::Rc};

use gpui_kit::{
    component::input::{Backspace, Input, InputEvent, InputState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::{
    model::{Conv, DmKey, TopicKey},
    types::StreamId,
};

use super::{
    composer::{ComposerDown, ComposerEscape, ComposerTab, ComposerUp},
    conversation::Target,
};
use crate::{icons, session::Session, theme::Theme};

pub enum SwitcherEvent {
    Open(Target),
    Dismiss,
}

#[derive(Clone)]
enum Kind {
    /// Narrows the list to this channel's topics.
    Channel { private: bool },
    Topic { key: TopicKey, channel: String },
    Person { user: u64, avatar: Option<String> },
    Group { key: DmKey },
}

#[derive(Clone)]
struct Item {
    label: String,
    kind: Kind,
    unread: usize,
    score: i64,
}

pub struct Switcher {
    session: Entity<Session>,
    theme: Rc<Theme>,
    input: Entity<InputState>,
    items: Vec<Item>,
    sel: usize,
    scroll: ScrollHandle,
    /// People picked with Tab, in order: Enter opens a DM with all of them.
    picked: Vec<u64>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SwitcherEvent> for Switcher {}

/// Fuzzy match of `q` (lowercase) in `text`: a prefix, a word start, a substring, or the
/// initials of words ("ab" for Ada Byron); scattered letters don't count. Higher is better:
/// a prefix beats a word start beats the rest, runs of consecutive characters count extra,
/// and long names lose a little.
fn fuzzy(q: &str, text: &str) -> Option<i64> {
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let qs: Vec<char> = q.chars().collect();
    let joined: String = t.iter().collect();
    let mut score = if joined.starts_with(q) {
        1000
    } else if joined.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(q)) {
        700
    } else if joined.contains(q) {
        400
    } else {
        0
    };
    if score == 0 {
        // Initials: each query character starts a word, in order.
        let initials: String = t.iter().enumerate().filter(|(i, c)| c.is_alphanumeric() && (*i == 0 || !t[i - 1].is_alphanumeric())).map(|(_, c)| *c).collect();
        let mut it = initials.chars();
        if !qs.iter().all(|q| it.any(|c| c == *q)) {
            return None;
        }
        score = 300;
    }
    let (mut qi, mut run, mut last) = (0, 0i64, usize::MAX);
    for (i, c) in t.iter().enumerate() {
        if qi < qs.len() && *c == qs[qi] {
            run = if last != usize::MAX && i == last + 1 { run + 1 } else { 0 };
            let word_start = i == 0 || !t[i - 1].is_alphanumeric();
            score += 10 + run * 15 + if word_start { 25 } else { 0 };
            last = i;
            qi += 1;
        }
    }
    Some(score - t.len() as i64)
}

impl Switcher {
    /// `people`: already picked (Add people in a DM starts with the people in it).
    pub fn new(session: Entity<Session>, theme: Rc<Theme>, people: Vec<u64>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(Self::placeholder(!people.is_empty())));
        let subs = vec![
            cx.subscribe_in(&input, window, |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    this.sel = 0;
                    this.refresh(cx);
                }
                InputEvent::PressEnter { .. } => this.accept(window, cx),
                _ => {}
            }),
            // Topic lists arrive after a channel is picked.
            cx.observe(&session, |this, _, cx| this.refresh(cx)),
        ];
        input.update(cx, |i, cx| i.focus(window, cx));
        let mut this = Self { session, theme, input, items: vec![], sel: 0, scroll: ScrollHandle::new(), picked: people, _subs: subs };
        this.refresh(cx);
        this
    }

    fn placeholder(picking: bool) -> &'static str {
        if picking { "Add someone else, or Enter to open" } else { "Where would you like to go?" }
    }

    /// Tab: add the highlighted person (or a group's people) to the picked ones.
    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(self.sel).cloned() else { return };
        let me = self.session.read(cx).state.me;
        match item.kind {
            Kind::Person { user, .. } => self.pick([user]),
            Kind::Group { key } => self.pick(key.others(me).collect::<Vec<_>>()),
            // A channel: Tab drills into its topics, like Enter. A topic: nothing to add.
            Kind::Channel { .. } => return self.accept(window, cx),
            Kind::Topic { .. } => return,
        }
        self.reset_query(window, cx);
    }

    fn pick(&mut self, users: impl IntoIterator<Item = u64>) {
        for u in users {
            if !self.picked.contains(&u) {
                self.picked.push(u);
            }
        }
    }

    fn unpick(&mut self, user: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.picked.retain(|u| *u != user);
        self.reset_query(window, cx);
    }

    /// An empty field (and its placeholder) for the next person, and the list for it.
    fn reset_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picking = !self.picked.is_empty();
        self.input.update(cx, |i, cx| {
            i.set_value("", window, cx);
            i.set_placeholder(Self::placeholder(picking), window, cx);
            i.focus(window, cx);
        });
        self.sel = 0;
        self.refresh(cx);
    }

    fn open_picked(&mut self, cx: &mut Context<Self>) {
        let me = self.session.read(cx).state.me;
        cx.emit(SwitcherEvent::Open(Target::Conv(Conv::Dm(DmKey::new(me, self.picked.clone())))));
    }

    /// `#channel › rest`: the channel the query is narrowed to, and the rest of the query.
    fn narrowed(&self, query: &str, cx: &App) -> Option<(StreamId, String)> {
        let rest = query.strip_prefix('#')?;
        let (chan, topic) = rest.split_once('›').or_else(|| rest.split_once('>'))?;
        let chan = chan.trim().to_lowercase();
        let s = self.session.read(cx);
        let sid = s.state.streams.values().find(|st| st.name.to_lowercase() == chan)?.stream_id;
        Some((sid, topic.trim().to_lowercase()))
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        let picking = !self.picked.is_empty();
        let narrowed = if picking { None } else { self.narrowed(&query, cx) };
        if let Some((sid, _)) = &narrowed {
            let sid = *sid;
            self.session.update(cx, |s, cx| s.load_topics(sid, cx));
        }
        let s = self.session.read(cx);
        let mut unread: HashMap<Conv, usize> = HashMap::new();
        for c in s.state.unread.values() {
            *unread.entry(c.clone()).or_default() += 1;
        }
        let topic_unread = |k: &TopicKey| unread.get(&Conv::Topic(k.clone())).copied().unwrap_or(0);
        let mut items: Vec<Item> = Vec::new();

        // Topics: what the session has listed, plus every topic name the store knows.
        let mut topics: HashMap<TopicKey, (String, u64)> = HashMap::new();
        for (k, name) in &s.state.topic_names {
            topics.insert(k.clone(), (name.clone(), 0));
        }
        for (sid, list) in &s.topics {
            for t in list {
                topics.insert(TopicKey::new(*sid, &t.name), (t.name.clone(), t.max_id));
            }
        }
        let chan_name = |sid: &StreamId| s.state.streams.get(sid).map(|st| st.name.clone()).unwrap_or_default();

        if let Some((sid, tq)) = narrowed {
            for (k, (name, max_id)) in topics.iter().filter(|(k, _)| k.stream_id == sid) {
                let display = if name.is_empty() { s.state.empty_topic_name.clone() } else { name.clone() };
                if let Some(score) = fuzzy(&tq, &display) {
                    let u = topic_unread(k);
                    // Recent topics first when nothing is typed.
                    let recency = if tq.is_empty() { (*max_id / 1000) as i64 } else { 0 };
                    items.push(Item {
                        label: display,
                        kind: Kind::Topic { key: k.clone(), channel: chan_name(&sid) },
                        unread: u,
                        score: score + recency + (u > 0) as i64 * 50,
                    });
                }
            }
        } else {
            let q = query.trim().trim_start_matches(['#', '@']).to_lowercase();
            let only_channels = query.starts_with('#') && !picking;
            // With people picked, only more people.
            let only_people = query.starts_with('@') || picking;
            if !only_people {
                for st in s.state.streams.values().filter(|st| !st.is_archived) {
                    if let Some(score) = fuzzy(&q, &st.name) {
                        let subscribed = s.state.subs.contains_key(&st.stream_id);
                        let u: usize = unread.iter().filter(|(c, _)| matches!(c, Conv::Topic(k) if k.stream_id == st.stream_id)).map(|(_, n)| n).sum();
                        items.push(Item {
                            label: st.name.clone(),
                            kind: Kind::Channel { private: st.invite_only },
                            unread: u,
                            score: score + if subscribed { 200 } else { 0 } + (u > 0) as i64 * 50,
                        });
                    }
                }
                if !only_channels && !q.is_empty() {
                    for (k, (name, _)) in &topics {
                        if !s.state.subs.contains_key(&k.stream_id) {
                            continue;
                        }
                        let display = if name.is_empty() { s.state.empty_topic_name.clone() } else { name.clone() };
                        if let Some(score) = fuzzy(&q, &display) {
                            let u = topic_unread(k);
                            items.push(Item {
                                label: display,
                                kind: Kind::Topic { key: k.clone(), channel: chan_name(&k.stream_id) },
                                unread: u,
                                score: score - 100 + (u > 0) as i64 * 50,
                            });
                        }
                    }
                }
            }
            if !only_channels {
                for u in s.state.users.values().filter(|u| !u.deactivated && u.user_id != s.state.me && !self.picked.contains(&u.user_id)) {
                    if let Some(score) = fuzzy(&q, &u.full_name) {
                        let key = DmKey::new(s.state.me, [u.user_id]);
                        let n = unread.get(&Conv::Dm(key.clone())).copied().unwrap_or(0);
                        let recent = s.state.recent_dms.contains_key(&key);
                        items.push(Item {
                            label: u.full_name.clone(),
                            kind: Kind::Person { user: u.user_id, avatar: u.avatar_url.clone() },
                            unread: n,
                            // Bots sink unless they're someone we talk to.
                            score: score + if recent { 300 } else if u.is_bot { -150 } else { 0 } + (n > 0) as i64 * 50,
                        });
                    }
                }
                for k in s.state.recent_dms.keys().filter(|k| !picking && k.others(s.state.me).count() > 1) {
                    let name = s.state.dm_display(k);
                    if let Some(score) = fuzzy(&q, &name) {
                        let n = unread.get(&Conv::Dm(k.clone())).copied().unwrap_or(0);
                        items.push(Item { label: name, kind: Kind::Group { key: k.clone() }, unread: n, score: score + 250 + (n > 0) as i64 * 50 });
                    }
                }
            }
            // Nothing typed: what's unread, then recent direct messages, then channels.
            if q.is_empty() {
                items.retain(|i| i.unread > 0 || matches!(i.kind, Kind::Channel { .. }) || i.score >= 250);
            }
        }
        items.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)));
        items.truncate(60);
        self.items = items;
        self.sel = self.sel.min(self.items.len().saturating_sub(1));
        cx.notify();
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.picked.is_empty() {
            // Enter adds whoever is highlighted (if anything is typed) and opens.
            if !self.input.read(cx).value().trim().is_empty()
                && let Some(Kind::Person { user, .. }) = self.items.get(self.sel).map(|i| i.kind.clone())
            {
                self.pick([user]);
            }
            return self.open_picked(cx);
        }
        let Some(item) = self.items.get(self.sel).cloned() else { return };
        let me = self.session.read(cx).state.me;
        match item.kind {
            Kind::Channel { .. } => {
                let text = format!("#{} › ", item.label);
                self.input.update(cx, |i, cx| i.set_value(text, window, cx));
                self.sel = 0;
                self.refresh(cx);
            }
            Kind::Topic { key, .. } => cx.emit(SwitcherEvent::Open(Target::Conv(Conv::Topic(key)))),
            Kind::Person { user, .. } => cx.emit(SwitcherEvent::Open(Target::Conv(Conv::Dm(DmKey::new(me, [user]))))),
            Kind::Group { key } => cx.emit(SwitcherEvent::Open(Target::Conv(Conv::Dm(key)))),
        }
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as isize;
        self.sel = ((self.sel as isize + by).rem_euclid(n)) as usize;
        self.scroll.scroll_to_item(self.sel);
        cx.notify();
    }
}

impl Render for Switcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let images = self.session.read(cx).images.clone();
        let rows: Vec<AnyElement> = self
            .items
            .clone()
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let selected = i == self.sel;
                let (lead, detail): (AnyElement, Option<String>) = match &item.kind {
                    Kind::Channel { private, .. } => (icons::channel(*private, 18., t.faint, if selected { t.bg_selected } else { t.popover }).into_any_element(), None),
                    Kind::Topic { channel, .. } => (icons::icon("messages-square", 18., t.faint).into_any_element(), Some(format!("#{channel}"))),
                    Kind::Person { user, avatar } => {
                        let img = images.update(cx, |im, cx| im.avatar(*user, avatar.as_deref(), cx));
                        (super::avatar::avatar(img, 22., t.user_color(*user), &item.label).into_any_element(), None)
                    }
                    Kind::Group { .. } => (icons::icon("users", 18., t.faint).into_any_element(), Some("Group".into())),
                };
                div()
                    .id(("switch", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(36.))
                    .px_2()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.bg_selected))
                    .hover(|d| d.bg(t.bg_selected.opacity(0.6)))
                    .child(div().w(px(22.)).flex().justify_center().child(lead))
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(t.fg_strong).font_weight(FontWeight::MEDIUM).child(item.label.clone()))
                    .children(detail.map(|d| div().flex_none().text_size(px(13.)).text_color(t.faint).child(d)))
                    .child(div().flex_1())
                    .when(item.unread > 0, |d| {
                        d.child(
                            div()
                                .h(px(16.))
                                .min_w(px(16.))
                                .px(px(5.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(t.mention_badge)
                                .text_color(white())
                                .text_size(px(11.))
                                .font_weight(FontWeight::BOLD)
                                .child(item.unread.to_string()),
                        )
                    })
                    .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                        this.sel = i;
                        if e.modifiers().shift { this.add(window, cx) } else { this.accept(window, cx) }
                    }))
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        let picking = !self.picked.is_empty();
        // The picked people, as chips before the field; × takes one out.
        let chips: Vec<AnyElement> = {
            let s = self.session.read(cx);
            self.picked
                .iter()
                .map(|u| {
                    let user = s.state.users.get(u);
                    let name = user.map(|u| u.full_name.clone()).unwrap_or_else(|| "Someone".into());
                    let avatar = user.and_then(|u| u.avatar_url.clone());
                    (*u, name, avatar)
                })
                .collect::<Vec<_>>()
        }
        .into_iter()
        .map(|(u, name, avatar)| {
            let img = images.update(cx, |im, cx| im.avatar(u, avatar.as_deref(), cx));
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .h(px(26.))
                .pl(px(3.))
                .pr(px(4.))
                .rounded_full()
                .bg(t.bg_selected)
                .text_size(px(14.))
                .text_color(t.fg_strong)
                .child(super::avatar::avatar(img, 20., t.user_color(u), &name))
                .child(name)
                .child(
                    div()
                        .id(("unpick", u as usize))
                        .cursor_pointer()
                        .rounded_full()
                        .hover(|d| d.bg(t.bg_hover))
                        .child(icons::icon("x", 14., t.icon))
                        .on_click(cx.listener(move |this, _, window, cx| this.unpick(u, window, cx))),
                )
                .into_any_element()
        })
        .collect();
        let hint = if picking {
            "Tab adds a person · Enter opens the conversation · ⌫ removes the last"
        } else {
            "↑↓ to move · Enter to open · Tab adds a person to a group · # channels · @ people"
        };
        div()
            .w(px(560.))
            .max_h(px(440.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(t.popover)
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            .p_4()
            .gap_3()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(|this, _: &ComposerUp, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &ComposerDown, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &ComposerTab, window, cx| this.add(window, cx)))
            .on_action(cx.listener(|_, _: &ComposerEscape, _, cx| cx.emit(SwitcherEvent::Dismiss)))
            // Backspace in an empty field takes out the last person picked.
            .capture_action(cx.listener(|this, _: &Backspace, window, cx| {
                if !this.picked.is_empty() && this.input.read(cx).value().is_empty() {
                    cx.stop_propagation();
                    this.picked.pop();
                    this.reset_query(window, cx);
                }
            }))
            .child(
                div()
                    .min_h(px(44.))
                    .px_2()
                    .py(px(7.))
                    .rounded(px(6.))
                    .bg(t.rail)
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1p5()
                    .text_size(px(17.))
                    .children(chips)
                    .child(div().flex_1().min_w(px(160.)).child(Input::new(&self.input).appearance(false))),
            )
            .child(
                div()
                    .id("switch-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .when(empty, |d| d.child(div().py_6().text_center().text_color(t.muted).child("Nothing matches.")))
                    .children(rows),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(t.faint)
                    .child(hint),
            )
    }
}
