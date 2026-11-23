//! Every channel in the organization, grouped by folder, with Join and Leave: how to see and
//! change what the sidebar shows.

use std::{collections::HashMap, rc::Rc};

use gpui_kit::{
    component::input::{Input, InputEvent, InputState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::types::StreamId;

use crate::{icons, session::Session, theme::Theme};

pub struct ChannelBrowser {
    session: Entity<Session>,
    theme: Rc<Theme>,
    filter: Entity<InputState>,
    /// Joins (true) and leaves (false) sent but not yet reflected in our subscriptions.
    pending: HashMap<StreamId, bool>,
    _subs: Vec<Subscription>,
}

impl ChannelBrowser {
    pub fn new(session: Entity<Session>, theme: Rc<Theme>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search channels"));
        let subs = vec![
            cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify()),
            // Subscription events land through the session; redraw when they do.
            cx.observe(&session, |this, session, cx| {
                let subs = &session.read(cx).state.subs;
                this.pending.retain(|sid, join| subs.contains_key(sid) != *join);
                cx.notify();
            }),
        ];
        filter.update(cx, |f, cx| f.focus(window, cx));
        Self { session, theme, filter, pending: HashMap::new(), _subs: subs }
    }

    pub fn set_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    fn toggle(&mut self, sid: StreamId, name: String, join: bool, cx: &mut Context<Self>) {
        self.pending.insert(sid, join);
        cx.notify();
        let this = cx.entity().downgrade();
        self.session.update(cx, |s, cx| {
            s.call(
                cx,
                move |c| async move { if join { c.subscribe(&[name]).await } else { c.unsubscribe(&[name]).await } },
                move |_, e, cx| {
                    tracing::warn!("changing subscription failed: {e}");
                    let _ = this.update(cx, |b, cx| {
                        b.pending.remove(&sid);
                        cx.notify();
                    });
                },
            )
        });
    }
}

impl Render for ChannelBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let q = self.filter.read(cx).value().to_lowercase();
        let s = self.session.read(cx);
        let mut channels: Vec<_> = s
            .state
            .streams
            .values()
            .filter(|st| !st.is_archived)
            .filter(|st| q.is_empty() || st.name.to_lowercase().contains(&q) || st.description.to_lowercase().contains(&q))
            .collect();
        channels.sort_by_key(|st| st.name.to_lowercase());
        let total = channels.len();
        let joined = channels.iter().filter(|st| s.state.subs.contains_key(&st.stream_id)).count();
        let folders: Vec<_> = s.state.realm.channel_folders.iter().filter(|f| !f.is_archived).collect();
        let mut groups: Vec<(String, Vec<_>)> = folders
            .iter()
            .map(|f| (f.name.clone(), channels.iter().filter(|st| st.folder_id == Some(f.id)).copied().collect::<Vec<_>>()))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let rest: Vec<_> =
            channels.iter().filter(|st| !st.folder_id.is_some_and(|id| folders.iter().any(|f| f.id == id))).copied().collect();
        if !rest.is_empty() {
            groups.push((if groups.is_empty() { "Channels".into() } else { "Other channels".into() }, rest));
        }

        let mut rows: Vec<AnyElement> = Vec::new();
        for (label, members) in groups {
            rows.push(
                div().pt_5().pb_1().px_2().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(label).into_any_element(),
            );
            for st in members {
                let sid = st.stream_id;
                let subscribed = s.state.subs.contains_key(&sid);
                let busy = self.pending.contains_key(&sid);
                let name = st.name.clone();
                let description = st.description.split_whitespace().collect::<Vec<_>>().join(" ");
                let button = div()
                    .id(("join", sid as usize))
                    .group("join")
                    .flex_none()
                    .w(px(84.))
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .rounded(px(4.))
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .map(|d| match (busy, subscribed) {
                        (true, _) => d.bg(t.bg_selected).text_color(t.muted).child("…"),
                        // "Joined", which reads "Leave" under the pointer.
                        (false, true) => d
                            .relative()
                            .bg(t.bg_selected.opacity(0.6))
                            .text_color(t.fg)
                            .hover(|s| s.bg(t.danger))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .group_hover("join", |s| s.invisible())
                                    .child(icons::icon("check", 14., t.fg))
                                    .child("Joined"),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(white())
                                    .invisible()
                                    .group_hover("join", |s| s.visible())
                                    .child("Leave"),
                            ),
                        (false, false) => d.bg(t.accent).text_color(white()).hover(|s| s.opacity(0.88)).child("Join"),
                    })
                    .when(!busy, |d| {
                        let name = name.clone();
                        d.on_click(cx.listener(move |this, _, _, cx| this.toggle(sid, name.clone(), !subscribed, cx)))
                    });
                rows.push(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .px_2()
                        .py_2()
                        .rounded(px(6.))
                        .hover(|s| s.bg(t.bg_hover))
                        .child(icons::channel(st.invite_only, 20., t.faint, t.bg))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_color(t.fg_strong).font_weight(FontWeight::MEDIUM).child(name))
                                .when(!description.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(t.muted)
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .child(description),
                                    )
                                }),
                        )
                        .child(button)
                        .into_any_element(),
                );
            }
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .child(
                crate::theme::drag_region(div().id("browser-header"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(48.))
                    .px_4()
                    .flex_none()
                    .border_b_1()
                    .border_color(t.border)
                    .child(icons::icon("compass", 20., t.faint))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(t.fg_strong).child("Browse channels"))
                    .child(div().text_size(px(13.)).text_color(t.muted).child(format!("{total} channels · {joined} joined"))),
            )
            .child(
                div()
                    .id("browser-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .max_w(px(760.))
                            .mx_auto()
                            .px_6()
                            .py_4()
                            .child(div().h(px(40.)).px_2().rounded(px(6.)).bg(t.composer_bg).border_1().border_color(t.composer_border).flex().items_center().child(Input::new(&self.filter).appearance(false)))
                            .children(rows),
                    ),
            )
    }
}
