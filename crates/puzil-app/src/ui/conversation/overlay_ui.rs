//! Iliad Overlay in the conversation view: votes, replies, threads, one-click reactions,
//! inline passage reactions, and the topic vote in the header.

use std::{collections::HashMap, rc::Rc, time::Duration};

use gpui_kit::{
    component::input::{Input, InputEvent, InputState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_overlay::{Kind, VoteTarget};
use puzil_render::{MessageAst, ast::ReplyMeta};
use puzil_zulip::{
    model::{Conv, TopicKey},
    types::{Message, MessageId},
};

use super::{Conversation, ConversationEvent, Item, Target};
use crate::icons;
use crate::{
    overlay::{Overlay, OverlayChanged, Thread},
    ui::blocks::BuiltMsg,
};

/// What the Overlay controls need to know about one message.
#[derive(Clone)]
pub(super) struct MsgInfo {
    pub id: MessageId,
    pub stream_id: Option<u64>,
    pub topic: String,
    pub sender_id: u64,
    pub sender_name: String,
    pub sender_is_bot: bool,
    pub mine: bool,
    /// Votes and inline reactions are offered (a channel the service covers).
    pub covered: bool,
    /// Unicode emoji codes we have reacted with.
    pub my_codes: Vec<String>,
}

const GAP_MINUTES: i64 = 30;

/// `"2 h later"`, as overlay.js describes a time gap.
pub(super) fn describe_gap(secs: i64) -> String {
    let hours = secs as f64 / 3600.;
    if hours < 1. {
        format!("{} min later", (secs as f64 / 60.).round())
    } else if hours < 48. {
        format!("{} h later", hours.round())
    } else {
        format!("{} days later", (hours / 24.).round())
    }
}

pub(super) fn gap_after(prev: i64, next: i64) -> Option<i64> {
    let g = next - prev;
    (g > GAP_MINUTES * 60).then_some(g)
}

fn score_text(s: i64) -> String {
    if s < 0 { format!("−{}", -s) } else { s.to_string() }
}

impl Conversation {
    pub(super) fn info(&self, m: &Message, cx: &App) -> MsgInfo {
        let s = self.session.read(cx);
        let sender = s.state.users.get(&m.sender_id);
        let covered = match (&self.overlay, m.stream_id) {
            (Some(o), Some(sid)) => o.read(cx).covers(sid, s.state.streams.get(&sid).map(|st| st.invite_only)),
            _ => false,
        };
        MsgInfo {
            id: m.id,
            stream_id: m.stream_id,
            topic: m.subject.clone(),
            sender_id: m.sender_id,
            sender_name: sender.map(|u| u.full_name.clone()).unwrap_or_else(|| m.sender_full_name.clone()),
            sender_is_bot: sender.is_some_and(|u| u.is_bot),
            mine: m.sender_id == s.state.me,
            covered,
            my_codes: m
                .reactions
                .iter()
                .filter(|r| r.user_id == s.state.me && r.reaction_type == "unicode_emoji")
                .map(|r| r.emoji_code.clone())
                .collect(),
        }
    }

    pub(super) fn on_overlay(&mut self, ch: &OverlayChanged, cx: &mut Context<Self>) {
        if !ch.inline.is_empty() {
            let mut built = self.built.borrow_mut();
            for (ix, it) in self.items.borrow().iter().enumerate() {
                if let Item::Msg { id, .. } = it
                    && ch.inline.contains(id)
                {
                    built.remove(id);
                    self.list.remeasure_items(ix..ix + 1);
                }
            }
        }
        if ch.status {
            // Coverage may have changed (signed in): rebuild with text indexes.
            self.built.borrow_mut().clear();
            self.list.remeasure();
            self.overlay_on_open(cx);
        }
        if ch.threads || ch.status {
            self.list.remeasure();
        }
        cx.notify();
    }

    /// When a topic opens: refresh votes and reactions, look for threads under it.
    pub(super) fn overlay_on_open(&mut self, cx: &mut Context<Self>) {
        let (Some(o), Some(Conv::Topic(k))) = (self.overlay.clone(), self.conv().cloned()) else {
            return;
        };
        let ids = self.session.read(cx).conv_ids(&Conv::Topic(k.clone()));
        o.update(cx, |o, _| o.refresh(ids));
        if !self.session.read(cx).topics.contains_key(&k.stream_id) {
            self.session.update(cx, |s, cx| s.load_topics(k.stream_id, cx));
        }
        self.look_for_threads(cx);
    }

    /// Threads branching from this topic (needs the channel's topic list; throttled).
    pub(super) fn look_for_threads(&mut self, cx: &mut Context<Self>) {
        let (Some(o), Some(Conv::Topic(k))) = (self.overlay.clone(), self.conv().cloned()) else {
            return;
        };
        let s = self.session.read(cx);
        let Some(topics) = s.topics.get(&k.stream_id).map(|t| t.iter().map(|t| t.name.clone()).collect::<Vec<_>>()) else {
            return;
        };
        let display = s.state.topic_display(&k);
        o.update(cx, |o, cx| o.find_threads(k.stream_id, &display, topics, cx));
    }

    /// Replies in this conversation, by the message they answer (same topic only).
    pub(super) fn count_replies(&mut self, cx: &App) {
        let Some(Conv::Topic(k)) = self.conv().cloned() else {
            return;
        };
        let s = self.session.read(cx);
        let here = puzil_overlay::encode_hash_component(&s.state.topic_display(&k));
        let ids = s.conv_ids(&Conv::Topic(k.clone()));
        let mut counts: HashMap<MessageId, usize> = HashMap::new();
        for id in &ids {
            let reply = self.replies.entry(*id).or_insert_with(|| {
                s.state
                    .messages
                    .get(id)
                    .filter(|m| m.content.contains("/near/"))
                    .and_then(|m| MessageAst::from_html(&m.content).reply)
            });
            if let Some(r) = reply
                && !r.is_thread
                && (ids.contains(&r.parent_id) || (r.stream_id == k.stream_id && r.topic_encoded == here))
            {
                *counts.entry(r.parent_id).or_default() += 1;
            }
        }
        self.reply_counts = counts;
    }

    fn overlay_update(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Overlay, &mut Context<Overlay>)) {
        if let Some(o) = &self.overlay {
            o.update(cx, f);
        }
    }

    fn vote(&mut self, kind: Kind, target: VoteTarget, direction: i64, cx: &mut Context<Self>) {
        self.overlay_update(cx, |o, cx| o.vote(kind, target, direction, cx));
    }

    fn vote_buttons(&self, kind: Kind, target: VoteTarget, label: &str, cx: &mut Context<Self>) -> Div {
        let t = self.theme.clone();
        let score = self
            .overlay
            .as_ref()
            .and_then(|o| {
                let o = o.read(cx);
                match kind {
                    Kind::Message => o.message_scores.get(&target.id).copied(),
                    Kind::Topic => o.topic_scores.get(&target.id).copied(),
                }
            })
            .unwrap_or_default();
        let btn = |glyph: &'static str, dir: i64, on: bool, cx: &mut Context<Self>| {
            let color = match (on, dir > 0) {
                (true, true) => hsla(0.06, 0.95, 0.55, 1.),
                (true, false) => t.accent,
                _ => t.icon,
            };
            div()
                .id(ElementId::named_usize(format!("vote-{label}-{}", target.id), dir as usize))
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_selected.opacity(0.7)))
                .child(icons::icon(glyph, 18., color))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.vote(kind, target, dir, cx)
                }))
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(btn("arrow-big-up", 1, score.my_vote == 1, cx))
            .child(div().min_w(px(16.)).text_center().text_color(t.fg).child(score_text(score.score)))
            .child(btn("arrow-big-down", -1, score.my_vote == -1, cx))
    }

    fn target(info: &MsgInfo) -> Option<VoteTarget> {
        Some(VoteTarget { id: info.id, stream_id: info.stream_id?, sender_id: info.sender_id, sender_is_bot: info.sender_is_bot })
    }

    /// The toolbar over a hovered message: vote, reply, thread, one-click reactions, and
    /// edit/delete for our own messages.
    /// `head`: the message starts a group, so the bar can sit in the padding above its name
    /// (elsewhere it straddles the previous message, as in Discord).
    pub(super) fn hover_bar(&self, info: &MsgInfo, head: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = self.theme.clone();
        let id = info.id;
        let button = |key: &str, ic: &'static str, color: Hsla| {
            div()
                .id(ElementId::named_usize(format!("{key}-{id}"), 0))
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_selected.opacity(0.7)))
                .child(icons::icon(ic, 20., color))
        };
        let sep = || div().w(px(1.)).h(px(20.)).mx_0p5().bg(t.divider);
        let mut bar = div()
            .id(ElementId::named_usize("hover-bar", id as usize))
            .on_hover(cx.listener(move |this, h: &bool, _, cx| {
                let next = if *h { Some(id) } else if this.bar_hovered == Some(id) { None } else { this.bar_hovered };
                if next != this.bar_hovered {
                    this.bar_hovered = next;
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .absolute()
            .top(px(if head { -2. } else { -16. }))
            .right(px(14.))
            .flex()
            .flex_row()
            .items_center()
            .p_0p5()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.bg)
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., if t.dark { 0.3 } else { 0.08 }),
                offset: point(px(0.), px(2.)),
                blur_radius: px(6.),
                spread_radius: px(0.),
                inset: false,
            }]);
        for (k, (name, code, glyph)) in puzil_overlay::QUICK.iter().enumerate() {
            let mine = info.my_codes.iter().any(|c| c == code);
            bar = bar.child(
                div()
                    .id(ElementId::named_usize(format!("quick-{id}"), k))
                    .size(px(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(mine, |d| d.bg(t.reaction_mine_bg))
                    .hover(|s| s.bg(t.bg_selected.opacity(0.7)))
                    .text_size(px(18.))
                    .child(*glyph)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_reaction(id, name.to_string(), code.to_string(), "unicode_emoji".into(), mine, cx)
                    })),
            );
        }
        let votes = info.covered.then(|| Self::target(info)).flatten();
        let replies = info.stream_id.is_some() && self.overlay.is_some();
        if let Some(target) = votes {
            bar = bar.child(sep()).child(self.vote_buttons(Kind::Message, target, "m", cx));
        }
        if !replies {
            // Direct messages: Zulip's quote-and-reply.
            let i2 = info.clone();
            bar = bar.child(sep()).child(
                button("reply", "corner-up-left", t.icon).on_click(cx.listener(move |this, _, w, cx| this.start_reply(&i2, w, cx))),
            );
        }
        if replies {
            bar = bar.child(sep());
            let i2 = info.clone();
            bar = bar.child(
                button("reply", "corner-up-left", t.icon)
                    .on_click(cx.listener(move |this, _, w, cx| this.start_reply(&i2, w, cx))),
            );
            bar = bar.child(
                button("thread", "messages-square", t.icon)
                    .on_click(cx.listener(move |this, _, w, cx| this.ask_thread(id, w, cx))),
            );
        }
        bar
    }

    pub(super) fn reply_chip(&self, info: &MsgInfo, reply: &ReplyMeta, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme.clone();
        let parent = reply.parent_id;
        let in_view = self.items.borrow().iter().any(|i| matches!(i, Item::Msg { id, .. } if *id == parent));
        let local = self
            .session
            .read(cx)
            .state
            .messages
            .get(&parent)
            .map(|m| (m.stream_id, m.subject.clone(), MessageAst::from_html(&m.content).plain_text()));
        let (snippet, elsewhere) = match local {
            Some((sid, topic, text)) => (text, sid != info.stream_id || topic.to_lowercase() != info.topic.to_lowercase()),
            None => {
                let s = self.overlay.as_ref().and_then(|o| o.update(cx, |o, cx| o.snippet(parent, cx)));
                match s {
                    Some((sid, topic, text)) => {
                        (text, Some(sid) != info.stream_id || topic.to_lowercase() != info.topic.to_lowercase())
                    }
                    None => (String::new(), false),
                }
            }
        };
        let snippet = puzil_overlay::shorten(&snippet.split_whitespace().collect::<Vec<_>>().join(" "), 120);
        let (face, color) = {
            let s = self.session.read(cx);
            let url = reply.author_id.and_then(|id| s.state.users.get(&id)).and_then(|u| u.avatar_url.clone());
            (url, t.user_color(reply.author_id.unwrap_or(0)))
        };
        let img = reply.author_id.and_then(|id| self.images.update(cx, |i, cx| i.avatar(id, face.as_deref(), cx)));
        let (sid, enc) = (reply.stream_id, reply.topic_encoded.clone());
        div()
            .id(ElementId::named_usize(format!("chip-{}", info.id), 0))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .min_w_0()
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(t.muted)
            .cursor_pointer()
            .hover(|s| s.text_color(t.fg_strong))
            .when(reply.is_thread, |d| {
                d.child(icons::icon("messages-square", 14., t.icon)).child(div().flex_none().child("Thread from"))
            })
            .child(super::super::avatar::avatar(img, 16., color, &reply.author))
            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(t.fg).child(format!("@{}", reply.author)))
            .when(elsewhere, |d| d.child(div().flex_none().text_color(t.faint).child("in another topic")))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(snippet))
            .on_click(cx.listener(move |this, _, _, cx| {
                if in_view {
                    this.flash(parent, cx);
                } else if let Some(topic) = super::decode_hash(&enc) {
                    cx.emit(ConversationEvent::Navigate(Target::Conv(Conv::Topic(TopicKey::new(sid, &topic)))));
                }
            }))
            .into_any_element()
    }

    /// Scrolls to a message and highlights it briefly.
    pub(super) fn flash(&mut self, id: MessageId, cx: &mut Context<Self>) {
        if let Some(ix) = self.items.borrow().iter().position(|i| matches!(i, Item::Msg { id: m, .. } if *m == id)) {
            self.list.scroll_to_reveal_item(ix);
        }
        self.flashing = Some(id);
        self.flash_count += 1;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| {
                if this.flashing == Some(id) {
                    this.flashing = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Below a message: its score, reply count, threads started from it, and the thread
    /// name box when one is being started.
    pub(super) fn meta_rows(&self, info: &MsgInfo, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme.clone();
        let mut out = Vec::new();
        let score =
            self.overlay.as_ref().and_then(|o| o.read(cx).message_scores.get(&info.id).copied()).map(|s| s.score).unwrap_or(0);
        let replies = self.reply_counts.get(&info.id).copied().unwrap_or(0);
        if (info.covered && score != 0) || replies > 0 {
            out.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .pt_1()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted)
                    .when(info.covered && score != 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_0p5()
                                .child(icons::icon(
                                    if score > 0 { "arrow-big-up" } else { "arrow-big-down" },
                                    14.,
                                    if score > 0 { hsla(0.06, 0.95, 0.55, 1.) } else { t.accent },
                                ))
                                .child(score_text(score)),
                        )
                    })
                    .when(replies > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .text_color(t.link)
                                .child(icons::icon("corner-up-left", 14., t.link))
                                .child(format!("{replies} {}", if replies == 1 { "reply" } else { "replies" })),
                        )
                    })
                    .into_any_element(),
            );
        }
        let threads: Vec<Thread> =
            self.overlay.as_ref().and_then(|o| o.read(cx).threads.get(&info.id).cloned()).unwrap_or_default();
        let here = puzil_overlay::unresolved(&info.topic).to_string();
        for (k, th) in threads.into_iter().enumerate() {
            let full = puzil_overlay::unresolved(&th.topic).to_string();
            let label = full.strip_prefix(&format!("{here}{}", puzil_overlay::SEPARATOR)).map(str::to_string).unwrap_or(full);
            let target = Target::Conv(Conv::Topic(TopicKey::new(th.stream_id, &th.topic)));
            out.push(
                div()
                    .id(ElementId::named_usize(format!("thread-{}", info.id), k))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .mt_1()
                    .max_w(px(480.))
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.sidebar)
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.faint))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .items_center()
                            .child(icons::icon("messages-square", 16., t.icon))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(t.fg_strong)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.link)
                                    .child("See thread"),
                            )
                            .child(icons::icon("chevron-right", 14., t.link)),
                    )
                    .when(!th.preview.is_empty(), |d| {
                        let p = if th.started_by.is_empty() {
                            th.preview.clone()
                        } else {
                            format!("{}: {}", th.started_by, th.preview)
                        };
                        d.child(
                            div()
                                .pl(px(24.))
                                .text_size(px(13.))
                                .text_color(t.muted)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(p),
                        )
                    })
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ConversationEvent::Navigate(target.clone()))))
                    .into_any_element(),
            );
        }
        if let Some((ask_id, input, _)) = &self.thread_ask
            && *ask_id == info.id
        {
            let room = puzil_overlay::MAX_TOPIC_CHARS as i64
                - here.chars().count() as i64
                - puzil_overlay::SEPARATOR.chars().count() as i64
                - input.read(cx).value().chars().count() as i64;
            out.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .mt_1()
                    .max_w(px(420.))
                    .child(div().text_xs().text_color(t.muted).child(format!("{here} /")))
                    .child(div().flex_1().child(Input::new(input)))
                    .child(
                        div()
                            .w(px(24.))
                            .text_xs()
                            .text_right()
                            .text_color(if room < 0 { t.danger } else { t.muted })
                            .child(room.to_string()),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    pub(super) fn start_reply(&mut self, info: &MsgInfo, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sid) = info.stream_id else { return self.quote_reply(info, window, cx) };
        let realm = self.session.read(cx).account.realm.clone();
        let header = puzil_overlay::reply_header(&realm, &info.sender_name, info.sender_id, sid, &info.topic, info.id);
        self.composer.update(cx, |c, cx| c.prefill(None, header, window, cx));
    }

    /// Zulip's quote-and-reply, for messages outside channels: a mention of the author linking
    /// to the message, then the message's source in a quote block.
    fn quote_reply(&mut self, info: &MsgInfo, window: &mut Window, cx: &mut Context<Self>) {
        let (realm, me, dm, fallback) = {
            let s = self.session.read(cx);
            let Some(m) = s.state.messages.get(&info.id) else { return };
            let others: Vec<u64> = match self.conv() {
                Some(Conv::Dm(k)) => k.others(s.state.me).collect(),
                _ => vec![],
            };
            (s.account.realm.clone(), s.state.me, others, MessageAst::from_html(&m.content).plain_text())
        };
        let _ = me;
        let ids = dm.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let link = format!("{realm}/#narrow/dm/{ids}-dm/near/{}", info.id);
        let head = format!("@_**{}|{}** [said]({link}):\n", info.sender_name, info.sender_id);
        let id = info.id;
        let client = self.session.read(cx).client.clone();
        let composer = self.composer.clone();
        // The quote should hold the source, not the rendered text; fetch it, falling back.
        let task = crate::runtime::on_tokio(async move { client.raw_message(id).await });
        cx.spawn_in(window, async move |_, cx| {
            let body = task.await.unwrap_or(fallback);
            let text = format!("{head}```quote\n{}\n```\n", body.trim_end());
            let _ = composer.update_in(cx, |c, window, cx| c.prefill(None, text, window, cx));
        })
        .detach();
    }

    /// The thread name box, under the message. Its counter shows the characters left in
    /// Zulip's 60-character topic limit for the whole `Parent / name`.
    pub(super) fn ask_thread(&mut self, id: MessageId, window: &mut Window, cx: &mut Context<Self>) {
        let topic = self.session.read(cx).state.messages.get(&id).map(|m| m.subject.clone()).unwrap_or_default();
        let room = puzil_overlay::MAX_TOPIC_CHARS as i64 - puzil_overlay::unresolved(&topic).chars().count() as i64 - 3;
        if room < 1 {
            tracing::warn!("topic too long to hold a thread");
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Thread name, then Enter"));
        let sub = cx.subscribe_in(&input, window, move |this, input, e: &InputEvent, window, cx| match e {
            InputEvent::PressEnter { .. } => {
                let name = input.read(cx).value().trim().to_string();
                if !name.is_empty() {
                    this.start_thread(id, name, window, cx);
                }
            }
            InputEvent::Blur => {
                this.thread_ask = None;
                this.list.remeasure();
                cx.notify();
            }
            _ => cx.notify(),
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        self.thread_ask = Some((id, input, sub));
        self.list.remeasure();
        cx.notify();
    }

    /// Opens a new topic `Parent / name` whose first message links back to this one.
    fn start_thread(&mut self, id: MessageId, name: String, _: &mut Window, cx: &mut Context<Self>) {
        let s = self.session.read(cx);
        let Some(m) = s.state.messages.get(&id) else {
            return;
        };
        let Some(sid) = m.stream_id else { return };
        let info = self.info(m, cx);
        let full = format!("{}{}{}", puzil_overlay::unresolved(&m.subject), puzil_overlay::SEPARATOR, name);
        if full.chars().count() > puzil_overlay::MAX_TOPIC_CHARS {
            tracing::warn!("thread name too long: {full}");
            return;
        }
        let realm = s.account.realm.clone();
        let header = puzil_overlay::thread_header(&realm, &info.sender_name, info.sender_id, sid, &info.topic, id);
        let root = info.topic.clone();
        self.thread_ask = None;
        self.overlay_update(cx, |o, _| o.recheck_threads(sid, &root));
        cx.emit(ConversationEvent::Compose { target: Target::NewTopic(sid), topic: Some(full), body: header });
    }

    /// The first message of a thread is on screen: show the thread under its source now.
    pub(super) fn note_thread_origin(&self, info: &MsgInfo, reply: &ReplyMeta, built: &BuiltMsg, cx: &mut Context<Self>) {
        let (Some(sid), true) = (info.stream_id, reply.is_thread) else {
            return;
        };
        let preview = built.paras.iter().map(|p| p.index_text.as_str()).collect::<Vec<_>>().join(" ");
        let t = Thread {
            stream_id: sid,
            topic: info.topic.clone(),
            started_by: info.sender_name.clone(),
            preview: puzil_overlay::shorten(&preview.split_whitespace().collect::<Vec<_>>().join(" "), 160),
        };
        self.overlay_update(cx, |o, cx| o.note_thread(reply.parent_id, t, cx));
    }

    /// The one-click emoji bar for reacting to the selected passage of a message.
    pub(super) fn selection_bar(&self, info: &MsgInfo, built: &BuiltMsg, cx: &mut Context<Self>) -> Option<Div> {
        if !info.covered || self.selection.borrow().dragging {
            return None;
        }
        let passage = built.passage(info.id, &self.selection.borrow())?;
        let t = self.theme.clone();
        let id = info.id;
        let passage = Rc::new(passage);
        let mut bar = div()
            .absolute()
            .top(px(-34.))
            .left(px(72.))
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .p_0p5()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.bg)
            .shadow_md()
            .child(div().px_2().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(t.muted).child("React to passage"));
        for (k, (name, _, glyph)) in puzil_overlay::QUICK.iter().enumerate() {
            let p = passage.clone();
            bar = bar.child(
                div()
                    .id(ElementId::named_usize(format!("inline-{id}"), k))
                    .size(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(17.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_hover))
                    .child(*glyph)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        *this.selection.borrow_mut() = Default::default();
                        let (s, e, h) = (p.start, p.end, p.hash.clone());
                        this.overlay_update(cx, |o, cx| o.put_inline(id, s, e, h, name.to_string(), true, cx));
                    })),
            );
        }
        Some(bar)
    }

    /// A click on an inline-reaction badge adds or takes back our reaction.
    pub(super) fn on_badge(&mut self, msg: MessageId, badge: usize, cx: &mut Context<Self>) {
        let Some(r) = self.built.borrow().get(&msg).and_then(|b| b.badges.get(badge).cloned()) else {
            return;
        };
        self.overlay_update(cx, |o, cx| o.put_inline(msg, r.start, r.end, r.hash.clone(), r.emoji.clone(), !r.mine, cx));
    }

    /// `Vote on topic ▲ 3 ▼` in the header of a covered topic.
    pub(super) fn topic_votes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let o = self.overlay.clone()?;
        let Some(Conv::Topic(k)) = self.conv().cloned() else {
            return None;
        };
        let s = self.session.read(cx);
        let invite_only = s.state.streams.get(&k.stream_id).map(|st| st.invite_only);
        let display = s.state.topic_display(&k);
        let topic = if display == s.state.empty_topic_name { String::new() } else { display };
        if !o.read(cx).covers(k.stream_id, invite_only) {
            return None;
        }
        let anchor = o.update(cx, |o, cx| o.topic_anchor(&k, &topic, cx))?;
        let sender_is_bot = self.session.read(cx).state.users.get(&anchor.sender_id).is_some_and(|u| u.is_bot);
        let target = VoteTarget { id: anchor.id, stream_id: anchor.stream_id, sender_id: anchor.sender_id, sender_is_bot };
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .px_0p5()
                .rounded(px(6.))
                .bg(self.theme.input_bg)
                .child(self.vote_buttons(Kind::Topic, target, "t", cx))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gaps() {
        assert_eq!(super::gap_after(0, 30 * 60), None);
        assert_eq!(super::gap_after(0, 31 * 60), Some(31 * 60));
        assert_eq!(super::describe_gap(45 * 60), "45 min later");
        assert_eq!(super::describe_gap(5 * 3600), "5 h later");
        assert_eq!(super::describe_gap(72 * 3600), "3 days later");
    }
}
