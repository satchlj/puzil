//! Sign-in: pick a server, then SSO (`mobile_flow_otp`), email and password, or a zuliprc.

use std::{collections::HashMap, rc::Rc, sync::Arc};

use gpui_kit::{
    component::input::{Input, InputEvent, InputState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::{
    Credentials, ServerSettings,
    auth::{self, SsoAttempt},
};

use crate::{accounts, icons, runtime::on_tokio, theme::Theme};

pub enum LoginEvent {
    SignedIn(accounts::Account, Credentials),
    /// Adding a server was abandoned.
    Cancel,
}

pub struct Login {
    theme: Rc<Theme>,
    realm: Entity<InputState>,
    email: Entity<InputState>,
    password: Entity<InputState>,
    settings: Option<ServerSettings>,
    attempt: Option<SsoAttempt>,
    busy: bool,
    message: Option<String>,
    cancellable: bool,
    /// Provider icons by URL: as served, and a light copy of a dark one-colour icon (Apple,
    /// GitHub) to show in dark mode.
    sso_icons: HashMap<String, (Arc<RenderImage>, Option<Arc<RenderImage>>)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<LoginEvent> for Login {}

impl Login {
    /// `cancellable`: shown over existing servers (adding one), so it can be dismissed.
    pub fn new(cancellable: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let default = std::env::var("PUZIL_REALM").unwrap_or_default();
        let realm = cx.new(|cx| InputState::new(window, cx).placeholder("your-org.zulipchat.com").default_value(default));
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("Email address"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("Password").masked(true));
        let subs = vec![
            cx.subscribe_in(&realm, window, |this, _, e: &InputEvent, _, cx| match e {
                InputEvent::PressEnter { .. } => this.load_settings(cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&email, window, |_, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&password, window, |this, _, e: &InputEvent, _, cx| match e {
                InputEvent::PressEnter { .. } => this.password_login(cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
        ];
        Self {
            theme: Rc::new(Theme::for_appearance(window.appearance())),
            realm,
            email,
            password,
            settings: None,
            attempt: None,
            busy: false,
            message: None,
            cancellable,
            sso_icons: HashMap::new(),
            _subs: subs,
        }
    }

    fn load_settings(&mut self, cx: &mut Context<Self>) {
        let realm = puzil_zulip::canonical_realm(self.realm.read(cx).value().trim());
        let realm = match realm {
            Ok(r) => r,
            Err(e) => {
                self.message = Some(format!("{e:#}"));
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { auth::server_settings(&auth::http(), &realm).await }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(s) => {
                        this.fetch_icons(&s, cx);
                        this.settings = Some(s);
                    }
                    Err(e) => this.message = Some(format!("Couldn't reach that server: {e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn fetch_icons(&mut self, s: &ServerSettings, cx: &mut Context<Self>) {
        for m in &s.external_authentication_methods {
            let Some(icon) = &m.display_icon else { continue };
            let url = if icon.starts_with("http") { icon.clone() } else { format!("{}/{}", s.realm_url.trim_end_matches('/'), icon.trim_start_matches('/')) };
            if self.sso_icons.contains_key(&url) {
                continue;
            }
            cx.spawn(async move |this, cx| {
                let u = url.clone();
                let bytes = on_tokio(async move { reqwest::get(&u).await?.error_for_status()?.bytes().await }).await;
                let Some(icon) = bytes.ok().and_then(|b| decode_icon(&b)) else { return };
                let _ = this.update(cx, |this, cx| {
                    this.sso_icons.insert(url, icon);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn start_sso(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(s) = &self.settings else { return };
        let m = s.external_authentication_methods[ix].clone();
        match SsoAttempt::new(s, &m) {
            Ok(a) => {
                // Single-flight: a new attempt replaces the old one, whose OTP is discarded.
                let url = a.browser_url.clone();
                self.attempt = Some(a);
                self.message = Some(format!("Finish signing in with {} in your browser.", m.display_name));
                cx.open_url(&url);
            }
            Err(e) => self.message = Some(format!("{e:#}")),
        }
        cx.notify();
    }

    /// A `zulip://login?…` URL from the browser (via the OS) or pasted by hand.
    pub fn handle_callback(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(a) = &self.attempt else {
            self.message = Some("Got a sign-in link, but no sign-in is in progress. Start again.".into());
            cx.notify();
            return;
        };
        match a.complete(url) {
            Ok(c) => {
                self.attempt = None;
                self.finish(c, cx);
            }
            // A junk URL must not cancel the real attempt.
            Err(e) => self.message = Some(format!("That sign-in link didn't work: {e:#}")),
        }
        cx.notify();
    }

    fn paste_callback(&mut self, cx: &mut Context<Self>) {
        match cx.read_from_clipboard().and_then(|c| c.text()) {
            Some(t) if t.trim_start().starts_with("zulip://") => self.handle_callback(t.trim(), cx),
            _ => {
                self.message = Some("The clipboard has no zulip:// link.".into());
                cx.notify();
            }
        }
    }

    fn password_login(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.settings.clone() else {
            return;
        };
        let (user, pw) = (self.email.read(cx).value().to_string(), self.password.read(cx).value().to_string());
        if user.is_empty() || pw.is_empty() {
            return;
        }
        self.busy = true;
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { auth::password_login(&auth::http(), &s, &user, &pw).await }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(c) => this.finish(c, cx),
                    Err(e) => this.message = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn import_zuliprc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(p) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                match puzil_zulip::zuliprc::load(&p) {
                    Ok(c) => this.finish(c, cx),
                    Err(e) => this.message = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Verifies credentials with `/users/me`, stores them, and signs in.
    fn finish(&mut self, creds: Credentials, cx: &mut Context<Self>) {
        self.busy = true;
        self.message = Some("Checking…".into());
        cx.spawn(async move |this, cx| {
            let c2 = creds.clone();
            let me = on_tokio(async move { auth::verify(&auth::http(), &c2).await }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match me {
                    Ok(me) => {
                        let mut c = creds;
                        c.user_id = Some(me.user_id);
                        let name = this.settings.as_ref().and_then(|s| s.realm_name.clone());
                        match accounts::add(&c, name) {
                            Ok(a) => cx.emit(LoginEvent::SignedIn(a, c)),
                            Err(e) => this.message = Some(format!("Couldn't save the key: {e:#}")),
                        }
                    }
                    Err(e) => this.message = Some(format!("The server refused these credentials: {e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// A provider icon, shrunk to 64px, plus a light copy when it is one dark colour.
fn decode_icon(bytes: &[u8]) -> Option<(Arc<RenderImage>, Option<Arc<RenderImage>>)> {
    let img = image::load_from_memory(bytes).ok()?.thumbnail(64, 64).into_rgba8();
    // Judged on solid pixels only: shrinking blends the edges with whatever colour the
    // transparent ones carry.
    let solid: Vec<_> = img.pixels().filter(|p| p[3] > 230).collect();
    let dark = solid
        .iter()
        .filter(|p| {
            let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
            mx < 90 && mx - mn < 24
        })
        .count();
    let dark_mono = !solid.is_empty() && dark * 100 >= solid.len() * 95;
    let render = |mut rgba: image::RgbaImage| {
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(rgba)]))
    };
    let light = dark_mono.then(|| {
        let mut l = img.clone();
        for p in l.pixels_mut() {
            p[0] = 235;
            p[1] = 235;
            p[2] = 238;
        }
        render(l)
    });
    Some((render(img), light))
}

/// The submit control at the end of a field: a small round arrow, accent-filled once there
/// is something to submit.
fn go(id: &'static str, ready: bool, busy: bool, t: &Theme) -> Stateful<Div> {
    let (bg, fg) = if ready && !busy { (t.accent, white()) } else { (t.bg_selected, t.muted) };
    div()
        .id(id)
        .size(px(30.))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .when(busy, |d| d.opacity(0.6))
        .when(ready && !busy, |d| d.cursor_pointer().hover(|s| s.opacity(0.88)))
        .child(icons::icon("arrow-right", 16., fg))
}

fn secondary(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: Option<Arc<RenderImage>>, t: &Theme) -> Stateful<Div> {
    let hover = t.bg_hover;
    div()
        .id(id.into())
        .h(px(40.))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.fg_strong)
        .border_1()
        .border_color(t.border)
        .hover(move |s| s.bg(hover))
        .gap_2()
        .children(icon.map(|i| gpui_kit::img(ImageSource::Render(i)).object_fit(ObjectFit::Contain).size(px(18.))))
        .child(label.into())
}

fn link(id: &'static str, text: &'static str, t: &Theme) -> Stateful<Div> {
    let hover = t.fg_strong;
    div().id(id).text_color(t.muted).cursor_pointer().hover(move |s| s.text_color(hover)).child(text)
}

impl Render for Login {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Rc::new(Theme::for_appearance(window.appearance()));
        self.theme = t.clone();
        // A field, with an optional trailing control (the submit arrow).
        let field = |input: &Entity<InputState>, trailing: Option<Stateful<Div>>| {
            div()
                .h(px(44.))
                .rounded(px(10.))
                .bg(t.input_bg)
                .border_1()
                .border_color(t.border)
                .pl_1p5()
                .pr_1p5()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(div().flex_1().min_w_0().child(Input::new(input).appearance(false)))
                .children(trailing)
        };
        let busy = self.busy;
        let title = |text: SharedString| {
            div()
                .text_center()
                .font_family(super::rich::DISPLAY_FONT)
                .text_size(px(22.))
                .font_weight(FontWeight::BOLD)
                .text_color(t.fg_strong)
                .child(text)
        };
        let mut card = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(400.))
            .p_8()
            .rounded(px(14.))
            .bg(t.bg)
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            .child(div().flex().justify_center().child(gpui_kit::img(ImageSource::Render(crate::icons::app_icon(64.))).size(px(64.))));
        match &self.settings {
            None => {
                card = card
                    .child(title(if self.cancellable { "Add a server".into() } else { "Welcome to Puzil".into() }))
                    .child(div().text_center().text_color(t.muted).pb_2().child("Enter your Zulip organization’s address."))
                    .child(field(
                        &self.realm,
                        Some(
                            go("continue", !self.realm.read(cx).value().trim().is_empty(), busy, &t)
                                .on_click(cx.listener(|this, _, _, cx| this.load_settings(cx))),
                        ),
                    ));
            }
            Some(s) => {
                // The server, found: its name heads the card, its address and a way back below.
                let name = s.realm_name.clone().unwrap_or_else(|| s.realm_url.clone());
                let host = s.realm_url.trim_start_matches("https://").trim_end_matches('/').to_string();
                card = card.child(title(name.into())).child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_center()
                        .gap_2()
                        .pb_2()
                        .text_size(px(14.))
                        .text_color(t.muted)
                        .child("Sign in")
                        .child(div().text_color(t.faint).child("·"))
                        .child(host)
                        .child(div().text_color(t.faint).child("·"))
                        .child(link("login-change", "Change", &t).on_click(cx.listener(|this, _, _, cx| {
                            this.settings = None;
                            this.attempt = None;
                            this.message = None;
                            cx.notify();
                        }))),
                );
                let methods = &s.external_authentication_methods;
                let realm_url = s.realm_url.clone();
                let icon_for = |m: &puzil_zulip::auth::ExternalMethod| {
                    let icon = m.display_icon.as_ref()?;
                    let url = if icon.starts_with("http") { icon.clone() } else { format!("{}/{}", realm_url.trim_end_matches('/'), icon.trim_start_matches('/')) };
                    let (img, light) = self.sso_icons.get(&url)?;
                    Some(if t.dark { light.clone().unwrap_or_else(|| img.clone()) } else { img.clone() })
                };
                if s.email_auth_enabled {
                    let ready = !self.email.read(cx).value().is_empty() && !self.password.read(cx).value().is_empty();
                    card = card.child(field(&self.email, None)).child(field(
                        &self.password,
                        Some(go("password", ready, busy, &t).on_click(cx.listener(|this, _, _, cx| this.password_login(cx)))),
                    ));
                    if !methods.is_empty() {
                        card = card.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_3()
                                .py_1()
                                .text_size(px(12.))
                                .text_color(t.faint)
                                .child(div().flex_1().h(px(1.)).bg(t.border))
                                .child("or continue with")
                                .child(div().flex_1().h(px(1.)).bg(t.border)),
                        );
                    }
                }
                // Providers: one wide button when alone, else a two-column grid of names.
                if methods.len() == 1 {
                    let text = format!("Continue with {}", methods[0].display_name);
                    card = card.child(secondary(("sso", 0usize), text, icon_for(&methods[0]), &t).on_click(cx.listener(|this, _, _, cx| this.start_sso(0, cx))));
                } else if !methods.is_empty() {
                    let pairs: Vec<Vec<usize>> = (0..methods.len()).collect::<Vec<_>>().chunks(2).map(|c| c.to_vec()).collect();
                    card = card.child(div().flex().flex_col().gap_2().children(pairs.into_iter().map(|pair| {
                        div().flex().flex_row().gap_2().children(pair.into_iter().map(|i| {
                            secondary(("sso", i), methods[i].display_name.clone(), icon_for(&methods[i]), &t)
                                .flex_1()
                                .on_click(cx.listener(move |this, _, _, cx| this.start_sso(i, cx)))
                        }))
                    })));
                }
            }
        }
        card = card
            .when_some(self.message.clone(), |d, m| d.child(div().text_size(px(13.)).text_center().text_color(t.muted).child(m)))
            .child(div().h(px(1.)).bg(t.border).mt_2())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_center()
                    .gap_2()
                    .text_size(px(13.))
                    .child(link("zuliprc", "Import a zuliprc file", &t).on_click(cx.listener(|this, _, w, cx| this.import_zuliprc(w, cx))))
                    .when(self.attempt.is_some(), |d| {
                        d.child(div().text_color(t.faint).child("·"))
                            .child(link("paste", "Paste sign-in link", &t).on_click(cx.listener(|this, _, _, cx| this.paste_callback(cx))))
                    })
                    .when(self.cancellable, |d| {
                        d.child(div().text_color(t.faint).child("·"))
                            .child(link("login-cancel", "Cancel", &t).on_click(cx.listener(|_, _, _, cx| cx.emit(LoginEvent::Cancel))))
                    }),
            );
        div()
            .id("login")
            .relative()
            .size_full()
            .overflow_y_scroll()
            .bg(t.rail)
            .text_color(t.fg)
            .child(
                crate::theme::drag_region(div().id("login-drag"))
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(crate::theme::TRAFFIC_LIGHTS)),
            )
            .child(div().min_h_full().py_12().flex().items_center().justify_center().child(card))
    }
}
