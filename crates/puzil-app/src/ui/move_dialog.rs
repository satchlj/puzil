//! The Move dialog: one dialog for renaming a topic, moving a topic to another channel, and
//! moving some of a topic's messages (which splits it, or merges them into an existing topic).

use std::rc::Rc;

use gpui_kit::{
    component::input::{InputEvent, InputState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::{
    ApiError,
    client::MoveRequest,
    model::{Conv, TopicKey},
    perms::Move,
    types::{MessageId, StreamId},
};

use super::{
    composer::{ComposerDown, ComposerEscape, ComposerUp},
    conversation::Target,
    kit,
};
use crate::{icons, runtime::on_tokio, session::Session, theme::Theme};

/// What the dialog was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMode {
    /// The whole topic, to a new name in the same channel.
    Rename,
    /// The whole topic, anywhere.
    Topic,
    /// Some messages, starting from this one.
    Messages(MessageId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    One,
    Later,
    All,
}

impl Scope {
    fn api(self) -> &'static str {
        match self {
            Self::One => "change_one",
            Self::Later => "change_later",
            Self::All => "change_all",
        }
    }
}

pub enum MoveEvent {
    /// The messages moved. `Some` when the open conversation went with them (Puzil follows it).
    Done(Option<Target>),
    Dismiss,
}

pub struct MoveDialog {
    session: Entity<Session>,
    theme: Rc<Theme>,
    mode: MoveMode,
    from: TopicKey,
    /// The topic's name as the server has it (`""` for general chat).
    from_name: String,
    /// The name shown for it.
    from_label: String,
    /// A message in the topic, for whole-topic moves.
    anchor: MessageId,
    scope: Scope,
    channel: StreamId,
    topic: Entity<InputState>,
    /// The highlighted topic suggestion, once the arrow keys were used.
    sel: Option<usize>,
    /// The channel list, while open: its filter field.
    channels: Option<Entity<InputState>>,
    notify_old: bool,
    notify_new: bool,
    busy: bool,
    error: Option<String>,
    /// The server allowed only part of the move (its time limit): from which message, and how many of how many.
    limit: Option<(MessageId, u64, u64)>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<MoveEvent> for MoveDialog {}

impl MoveDialog {
    pub fn new(
        session: Entity<Session>,
        theme: Rc<Theme>,
        mode: MoveMode,
        from: TopicKey,
        anchor: MessageId,
        to: Option<StreamId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (from_name, from_label, can_drop) = {
            let st = &session.read(cx).state;
            // A drop target the messages can go to: we may move out, and can read and post there.
            let can_drop = |sid: StreamId| {
                st.can_move(from.stream_id, Move::Channel)
                    && st.streams.get(&sid).is_some_and(|d| !d.is_archived && st.in_setting(&d.can_send_message_group, st.me))
                    && st.content_access(sid)
            };
            (st.topic_name(&from), st.topic_display(&from), to.is_some_and(can_drop))
        };
        let topic = cx.new(|cx| InputState::new(window, cx).placeholder("Topic"));
        topic.update(cx, |i, cx| {
            i.set_value(from_label.clone(), window, cx);
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        let subs = vec![
            cx.subscribe_in(&topic, window, |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    this.sel = None;
                    this.error = None;
                    this.limit = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.enter(window, cx),
                _ => {}
            }),
            // Topic lists arrive from the server; redraw when they do.
            cx.observe(&session, |_, _, cx| cx.notify()),
        ];
        // A drop on a channel the messages can't go to is just a move within this one.
        let channel = to.filter(|_| can_drop).unwrap_or(from.stream_id);
        session.update(cx, |s, cx| s.load_topics(channel, cx));
        let scope = if matches!(mode, MoveMode::Messages(_)) { Scope::One } else { Scope::All };
        Self {
            session,
            theme,
            mode,
            from,
            from_name,
            from_label,
            anchor,
            scope,
            channel,
            topic,
            sel: None,
            channels: None,
            notify_old: false,
            notify_new: true,
            busy: false,
            error: None,
            limit: None,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    fn typed(&self, cx: &App) -> String {
        self.topic.read(cx).value().trim().to_string()
    }

    /// Existing topics in the destination channel that match what's typed (not the source
    /// topic itself), newest first.
    fn suggestions(&self, cx: &App) -> Vec<String> {
        let q = self.typed(cx).to_lowercase();
        let s = self.session.read(cx);
        let Some(list) = s.topics.get(&self.channel) else { return Vec::new() };
        list.iter()
            .filter(|t| !(self.channel == self.from.stream_id && t.name.to_lowercase() == self.from.topic))
            // General chat by its label: typing that label moves messages there.
            .map(|t| if t.name.is_empty() { s.state.empty_topic_name.clone() } else { t.name.clone() })
            .filter(|n| q.is_empty() || n.to_lowercase().contains(&q))
            .take(6)
            .collect()
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.suggestions(cx).len() as isize;
        if n == 0 {
            return;
        }
        self.sel = Some(match self.sel {
            None if by > 0 => 0,
            None => (n - 1) as usize,
            Some(i) => (i as isize + by).rem_euclid(n) as usize,
        });
        cx.notify();
    }

    fn pick_topic(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.topic.update(cx, |i, cx| i.set_value(name, window, cx));
        self.sel = None;
        cx.notify();
    }

    fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = self.sel.and_then(|i| self.suggestions(cx).get(i).cloned()) {
            return self.pick_topic(name, window, cx);
        }
        self.submit(self.limit.map(|(first, _, _)| first), cx);
    }

    fn toggle_channels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.channels.take().is_none() {
            let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search channels"));
            self._subs.push(cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify()));
            filter.update(cx, |f, cx| f.focus(window, cx));
            self.channels = Some(filter);
        } else {
            // Its filter had focus: back to the topic, so Enter and Escape still reach the dialog.
            self.topic.update(cx, |i, cx| i.focus(window, cx));
        }
        cx.notify();
    }

    fn pick_channel(&mut self, sid: StreamId, window: &mut Window, cx: &mut Context<Self>) {
        self.channel = sid;
        self.channels = None;
        self.sel = None;
        self.error = None;
        // The other channel has its own time limit: the allowed part no longer applies.
        self.limit = None;
        self.session.update(cx, |s, cx| s.load_topics(sid, cx));
        self.topic.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    /// Channels the messages can go to: those we can read, current first.
    fn destinations(&self, cx: &App) -> Vec<(StreamId, String, bool)> {
        let s = self.session.read(cx);
        let mut v: Vec<_> = s
            .state
            .streams
            .values()
            .filter(|st| {
                st.stream_id == self.from.stream_id
                    || (!st.is_archived && s.state.content_access(st.stream_id) && s.state.in_setting(&st.can_send_message_group, s.state.me))
            })
            .map(|st| (st.stream_id, st.name.clone(), st.invite_only))
            .collect();
        v.sort_by_key(|(id, name, _)| (*id != self.from.stream_id, name.to_lowercase()));
        v
    }

    fn counts(&self, cx: &App) -> (Option<usize>, Option<usize>) {
        let MoveMode::Messages(id) = self.mode else { return (None, None) };
        let s = self.session.read(cx);
        let conv = Conv::Topic(self.from.clone());
        let ids = s.conv_ids(&conv);
        let later = ids.iter().filter(|&&m| m >= id).count();
        let all = s.conv_data(&conv).is_some_and(|d| d.found_oldest).then_some(ids.len());
        (Some(later), all)
    }

    /// Sends the move; `from` (a message and the count allowed) retries just the part the
    /// time limit allows.
    fn submit(&mut self, part: Option<MessageId>, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let typed = self.typed(cx);
        if typed.is_empty() {
            self.error = Some("Type a topic name.".into());
            return cx.notify();
        }
        let topic = self.session.read(cx).state.topic_from_typed(&typed);
        let same_channel = self.channel == self.from.stream_id;
        if topic != self.from_name && !self.session.read(cx).state.can_move(self.from.stream_id, Move::Topic) {
            self.error = Some("You can move these messages to another channel, but not to another topic.".into());
            return cx.notify();
        }
        if same_channel && topic == self.from_name {
            self.error = Some(if self.mode == MoveMode::Rename { "Type a new name for the topic.".into() } else { "Pick another topic or channel.".into() });
            return cx.notify();
        }
        let (message, scope) = match (part, self.mode) {
            (Some(first), _) => (first, "change_later"),
            (None, MoveMode::Messages(id)) => (id, self.scope.api()),
            (None, _) => (self.anchor, "change_all"),
        };
        let req = MoveRequest {
            message,
            scope,
            stream_id: (!same_channel).then_some(self.channel),
            topic: (topic != self.from_name).then(|| topic.clone()),
            notify_old: self.notify_old,
            notify_new: self.notify_new,
        };
        // Follow the open conversation when the move takes it (or its newest messages) along.
        let follow = !matches!((self.mode, self.scope), (MoveMode::Messages(_), Scope::One));
        let dest = Target::Conv(Conv::Topic(TopicKey::new(self.channel, &topic)));
        let new_name = topic.clone();
        let client = self.session.read(cx).client.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { client.move_messages(&req).await }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(()) => {
                        // Remember the display name so the destination shows it before events land.
                        let key = TopicKey::new(this.channel, &new_name);
                        this.session.update(cx, |s, _| {
                            s.state.topic_names.insert(key, new_name.clone());
                        });
                        cx.emit(MoveEvent::Done(follow.then_some(dest)));
                    }
                    Err(ApiError::MoveTimeLimit { first_allowed: Some(first), allowed, total, .. }) if allowed > 0 => {
                        this.limit = Some((first, allowed, total));
                    }
                    Err(ApiError::MoveTimeLimit { msg, .. }) => this.error = Some(msg),
                    Err(ApiError::Api { msg, .. }) => this.error = Some(msg),
                    Err(e) => this.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn title(&self) -> &'static str {
        match self.mode {
            MoveMode::Rename => "Rename topic",
            MoveMode::Topic => "Move topic",
            MoveMode::Messages(_) => "Move messages",
        }
    }
}

impl Render for MoveDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let s = self.session.read(cx);
        let from_channel = s.state.streams.get(&self.from.stream_id).map(|st| st.name.clone()).unwrap_or_default();
        let can_change_channel = self.mode != MoveMode::Rename && s.state.can_move(self.from.stream_id, Move::Channel);
        let channel_name = s.state.streams.get(&self.channel).map(|st| st.name.clone()).unwrap_or_default();
        let private = s.state.streams.get(&self.channel).is_some_and(|st| st.invite_only);
        let topic_focused = self.topic.read(cx).focus_handle(cx).is_focused(window);
        let typed = self.typed(cx);
        let suggestions = if topic_focused { self.suggestions(cx) } else { Vec::new() };
        // Whether what's typed names an existing topic there (the whole list, not just the six shown).
        let exact = !typed.is_empty() && {
            let s = self.session.read(cx);
            let want = s.state.topic_from_typed(&typed).to_lowercase();
            let is_source = self.channel == self.from.stream_id && want == self.from.topic;
            !is_source && s.topics.get(&self.channel).is_some_and(|l| l.iter().any(|t| t.name.to_lowercase() == want))
        };
        let (later, all) = self.counts(cx);

        let mut body = div().flex().flex_col().gap(px(14.));

        if let MoveMode::Messages(_) = self.mode {
            let opt = |id: &'static str, label: String, scope: Scope, cx: &mut Context<Self>| {
                kit::segment(&t, id, label, self.scope == scope, true).on_click(cx.listener(move |this, _, _, cx| {
                    this.scope = scope;
                    this.limit = None;
                    cx.notify();
                }))
            };
            let later_label = later.map(|n| format!("This and later ({n})")).unwrap_or("This and later".into());
            let all_label = all.map(|n| format!("Whole topic ({n})")).unwrap_or("Whole topic".into());
            body = body.child(
                div().child(kit::label(&t, "Which messages")).child(
                    kit::segments(&t)
                        .child(opt("scope-one", "This message".into(), Scope::One, cx))
                        .child(div().w(px(1.)).bg(t.divider))
                        .child(opt("scope-later", later_label, Scope::Later, cx))
                        .child(div().w(px(1.)).bg(t.divider))
                        .child(opt("scope-all", all_label, Scope::All, cx)),
                ),
            );
        }

        if self.mode != MoveMode::Rename {
            let mut channel = div().child(kit::label(&t, "Channel")).child(
                kit::picker(&t, "channel-picker", format!("# {channel_name}"), can_change_channel)
                    .when(private, |d| d.child(icons::icon("lock", 13., t.muted)))
                    .when(can_change_channel, |d| d.on_click(cx.listener(|this, _, window, cx| this.toggle_channels(window, cx)))),
            );
            if let Some(filter) = &self.channels {
                let q = filter.read(cx).value().to_lowercase();
                let rows: Vec<AnyElement> = self
                    .destinations(cx)
                    .into_iter()
                    .filter(|(_, n, _)| q.is_empty() || n.to_lowercase().contains(&q))
                    .take(50)
                    .map(|(sid, name, private)| {
                        kit::menu_item(&t, ("dest", sid as usize), name, sid == self.channel)
                            .child(div().flex_none().when(private, |d| d.child(icons::icon("lock", 12., t.muted))))
                            .on_click(cx.listener(move |this, _, window, cx| this.pick_channel(sid, window, cx)))
                            .into_any_element()
                    })
                    .collect();
                channel = channel.child(
                    kit::menu(&t, "dest-menu", 400.)
                        .mt_1()
                        .child(div().pb_1().child(kit::field(&t, filter, true)))
                        .child(div().id("dest-list").max_h(px(220.)).overflow_y_scroll().children(rows)),
                );
            } else if !can_change_channel && self.mode != MoveMode::Rename {
                channel = channel.child(kit::hint(&t, "You can't move messages out of this channel."));
            }
            body = body.child(channel);
        }

        let mut topic = div().child(kit::label(&t, "Topic")).child(kit::field(&t, &self.topic, topic_focused));
        if !suggestions.is_empty() || (topic_focused && !typed.is_empty() && !exact && self.mode != MoveMode::Rename) {
            let mut list = kit::menu(&t, "topic-suggestions", 400.).mt_1();
            for (i, name) in suggestions.iter().enumerate() {
                let n = name.clone();
                list = list.child(
                    kit::menu_item(&t, ("sugg", i), name.clone(), false)
                        .when(self.sel == Some(i), |d| d.bg(t.bg_hover))
                        .child(div().flex_none().text_size(px(12.)).text_color(t.muted).child("existing topic"))
                        .on_click(cx.listener(move |this, _, window, cx| this.pick_topic(n.clone(), window, cx))),
                );
            }
            if !typed.is_empty() && !exact {
                list = list.child(
                    div()
                        .flex()
                        .flex_row()
                        .px_2()
                        .h(px(30.))
                        .items_center()
                        .text_size(px(14.))
                        .child(div().flex_1().child(typed.clone()))
                        .child(div().text_size(px(12.)).text_color(t.muted).child("new topic")),
                );
            }
            topic = topic.child(list);
        }
        if exact {
            topic = topic.child(kit::hint(&t, "That topic exists: the messages merge into it."));
        } else if self.mode != MoveMode::Rename {
            topic = topic.child(kit::hint(&t, "Picking an existing topic merges these messages into it."));
        }
        body = body.child(topic);

        body = body.child(
            div()
                .flex()
                .flex_col()
                .child(kit::checkbox(&t, "notify-old", self.notify_old, "Post a notice in the old topic").on_click(cx.listener(|this, _, _, cx| {
                    this.notify_old = !this.notify_old;
                    cx.notify();
                })))
                .child(kit::checkbox(&t, "notify-new", self.notify_new, "Post a notice in the new topic").on_click(cx.listener(|this, _, _, cx| {
                    this.notify_new = !this.notify_new;
                    cx.notify();
                }))),
        );

        if let Some((_, allowed, total)) = self.limit {
            body = body.child(kit::note(
                &t,
                true,
                div().child(format!(
                    "Only recent messages can be moved {}. {allowed} of the {total} can move.",
                    if self.channel != self.from.stream_id { "to another channel" } else { "to another topic" }
                )),
            ));
        }
        if let Some(e) = &self.error {
            body = body.child(div().text_size(px(13.)).text_color(t.danger).child(e.clone()));
        }

        let action = match (self.limit, self.mode) {
            (Some((_, n, _)), _) => format!("Move those {n}"),
            (None, MoveMode::Rename) => "Rename".into(),
            (None, MoveMode::Messages(_)) => match (self.scope, later, all) {
                (Scope::One, _, _) => "Move 1 message".into(),
                (Scope::Later, Some(n), _) => format!("Move {n} message{}", if n == 1 { "" } else { "s" }),
                (Scope::All, _, Some(n)) => format!("Move {n} message{}", if n == 1 { "" } else { "s" }),
                _ => "Move messages".into(),
            },
            (None, MoveMode::Topic) => "Move topic".into(),
        };
        let limit = self.limit;
        div()
            .id("move-dialog")
            .key_context("MoveDialog")
            .track_focus(&self.focus)
            .w(px(440.))
            .p_5()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .bg(t.popover)
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            .text_color(t.fg)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(|this, _: &ComposerUp, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &ComposerDown, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &ComposerEscape, window, cx| {
                if this.channels.take().is_some() {
                    this.topic.update(cx, |i, cx| i.focus(window, cx));
                    cx.notify();
                } else if this.sel.take().is_some() {
                    cx.notify();
                } else {
                    cx.emit(MoveEvent::Dismiss);
                }
            }))
            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(t.fg_strong).child(self.title()))
            .child(
                div()
                    .pt_1()
                    .pb_4()
                    .text_size(px(13.))
                    .text_color(t.muted)
                    .child(format!("From #{from_channel} › {}", self.from_label)),
            )
            .child(body)
            .child(
                div()
                    .pt(px(18.))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(kit::secondary(&t, "move-cancel", "Cancel").on_click(cx.listener(|_, _, _, cx| cx.emit(MoveEvent::Dismiss))))
                    .child(
                        kit::primary(&t, "move-go", if self.busy { "Moving…".to_string() } else { action }, !self.busy).on_click(cx.listener(
                            move |this, _, _, cx| this.submit(limit.map(|(first, _, _)| first), cx),
                        )),
                    ),
            )
    }
}
