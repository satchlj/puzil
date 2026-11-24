//! Server rail, direct messages, and the channel list with nested topic trees.
//!
//! A topic named `Parent / Child` sits under `Parent` (Iliad's sub-topic convention). A
//! parent that has no messages of its own is shown as a plain folder.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use gpui_kit::{prelude::FluentBuilder as _, *};
use puzil_zulip::{
    model::{Conv, DmKey, TopicKey},
    types::{MessageId, StreamId},
};

use super::{avatar, conversation::Target, sidebar_menu::SideMenu};
use crate::{
    icons,
    session::{Session, Status, Updated},
    theme::Theme,
};

pub enum SidebarEvent {
    Open(Target),
    /// Show every channel, to join or leave some.
    Browse,
    /// Open Settings.
    Settings,
}


/// What the sidebar remembers between launches, per account.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Saved {
    /// Channels whose topics are showing.
    expanded: Vec<StreamId>,
    /// Channels showing all topics, not only the active ones.
    show_all: Vec<StreamId>,
    open_folders: Vec<(StreamId, String)>,
    collapsed_groups: Vec<String>,
}
const SIDEBAR_W: f32 = 240.;

#[derive(Default, Clone, Copy)]
struct Count {
    unread: usize,
    mentions: usize,
}

impl Count {
    fn add(&mut self, o: Count) {
        self.unread += o.unread;
        self.mentions += o.mentions;
    }
}

/// How a channel's topics are ordered (Overlay's sort control). Vote orders apply to the
/// channels the Overlay service covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Recent,
    Hot,
    Top,
    Bottom,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Sort::Recent => Sort::Hot,
            Sort::Hot => Sort::Top,
            Sort::Top => Sort::Bottom,
            Sort::Bottom => Sort::Recent,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Sort::Recent => "Recent",
            Sort::Hot => "Hot",
            Sort::Top => "Most upvoted",
            Sort::Bottom => "Least upvoted",
        }
    }
}

/// Unread counts per conversation and per channel.
type Counts = (HashMap<Conv, Count>, HashMap<StreamId, Count>);

pub struct Sidebar {
    /// The unread counts, kept until the session next changes: counting walks every unread
    /// message (tens of thousands with a Zulip backlog), and the sidebar also redraws for
    /// hovers and images.
    counts: Option<Rc<Counts>>,
    pub(super) session: Entity<Session>,
    sort: Sort,
    pub(super) theme: Rc<Theme>,
    pub selected: Option<Target>,
    expanded: HashSet<StreamId>,
    /// Expanded topic folders: (stream, lowercased parent path).
    open_folders: HashSet<(StreamId, String)>,
    show_all: HashSet<StreamId>,
    all_dms: bool,
    /// Whether the account card's panel is open.
    pub(super) account_panel: bool,
    /// When a press outside closed the panel, so the click it began on the card doesn't
    /// reopen it.
    pub(super) panel_closed_at: Option<std::time::Instant>,
    /// The last thing expanded and when, so its newly shown rows can settle in.
    reveal: Option<(String, std::time::Instant)>,
    /// When the server rail was last toggled (the sidebar widens or narrows smoothly).
    rail_toggled: Option<std::time::Instant>,
    /// The open right-click menu and where it was opened.
    pub(super) menu: Option<(super::sidebar_menu::SideMenu, Point<Pixels>)>,
    /// Where `Saved` lives, and what was last written there.
    saved_path: std::path::PathBuf,
    last_saved: Option<Saved>,
    /// Collapsed channel groups (pinned, a folder, or the rest), by key.
    collapsed_groups: HashSet<String>,
    /// The server rail; hidden until there's more than one organization to switch between.
    rail_open: bool,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

/// One topic-tree node.
#[derive(Default)]
struct Node {
    /// The real topic at this path, if there is one: (display name, key).
    topic: Option<(String, TopicKey)>,
    label: String,
    path: String,
    max_id: MessageId,
    count: Count,
    children: Vec<Node>,
}

impl Sidebar {
    /// The sidebar's width while it can be drawn from the previous frame's copy (see
    /// [`super::workspace`]): `None` while the rail slides in or out, which animates
    /// every frame.
    pub fn cached_width(&self) -> Option<f32> {
        let sliding = self.rail_toggled.is_some_and(|t| t.elapsed().as_millis() < 260);
        (!sliding).then_some(if self.rail_open { 72. + SIDEBAR_W } else { SIDEBAR_W })
    }

    pub fn new(session: Entity<Session>, theme: Rc<Theme>, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&session, |_, _, up: &Rc<Updated>, cx| {
            if up.sidebar {
                cx.notify();
            }
        });
        let images = session.read(cx).images.clone();
        let mut subs = vec![
            sub,
            cx.observe(&session, |this: &mut Self, _, _| this.counts = None),
            cx.observe(&images, |_, _, cx| cx.notify()),
            cx.observe(&crate::settings::handle(cx), |_, _, cx| cx.notify()),
            cx.observe(&crate::servers::handle(cx), |_, _, cx| cx.notify()),
        ];
        if let Some(o) = session.read(cx).overlay.clone() {
            subs.push(cx.observe(&o, |_, _, cx| cx.notify()));
        }
        let saved_path = crate::accounts::store_path(&session.read(cx).account).with_extension("sidebar.json");
        let saved: Saved = std::fs::read_to_string(&saved_path).ok().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
        for sid in &saved.expanded {
            session.update(cx, |s, cx| s.load_topics(*sid, cx));
        }
        Self {
            counts: None,
            session,
            sort: Sort::Recent,
            theme,
            selected: None,
            expanded: saved.expanded.iter().copied().collect(),
            open_folders: saved.open_folders.iter().cloned().collect(),
            show_all: saved.show_all.iter().copied().collect(),
            all_dms: false,
            account_panel: false,
            panel_closed_at: None,
            reveal: None,
            rail_toggled: None,
            menu: None,
            saved_path,
            last_saved: Some(saved.clone()),
            collapsed_groups: saved.collapsed_groups.iter().cloned().collect(),
            rail_open: false,
            scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    fn note_reveal(&mut self, key: String) {
        self.reveal = Some((key, std::time::Instant::now()));
    }

    fn revealing(&self, key: &str) -> bool {
        self.reveal.as_ref().is_some_and(|(k, t)| k == key && t.elapsed().as_millis() < 400)
    }

    /// Lets freshly revealed rows settle in one after another (`first` numbers the first
    /// of them within the reveal, for the stagger).
    fn stagger(rows: &mut [AnyElement], key: &str, first: usize) {
        for (k, slot) in rows.iter_mut().enumerate() {
            let el = std::mem::replace(slot, div().into_any_element());
            let n = first + k;
            let delay = (n.min(14) * 12) as u64;
            *slot = super::motion::arrive_after(SharedString::from(format!("reveal-{key}-{n}")), div().child(el), 4., delay)
                .into_any_element();
        }
    }

    /// Writes the remembered state if it changed since the last write.
    fn persist(&mut self) {
        let mut now = Saved {
            expanded: self.expanded.iter().copied().collect(),
            show_all: self.show_all.iter().copied().collect(),
            open_folders: self.open_folders.iter().cloned().collect(),
            collapsed_groups: self.collapsed_groups.iter().cloned().collect(),
        };
        now.expanded.sort_unstable();
        now.show_all.sort_unstable();
        now.open_folders.sort();
        now.collapsed_groups.sort();
        if self.last_saved.as_ref() == Some(&now) {
            return;
        }
        match serde_json::to_string(&now) {
            Ok(j) => {
                if let Err(e) = std::fs::write(&self.saved_path, j) {
                    tracing::warn!("saving sidebar state: {e}");
                }
            }
            Err(e) => tracing::warn!("saving sidebar state: {e}"),
        }
        self.last_saved = Some(now);
    }

    pub fn set_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Selects `t` and makes sure its channel is expanded.
    pub fn select(&mut self, t: Target, cx: &mut Context<Self>) {
        if let Target::Conv(Conv::Topic(k)) = &t {
            self.expand(k.stream_id, cx);
            let name = self.session.read(cx).state.topic_display(k).to_lowercase();
            let mut path = String::new();
            for part in name.split(" / ").collect::<Vec<_>>().iter().rev().skip(1).rev() {
                if !path.is_empty() {
                    path.push_str(" / ");
                }
                path.push_str(part);
                self.open_folders.insert((k.stream_id, path.clone()));
            }
        }
        self.selected = Some(t);
        cx.notify();
    }

    fn expand(&mut self, sid: StreamId, cx: &mut Context<Self>) {
        if self.expanded.insert(sid) {
            self.session.update(cx, |s, cx| s.load_topics(sid, cx));
        }
    }

    fn open(&mut self, t: Target, cx: &mut Context<Self>) {
        self.selected = Some(t.clone());
        cx.emit(SidebarEvent::Open(t));
        cx.notify();
    }

    fn counts(s: &Session) -> Counts {
        let mut conv: HashMap<Conv, Count> = HashMap::new();
        let mut stream: HashMap<StreamId, Count> = HashMap::new();
        for (id, c) in &s.state.unread {
            if !s.state.counts_unread(c) {
                continue;
            }
            let n = Count { unread: 1, mentions: s.state.mentions.contains(id) as usize };
            conv.entry(c.clone()).or_default().add(n);
            if let Conv::Topic(k) = c {
                stream.entry(k.stream_id).or_default().add(n);
            }
        }
        (conv, stream)
    }

    /// Sort values of a covered channel's topics under a vote order (larger first), loading
    /// topic anchors, scores and latest-message times as needed (overlay.js `sortValue`).
    fn sort_values(&self, sid: StreamId, cx: &mut Context<Self>) -> Option<HashMap<TopicKey, f64>> {
        if self.sort == Sort::Recent {
            return None;
        }
        let overlay = self.session.read(cx).overlay.clone()?;
        let (topics, invite_only) = {
            let s = self.session.read(cx);
            let t: Vec<(TopicKey, String, MessageId)> =
                s.topics.get(&sid)?.iter().map(|t| (TopicKey::new(sid, &t.name), t.name.clone(), t.max_id)).collect();
            (t, s.state.streams.get(&sid).map(|st| st.invite_only))
        };
        if !overlay.read(cx).covers(sid, invite_only) {
            return None;
        }
        let now = chrono::Utc::now().timestamp();
        let mut out = HashMap::new();
        let session = self.session.clone();
        for (key, name, max_id) in topics {
            let score = overlay.update(cx, |o, cx| {
                o.topic_anchor(&key, &name, cx).and_then(|a| o.topic_scores.get(&a.id).copied()).map(|s| s.score).unwrap_or(0)
            });
            let v = match self.sort {
                Sort::Top => score as f64,
                Sort::Bottom => -(score as f64),
                _ => {
                    let hours = session
                        .update(cx, |s, cx| s.message_time(max_id, cx))
                        .map(|t| (now - t) as f64 / 3600.)
                        .unwrap_or(24. * 30.);
                    puzil_overlay::hot(score, hours)
                }
            };
            out.insert(key, v);
        }
        Some(out)
    }

    fn tree(
        &self,
        s: &Session,
        sid: StreamId,
        counts: &HashMap<Conv, Count>,
        values: Option<&HashMap<TopicKey, f64>>,
    ) -> Vec<Node> {
        // Topics: the server's list, plus any we know of with unreads or messages.
        let mut topics: BTreeMap<String, (String, MessageId)> = BTreeMap::new();
        for t in s.topics.get(&sid).into_iter().flatten() {
            topics.insert(t.name.to_lowercase(), (t.name.clone(), t.max_id));
        }
        for (c, _) in counts.iter() {
            if let Conv::Topic(k) = c
                && k.stream_id == sid
            {
                topics.entry(k.topic.clone()).or_insert_with(|| (s.state.topic_display(k), 0));
            }
        }
        let mut root = Node::default();
        for (lc, (name, max_id)) in topics {
            let key = TopicKey { stream_id: sid, topic: lc };
            let count = counts.get(&Conv::Topic(key.clone())).copied().unwrap_or_default();
            let display = if name.is_empty() { s.state.empty_topic_name.clone() } else { name };
            let parts: Vec<&str> = display.split(" / ").collect();
            let mut node = &mut root;
            let mut path = String::new();
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    path.push_str(" / ");
                }
                path.push_str(&part.to_lowercase());
                let ix = match node.children.iter().position(|c| c.path == path) {
                    Some(ix) => ix,
                    None => {
                        node.children.push(Node { label: part.to_string(), path: path.clone(), ..Default::default() });
                        node.children.len() - 1
                    }
                };
                node = &mut node.children[ix];
                node.max_id = node.max_id.max(max_id);
                node.count.add(count);
            }
            node.topic = Some((display.clone(), key));
        }
        // A folder ranks by its best topic.
        fn value(n: &Node, values: &HashMap<TopicKey, f64>) -> f64 {
            let own = n.topic.as_ref().and_then(|(_, k)| values.get(k).copied()).unwrap_or(f64::NEG_INFINITY);
            n.children.iter().map(|c| value(c, values)).fold(own, f64::max)
        }
        fn sort(v: &mut Vec<Node>, values: Option<&HashMap<TopicKey, f64>>) {
            match values {
                Some(vals) => v.sort_by(|a, b| {
                    value(b, vals).partial_cmp(&value(a, vals)).unwrap_or(std::cmp::Ordering::Equal).then(b.max_id.cmp(&a.max_id))
                }),
                None => v.sort_by(|a, b| b.max_id.cmp(&a.max_id).then(a.label.cmp(&b.label))),
            }
            for n in v.iter_mut() {
                sort(&mut n.children, values);
            }
        }
        sort(&mut root.children, values);
        root.children
    }

    fn badge(t: &Theme, c: Count) -> Option<Div> {
        (c.unread > 0).then(|| {
            let pill = |bg: Hsla, text: String| {
                div()
                    .h(px(16.))
                    .min_w(px(16.))
                    .px(px(5.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(bg)
                    .text_color(white())
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .child(text)
            };
            div()
                .flex()
                .flex_row()
                .flex_none()
                .gap_1()
                .when(c.mentions > 0, |d| d.child(pill(t.mention_badge, "@".into())))
                .child(pill(if c.mentions > 0 { t.mention_badge } else { t.unread_badge }, c.unread.to_string()))
        })
    }

    /// A sidebar row: Discord's 32px pill with hover and selected states. Unread rows get
    /// bright bold text and a notch at the sidebar's edge.
    fn row(&self, id: impl Into<ElementId>, selected: bool, unread: bool) -> Stateful<Div> {
        let t = &self.theme;
        div()
            .id(id.into())
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .mx_2()
            .px_2()
            .h(px(32.))
            .rounded(px(4.))
            .cursor_pointer()
            .font_weight(FontWeight::MEDIUM)
            .text_color(if selected || unread { t.fg_strong } else { t.muted })
            .when(unread, |d| {
                d.font_weight(FontWeight::SEMIBOLD)
                    .child(div().absolute().left(px(-8.)).top(px(12.)).w(px(4.)).h(px(8.)).rounded_r(px(4.)).bg(t.fg_strong))
            })
            .when(selected, |d| d.bg(t.bg_selected))
            .when(!selected, |d| d.hover(|s| s.bg(t.bg_selected.opacity(0.6)).text_color(t.fg)))
    }

    fn render_nodes(
        &self,
        sid: StreamId,
        nodes: &[Node],
        depth: usize,
        active: Option<&dyn Fn(&Node) -> bool>,
        cx: &Context<Self>,
        out: &mut Vec<AnyElement>,
    ) {
        let t = self.theme.clone();
        let visible: Vec<&Node> = nodes
            .iter()
            .filter(|n| {
                active.is_none_or(|f| f(n))
            })
            .collect();
        let last = visible.len().saturating_sub(1);
        for (i, n) in visible.into_iter().enumerate() {
            let folder = !n.children.is_empty();
            let open = self.open_folders.contains(&(sid, n.path.clone()));
            let selected = matches!((&self.selected, &n.topic), (Some(Target::Conv(Conv::Topic(k))), Some((_, nk))) if k == nk);
            let target = n.topic.as_ref().map(|(_, k)| Target::Conv(Conv::Topic(k.clone())));
            let path = n.path.clone();
            // Discord's thread spine: a line down from the channel that curves into each row.
            let spine = div()
                .relative()
                .w(px(14.))
                .h_full()
                .flex_none()
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .w(px(10.))
                        .h(px(15.))
                        .border_l_1()
                        .border_b_1()
                        .rounded_bl(px(8.))
                        .border_color(t.quote_bar),
                )
                .when(i < last, |d| d.child(div().absolute().left_0().top_0().bottom_0().border_l_1().border_color(t.quote_bar)));
            let mut row = self
                .row(ElementId::named_usize(format!("topic-{sid}-{}", n.path), depth), selected, n.count.unread > 0)
                .h(px(30.))
                .pl(px(16. + depth as f32 * 14.))
                .text_size(px(14.))
                .child(spine)
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(n.label.clone()))
                .when(folder, |d| {
                    let path = path.clone();
                    d.child(
                        div()
                            .id(ElementId::named_usize(format!("fold-{sid}-{}", n.path), depth))
                            .flex_none()
                            .child(icons::icon(if open { "chevron-down" } else { "chevron-right" }, 14., t.faint))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if !this.open_folders.remove(&(sid, path.clone())) {
                                    this.open_folders.insert((sid, path.clone()));
                                    this.note_reveal(format!("folder-{sid}-{path}"));
                                }
                                cx.notify();
                            })),
                    )
                })
                .children(Self::badge(
                    &t,
                    if folder && open { n.topic.as_ref().map(|_| Count::default()).unwrap_or(n.count) } else { n.count },
                ));
            if let Some((name, k)) = n.topic.clone() {
                let anchor = n.max_id;
                row = row.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.open_menu(SideMenu::Topic(k.clone(), name.clone(), anchor), e.position, cx)
                    }),
                );
            }
            row = match target {
                Some(target) => row.on_click(cx.listener(move |this, _, _, cx| this.open(target.clone(), cx))),
                None => row.on_click(cx.listener(move |this, _, _, cx| {
                    if !this.open_folders.remove(&(sid, path.clone())) {
                        this.open_folders.insert((sid, path.clone()));
                        this.note_reveal(format!("folder-{sid}-{path}"));
                    }
                    cx.notify();
                })),
            };
            out.push(row.into_any_element());
            if folder && open {
                let start = out.len();
                self.render_nodes(sid, &n.children, depth + 1, None, cx, out);
                let key = format!("folder-{sid}-{}", n.path);
                if self.revealing(&key) {
                    Self::stagger(&mut out[start..], &key, 0);
                }
            }
        }
    }

    /// A category header: small caps, like Discord's.
    fn section(&self, label: &str) -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_4()
            .pt(px(16.))
            .pb_1()
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(self.theme.muted)
            .child(label.to_string())
    }

    /// Every server Puzil is signed in to (the current one marked), then "+" to add one.
    fn rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let (sessions, active) = {
            let s = crate::servers::handle(cx).read(cx);
            (s.sessions.clone(), s.active)
        };
        let tiles: Vec<AnyElement> = sessions
            .iter()
            .enumerate()
            .map(|(i, session)| {
                let (name, realm, icon_url, mentions, images) = {
                    let s = session.read(cx);
                    let name = if s.state.realm.name.is_empty() { s.account.realm.clone() } else { s.state.realm.name.clone() };
                    (name, s.account.realm.clone(), s.state.realm.icon_url.clone(), s.state.mentions.len(), s.images.clone())
                };
                let icon = images.update(cx, |im, cx| im.realm_icon(&realm, icon_url.as_deref(), cx));
                let initials: String =
                    name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase();
                let selected = i == active;
                let tile = div().size(px(48.)).rounded(px(if selected { 16. } else { 24. })).overflow_hidden().flex().items_center().justify_center();
                let tile = match icon {
                    Some(im) => tile.child(gpui_kit::img(ImageSource::Render(im)).object_fit(ObjectFit::Cover).size_full()),
                    None => tile.bg(t.accent).text_color(white()).font_weight(FontWeight::BOLD).child(initials),
                };
                div()
                    .id(("server", i))
                    .relative()
                    .cursor_pointer()
                    .when(selected, |d| d.child(div().absolute().left(px(-12.)).top(px(4.)).w(px(4.)).h(px(40.)).rounded_r(px(4.)).bg(t.fg_strong)))
                    .child(tile)
                    .when(mentions > 0, |d| {
                        d.child(
                            div()
                                .absolute()
                                .right(px(-4.))
                                .bottom(px(-4.))
                                .h(px(20.))
                                .min_w(px(20.))
                                .px_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .border_4()
                                .border_color(t.rail)
                                .bg(t.mention_badge)
                                .text_color(white())
                                .text_size(px(11.))
                                .font_weight(FontWeight::BOLD)
                                .child(mentions.to_string()),
                        )
                    })
                    .on_click(cx.listener(move |_, _, _, cx| crate::servers::request(cx, crate::servers::ServersEvent::Switch(i))))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .w(px(72.))
            .flex_none()
            .bg(t.rail)
            .pt_3()
            .children(tiles)
            .child(div().w(px(32.)).h(px(2.)).rounded_full().bg(t.bg_selected))
            .child(
                div()
                    .id("add-server")
                    .size(px(48.))
                    .rounded(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(t.sidebar)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.accent))
                    .group("add-server")
                    .child(icons::icon("plus", 22., t.accent).group_hover("add-server", |s| s.text_color(white())))
                    .on_click(cx.listener(|_, _, _, cx| crate::servers::request(cx, crate::servers::ServersEvent::Add))),
            )
    }

    /// The account strip at the foot of the sidebar: who we are and whether we're live.
    fn panel(&self, me: &str, img: Option<Arc<RenderImage>>, me_id: u64, status: &Status, cx: &Context<Self>) -> impl IntoElement {
        let t = &self.theme;
        let rail_open = self.rail_open;
        let invisible = self.session.read(cx).user_settings.get("presence_enabled").and_then(serde_json::Value::as_bool) == Some(false);
        let (dot, label) = match status {
            Status::Live if invisible => (t.faint, "Invisible".to_string()),
            Status::Live => (hsla(0.38, 0.62, 0.45, 1.), "Online".to_string()),
            Status::Connecting => (t.faint, "Connecting…".to_string()),
            Status::Retrying(e) => (hsla(0.12, 0.9, 0.5, 1.), format!("Reconnecting · {e}")),
            Status::SignedOut(e) => (t.danger, e.clone()),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(px(52.))
            .flex_none()
            .m_2()
            .px_2()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.panel)
            .shadow_sm()
            // The server rail's toggle, at the card's left: next to where the rail appears.
            .child(
                div()
                    .id("toggle-rail")
                    .flex_none()
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_selected.opacity(0.6)))
                    .child(icons::icon(if rail_open { "panel-left-close" } else { "panel-left-open" }, 18., t.icon))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.rail_open = !this.rail_open;
                        this.rail_toggled = Some(std::time::Instant::now());
                        // Redraw once the slide is over, to drop the hidden rail.
                        cx.spawn(async |this, cx| {
                            cx.background_executor().timer(std::time::Duration::from_millis(280)).await;
                            let _ = this.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                        cx.notify();
                    })),
            )
            .child(avatar::with_status(avatar::avatar(img, 32., t.user_color(me_id), me), 32., dot, t.panel))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.fg_strong)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(me.to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(t.muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(label),
                    ),
            )
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // From Settings: DMs shown while collapsed (the most recent, unread first), how
        // recent a message makes a topic active, and whether muted channels show.
        let prefs = crate::settings::effective(cx, Some(&self.session.read(cx).account.realm.clone()));
        let dms_shown = prefs.dms_shown;
        let active_secs = prefs.active_days as i64 * 24 * 3600;
        let show_muted = prefs.show_muted_channels;
        let menu = self.render_menu(cx);
        self.persist();
        let t = self.theme.clone();
        let sort_values: HashMap<StreamId, HashMap<TopicKey, f64>> =
            self.expanded.clone().into_iter().filter_map(|sid| Some((sid, self.sort_values(sid, cx)?))).collect();
        let session = self.session.clone();
        let images = session.read(cx).images.clone();
        // Expanded channels need their topics (a fresh register empties that cache).
        let missing: Vec<StreamId> = {
            let s = session.read(cx);
            self.expanded.iter().copied().filter(|sid| !s.topics.contains_key(sid)).collect()
        };
        for sid in missing {
            session.update(cx, |s, cx| s.load_topics(sid, cx));
        }
        // When each expanded channel's topics last saw a message (fetched as needed).
        let newest: Vec<MessageId> = {
            let s = session.read(cx);
            self.expanded.iter().filter_map(|sid| s.topics.get(sid)).flatten().map(|t| t.max_id).collect()
        };
        let times: HashMap<MessageId, i64> =
            newest.into_iter().filter_map(|id| session.update(cx, |s, cx| s.message_time(id, cx)).map(|t| (id, t))).collect();
        let cutoff = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
            - active_secs;
        let open_topic = match &self.selected {
            Some(Target::Conv(Conv::Topic(k))) => Some(k.clone()),
            _ => None,
        };
        fn holds(n: &Node, k: &TopicKey) -> bool {
            n.topic.as_ref().is_some_and(|(_, nk)| nk == k) || n.children.iter().any(|c| holds(c, k))
        }
        let active = |n: &Node| {
            n.count.unread > 0
                || open_topic.as_ref().is_some_and(|k| holds(n, k))
                || times.get(&n.max_id).is_some_and(|t| *t >= cutoff)
        };

        // Direct messages, most recent first, with whatever pictures we have.
        struct DmRow {
            key: DmKey,
            name: String,
            other: u64,
            group: bool,
            avatar: Option<String>,
        }
        let all_counts = self.counts.get_or_insert_with(|| Rc::new(Self::counts(session.read(cx)))).clone();
        let (dm_rows, me_name, me_id, me_avatar, _realm_icon) = {
            let s = session.read(cx);
            let counts = &all_counts.0;
            let mut dms: Vec<(DmKey, MessageId)> = s.state.recent_dms.iter().map(|(k, v)| (k.clone(), *v)).collect();
            for c in counts.keys() {
                if let Conv::Dm(k) = c
                    && !s.state.recent_dms.contains_key(k)
                {
                    dms.push((k.clone(), u64::MAX));
                }
            }
            dms.sort_by(|a, b| b.1.cmp(&a.1));
            let rows: Vec<DmRow> = dms
                .into_iter()
                .map(|(k, _)| {
                    let others: Vec<u64> = k.others(s.state.me).collect();
                    let other = others.first().copied().unwrap_or(s.state.me);
                    DmRow {
                        name: s.state.dm_display(&k),
                        group: others.len() > 1,
                        avatar: s.state.users.get(&other).and_then(|u| u.avatar_url.clone()),
                        other,
                        key: k,
                    }
                })
                .collect();
            let me = s.state.users.get(&s.state.me);
            (
                rows,
                me.map(|u| u.full_name.clone()).unwrap_or_else(|| s.account.email.clone()),
                s.state.me,
                me.and_then(|u| u.avatar_url.clone()),
                s.state.realm.icon_url.clone(),
            )
        };
        let me_img = images.update(cx, |i, cx| i.avatar(me_id, me_avatar.as_deref(), cx));
        let dm_imgs: Vec<Option<Arc<RenderImage>>> = dm_rows
            .iter()
            .map(|r| if r.group { None } else { images.update(cx, |i, cx| i.avatar(r.other, r.avatar.as_deref(), cx)) })
            .collect();

        let s = session.read(cx);
        let (counts, stream_counts) = (&all_counts.0, &all_counts.1);
        let mut total = Count::default();
        counts.values().for_each(|c| total.add(*c));

        let mut rows: Vec<AnyElement> = Vec::new();
        let all_dms = self.all_dms;
        let dm_unread = dm_rows.iter().filter(|r| counts.get(&Conv::Dm(r.key.clone())).is_some_and(|c| c.unread > 0)).count();
        rows.push(div().h(px(8.)).into_any_element());
        // Collapsed: the few most recent, unread first (the selected one always stays).
        let mut order: Vec<usize> = (0..dm_rows.len()).collect();
        if !all_dms {
            let unread = |i: &usize| counts.get(&Conv::Dm(dm_rows[*i].key.clone())).is_some_and(|c| c.unread > 0);
            order.sort_by_key(|i| (!unread(i), *i));
            let selected = |i: &usize| self.selected == Some(Target::Conv(Conv::Dm(dm_rows[*i].key.clone())));
            let keep: HashSet<usize> =
                order.iter().take(dms_shown).copied().chain(order.iter().copied().filter(selected)).collect();
            order.retain(|i| keep.contains(i));
        }
        let mut dm_rows: Vec<Option<(DmRow, Option<Arc<RenderImage>>)>> = dm_rows.into_iter().zip(dm_imgs).map(Some).collect();
        let (dm_total, dm_shown) = (dm_rows.len(), order.len());
        let dm_start = rows.len();
        for i in order {
            let Some((r, img)) = dm_rows[i].take() else { continue };
            let conv = Conv::Dm(r.key.clone());
            let c = counts.get(&conv).copied().unwrap_or_default();
            let selected = self.selected == Some(Target::Conv(conv.clone()));
            let face = if r.group {
                div()
                    .size(px(24.))
                    .flex_none()
                    .rounded_full()
                    .bg(t.user_color(r.other))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icons::icon("users", 13., white()))
            } else {
                avatar::avatar(img, 24., t.user_color(r.other), &r.name)
            };
            let target = Target::Conv(conv);
            rows.push(
                self.row(ElementId::named_usize("dm", i), selected, c.unread > 0)
                    .h(px(34.))
                    .gap_2()
                    .child(face)
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(r.name))
                    .children(Self::badge(&t, c))
                    .on_click(cx.listener(move |this, _, _, cx| this.open(target.clone(), cx)))
                    .on_mouse_down(MouseButton::Right, {
                        let k = r.key.clone();
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| this.open_menu(SideMenu::Dm(k.clone()), e.position, cx))
                    })
                    .into_any_element(),
            );
        }
        if self.revealing("dms") && rows.len() > dm_start + dms_shown {
            Self::stagger(&mut rows[dm_start + dms_shown..], "dms", 0);
        }
        // No "Direct messages" heading; a quiet toggle under them instead, saying how many
        // more there are and how many of those have unread messages.
        if dm_total > dms_shown {
            let hidden_unread = dm_unread.saturating_sub(dm_shown.min(dm_unread));
            let label = if all_dms {
                "Show fewer".to_string()
            } else if hidden_unread > 0 {
                format!("{} more · {hidden_unread} unread", dm_total - dm_shown)
            } else {
                format!("{} more", dm_total - dm_shown)
            };
            rows.push(
                self.row("all-dms", false, false)
                    .h(px(26.))
                    .text_size(px(13.))
                    .text_color(t.faint)
                    .child(icons::icon(if all_dms { "chevron-up" } else { "chevron-down" }, 14., t.faint))
                    .child(label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.all_dms = !this.all_dms;
                        if this.all_dms {
                            this.note_reveal("dms".into());
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        rows.push(div().mx_4().mt_2().h(px(1.)).bg(t.divider).into_any_element());

        // Channels, grouped as Zulip does: pinned, then each channel folder in the
        // organization's order, then the rest. Muted ones are dimmed.
        let mut subs: Vec<_> = s
            .state
            .subs
            .values()
            .filter_map(|sub| s.state.streams.get(&sub.stream_id).map(|st| (sub, st)))
            .filter(|(_, st)| !st.is_archived)
            .collect();
        subs.sort_by_key(|(_, st)| st.name.to_lowercase());
        let folders: Vec<_> = s.state.realm.channel_folders.iter().filter(|f| !f.is_archived).collect();
        let mut groups: Vec<(String, String, Vec<_>)> = Vec::new();
        let pinned: Vec<_> = subs.iter().filter(|(sub, _)| sub.pin_to_top).copied().collect();
        if !pinned.is_empty() {
            groups.push(("pinned".into(), "Pinned channels".into(), pinned));
        }
        for f in &folders {
            let inside: Vec<_> = subs.iter().filter(|(sub, st)| !sub.pin_to_top && st.folder_id == Some(f.id)).copied().collect();
            if !inside.is_empty() {
                groups.push((format!("folder-{}", f.id), f.name.clone(), inside));
            }
        }
        let rest: Vec<_> = subs
            .iter()
            .filter(|(sub, st)| !sub.pin_to_top && !st.folder_id.is_some_and(|id| folders.iter().any(|f| f.id == id)))
            .copied()
            .collect();
        if !rest.is_empty() {
            let label = if groups.is_empty() { "" } else { "Other channels" };
            groups.push(("other".into(), label.into(), rest));
        }
        let overlay_ready = s.overlay.as_ref().is_some_and(|o| o.read(cx).ready());
        for (gi, (key, label, members)) in groups.into_iter().enumerate() {
            let collapsed = self.collapsed_groups.contains(&key);
            let group_unread =
                members.iter().any(|(sub, st)| !sub.is_muted && stream_counts.get(&st.stream_id).is_some_and(|c| c.unread > 0));
            let toggle_key = key.clone();
            // Without folders the one group needs no heading (only the sort control, if any).
            let labeled = !label.is_empty();
            let collapsed = collapsed && labeled;
            if !labeled && !(overlay_ready && gi == 0) {
                // nothing to show above the channels
            } else {
            rows.push(
                self.section(&label)
                    .id(SharedString::from(format!("group-{key}")))
                    .when(labeled, |d| {
                        d.cursor_pointer()
                            .hover(|d| d.text_color(t.fg))
                            .child(icons::icon(if collapsed { "chevron-right" } else { "chevron-down" }, 12., t.muted))
                            .when(collapsed && group_unread, |d| d.child(div().size(px(6.)).rounded_full().bg(t.fg_strong)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.collapsed_groups.remove(&toggle_key) {
                                    this.collapsed_groups.insert(toggle_key.clone());
                                } else {
                                    this.note_reveal(format!("group-{toggle_key}"));
                                }
                                cx.notify();
                            }))
                    })
                    .when(overlay_ready && gi == 0, |d| {
                        d.child(div().flex_1()).child(
                            div()
                                .id("topic-sort")
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_0p5()
                                .cursor_pointer()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.faint)
                                .hover(|st| st.text_color(t.fg_strong))
                                .child(self.sort.label())
                                .child(icons::icon("chevrons-up-down", 12., t.faint))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.sort = this.sort.next();
                                    cx.notify();
                                })),
                        )
                    })
                    .into_any_element(),
            );
            }
            let group_key = format!("group-{key}");
            let group_revealing = self.revealing(&group_key);
            let mut revealed = 0;
            for (sub, st) in members {
                let sid = st.stream_id;
                let c = stream_counts.get(&sid).copied().unwrap_or_default();
                let open = self.expanded.contains(&sid);
                let unread = c.unread > 0 && !sub.is_muted;
                // A collapsed group still shows its unread channels and the one we're in.
                let current = matches!(&self.selected, Some(Target::Conv(Conv::Topic(k))) if k.stream_id == sid)
                    || self.selected == Some(Target::NewTopic(sid));
                if collapsed && !unread && !current {
                    continue;
                }
                if sub.is_muted && !show_muted && !current {
                    continue;
                }
                // Rows a just-expanded group adds settle in; ones it already showed stay put.
                let row_start = rows.len();
                let was_hidden = !unread && !current;
                rows.push(
                    self.row(ElementId::named_usize("chan", sid as usize), false, unread)
                        .group("chan")
                        .when(sub.is_muted, |d| d.opacity(0.45))
                        .child(icons::channel(st.invite_only, 20., if unread { t.fg_strong } else { t.faint }, t.sidebar))
                        .child(
                            div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(st.name.clone()),
                        )
                        .child(
                            div()
                                .id(ElementId::named_usize("new-topic", sid as usize))
                                .invisible()
                                .group_hover("chan", |s| s.visible())
                                .child(icons::icon("plus", 16., t.icon))
                                .hover(|s| s.opacity(0.7))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.open(Target::NewTopic(sid), cx)
                                })),
                        )
                        .when(!open, |d| d.children(Self::badge(&t, c)))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, e: &MouseDownEvent, _, cx| this.open_menu(SideMenu::Channel(sid), e.position, cx)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.expanded.contains(&sid) {
                                this.expanded.remove(&sid);
                            } else {
                                this.expand(sid, cx);
                                this.note_reveal(format!("chan-{sid}"));
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                );
                if open {
                    let tree = self.tree(s, sid, &counts, sort_values.get(&sid));
                    let all = self.show_all.contains(&sid);
                    let n = tree.len();
                    // Active topics, then a row for the rest.
                    let quiet = tree.iter().filter(|node| !active(node)).count();
                    let tree_start = rows.len();
                    self.render_nodes(sid, &tree, 0, (!all).then_some(&active as &dyn Fn(&Node) -> bool), cx, &mut rows);
                    // "N more topics": the active ones come first, the rest settle in after.
                    let all_key = format!("all-{sid}");
                    if self.revealing(&all_key) {
                        let skip = (tree_start + (n - quiet)).min(rows.len());
                        Self::stagger(&mut rows[skip..], &all_key, 0);
                    }
                    if quiet > 0 {
                        let label = if all {
                            "Show fewer".to_string()
                        } else if quiet == n {
                            format!("{n} topic{}", if n == 1 { "" } else { "s" })
                        } else {
                            format!("{quiet} more topic{}", if quiet == 1 { "" } else { "s" })
                        };
                        rows.push(
                            self.row(ElementId::named_usize("more", sid as usize), false, false)
                                .h(px(26.))
                                .pl(px(38.))
                                .text_size(px(12.))
                                .text_color(t.faint)
                                .child(icons::icon(if all { "chevron-up" } else { "chevron-down" }, 12., t.faint))
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.show_all.remove(&sid) {
                                        this.show_all.insert(sid);
                                        this.note_reveal(format!("all-{sid}"));
                                    }
                                    cx.notify();
                                }))
                                .into_any_element(),
                        );
                    }
                    if s.topics.get(&sid).is_none() && n == 0 {
                        rows.push(
                            div()
                                .pl(px(46.))
                                .h(px(26.))
                                .text_size(px(12.))
                                .text_color(t.faint)
                                .child(if s.topics_failed(sid) { "Couldn't load topics" } else { "Loading topics…" })
                                .into_any_element(),
                        );
                    }
                    let chan_key = format!("chan-{sid}");
                    if self.revealing(&chan_key) {
                        Self::stagger(&mut rows[tree_start..], &chan_key, 0);
                    }
                }
                if group_revealing && was_hidden {
                    let n = rows.len() - row_start;
                    Self::stagger(&mut rows[row_start..], &group_key, revealed);
                    revealed += n;
                }
            }
        }

        rows.push(
            self.row("browse-channels", false, false)
                .mt_2()
                .text_size(px(14.))
                .child(icons::icon("compass", 18., t.faint))
                .child("Browse channels")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.selected = None;
                    cx.emit(SidebarEvent::Browse);
                    cx.notify();
                }))
                .into_any_element(),
        );

        let realm_name = if s.state.realm.name.is_empty() { s.account.realm.clone() } else { s.state.realm.name.clone() };
        let status = s.status.clone();

        let rail_open = self.rail_open;
        // The header strip runs across the rail and the sidebar, so the window's traffic
        // lights (floating at its left) never straddle a border.
        let header = crate::theme::drag_region(div().id("server-header"))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(px(48.))
            .flex_none()
            .pl(px(crate::theme::TRAFFIC_LIGHTS_WIDTH))
            .pr_3()
            .bg(t.sidebar)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(t.fg_strong)
                    .font_weight(FontWeight::SEMIBOLD)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(realm_name.clone()),
            )
            .when(!rail_open && total.mentions > 0, |d| {
                d.child(Self::badge(&t, Count { unread: total.mentions, mentions: total.mentions }).unwrap_or_else(div))
            });

        // Toggling the rail widens or narrows the sidebar over 200ms; the rail stays drawn
        // while it slides out, clipped by the shrinking edge.
        let sliding = self.rail_toggled.is_some_and(|t| t.elapsed().as_millis() < 260);
        let root = div()
            .flex()
            .flex_col()
            .h_full()
            .flex_none()
            .overflow_hidden()
            .w(px(if rail_open { 72. + SIDEBAR_W } else { SIDEBAR_W }))
            .children(menu)
            .child(header)
            .child({
            let row = div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h_0()
                .when(rail_open || sliding, |d| d.child(self.rail(cx)))
                .child(
                    // The account card floats over the list; the list scrolls behind it and
                    // keeps room at its end so the last channel can clear it.
                    div()
                        .relative()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .w(px(SIDEBAR_W))
                        .h_full()
                        .bg(t.sidebar)
                        .child(
                            div()
                                .id("sidebar-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .pb(px(76.))
                                .text_size(px(16.))
                                .children(rows),
                        )
                        // `occlude`: the card takes the pointer, so the rows behind it can't be hovered or clicked.
                        .child(
                            div()
                                .id("account-card")
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .occlude()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    // The press that started this click may have just closed it.
                                    let just_closed = this.panel_closed_at.take().is_some_and(|t| t.elapsed().as_millis() < 500);
                                    this.account_panel = !this.account_panel && !just_closed;
                                    cx.notify();
                                }))
                                .child(self.panel(&me_name, me_img, me_id, &status, cx)),
                        )
                        .children(self.render_account_panel(cx)),
                );
            // While the rail slides, the rail and sidebar move left together as the column
            // narrows: the rail leaves (or enters) through the left edge, and nothing jumps
            // when it's finally dropped.
            if sliding {
                let (from, to) = if rail_open { (-72., 0.) } else { (0., -72.) };
                row.with_animation(
                    SharedString::from(format!("rail-row-{rail_open}")),
                    Animation::new(std::time::Duration::from_millis(200)).with_easing(ease_out_quint()),
                    move |el, d| el.ml(px(from + (to - from) * d)),
                )
                .into_any_element()
            } else {
                row.into_any_element()
            }
            });
        if !sliding {
            return root.into_any_element();
        }
        let (from, to) = if rail_open { (SIDEBAR_W, 72. + SIDEBAR_W) } else { (72. + SIDEBAR_W, SIDEBAR_W) };
        root.with_animation(
            SharedString::from(format!("rail-{rail_open}")),
            Animation::new(std::time::Duration::from_millis(200)).with_easing(ease_out_quint()),
            move |el, d| el.w(px(from + (to - from) * d)),
        )
        .into_any_element()
    }
}
