//! The right-click menu on a message (reply and thread, edit, copy, mark unread, delete), and
//! the small reaction picker behind a reaction row's add button.

use gpui_kit::{prelude::FluentBuilder as _, *};
use puzil_render::MessageAst;
use puzil_zulip::types::MessageId;

use super::Conversation;
use crate::icons;

impl Conversation {
    pub(super) fn open_menu(&mut self, id: MessageId, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some((id, at));
        cx.notify();
    }

    pub(super) fn open_picker(&mut self, id: MessageId, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.picker = Some((id, at));
        cx.notify();
    }

    fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn render_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, at) = self.picker?;
        let t = self.theme.clone();
        let info = {
            let s = self.session.read(cx);
            let m = s.state.messages.get(&id)?;
            self.info(m, cx)
        };
        let mut row = div()
            .id("reaction-picker")
            .occlude()
            .flex()
            .flex_row()
            .gap_1()
            .p_1()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_picker(cx)));
        for (k, (name, code, glyph)) in puzil_overlay::QUICK.iter().enumerate() {
            let mine = info.my_codes.iter().any(|c| c == code);
            row = row.child(
                div()
                    .id(("pick", k))
                    .size(px(34.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .when(mine, |d| d.bg(t.reaction_mine_bg))
                    .hover(|s| s.bg(t.bg_selected))
                    .cursor_pointer()
                    .text_size(px(19.))
                    .child(*glyph)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_picker(cx);
                        this.toggle_reaction(id, name.to_string(), code.to_string(), "unicode_emoji".into(), mine, cx)
                    })),
            );
        }
        let row = super::super::motion::pop_in(("picker-in", id as usize), row);
        Some(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(row)).with_priority(1).into_any_element())
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Marks this message and everything after it in the conversation unread, and stops
    /// marking them read again until the conversation is reopened (as Zulip's web app does).
    fn mark_unread_from(&mut self, id: MessageId, cx: &mut Context<Self>) {
        self.read_paused = true;
        let ids: Vec<MessageId> = self.shown_ids().into_iter().filter(|m| *m >= id).collect();
        self.session.update(cx, |s, cx| {
            s.call(cx, move |c| async move { c.update_flags(&ids, false, "read").await }, |_, e, _| tracing::warn!("mark unread failed: {e}"))
        });
    }

    pub(super) fn render_menu(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, at) = self.menu?;
        let t = self.theme.clone();
        let (info, text, link) = {
            let s = self.session.read(cx);
            let m = s.state.messages.get(&id)?;
            let link = m.stream_id.map(|sid| puzil_overlay::message_link(&s.account.realm, sid, &m.subject, id));
            (self.info(m, cx), MessageAst::from_html(&m.content).plain_text(), link)
        };
        let item = |key: &'static str, label: &'static str, ic: &'static str, danger: bool| {
            let fg = if danger { t.danger } else { t.fg };
            div()
                .id(key)
                .group(key)
                .flex()
                .flex_row()
                .items_center()
                .h(px(32.))
                .px_2()
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(14.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(fg)
                .hover(|s| s.bg(if danger { t.danger } else { t.accent }).text_color(white()))
                .child(div().flex_1().child(label))
                .child(icons::icon(ic, 16., fg).group_hover(key, |s| s.text_color(white())))
        };
        let sep = || div().h(px(1.)).mx_1().my_1().bg(t.divider);
        let mut menu = div()
            .id("message-menu")
            .occlude()
            .w(px(220.))
            .p_1p5()
            .flex()
            .flex_col()
            .rounded(px(6.))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)));

        let mut actions = 0;

        if info.stream_id.is_none() {
            let i2 = info.clone();
            menu = menu.child(item("menu-reply", "Reply", "corner-up-left", false).on_click(cx.listener(move |this, _, w, cx| {
                this.close_menu(cx);
                this.start_reply(&i2, w, cx)
            })));
            actions += 1;
        }
        if info.stream_id.is_some() && self.overlay.is_some() {
            let i2 = info.clone();
            menu = menu
                .child(item("menu-reply", "Reply", "corner-up-left", false).on_click(cx.listener(move |this, _, w, cx| {
                    this.close_menu(cx);
                    this.start_reply(&i2, w, cx)
                })))
                .child(item("menu-thread", "Start thread", "messages-square", false).on_click(cx.listener(move |this, _, w, cx| {
                    this.close_menu(cx);
                    this.ask_thread(id, w, cx)
                })));
            actions += 2;
        }
        if info.mine {
            menu = menu.child(item("menu-edit", "Edit message", "pencil", false).on_click(cx.listener(move |this, _, w, cx| {
                this.close_menu(cx);
                this.composer.update(cx, |c, cx| c.start_edit(id, w, cx));
            })));
            actions += 1;
        }
        if actions > 0 {
            menu = menu.child(sep());
        }
        menu = menu.child(item("menu-copy", "Copy text", "copy", false).on_click(cx.listener(move |this, _, _, cx| {
            this.close_menu(cx);
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        })));
        if let Some(link) = link {
            menu = menu.child(item("menu-link", "Copy message link", "link", false).on_click(cx.listener(move |this, _, _, cx| {
                this.close_menu(cx);
                cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
            })));
        }
        menu = menu.child(item("menu-unread", "Mark unread", "mail", false).on_click(cx.listener(move |this, _, _, cx| {
            this.close_menu(cx);
            this.mark_unread_from(id, cx)
        })));
        if info.mine {
            menu = menu.child(sep()).child(item("menu-delete", "Delete message", "trash", true).on_click(cx.listener(
                move |this, _, w, cx| {
                    this.close_menu(cx);
                    this.delete(id, w, cx)
                },
            )));
        }
        let _ = window;
        let menu = super::super::motion::pop_in(("menu-in", id as usize), menu);
        Some(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(1).into_any_element())
    }
}
