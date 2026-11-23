//! Sign-in: pick a server, then SSO (`mobile_flow_otp`), email and password, or a zuliprc.

use std::rc::Rc;

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
    _subs: Vec<Subscription>,
}

impl EventEmitter<LoginEvent> for Login {}

impl Login {
    /// `cancellable`: shown over existing servers (adding one), so it can be dismissed.
    pub fn new(cancellable: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let default = std::env::var("PUZIL_REALM").unwrap_or_default();
        let realm = cx.new(|cx| InputState::new(window, cx).placeholder("your-org.zulipchat.com").default_value(default));
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("Email"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("Password").masked(true));
        let subs = vec![
            cx.subscribe_in(&realm, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.load_settings(cx);
                }
            }),
            cx.subscribe_in(&password, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.password_login(cx);
                }
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
                    Ok(s) => this.settings = Some(s),
                    Err(e) => this.message = Some(format!("Couldn't reach that server: {e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, t: &Theme, primary: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(px(44.))
        .px_4()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .cursor_pointer()
        .font_weight(FontWeight::MEDIUM)
        .text_color(if primary { white() } else { t.fg_strong })
        .bg(if primary { t.accent } else { t.bg_selected })
        .hover(|s| s.opacity(0.88))
        .child(label.into())
}

fn label(text: &str, t: &Theme) -> Div {
    div().pt_2().text_size(px(12.)).font_weight(FontWeight::BOLD).text_color(t.muted).child(text.to_uppercase())
}

impl Render for Login {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Rc::new(Theme::for_appearance(window.appearance()));
        self.theme = t.clone();
        let field = |input: &Entity<InputState>| {
            div().h(px(40.)).rounded(px(4.)).bg(t.rail).px_1().flex().items_center().child(Input::new(input).appearance(false))
        };
        let mut card = div()
            .flex()
            .flex_col()
            .gap_2()
            .w(px(480.))
            .p_8()
            .rounded(px(8.))
            .bg(t.bg)
            .shadow_lg()
            .child(
                div().flex().justify_center().pb_2().child(
                    gpui_kit::img(ImageSource::Render(crate::icons::app_icon())).size(px(88.)),
                ),
            )
            .child(
                div()
                    .text_center()
                    .font_family(super::rich::DISPLAY_FONT)
                    .text_size(px(22.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(t.fg_strong)
                    .child(if self.cancellable { "Add a server" } else { "Welcome to Puzil" }),
            )
            .child(div().text_center().text_color(t.muted).pb_3().child("Sign in to your Zulip organization."))
            .child(label("Organization URL", &t))
            .child(field(&self.realm));
        if self.settings.is_none() {
            card =
                card.child(div().pt_3().child(
                    button("continue", "Continue", &t, true).on_click(cx.listener(|this, _, _, cx| this.load_settings(cx))),
                ));
        }
        if let Some(s) = &self.settings {
            card = card.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_size(px(13.))
                    .text_color(t.muted)
                    .child(icons::icon("circle-check", 14., hsla(0.38, 0.62, 0.45, 1.)))
                    .child(format!(
                        "{} · Zulip {}",
                        s.realm_name.clone().unwrap_or_else(|| s.realm_url.clone()),
                        s.zulip_version
                    )),
            );
            if s.email_auth_enabled {
                card = card
                    .child(label("Email", &t))
                    .child(field(&self.email))
                    .child(label("Password", &t))
                    .child(field(&self.password))
                    .child(div().pt_3().child(
                        button("password", "Log in", &t, true).on_click(cx.listener(|this, _, _, cx| this.password_login(cx))),
                    ));
            }
            for (i, m) in s.external_authentication_methods.iter().enumerate() {
                card = card.child(
                    button(("sso", i), format!("Continue with {}", m.display_name), &t, !s.email_auth_enabled && i == 0)
                        .on_click(cx.listener(move |this, _, _, cx| this.start_sso(i, cx))),
                );
            }
        }
        card = card
            .when(self.busy, |d| d.child(div().text_size(px(13.)).text_color(t.muted).child("Working…")))
            .when_some(self.message.clone(), |d, m| d.child(div().text_size(px(13.)).text_color(t.muted).child(m)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_4()
                    .pt_3()
                    .text_size(px(14.))
                    .child(
                        div()
                            .id("zuliprc")
                            .text_color(t.link)
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.8))
                            .child("Import a zuliprc file")
                            .on_click(cx.listener(|this, _, w, cx| this.import_zuliprc(w, cx))),
                    )
                    .when(self.cancellable, |d| {
                        d.child(div().flex_1()).child(
                            div()
                                .id("login-cancel")
                                .text_color(t.muted)
                                .cursor_pointer()
                                .hover(|s| s.text_color(t.fg))
                                .child("Cancel")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(LoginEvent::Cancel))),
                        )
                    })
                    .when(self.attempt.is_some(), |d| {
                        d.child(
                            div()
                                .id("paste")
                                .text_color(t.link)
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.8))
                                .child("Paste sign-in link")
                                .on_click(cx.listener(|this, _, _, cx| this.paste_callback(cx))),
                        )
                    }),
            );
        div()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
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
            .child(card)
    }
}
