//! A person's card, opened by clicking their avatar or name: who they are, and a button to
//! message them directly.

use gpui_kit::{
    component::input::{Input, InputEvent, InputState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::model::{Conv, DmKey};

use super::{Conversation, ConversationEvent, Target};
use crate::icons;

fn role_name(role: u32, bot: bool, guest: bool) -> &'static str {
    match (bot, guest, role) {
        (true, _, _) => "Bot",
        (_, true, _) | (_, _, 600) => "Guest",
        (_, _, 100) => "Owner",
        (_, _, 200) => "Administrator",
        (_, _, 300) => "Moderator",
        _ => "Member",
    }
}

impl Conversation {
    pub(super) fn open_profile(&mut self, user: u64, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.profile = Some((user, at));
        // A one-off message box: Enter sends a DM without leaving this conversation.
        let first = self.session.read(cx).state.users.get(&user).map(|u| u.full_name.clone()).unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(format!("Message @{first}")));
        let sub = cx.subscribe_in(&input, window, move |this, input, e: &InputEvent, window, cx| {
            if matches!(e, InputEvent::PressEnter { .. }) {
                let text = input.read(cx).value().trim().to_string();
                if text.is_empty() {
                    return;
                }
                let me = this.session.read(cx).state.me;
                this.session.update(cx, |s, cx| s.send(Conv::Dm(DmKey::new(me, [user])), String::new(), text, cx));
                this.profile_sent = true;
                input.update(cx, |i, cx| i.set_value("", window, cx));
                cx.notify();
            }
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        self.profile_input = Some((input, sub));
        self.profile_sent = false;
        cx.notify();
    }

    fn close_profile(&mut self, cx: &mut Context<Self>) {
        self.profile_input = None;
        if self.profile.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn render_profile(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (uid, at) = self.profile?;
        let t = self.theme.clone();
        let (name, email, role, avatar_url, me) = {
            let s = self.session.read(cx);
            let u = s.state.users.get(&uid)?;
            // Zulip hides most addresses behind `user123@…` placeholders; those say nothing.
            let shown = u.delivery_email.clone().filter(|e| !e.is_empty()).unwrap_or_else(|| u.email.clone());
            let placeholder = shown.strip_prefix("user").is_some_and(|r| r.split('@').next().is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())));
            (u.full_name.clone(), (!placeholder).then_some(shown), role_name(u.role, u.is_bot, u.is_guest), u.avatar_url.clone(), s.state.me)
        };
        let img = self.images.update(cx, |i, cx| i.avatar(uid, avatar_url.as_deref(), cx));
        let banner = t.user_color(uid);
        let card = div()
            .id("profile-card")
            .occlude()
            .w(px(300.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .overflow_hidden()
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_profile(cx)))
            .child(div().h(px(60.)).bg(banner))
            .child(
                div()
                    .relative()
                    .px_4()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .mt(px(-40.))
                            .mb_1()
                            .size(px(88.))
                            .rounded_full()
                            .bg(t.popover)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(super::super::avatar::avatar(img, 80., t.user_color(uid), &name)),
                    )
                    .child(div().text_size(px(20.)).line_height(px(26.)).font_weight(FontWeight::BOLD).text_color(t.fg_strong).child(name.clone()))
                    .when_some(email, |d, e| d.child(div().text_size(px(14.)).text_color(t.muted).child(e)))
                    .child(
                        div().pt_1().flex().flex_row().child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded(px(4.))
                                .bg(t.bg_selected.opacity(0.6))
                                .text_size(px(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.fg)
                                .child(role),
                        ),
                    )
                    .when(uid != me, |d| {
                        let open = div()
                            .id("profile-open-dm")
                            .flex_none()
                            .size(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.bg_selected.opacity(0.7)))
                            .child(icons::icon("messages-square", 18., t.icon))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_profile(cx);
                                let me = this.session.read(cx).state.me;
                                cx.emit(ConversationEvent::Navigate(Target::Conv(Conv::Dm(DmKey::new(me, [uid])))));
                            }));
                        // One row: a message box that sends on Enter, and at its end a button
                        // that opens the conversation instead.
                        d.child(
                            div()
                                .mt_3()
                                .h(px(40.))
                                .pl_1()
                                .pr_1()
                                .rounded(px(6.))
                                .bg(t.composer_bg)
                                .border_1()
                                .border_color(t.composer_border)
                                .flex()
                                .flex_row()
                                .items_center()
                                .children(
                                    self.profile_input
                                        .as_ref()
                                        .map(|(i, _)| div().flex_1().min_w_0().child(Input::new(i).appearance(false))),
                                )
                                .child(open),
                        )
                        .when(self.profile_sent, |d| {
                            d.child(div().pt_1().text_size(px(12.)).text_color(t.muted).child("Sent. Keep typing to send another."))
                        })
                    }),
            );
        let card = super::super::motion::pop_in(("profile-in", uid as usize), card);
        Some(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(card)).with_priority(1).into_any_element())
    }
}
