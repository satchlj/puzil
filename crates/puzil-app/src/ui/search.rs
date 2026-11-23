//! Search (⌘F): the local index answers as you type; the server is asked after a pause for
//! history that isn't cached, and its answers are merged in. Zulip's `from:`, `in:`,
//! `topic:` and `has:` work. Picking a result opens its conversation at the message.

use std::{collections::BTreeMap, rc::Rc, time::Duration};

use gpui_kit::{
    component::input::{Input, InputEvent, InputState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_render::MessageAst;
use puzil_zulip::{
    client::Anchor,
    model::{Conv, TopicKey},
    types::{Message, MessageId, StreamId},
};
use serde_json::json;

use super::{conversation::Target, fmt};
use crate::{icons, runtime::on_tokio, session::Session, theme::Theme};

pub enum SearchEvent {
    Open(Target, MessageId),
}

/// A parsed query: free words plus Zulip's operators.
#[derive(Debug, Default, Clone, PartialEq)]
struct Query {
    words: String,
    sender: Option<u64>,
    channel: Option<StreamId>,
    topic: Option<String>,
    /// `has:link`, `has:image`, `has:attachment` (the server answers these).
    has: Vec<String>,
}

struct Hit {
    message: Message,
    /// Matches wrapped in U+0002…U+0003.
    snippet: String,
}

pub struct SearchView {
    session: Entity<Session>,
    theme: Rc<Theme>,
    input: Entity<InputState>,
    query: Query,
    /// Results by id, newest last.
    hits: BTreeMap<MessageId, Hit>,
    /// Bumped per query, so late answers to old queries are dropped.
    generation: u64,
    searching_server: bool,
    server_error: Option<String>,
    sel: usize,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SearchEvent> for SearchView {}

/// Splits `text` on whitespace, keeping `"quoted phrases"` together.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Marks each occurrence of the query's words in `text`, around the first one.
fn make_snippet(text: &str, words: &str) -> String {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = text.to_lowercase();
    let terms: Vec<String> = words.split_whitespace().map(|w| w.to_lowercase()).filter(|w| !w.is_empty()).collect();
    let first = terms.iter().filter_map(|t| lower.find(t.as_str())).min().unwrap_or(0);
    let mut start = first.saturating_sub(60);
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (first + 160).min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let window = &text[start..end];
    let wl = window.to_lowercase();
    // Byte ranges of matches (lowercasing can change lengths for a few scripts; skip then).
    let mut marks: Vec<(usize, usize)> = Vec::new();
    if wl.len() == window.len() {
        for t in &terms {
            let mut from = 0;
            while let Some(i) = wl[from..].find(t.as_str()) {
                marks.push((from + i, from + i + t.len()));
                from += i + t.len();
            }
        }
    }
    marks.sort();
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let mut at = 0;
    for (a, b) in marks {
        if a < at {
            continue;
        }
        out.push_str(&window[at..a]);
        out.push('\u{2}');
        out.push_str(&window[a..b]);
        out.push('\u{3}');
        at = b;
    }
    out.push_str(&window[at..]);
    if end < text.len() {
        out.push('…');
    }
    out
}

impl SearchView {
    pub fn new(session: Entity<Session>, theme: Rc<Theme>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search messages   from:name  in:channel  topic:name  has:link"));
        let subs = vec![cx.subscribe_in(&input, window, |this, _, e: &InputEvent, _, cx| match e {
            InputEvent::Change => this.search(false, cx),
            InputEvent::PressEnter { .. } => this.open_selected(cx),
            _ => {}
        })];
        input.update(cx, |i, cx| i.focus(window, cx));
        Self {
            session,
            theme,
            input,
            query: Query::default(),
            hits: BTreeMap::new(),
            generation: 0,
            searching_server: false,
            server_error: None,
            sel: 0,
            scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    pub fn set_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    fn parse(&self, text: &str, cx: &App) -> Query {
        let s = self.session.read(cx);
        let mut q = Query::default();
        let mut words = Vec::new();
        for tok in tokens(text) {
            let (op, arg) = match tok.split_once(':') {
                Some((op, arg)) if !arg.is_empty() => (op.to_lowercase(), arg.trim_start_matches(['#', '@']).to_lowercase()),
                _ => {
                    words.push(tok);
                    continue;
                }
            };
            match op.as_str() {
                "from" | "sender" => {
                    // The best name match: exact, then starts with, then contains.
                    let mut best: Option<(u8, u64)> = None;
                    for u in s.state.users.values() {
                        let n = u.full_name.to_lowercase();
                        let rank = if n == arg { 0 } else if n.starts_with(&arg) { 1 } else if n.contains(&arg) { 2 } else { continue };
                        if best.is_none_or(|(r, _)| rank < r) {
                            best = Some((rank, u.user_id));
                        }
                    }
                    q.sender = best.map(|(_, id)| id);
                }
                "in" | "channel" | "stream" => {
                    q.channel = s
                        .state
                        .streams
                        .values()
                        .filter(|st| st.name.to_lowercase().starts_with(&arg))
                        .min_by_key(|st| (st.name.to_lowercase() != arg, !s.state.subs.contains_key(&st.stream_id)))
                        .map(|st| st.stream_id);
                }
                "topic" => q.topic = Some(arg),
                "has" => q.has.push(arg),
                _ => words.push(tok),
            }
        }
        q.words = words.join(" ");
        q
    }

    fn search(&mut self, _now: bool, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let q = self.parse(&text, cx);
        if q == self.query && !self.hits.is_empty() {
            return;
        }
        self.query = q.clone();
        self.generation += 1;
        self.hits.clear();
        self.sel = 0;
        self.server_error = None;
        let empty = q.words.trim().is_empty() && q.sender.is_none() && q.channel.is_none() && q.topic.is_none() && q.has.is_empty();
        if empty {
            self.searching_server = false;
            cx.notify();
            return;
        }
        let generation = self.generation;
        // The local index first: instant, offline.
        if q.has.is_empty() {
            let store = self.session.read(cx).store.clone();
            let lq = q.clone();
            cx.spawn(async move |this, cx| {
                let hits = cx
                    .background_spawn(async move {
                        let prefix = match (&lq.channel, &lq.topic) {
                            (Some(sid), Some(t)) => Some(puzil_store::conv_to_str(&Conv::Topic(TopicKey::new(*sid, t)))),
                            (Some(sid), None) => Some(format!("t:{sid}:")),
                            _ => None,
                        };
                        let words = puzil_store::fts_words(&lq.words);
                        let mut hits = store.search_hits(&words, lq.sender, prefix.as_deref(), 200).unwrap_or_default();
                        if lq.channel.is_none()
                            && let Some(t) = &lq.topic
                        {
                            hits.retain(|h| h.message.subject.to_lowercase().contains(t.as_str()));
                        }
                        hits
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    for h in hits {
                        let snippet = h.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
                        this.hits.entry(h.message.id).or_insert(Hit { message: h.message, snippet });
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        // Then the server, once typing pauses.
        self.searching_server = true;
        let client = self.session.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(350)).await;
            if this.read_with(cx, |this, _| this.generation != generation).unwrap_or(true) {
                return;
            }
            let mut narrow = Vec::new();
            if !q.words.trim().is_empty() {
                narrow.push(json!({ "operator": "search", "operand": q.words }));
            }
            if let Some(id) = q.sender {
                narrow.push(json!({ "operator": "sender", "operand": id }));
            }
            if let Some(sid) = q.channel {
                narrow.push(json!({ "operator": "channel", "operand": sid }));
            }
            if let Some(t) = &q.topic {
                narrow.push(json!({ "operator": "topic", "operand": t }));
            }
            for h in &q.has {
                narrow.push(json!({ "operator": "has", "operand": h }));
            }
            let narrow = serde_json::Value::Array(narrow);
            let page = on_tokio(async move { client.messages(&narrow, Anchor::Newest, 100, 0).await }).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.searching_server = false;
                match page {
                    Ok(p) => {
                        for m in p.messages {
                            let text = MessageAst::from_html(&m.content).plain_text();
                            let snippet = make_snippet(&text, &this.query.words);
                            this.hits.entry(m.id).or_insert(Hit { message: m, snippet });
                        }
                    }
                    Err(e) => this.server_error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn ordered(&self) -> Vec<MessageId> {
        self.hits.keys().rev().copied().collect()
    }

    fn open_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.ordered().get(self.sel).copied() {
            self.open(id, cx);
        }
    }

    fn open(&mut self, id: MessageId, cx: &mut Context<Self>) {
        let Some(h) = self.hits.get(&id) else { return };
        let me = self.session.read(cx).state.me;
        let target = Target::Conv(puzil_zulip::model::conv_of(me, &h.message));
        cx.emit(SearchEvent::Open(target, id));
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.hits.len() as isize;
        if n == 0 {
            return;
        }
        self.sel = (self.sel as isize + by).rem_euclid(n) as usize;
        self.scroll.scroll_to_item(self.sel);
        cx.notify();
    }
}

/// `text` with U+0002…U+0003 spans highlighted.
fn highlighted(snippet: &str, t: &Theme) -> StyledText {
    let mut text = String::new();
    let mut ranges = Vec::new();
    let mut start = None;
    for c in snippet.chars() {
        match c {
            '\u{2}' => start = Some(text.len()),
            '\u{3}' => {
                if let Some(s) = start.take() {
                    ranges.push((s..text.len(), HighlightStyle { background_color: Some(t.passage), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() }));
                }
            }
            c => text.push(c),
        }
    }
    StyledText::new(text).with_highlights(ranges)
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let images = self.session.read(cx).images.clone();
        let ids = self.ordered();
        let rows: Vec<AnyElement> = ids
            .iter()
            .enumerate()
            .filter_map(|(i, id)| {
                let h = self.hits.get(id)?;
                let m = &h.message;
                let (place, name, avatar_url) = {
                    let s = self.session.read(cx);
                    let place = match m.stream_id {
                        Some(sid) => {
                            let chan = s.state.streams.get(&sid).map(|st| st.name.clone()).unwrap_or_default();
                            let topic = if m.subject.is_empty() { s.state.empty_topic_name.clone() } else { m.subject.clone() };
                            format!("#{chan} › {topic}")
                        }
                        None => match puzil_zulip::model::conv_of(s.state.me, m) {
                            Conv::Dm(k) => format!("@{}", s.state.dm_display(&k)),
                            _ => "Direct message".to_string(),
                        },
                    };
                    let u = s.state.users.get(&m.sender_id);
                    (place, u.map(|u| u.full_name.clone()).unwrap_or_else(|| m.sender_full_name.clone()), u.and_then(|u| u.avatar_url.clone()))
                };
                let img = images.update(cx, |im, cx| im.avatar(m.sender_id, avatar_url.as_deref(), cx));
                let id = *id;
                Some(
                    div()
                        .id(("hit", i))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .when(i == self.sel, |d| d.bg(t.bg_selected.opacity(0.7)))
                        .hover(|d| d.bg(t.bg_selected.opacity(0.45)))
                        .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(place))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_3()
                                .child(super::avatar::avatar(img, 32., t.user_color(m.sender_id), &name).mt_0p5())
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_baseline()
                                                .gap_2()
                                                .child(div().font_weight(FontWeight::MEDIUM).text_color(t.fg_strong).child(name))
                                                .child(div().text_size(px(12.)).text_color(t.faint).child(fmt::datetime(m.timestamp))),
                                        )
                                        .child(div().text_color(t.fg).line_height(px(22.)).child(highlighted(&h.snippet, &t))),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.open(id, cx)))
                        .into_any_element(),
                )
            })
            .collect();
        let has_query = !self.input.read(cx).value().trim().is_empty();
        let status = if !has_query {
            "Type to search everything Puzil has cached; the server fills in the rest.".to_string()
        } else if let Some(e) = &self.server_error {
            format!("{} results from the cache · the server couldn't be searched ({e})", rows.len())
        } else if self.searching_server {
            format!("{} results · searching the server…", rows.len())
        } else {
            format!("{} result{}", rows.len(), if rows.len() == 1 { "" } else { "s" })
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .on_action(cx.listener(|this, _: &super::composer::ComposerUp, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &super::composer::ComposerDown, _, cx| this.step(1, cx)))
            .child(
                crate::theme::drag_region(div().id("search-header"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(48.))
                    .px_4()
                    .flex_none()
                    .border_b_1()
                    .border_color(t.border)
                    .child(icons::icon("search", 20., t.faint))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(t.fg_strong).child("Search")),
            )
            .child(
                div().flex_none().px_6().pt_4().child(
                    div().max_w(px(820.)).mx_auto().flex().flex_col().gap_2().child(
                        div()
                            .h(px(42.))
                            .px_2()
                            .rounded(px(6.))
                            .bg(t.composer_bg)
                            .border_1()
                            .border_color(t.composer_border)
                            .flex()
                            .items_center()
                            .child(Input::new(&self.input).appearance(false)),
                    )
                    .child(div().px_1().text_size(px(12.)).text_color(t.faint).child(status)),
                ),
            )
            .child(
                div()
                    .id("search-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().max_w(px(820.)).mx_auto().px_6().py_3().flex().flex_col().gap_0p5().children(rows)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{make_snippet, tokens};

    #[test]
    fn tokens_keep_quotes() {
        assert_eq!(tokens(r#"from:"Ada Lovelace" latent  topic:x"#), ["from:Ada Lovelace", "latent", "topic:x"]);
    }

    #[test]
    fn snippets_mark_every_term() {
        let s = make_snippet("The natural Latent and another latent here", "latent");
        assert_eq!(s, "The natural \u{2}Latent\u{3} and another \u{2}latent\u{3} here");
    }
}
