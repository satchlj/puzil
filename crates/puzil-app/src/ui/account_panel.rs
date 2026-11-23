//! The panel the account card opens: whether you show as online, light or dark mode, and
//! the way into full Settings.

use gpui_kit::{prelude::FluentBuilder as _, *};
use serde_json::{Value, json};

use super::sidebar::{Sidebar, SidebarEvent};
use crate::{
    icons,
    settings::{self, Mode, Scope},
};

impl Sidebar {
    /// Quick changes go where the setting currently comes from: this server's override if
    /// it has one, else the app default.
    fn quick_scope(&self, overridden: bool, cx: &App) -> Scope {
        if overridden { Scope::Server(self.session.read(cx).account.realm.clone()) } else { Scope::App }
    }

    pub(super) fn render_account_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.account_panel {
            return None;
        }
        let t = self.theme.clone();
        let realm = self.session.read(cx).account.realm.clone();
        let st = settings::handle(cx).read(cx);
        let layer = st.layer(&Scope::Server(realm.clone()));
        let eff = st.resolve(Some(&realm));
        let mode_scope = self.quick_scope(layer.mode.is_some(), cx);
        let online = self.session.read(cx).user_settings.get("presence_enabled").and_then(Value::as_bool);

        let label = |text: &str| div().px_1().pt_2().pb_1().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(text.to_string());
        let modes = div().flex().flex_row().p_0p5().gap_0p5().rounded(px(6.)).bg(t.input_bg).children(
            [("System", Mode::System, "monitor"), ("Light", Mode::Light, "sun"), ("Dark", Mode::Dark, "moon")].into_iter().map(|(name, m, ic)| {
                let selected = eff.mode == m;
                let scope = mode_scope.clone();
                div()
                    .id(SharedString::from(format!("qp-mode-{name}")))
                    .flex_1()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .when(selected, |d| d.bg(t.panel).text_color(t.fg_strong).shadow_sm())
                    .when(!selected, |d| d.text_color(t.muted))
                    .child(icons::icon(ic, 13., if selected { t.fg_strong } else { t.icon }))
                    .child(name)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let scope = scope.clone();
                        settings::handle(cx).update(cx, |s, cx| s.update(&scope, |l| l.mode = Some(m), cx));
                    }))
            }),
        );
        // Same segmented control as the mode switch: Online or Invisible (Zulip presence).
        let presence = online.map(|on| {
            div().flex().flex_row().p_0p5().gap_0p5().rounded(px(6.)).bg(t.input_bg).children(
                [("Online", true), ("Invisible", false)].into_iter().map(|(name, value)| {
                    let selected = on == value;
                    let dot = if value { hsla(0.38, 0.62, 0.45, 1.) } else { t.faint };
                    div()
                        .id(SharedString::from(format!("qp-presence-{name}")))
                        .flex_1()
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_1p5()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .when(selected, |d| d.bg(t.panel).text_color(t.fg_strong).shadow_sm())
                        .when(!selected, |d| d.text_color(t.muted))
                        .child(div().size(px(8.)).rounded_full().bg(dot))
                        .child(name)
                        .when(!selected, |d| {
                            d.on_click(cx.listener(move |this, _, _, cx| {
                                this.session.update(cx, |s, cx| s.set_zulip_setting("presence_enabled", json!(value), cx))
                            }))
                        })
                }),
            )
        });
        let panel = div()
            .id("account-panel")
            .occlude()
            .flex()
            .flex_col()
            .p_2()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.account_panel = false;
                this.panel_closed_at = Some(std::time::Instant::now());
                cx.notify();
            }))
            .when(presence.is_some(), |d| d.child(label("Status")))
            .children(presence)
            .child(label("Mode"))
            .child(modes)
            .child(div().h(px(1.)).my_2().bg(t.divider))
            .children(crate::updates::handle(cx).read(cx).available.clone().map(|(v, url)| {
                div()
                    .id("qp-update")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(34.))
                    .px_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .bg(t.accent.opacity(0.12))
                    .hover(|s| s.bg(t.accent.opacity(0.2)))
                    .child(icons::icon("arrow-down", 16., t.accent))
                    .child(div().flex_1().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(t.fg).child(format!("Puzil {v} is available")))
                    .on_click(move |_, _, cx| cx.open_url(&url))
            }))
            .child(
                div()
                    .id("qp-add-server")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(34.))
                    .px_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_hover))
                    .child(icons::icon("plus", 16., t.icon))
                    .child(div().flex_1().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(t.fg).child("Add a server"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.account_panel = false;
                        crate::servers::request(cx, crate::servers::ServersEvent::Add);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("qp-settings")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(34.))
                    .px_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_hover))
                    .child(icons::icon("settings", 16., t.icon))
                    .child(div().flex_1().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(t.fg).child("Settings"))
                    .child(icons::icon("chevron-right", 14., t.faint))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.account_panel = false;
                        cx.emit(SidebarEvent::Settings);
                        cx.notify();
                    })),
            );
        // Positioned by a wrapper: the entrance animation offsets the panel relatively.
        Some(
            div()
                .absolute()
                .left(px(8.))
                .right(px(8.))
                .bottom(px(68.))
                .child(super::motion::pop_in("account-panel-in", panel))
                .into_any_element(),
        )
    }
}
