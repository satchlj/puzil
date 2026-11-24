//! Settings, in the main pane. Puzil's own settings have two layers: "All servers" (the
//! defaults) and the current server, whose values override them. Settings Zulip owns
//! (notifications, privacy, profile, muting) are changed on the server, so they apply on
//! every client; whatever Puzil doesn't edit links straight to the page in Zulip web.

use std::rc::Rc;

use gpui_kit::{
    component::input::{Input, InputEvent, InputState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::types::Visibility;
use serde_json::{Value, json};

use crate::{
    icons,
    session::Session,
    settings::{self, Mode, Motion, Prefs, Scope},
    theme::{Palette, Theme},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Sidebar,
    Messages,
    Notifications,
    Privacy,
    Account,
    Muted,
    Organization,
    Advanced,
}

impl Section {
    const ALL: [Section; 9] = [
        Section::Appearance,
        Section::Sidebar,
        Section::Messages,
        Section::Notifications,
        Section::Privacy,
        Section::Account,
        Section::Muted,
        Section::Organization,
        Section::Advanced,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Sidebar => "Sidebar",
            Self::Messages => "Messages",
            Self::Notifications => "Notifications",
            Self::Privacy => "Privacy",
            Self::Account => "Account",
            Self::Muted => "Muted",
            Self::Organization => "Organization",
            Self::Advanced => "Advanced",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Appearance => "palette",
            Self::Sidebar => "panel-left",
            Self::Messages => "messages-square",
            Self::Notifications => "bell",
            Self::Privacy => "eye-off",
            Self::Account => "user",
            Self::Muted => "volume-x",
            Self::Organization => "building-2",
            Self::Advanced => "settings-2",
        }
    }
    /// Whether this section's settings are Puzil's own (layered) or Zulip's (per server).
    fn layered(self) -> bool {
        matches!(self, Self::Appearance | Self::Sidebar | Self::Messages)
    }
}

pub struct SettingsView {
    session: Entity<Session>,
    theme: Rc<Theme>,
    section: Section,
    /// Editing the defaults (`App`) or this server's overrides.
    scope: Scope,
    full_name: Entity<InputState>,
    status_text: Entity<InputState>,
    new_email: Entity<InputState>,
    old_password: Entity<InputState>,
    new_password: Entity<InputState>,
    /// The last account change's outcome, shown under the Sign-in rows.
    account_note: Option<(bool, String)>,
    uploading_avatar: bool,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(session: Entity<Session>, theme: Rc<Theme>, section: Section, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (name, status) = {
            let s = session.read(cx);
            let me = s.state.users.get(&s.state.me);
            (me.map(|u| u.full_name.clone()).unwrap_or_default(), String::new())
        };
        let full_name = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let status_text = cx.new(|cx| InputState::new(window, cx).placeholder("What are you up to?").default_value(status));
        let new_email = cx.new(|cx| InputState::new(window, cx).placeholder("new@address"));
        let old_password = cx.new(|cx| InputState::new(window, cx).placeholder("Current password").masked(true));
        let new_password = cx.new(|cx| InputState::new(window, cx).placeholder("New password").masked(true));
        let subs = vec![
            cx.subscribe(&full_name, |this, input, e: &InputEvent, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    let v = input.read(cx).value().trim().to_string();
                    if !v.is_empty() {
                        this.session.update(cx, |s, cx| s.set_zulip_setting("full_name", json!(v), cx));
                    }
                }
            }),
            cx.subscribe(&status_text, |this, input, e: &InputEvent, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    let v = input.read(cx).value().trim().to_string();
                    this.session.update(cx, |s, cx| {
                        s.call(cx, move |c| async move { c.set_status(None, Some(&v), None).await }, |_, e, _| tracing::warn!("status: {e}"))
                    });
                }
            }),
            cx.subscribe(&new_email, |this, input, e: &InputEvent, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    let v = input.read(cx).value().trim().to_string();
                    if v.contains('@') {
                        this.account_call(move |c| async move { c.change_email(&v).await }, "Check your new address for a confirmation link.", cx);
                    }
                }
            }),
            cx.subscribe(&new_password, |this, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    let old = this.old_password.read(cx).value().to_string();
                    let new = this.new_password.read(cx).value().to_string();
                    if !old.is_empty() && !new.is_empty() {
                        this.account_call(move |c| async move { c.change_password(&old, &new).await }, "Password changed.", cx);
                    }
                }
            }),
            cx.observe(&settings::handle(cx), |_, _, cx| cx.notify()),
            cx.observe(&session, |_, _, cx| cx.notify()),
        ];
        let realm = session.read(cx).account.realm.clone();
        Self {
            session,
            theme,
            section,
            scope: Scope::Server(realm),
            full_name,
            status_text,
            new_email,
            old_password,
            new_password,
            account_note: None,
            uploading_avatar: false,
            scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    pub fn set_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Runs an account change and reports how it went under the Sign-in rows.
    fn account_call<F, Fut>(&mut self, f: F, ok: &'static str, cx: &mut Context<Self>)
    where
        F: FnOnce(puzil_zulip::Client) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), puzil_zulip::ApiError>> + Send + 'static,
    {
        let client = self.session.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            let r = crate::runtime::on_tokio(async move { f(client).await }).await;
            let _ = this.update(cx, |this, cx| {
                this.account_note = Some(match r {
                    Ok(()) => (true, ok.to_string()),
                    Err(e) => (false, e.to_string()),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_avatar(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Use as picture".into()) });
        let client = self.session.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                this.uploading_avatar = true;
                cx.notify();
            });
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "avatar.png".into());
            let mime = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("gif") => "image/gif",
                Some("webp") => "image/webp",
                _ => "image/png",
            };
            let r = match std::fs::read(&path) {
                Ok(bytes) => crate::runtime::on_tokio(async move { client.set_avatar(&name, bytes, mime).await }).await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            let _ = this.update(cx, |this, cx| {
                this.uploading_avatar = false;
                this.account_note = Some(match r {
                    Ok(()) => (true, "Picture updated.".into()),
                    Err(e) => (false, format!("Couldn't change the picture: {e}")),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn realm(&self, cx: &App) -> String {
        self.session.read(cx).account.realm.clone()
    }

    fn web(&self, path: &str, cx: &App) -> String {
        format!("{}/#{path}", self.realm(cx))
    }

    // ---- building blocks ----

    fn heading(&self, text: &str) -> Div {
        div().pt_6().pb_2().text_size(px(12.)).font_weight(FontWeight::BOLD).text_color(self.theme.muted).child(text.to_uppercase())
    }

    /// A row: label and description on the left, `control` on the right, and for layered
    /// settings in a server scope, whether it overrides the default (with a reset).
    fn row(&self, label: &str, desc: Option<&str>, control: impl IntoElement, layer: Option<(bool, String, AnyElement)>) -> Div {
        let t = &self.theme;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .py_3()
            .border_b_1()
            .border_color(t.divider)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_color(t.fg_strong).font_weight(FontWeight::MEDIUM).child(label.to_string()))
                    .children(desc.map(|d| div().text_size(px(13.)).text_color(t.muted).child(d.to_string())))
                    .children(layer.map(|(overridden, default, reset)| {
                        div().flex().flex_row().items_center().gap_2().text_size(px(12.)).map(|d| {
                            if overridden {
                                d.child(div().size(px(6.)).rounded_full().bg(t.accent))
                                    .child(div().text_color(t.accent).child("Overrides the default for this server"))
                                    .child(reset)
                            } else {
                                d.child(div().text_color(t.faint).child(format!("Using the default ({default})")))
                            }
                        })
                    })),
            )
            .child(div().flex_none().child(control))
    }

    fn toggle(&self, id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
        let t = &self.theme;
        div()
            .id(id.into())
            .w(px(40.))
            .h(px(24.))
            .p(px(3.))
            .rounded_full()
            .cursor_pointer()
            .bg(if on { t.accent } else { t.bg_selected })
            .flex()
            .when(on, |d| d.justify_end())
            .child(div().size(px(18.)).rounded_full().bg(white()).shadow_sm())
    }

    fn segmented<T: Clone + PartialEq + 'static>(
        &self,
        key: &str,
        options: &[(&str, T)],
        current: &T,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self, T, &mut Context<Self>) + Clone + 'static,
    ) -> Div {
        let t = self.theme.clone();
        div().flex().flex_row().p_0p5().gap_0p5().rounded(px(6.)).bg(t.input_bg).children(options.iter().enumerate().map(|(i, (label, v))| {
            let selected = v == current;
            let (v, on) = (v.clone(), on.clone());
            div()
                .id(ElementId::named_usize(format!("seg-{key}"), i))
                .px_3()
                .h(px(28.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .when(selected, |d| d.bg(t.panel).text_color(t.fg_strong).shadow_sm())
                .when(!selected, |d| d.text_color(t.muted).hover(|s| s.text_color(t.fg)))
                .child(label.to_string())
                .on_click(cx.listener(move |this, _, _, cx| on(this, v.clone(), cx)))
        }))
    }

    fn stepper(&self, key: &str, value: u32, min: u32, max: u32, unit: &str, cx: &mut Context<Self>, set: impl Fn(&mut Prefs, u32) + Clone + 'static) -> Div {
        let t = self.theme.clone();
        let btn = |id: String, ic: &'static str, to: u32, enabled: bool, set: Box<dyn Fn(&mut Prefs, u32)>, cx: &mut Context<Self>| {
            let set = Rc::new(set);
            div()
                .id(SharedString::from(id))
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .bg(t.input_bg)
                .when(enabled, |d| {
                    d.cursor_pointer().hover(|s| s.bg(t.bg_selected)).on_click(cx.listener(move |this, _, _, cx| {
                        let set = set.clone();
                        this.change(move |p| set(p, to), cx)
                    }))
                })
                .when(!enabled, |d| d.opacity(0.4))
                .child(icons::icon(ic, 14., t.icon))
        };
        let (s1, s2) = (set.clone(), set);
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(btn(format!("{key}-dec"), "minus", value.saturating_sub(1).max(min), value > min, Box::new(move |p, v| s1(p, v)), cx))
            .child(div().min_w(px(64.)).text_center().text_color(t.fg_strong).font_weight(FontWeight::MEDIUM).child(format!("{value} {unit}")))
            .child(btn(format!("{key}-inc"), "plus", (value + 1).min(max), value < max, Box::new(move |p, v| s2(p, v)), cx))
    }

    fn link_button(&self, id: &str, label: &str, url: String) -> Stateful<Div> {
        let t = &self.theme;
        div()
            .id(SharedString::from(id.to_string()))
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .px_3()
            .h(px(32.))
            .rounded(px(4.))
            .bg(t.input_bg)
            .cursor_pointer()
            .hover(|s| s.bg(t.bg_selected))
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.fg)
            .child(label.to_string())
            .child(icons::icon("external-link", 14., t.icon))
            .on_click(move |_, _, cx| cx.open_url(&url))
    }

    /// A filled button for an action here in Puzil (add `on_click`).
    fn action_button(&self, id: &str, label: &str) -> Stateful<Div> {
        let t = &self.theme;
        div()
            .id(SharedString::from(id.to_string()))
            .flex()
            .items_center()
            .px_3()
            .h(px(32.))
            .rounded(px(4.))
            .bg(t.accent)
            .cursor_pointer()
            .hover(|s| s.opacity(0.9))
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(white())
            .child(label.to_string())
    }

    /// Writes to the layer being edited.
    fn change(&mut self, f: impl FnOnce(&mut Prefs), cx: &mut Context<Self>) {
        let scope = self.scope.clone();
        settings::handle(cx).update(cx, |s, cx| s.update(&scope, f, cx));
    }

    /// For a layered setting in a server scope: whether it's overridden here, the default's
    /// description, and a reset link.
    fn layer_info(&self, key: &'static str, overridden: bool, default: String, cx: &mut Context<Self>, clear: impl Fn(&mut Prefs) + 'static) -> Option<(bool, String, AnyElement)> {
        if self.scope == Scope::App {
            return None;
        }
        let t = &self.theme;
        let reset = div()
            .id(SharedString::from(format!("reset-{key}")))
            .text_color(t.link)
            .cursor_pointer()
            .hover(|s| s.underline())
            .child("Reset")
            .on_click(cx.listener(move |this, _, _, cx| this.change(&clear, cx)))
            .into_any_element();
        Some((overridden, default, reset))
    }

    fn zulip_bool(&self, cx: &App, name: &str) -> Option<bool> {
        self.session.read(cx).user_settings.get(name).and_then(Value::as_bool)
    }

    fn zulip_toggle_row(&self, label: &str, desc: Option<&str>, name: &'static str, cx: &mut Context<Self>) -> Div {
        match self.zulip_bool(cx, name) {
            Some(on) => self.row(
                label,
                desc,
                self.toggle(SharedString::from(format!("z-{name}")), on)
                    .on_click(cx.listener(move |this, _, _, cx| this.session.update(cx, |s, cx| s.set_zulip_setting(name, json!(!on), cx)))),
                None,
            ),
            None => self.row(label, desc, div().text_size(px(13.)).text_color(self.theme.faint).child("Loading…"), None),
        }
    }

    // ---- sections ----

    fn appearance(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme.clone();
        let st = settings::handle(cx).read(cx);
        let layer = st.layer(&self.scope);
        let app = st.layer(&Scope::App);
        let eff = st.resolve(match &self.scope {
            Scope::Server(r) => Some(r.as_str()),
            Scope::App => None,
        });
        let app_eff = st.resolve(None);
        let mut out = vec![self.heading("Theme").into_any_element()];
        // Theme cards: each palette in light and dark, click to choose.
        let cards = Palette::ALL.iter().map(|p| {
            let p = *p;
            let selected = eff.palette == p;
            let (l, d) = (Theme::of(p, false), Theme::of(p, true));
            let swatch = |th: &Theme| {
                div()
                    .flex_1()
                    .h(px(64.))
                    .flex()
                    .flex_row()
                    .child(div().w(px(18.)).h_full().bg(th.sidebar))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .bg(th.bg)
                            .p_1p5()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().w(px(36.)).h(px(5.)).rounded_full().bg(th.fg_strong))
                            .child(div().w(px(50.)).h(px(4.)).rounded_full().bg(th.muted))
                            .child(div().w(px(28.)).h(px(4.)).rounded_full().bg(th.link))
                            .child(div().mt_auto().w(px(22.)).h(px(9.)).rounded(px(3.)).bg(th.accent)),
                    )
            };
            div()
                .id(SharedString::from(format!("palette-{}", p.name())))
                .w(px(196.))
                .flex()
                .flex_col()
                .gap_1p5()
                .p_1p5()
                .rounded(px(8.))
                .border_2()
                .border_color(if selected { t.accent } else { gpui_kit::transparent_black() })
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_hover))
                .child(div().flex().flex_row().rounded(px(6.)).overflow_hidden().border_1().border_color(t.border).child(swatch(&l)).child(swatch(&d)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_0p5()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.fg_strong)
                        .child(p.label())
                        .when(selected, |d| d.child(icons::icon("check", 14., t.accent))),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.change(move |l| l.palette = Some(p.name().to_string()), cx)))
        });
        out.push(div().flex().flex_row().flex_wrap().gap_2().children(cards).into_any_element());
        if let Some(info) = self.layer_info("palette", layer.palette.is_some(), app_eff.palette.label().to_string(), cx, |l| l.palette = None) {
            out.push(self.row("Theme for this server", None, div(), Some(info)).into_any_element());
        }
        let mode = self.segmented(
            "mode",
            &[("System", Mode::System), ("Light", Mode::Light), ("Dark", Mode::Dark)],
            &eff.mode,
            cx,
            |this, v, cx| this.change(move |l| l.mode = Some(v), cx),
        );
        let mode_info = self.layer_info("mode", layer.mode.is_some(), format!("{:?}", app_eff.mode), cx, |l| l.mode = None);
        out.push(self.row("Mode", Some(&format!("Light, dark, or follow {}.", crate::platform::OS_NAME)), mode, mode_info).into_any_element());
        let clock = self.segmented("clock", &[("12-hour", false), ("24-hour", true)], &eff.clock_24h, cx, |this, v, cx| {
            this.change(move |l| l.clock_24h = Some(v), cx)
        });
        let clock_info = self.layer_info("clock", layer.clock_24h.is_some(), if app_eff.clock_24h { "24-hour" } else { "12-hour" }.into(), cx, |l| l.clock_24h = None);
        out.push(self.row("Time format", None, clock, clock_info).into_any_element());
        let motion = self.segmented(
            "motion",
            &[("System", Motion::System), ("Reduced", Motion::Reduced), ("Full", Motion::Full)],
            &eff.motion,
            cx,
            |this, v, cx| this.change(move |l| l.motion = Some(v), cx),
        );
        let motion_info = self.layer_info("motion", layer.motion.is_some(), format!("{:?}", app_eff.motion), cx, |l| l.motion = None);
        out.push(self.row("Motion", Some(if cfg!(target_os = "macos") { "Animations follow macOS's Reduce Motion unless set here." } else { "Animations follow the system's reduced-motion setting unless set here." }), motion, motion_info).into_any_element());
        let _ = app;
        out
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let st = settings::handle(cx).read(cx);
        let layer = st.layer(&self.scope);
        let eff = st.resolve(match &self.scope {
            Scope::Server(r) => Some(r.as_str()),
            Scope::App => None,
        });
        let def = st.resolve(None);
        let mut out = vec![self.heading("Direct messages").into_any_element()];
        let dms = self.stepper("dms", eff.dms_shown as u32, 1, 20, "shown", cx, |p, v| p.dms_shown = Some(v as usize));
        let info = self.layer_info("dms", layer.dms_shown.is_some(), def.dms_shown.to_string(), cx, |l| l.dms_shown = None);
        out.push(self.row("Recent DMs shown", Some("How many direct messages show before \u{201c}N more\u{201d}. Unread ones always show."), dms, info).into_any_element());
        out.push(self.heading("Channels").into_any_element());
        let days = self.stepper("days", eff.active_days, 1, 90, "days", cx, |p, v| p.active_days = Some(v));
        let info = self.layer_info("days", layer.active_days.is_some(), format!("{} days", def.active_days), cx, |l| l.active_days = None);
        out.push(self.row("Active topics", Some("A topic with a message this recent shows under its channel without expanding \u{201c}N more topics\u{201d}."), days, info).into_any_element());
        let on = eff.show_muted_channels;
        let toggle = self.toggle("muted-chans", on).on_click(cx.listener(move |this, _, _, cx| this.change(move |l| l.show_muted_channels = Some(!on), cx)));
        let info = self.layer_info("muted-chans", layer.show_muted_channels.is_some(), if def.show_muted_channels { "shown" } else { "hidden" }.into(), cx, |l| l.show_muted_channels = None);
        out.push(self.row("Show muted channels", Some("Muted channels stay in the sidebar, dimmed. Off hides them."), toggle, info).into_any_element());
        let on = eff.drag_topics;
        let toggle = self.toggle("drag-topics", on).on_click(cx.listener(move |this, _, _, cx| this.change(move |l| l.drag_topics = Some(!on), cx)));
        let info = self.layer_info("drag-topics", layer.drag_topics.is_some(), if def.drag_topics { "on" } else { "off" }.into(), cx, |l| l.drag_topics = None);
        out.push(self.row("Drag topics to move them", Some("Drop a topic on a channel to open Move topic with that channel chosen."), toggle, info).into_any_element());
        out
    }

    fn messages(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let st = settings::handle(cx).read(cx);
        let layer = st.layer(&self.scope);
        let eff = st.resolve(match &self.scope {
            Scope::Server(r) => Some(r.as_str()),
            Scope::App => None,
        });
        let def = st.resolve(None);
        let mut out = vec![self.heading("Reading").into_any_element()];
        let on = eff.auto_read;
        let toggle = self.toggle("auto-read", on).on_click(cx.listener(move |this, _, _, cx| this.change(move |l| l.auto_read = Some(!on), cx)));
        let info = self.layer_info("auto-read", layer.auto_read.is_some(), if def.auto_read { "on" } else { "off" }.into(), cx, |l| l.auto_read = None);
        out.push(self.row("Mark messages read as you see them", Some("Messages on screen are marked read while the window is focused. Off: only \u{201c}Mark as read\u{201d} in the sidebar menus does."), toggle, info).into_any_element());
        out.push(self.heading("Layout").into_any_element());
        let mins = self.stepper("group", eff.group_minutes, 1, 60, "min", cx, |p, v| p.group_minutes = Some(v));
        let info = self.layer_info("group", layer.group_minutes.is_some(), format!("{} min", def.group_minutes), cx, |l| l.group_minutes = None);
        out.push(self.row("Group consecutive messages", Some("Messages from one person within this long share a name and avatar."), mins, info).into_any_element());
        out
    }

    fn notifications(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![
            div()
                .pt_4()
                .text_size(px(13.))
                .text_color(self.theme.muted)
                .child("These are your Zulip notification settings: they apply to every Zulip app. Puzil starts delivering desktop notifications in a later phase and will follow them.")
                .into_any_element(),
            self.heading("Direct messages and mentions").into_any_element(),
        ];
        out.push(self.zulip_toggle_row("Desktop notifications", None, "enable_desktop_notifications", cx).into_any_element());
        out.push(self.zulip_toggle_row("Sounds", None, "enable_sounds", cx).into_any_element());
        out.push(self.zulip_toggle_row("Show message text in notifications", None, "pm_content_in_desktop_notifications", cx).into_any_element());
        out.push(self.zulip_toggle_row("Notify for @all, @everyone and @topic", None, "wildcard_mentions_notify", cx).into_any_element());
        out.push(self.heading("Channels").into_any_element());
        out.push(self.zulip_toggle_row("Desktop notifications for channel messages", None, "enable_stream_desktop_notifications", cx).into_any_element());
        out.push(self.zulip_toggle_row("Sounds for channel messages", None, "enable_stream_audible_notifications", cx).into_any_element());
        out.push(self.zulip_toggle_row("Desktop notifications for followed topics", None, "enable_followed_topic_desktop_notifications", cx).into_any_element());
        out.push(self.heading("When you're away").into_any_element());
        out.push(self.zulip_toggle_row("Email notifications", None, "enable_offline_email_notifications", cx).into_any_element());
        out.push(self.zulip_toggle_row("Mobile push notifications", None, "enable_offline_push_notifications", cx).into_any_element());
        out.push(self.zulip_toggle_row("Digest emails", None, "enable_digest_emails", cx).into_any_element());
        out.push(
            self.row("Per-channel notification settings", None, self.link_button("web-notif", "Open in Zulip", self.web("settings/notifications", cx)), None)
                .into_any_element(),
        );
        out
    }

    fn privacy(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![self.heading("Activity").into_any_element()];
        out.push(self.zulip_toggle_row("Send read receipts", Some("Others can see when you've read their messages."), "send_read_receipts", cx).into_any_element());
        out.push(self.zulip_toggle_row("Show when you're typing in DMs", None, "send_private_typing_notifications", cx).into_any_element());
        out.push(self.zulip_toggle_row("Show when you're typing in channels", None, "send_stream_typing_notifications", cx).into_any_element());
        out.push(self.zulip_toggle_row("Show your availability", Some("Off: you always appear offline."), "presence_enabled", cx).into_any_element());
        // Zulip's email_address_visibility: 1 everyone, 2 members, 3 admins, 4 nobody, 5 moderators.
        match self.session.read(cx).user_settings.get("email_address_visibility").and_then(Value::as_u64) {
            Some(v) => {
                let seg = self.segmented(
                    "email-vis",
                    &[("Everyone", 1u64), ("Members", 2), ("Moderators", 5), ("Admins", 3), ("Nobody", 4)],
                    &v,
                    cx,
                    |this, v, cx| this.session.update(cx, |s, cx| s.set_zulip_setting("email_address_visibility", json!(v), cx)),
                );
                out.push(self.row("Who can see your email address", None, seg, None).into_any_element());
            }
            None => out.push(self.row("Who can see your email address", None, div().text_size(px(13.)).text_color(self.theme.faint).child("Loading…"), None).into_any_element()),
        }
        out
    }

    fn account(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme.clone();
        let field = |input: &Entity<InputState>| {
            div().w(px(280.)).h(px(36.)).px_1().rounded(px(6.)).bg(t.composer_bg).border_1().border_color(t.composer_border).flex().items_center().child(Input::new(input).appearance(false))
        };
        let small_button = |id: &str, label: &str, danger: bool| {
            div()
                .id(SharedString::from(id.to_string()))
                .px_3()
                .h(px(32.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .bg(t.input_bg)
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_selected))
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if danger { t.danger } else { t.fg })
                .child(label.to_string())
        };
        let (me, my_avatar, delivery) = {
            let s = self.session.read(cx);
            let u = s.state.users.get(&s.state.me);
            (s.state.me, u.and_then(|u| u.avatar_url.clone()), u.and_then(|u| u.delivery_email.clone()).unwrap_or_default())
        };
        let images = self.session.read(cx).images.clone();
        let img = images.update(cx, |i, cx| i.avatar(me, my_avatar.as_deref(), cx));
        let mut out = vec![self.heading("Profile").into_any_element()];
        let picture = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .child(super::avatar::avatar(img, 56., t.user_color(me), ""))
            .child(
                small_button("avatar-change", if self.uploading_avatar { "Uploading…" } else { "Change picture" }, false)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_avatar(cx))),
            )
            .child(small_button("avatar-remove", "Remove", true).on_click(cx.listener(|this, _, _, cx| {
                this.account_call(|c| async move { c.delete_avatar().await }, "Picture removed.", cx)
            })));
        out.push(self.row("Picture", Some("PNG, JPEG, GIF or WebP. Zulip crops it to a square."), picture, None).into_any_element());
        out.push(self.row("Name", Some("Press Enter to save."), field(&self.full_name), None).into_any_element());
        out.push(self.row("Status", Some("Shown next to your name. Press Enter to set; empty clears it."), field(&self.status_text), None).into_any_element());
        let tz = self.session.read(cx).user_settings.get("timezone").and_then(Value::as_str).unwrap_or("").to_string();
        let mac_tz = std::fs::read_link("/etc/localtime")
            .ok()
            .and_then(|p| p.to_string_lossy().split("zoneinfo/").nth(1).map(str::to_string))
            .unwrap_or_default();
        let tz_control = if !mac_tz.is_empty() && mac_tz != tz {
            let z = mac_tz.clone();
            small_button("tz-mac", &format!("Use this Mac's ({mac_tz})"), false)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let z = z.clone();
                    this.session.update(cx, |s, cx| s.set_zulip_setting("timezone", json!(z), cx))
                }))
                .into_any_element()
        } else {
            div().text_size(px(13.)).text_color(t.faint).child("Matches this Mac").into_any_element()
        };
        out.push(self.row("Time zone", Some(if tz.is_empty() { "Not set" } else { tz.as_str() }), tz_control, None).into_any_element());
        out.push(self.row("Pronouns and other profile fields", None, self.link_button("web-profile", "Open in Zulip", self.web("settings/profile", cx)), None).into_any_element());

        out.push(self.heading("Sign-in").into_any_element());
        out.push(
            self.row(
                "Email",
                Some(&format!("Currently {}. Enter a new address and press Enter; Zulip emails it a confirmation link.", if delivery.is_empty() { "hidden" } else { delivery.as_str() })),
                field(&self.new_email),
                None,
            )
            .into_any_element(),
        );
        out.push(
            self.row(
                "Password",
                Some("For accounts that sign in with a password. Fill both, then press Enter in the second."),
                div().flex().flex_col().gap_1().child(field(&self.old_password)).child(field(&self.new_password)),
                None,
            )
            .into_any_element(),
        );
        if let Some((ok, note)) = &self.account_note {
            out.push(div().pt_2().text_size(px(13.)).text_color(if *ok { t.fg } else { t.danger }).child(note.clone()).into_any_element());
        }
        out.push(
            self.row("API key", Some("Puzil signs in with yours; making a new one signs Puzil out."), self.link_button("web-api", "Open in Zulip", self.web("settings/account-and-privacy", cx)), None)
                .into_any_element(),
        );
        let server = {
            let s = self.session.read(cx);
            if s.state.realm.name.is_empty() { s.account.realm.clone() } else { s.state.realm.name.clone() }
        };
        out.push(
            self.row(
                &format!("Sign out of {server}"),
                Some(&format!("Removes this server from Puzil and its key from {}. Your messages stay on the server.", crate::platform::SECRET_STORE)),
                small_button("sign-out", "Sign out", true).on_click(cx.listener(move |this, _, window, cx| {
                    let account = this.session.read(cx).account.clone();
                    let answer = window.prompt(PromptLevel::Warning, "Sign out of this server?", None, &["Sign out", "Cancel"], cx);
                    cx.spawn(async move |_, cx| {
                        if answer.await.ok() == Some(0) {
                            let _ = cx.update(|cx| crate::servers::request(cx, crate::servers::ServersEvent::SignOut(account)));
                        }
                    })
                    .detach();
                })),
                None,
            )
            .into_any_element(),
        );
        out
    }

    fn muted(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme.clone();
        let s = self.session.read(cx);
        let mut users: Vec<(u64, String)> =
            s.state.muted_users.iter().map(|id| (*id, s.state.users.get(id).map(|u| u.full_name.clone()).unwrap_or_else(|| format!("User {id}")))).collect();
        users.sort_by(|a, b| a.1.cmp(&b.1));
        let mut topics: Vec<(u64, String, String)> = s
            .state
            .user_topics
            .iter()
            .filter(|(_, v)| **v == Visibility::Muted)
            .map(|(k, _)| {
                (k.stream_id, s.state.topic_display(k), s.state.streams.get(&k.stream_id).map(|st| st.name.clone()).unwrap_or_default())
            })
            .collect();
        topics.sort_by(|a, b| (a.2.to_lowercase(), a.1.to_lowercase()).cmp(&(b.2.to_lowercase(), b.1.to_lowercase())));
        let mut chans: Vec<(u64, String)> = s
            .state
            .subs
            .values()
            .filter(|sub| sub.is_muted)
            .filter_map(|sub| s.state.streams.get(&sub.stream_id).map(|st| (st.stream_id, st.name.clone())))
            .collect();
        chans.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        let unmute = |id: String| {
            div()
                .id(SharedString::from(id))
                .px_3()
                .h(px(28.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .bg(t.input_bg)
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_selected))
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.fg)
                .child("Unmute")
        };
        let empty = |what: &str| div().py_3().text_size(px(13.)).text_color(t.faint).child(format!("No muted {what}.")).into_any_element();
        let mut out = vec![self.heading("Channels").into_any_element()];
        if chans.is_empty() {
            out.push(empty("channels"));
        }
        for (sid, name) in chans {
            out.push(
                self.row(&format!("#{name}"), None, unmute(format!("um-c-{sid}")).on_click(cx.listener(move |this, _, _, cx| {
                    this.session.update(cx, |s, cx| s.call(cx, move |c| async move { c.set_subscription_property(sid, "is_muted", json!(false)).await }, |_, e, _| tracing::warn!("unmute: {e}")))
                })), None)
                .into_any_element(),
            );
        }
        out.push(self.heading("Topics").into_any_element());
        if topics.is_empty() {
            out.push(empty("topics"));
        }
        for (i, (sid, topic, chan)) in topics.into_iter().enumerate() {
            let tp = topic.clone();
            out.push(
                self.row(&topic, Some(&format!("#{chan}")), unmute(format!("um-t-{i}")).on_click(cx.listener(move |this, _, _, cx| {
                    let tp = tp.clone();
                    this.session.update(cx, |s, cx| s.call(cx, move |c| async move { c.set_topic_visibility(sid, &tp, 0).await }, |_, e, _| tracing::warn!("unmute: {e}")))
                })), None)
                .into_any_element(),
            );
        }
        out.push(self.heading("People").into_any_element());
        if users.is_empty() {
            out.push(empty("people"));
        }
        for (uid, name) in users {
            out.push(
                self.row(&name, None, unmute(format!("um-u-{uid}")).on_click(cx.listener(move |this, _, _, cx| {
                    this.session.update(cx, |s, cx| s.call(cx, move |c| async move { c.unmute_user(uid).await }, |_, e, _| tracing::warn!("unmute: {e}")))
                })), None)
                .into_any_element(),
            );
        }
        out
    }

    fn organization(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![
            div()
                .pt_4()
                .text_size(px(13.))
                .text_color(self.theme.muted)
                .child("You're an administrator here. Organization settings are managed in Zulip.")
                .into_any_element(),
            self.heading("In Zulip").into_any_element(),
        ];
        for (id, label, path) in [
            ("org-profile", "Organization profile", "organization/organization-profile"),
            ("org-settings", "Organization settings", "organization/organization-settings"),
            ("org-perms", "Permissions", "organization/organization-permissions"),
            ("org-users", "Users", "organization/users"),
            ("org-channels", "Channels", "channels/all"),
            ("org-emoji", "Custom emoji", "organization/emoji-settings"),
        ] {
            out.push(self.row(label, None, self.link_button(id, "Open in Zulip", self.web(path, cx)), None).into_any_element());
        }
        out
    }

    fn advanced(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme.clone();
        let dir = crate::accounts::data_dir();
        let shown = dir.display().to_string();
        let open_dir = dir.clone();
        let mut out = vec![self.heading("Data").into_any_element()];
        out.push(
            self.row(
                "Where Puzil keeps its data",
                Some(&shown),
                div()
                    .id("reveal-data")
                    .px_3()
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(t.input_bg)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_selected))
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(crate::platform::SHOW_IN_FILES)
                    .on_click(move |_, _, cx| cx.reveal_path(&open_dir)),
                None,
            )
            .into_any_element(),
        );
        let log = crate::accounts::log_dir().join("puzil.log");
        let log_shown = log.display().to_string();
        out.push(
            self.row(
                "Logs",
                Some(&log_shown),
                div()
                    .id("reveal-logs")
                    .px_3()
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(t.input_bg)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_selected))
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(crate::platform::SHOW_IN_FILES)
                    .on_click(move |_, _, cx| cx.reveal_path(&log)),
                None,
            )
            .into_any_element(),
        );
        out.push(self.heading("Keyboard shortcuts").into_any_element());
        for (keys, what) in [
            (key!("K"), "Jump to a channel, topic or person"),
            (key!("F"), "Search messages"),
            (key!(","), "Settings"),
            ("↑ in an empty composer", "Select the newest message"),
            ("↑ / ↓", "Move between messages"),
            ("Enter", "Edit the selected message (yours)"),
            ("⌫", "Delete the selected message (yours)"),
            ("Esc", "Back to the composer"),
            ("Shift+Enter", "New line in a message"),
        ] {
            out.push(
                self.row(
                    what,
                    None,
                    div().px_2().py_0p5().rounded(px(4.)).bg(t.input_bg).text_size(px(13.)).font_family(super::rich::MONO_FONT).child(keys),
                    None,
                )
                .into_any_element(),
            );
        }
        out.push(self.heading("About").into_any_element());
        let update = {
            use crate::updates::{self, Phase};
            let u = updates::handle(cx).read(cx);
            let installable = u.can_install();
            match (u.available.clone(), u.phase.clone()) {
                // A build from source on Linux isn't updated.
                (None, _) if !updates::supported() => div().into_any_element(),
                (None, _) => div().text_size(px(13.)).text_color(t.faint).child("Up to date").into_any_element(),
                (Some(r), Phase::Idle) if installable => self
                    .action_button("update", &format!("Update to {} and restart", r.version))
                    .on_click(|_, _, cx| updates::install(cx))
                    .into_any_element(),
                (Some(r), Phase::Idle) => self.link_button("update", &format!("Download {}", r.version), r.page).into_any_element(),
                (Some(_), Phase::Downloading(n)) => div().text_size(px(13.)).text_color(t.muted).child(format!("Downloading… {n}%")).into_any_element(),
                (Some(_), Phase::Installing) => div().text_size(px(13.)).text_color(t.muted).child("Installing…").into_any_element(),
                (Some(r), Phase::Failed(msg)) => div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap_1()
                    .child(self.link_button("update", &format!("Download {}", r.version), r.page))
                    .child(div().max_w(px(320.)).text_size(px(12.)).text_color(t.danger).child(msg))
                    .into_any_element(),
            }
        };
        out.push(self.row("Puzil", Some(concat!("Version ", env!("CARGO_PKG_VERSION"))), update, None).into_any_element());
        out
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let realm = self.realm(cx);
        let server_name = {
            let s = self.session.read(cx);
            if s.state.realm.name.is_empty() { realm.clone() } else { s.state.realm.name.clone() }
        };
        let is_admin = {
            let s = self.session.read(cx);
            s.state.users.get(&s.state.me).is_some_and(|u| u.role <= 200)
        };
        if self.section == Section::Organization && !is_admin {
            self.section = Section::Account;
        }
        let section = self.section;
        let body = match section {
            Section::Appearance => self.appearance(cx),
            Section::Sidebar => self.sidebar(cx),
            Section::Messages => self.messages(cx),
            Section::Notifications => self.notifications(cx),
            Section::Privacy => self.privacy(cx),
            Section::Account => self.account(cx),
            Section::Muted => self.muted(cx),
            Section::Organization => self.organization(cx),
            Section::Advanced => self.advanced(cx),
        };
        let scope_switch = section.layered().then(|| {
            let current = self.scope.clone();
            self.segmented(
                "scope",
                &[("All servers", Scope::App), (server_name.as_str(), Scope::Server(realm.clone()))],
                &current,
                cx,
                |this, v, cx| {
                    this.scope = v;
                    cx.notify();
                },
            )
        });
        let scope_note = section.layered().then(|| {
            div().text_size(px(12.)).text_color(t.faint).child(match &self.scope {
                Scope::App => "Defaults for every server. A server can override any of them.".to_string(),
                Scope::Server(_) => format!("Changes here apply to {server_name} only."),
            })
        });
        // Organization is only for administrators (Zulip roles 100 owner, 200 admin).
        let admin = {
            let s = self.session.read(cx);
            s.state.users.get(&s.state.me).is_some_and(|u| u.role <= 200)
        };
        let nav = Section::ALL.iter().filter(|s| admin || **s != Section::Organization).map(|s| {
            let s = *s;
            let selected = s == section;
            div()
                .id(SharedString::from(format!("nav-{}", s.label())))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(32.))
                .px_2()
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(14.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if selected { t.fg_strong } else { t.muted })
                .when(selected, |d| d.bg(t.bg_selected))
                .when(!selected, |d| d.hover(|h| h.bg(t.bg_selected.opacity(0.5))))
                .child(icons::icon(s.icon(), 16., if selected { t.fg_strong } else { t.icon }))
                .child(s.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.section = s;
                    cx.notify();
                }))
        });
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .child(
                crate::theme::drag_region(div().id("settings-header"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(48.))
                    .px_4()
                    .flex_none()
                    .bg(t.bg)
                    .border_b_1()
                    .border_color(t.border)
                    .child(icons::icon("settings", 20., t.faint))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(t.fg_strong).child("Settings")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(div().w(px(200.)).flex_none().p_3().flex().flex_col().gap_0p5().border_r_1().border_color(t.divider).children(nav))
                    .child(
                        div().id("settings-scroll").flex_1().min_w_0().overflow_y_scroll().track_scroll(&self.scroll).child(
                            div()
                                .max_w(px(760.))
                                .px_8()
                                .py_6()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_4()
                                        .child(div().flex_1().font_family(super::rich::DISPLAY_FONT).text_size(px(20.)).font_weight(FontWeight::BOLD).text_color(t.fg_strong).child(section.label()))
                                        .children(scope_switch),
                                )
                                .children(scope_note.map(|n| div().pt_1().child(n)))
                                .children(body),
                        ),
                    ),
            )
    }
}
