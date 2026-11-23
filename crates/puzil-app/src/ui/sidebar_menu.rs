//! Right-click menus on the sidebar's channels, topics and direct messages.

use gpui_kit::*;
use puzil_zulip::{
    model::{DmKey, TopicKey},
    types::{StreamId, Visibility},
};
use serde_json::json;

use super::sidebar::Sidebar;
use crate::{icons, session::Session};

#[derive(Debug, Clone)]
pub enum SideMenu {
    Channel(StreamId),
    /// A topic: its key, display name, and a message in it (renames go through one).
    Topic(TopicKey, String, u64),
    Dm(DmKey),
}

/// Runs a server call for a menu item, logging a failure.
fn call<F, Fut>(session: &Entity<Session>, cx: &mut App, what: &'static str, f: F)
where
    F: FnOnce(puzil_zulip::Client) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), puzil_zulip::ApiError>> + Send + 'static,
{
    session.update(cx, |s, cx| s.call(cx, f, move |_, e, _| tracing::warn!("{what} failed: {e}")));
}

impl Sidebar {
    pub(super) fn open_menu(&mut self, m: SideMenu, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some((m, at));
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (m, at) = self.menu.clone()?;
        let m_key = m.clone();
        let t = self.theme.clone();
        let session = self.session.clone();
        let (realm, me) = {
            let s = session.read(cx);
            (s.account.realm.clone(), s.state.me)
        };
        let item = |key: &'static str, label: String, ic: &'static str, danger: bool| {
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
        let copy = |key: &'static str, link: String| {
            item(key, "Copy link".into(), "link", false).on_click(cx.listener(move |this, _, _, cx| {
                this.close_menu(cx);
                cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
            }))
        };
        let mark_read = |key: &'static str, narrow: serde_json::Value| {
            let session = session.clone();
            item(key, "Mark as read".into(), "check-check", false).on_click(cx.listener(move |this, _, _, cx| {
                this.close_menu(cx);
                let n = narrow.clone();
                call(&session, cx, "mark as read", move |c| async move { c.mark_narrow_read(&n).await });
            }))
        };
        let mut menu = div()
            .id("sidebar-menu")
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
        match m {
            SideMenu::Channel(sid) => {
                let (name, muted) = {
                    let s = session.read(cx);
                    (
                        s.state.streams.get(&sid).map(|st| st.name.clone()).unwrap_or_default(),
                        s.state.subs.get(&sid).is_some_and(|sub| sub.is_muted),
                    )
                };
                let link = format!("{realm}/#narrow/channel/{sid}");
                let s2 = session.clone();
                let s3 = session.clone();
                menu = menu
                    .child(mark_read("m-read", json!([{ "operator": "channel", "operand": sid }])))
                    .child(item("m-mute", if muted { "Unmute channel" } else { "Mute channel" }.into(), "bell-off", false).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.close_menu(cx);
                            call(&s2, cx, "muting", move |c| async move { c.set_subscription_property(sid, "is_muted", json!(!muted)).await });
                        }),
                    ))
                    .child(copy("m-link", link))
                    .child(sep())
                    .child(item("m-leave", "Leave channel".into(), "log-out", true).on_click(cx.listener(move |this, _, window, cx| {
                        this.close_menu(cx);
                        let answer = window.prompt(
                            PromptLevel::Warning,
                            &format!("Leave #{name}?"),
                            Some("It disappears from the sidebar. You can join again from Browse channels."),
                            &["Leave", "Cancel"],
                            cx,
                        );
                        let (session, name) = (s3.clone(), name.clone());
                        cx.spawn(async move |_, cx| {
                            if answer.await.ok() == Some(0) {
                                let _ = cx.update(|cx| call(&session, cx, "leaving", move |c| async move { c.unsubscribe(&[name]).await }));
                            }
                        })
                        .detach();
                    })));
            }
            SideMenu::Topic(k, name, anchor) => {
                let policy = session.read(cx).state.user_topics.get(&k).copied().unwrap_or(Visibility::Inherit);
                let link = format!("{realm}/#narrow/channel/{}/topic/{}", k.stream_id, puzil_overlay::encode_hash_component(&name));
                let resolved = name.starts_with(puzil_overlay::RESOLVED_PREFIX);
                let renamed = if resolved {
                    puzil_overlay::unresolved(&name).to_string()
                } else {
                    format!("{}{name}", puzil_overlay::RESOLVED_PREFIX)
                };
                let sid = k.stream_id;
                let set = |key: &'static str, label: &str, ic: &'static str, to: u8| {
                    let (session, topic) = (session.clone(), name.clone());
                    item(key, label.into(), ic, false).on_click(cx.listener(move |this, _, _, cx| {
                        this.close_menu(cx);
                        let topic = topic.clone();
                        call(&session, cx, "topic setting", move |c| async move { c.set_topic_visibility(sid, &topic, to).await });
                    }))
                };
                let s2 = session.clone();
                menu = menu.child(mark_read(
                    "m-read",
                    json!([{ "operator": "channel", "operand": sid }, { "operator": "topic", "operand": name }]),
                ));
                menu = match policy {
                    Visibility::Followed => menu.child(set("m-follow", "Unfollow topic", "bell-off", 0)),
                    _ => menu.child(set("m-follow", "Follow topic", "bell", 3)),
                };
                menu = match policy {
                    Visibility::Muted => menu.child(set("m-mute", "Unmute topic", "volume-x", 0)),
                    _ => menu.child(set("m-mute", "Mute topic", "volume-x", 1)),
                };
                menu = menu
                    .child(
                        item("m-resolve", if resolved { "Mark as unresolved" } else { "Mark as resolved" }.into(), "circle-check", false)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_menu(cx);
                                let to = renamed.clone();
                                call(&s2, cx, "resolving", move |c| async move { c.rename_topic(anchor, &to).await });
                            })),
                    )
                    .child(copy("m-link", link));
            }
            SideMenu::Dm(k) => {
                let others: Vec<u64> = k.others(me).collect();
                let ids = others.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
                let link = format!("{realm}/#narrow/dm/{ids}-dm");
                menu = menu.child(mark_read("m-read", json!([{ "operator": "dm", "operand": others }]))).child(copy("m-link", link));
            }
        }
        let key = match &m_key {
            SideMenu::Channel(sid) => format!("c{sid}"),
            SideMenu::Topic(k, _, _) => format!("t{}{}", k.stream_id, k.topic),
            SideMenu::Dm(k) => format!("d{:?}", k.0),
        };
        let menu = super::motion::pop_in(SharedString::from(format!("side-menu-{key}")), menu);
        Some(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(1).into_any_element())
    }
}
