//! The signed-in window: rail and sidebar on the left, one conversation on the right.

use std::rc::Rc;

use gpui_kit::{prelude::FluentBuilder as _, *};
use puzil_render::ast::Media;

use super::{
    browser::ChannelBrowser,
    conversation::{Conversation, ConversationEvent, Target},
    sidebar::{Sidebar, SidebarEvent},
};
use crate::{
    media::{ImgState, fit},
    session::Session,
    theme::Theme,
};

actions!(workspace, [CloseLightbox, QuickSwitch, OpenSearch, OpenSettings, ShowAbout]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", CloseLightbox, Some("Lightbox")),
        KeyBinding::new("cmd-k", QuickSwitch, None),
        KeyBinding::new("ctrl-k", QuickSwitch, None),
        KeyBinding::new("cmd-f", OpenSearch, None),
        KeyBinding::new("ctrl-f", OpenSearch, None),
    ]);
}

pub struct Workspace {
    pub session: Entity<Session>,
    sidebar: Entity<Sidebar>,
    conversation: Option<Entity<Conversation>>,
    conversation_sub: Option<Subscription>,
    /// The channel browser, when it's showing in place of the conversation.
    browser: Option<Entity<ChannelBrowser>>,
    /// Search, when it's showing in place of the conversation.
    search: Option<(Entity<super::search::SearchView>, Subscription)>,
    /// Settings, when showing in place of the conversation.
    settings: Option<(Entity<super::settings_view::SettingsView>, ())>,
    /// The first-run welcome, until it's finished.
    welcome: Option<(Entity<super::welcome::Welcome>, Subscription)>,
    active: bool,
    /// The quick switcher, while it's open.
    switcher: Option<(Entity<super::switcher::Switcher>, Subscription)>,
    theme: Rc<Theme>,
    /// The image shown full-size over everything, if any.
    lightbox: Option<Media>,
    lightbox_focus: FocusHandle,
    /// Holds focus when nothing inside does, so window-wide keys (⌘K) still arrive.
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl Workspace {
    pub fn new(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme = Rc::new(Theme::for_appearance(window.appearance()));
        let sidebar = cx.new(|cx| Sidebar::new(session.clone(), theme.clone(), cx));
        let subs = vec![
            cx.subscribe_in(&sidebar, window, |this, _, e: &SidebarEvent, window, cx| match e {
                SidebarEvent::Open(t) => this.open(t.clone(), window, cx),
                SidebarEvent::Settings => this.show_settings(super::settings_view::Section::Appearance, window, cx),
                SidebarEvent::Browse => {
                    this.search = None;
                    this.settings = None;
                    let (session, theme) = (this.session.clone(), this.theme.clone());
                    this.browser = Some(cx.new(|cx| ChannelBrowser::new(session, theme, window, cx)));
                    cx.notify();
                }
            }),
            cx.observe_window_appearance(window, |this, window, cx| this.refresh_theme(window, cx)),
            cx.observe_in(&crate::settings::handle(cx), window, |this, _, window, cx| {
                if this.active {
                    Self::choose_theme(&this.session, cx);
                    this.refresh_theme(window, cx);
                }
                cx.notify();
            }),
        ];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let welcome = (crate::settings::handle(cx).read(cx).layer(&crate::settings::Scope::App).onboarded != Some(true)).then(|| {
            let w = cx.new(|cx| super::welcome::Welcome::new(theme.clone(), cx));
            let sub = cx.subscribe(&w, |this: &mut Self, _, _: &super::welcome::WelcomeEvent, cx| {
                this.welcome = None;
                cx.notify();
            });
            (w, sub)
        });
        let images = session.read(cx).images.clone();
        let mut subs = subs;
        subs.push(cx.observe(&images, |_, _, cx| cx.notify()));
        Self {
            session,
            sidebar,
            conversation: None,
            conversation_sub: None,
            browser: None,
            search: None,
            settings: None,
            welcome,
            active: false,
            switcher: None,
            theme,
            lightbox: None,
            lightbox_focus: cx.focus_handle(),
            focus,
            _subs: subs,
        }
    }

    pub fn open(&mut self, t: Target, window: &mut Window, cx: &mut Context<Self>) {
        if self.welcome.take().is_some() {
            // Going somewhere from the sidebar counts as finishing the welcome.
            crate::settings::handle(cx).update(cx, |s, cx| s.update(&crate::settings::Scope::App, |l| l.onboarded = Some(true), cx));
        }
        if self.browser.take().is_some() | self.search.take().is_some() | self.settings.take().is_some() {
            cx.notify();
        }
        if self.conversation.as_ref().is_some_and(|c| c.read(cx).target == t) {
            return;
        }
        self.sidebar.update(cx, |s, cx| s.select(t.clone(), cx));
        let conv = cx.new(|cx| Conversation::new(self.session.clone(), t, self.theme.clone(), window, cx));
        self.conversation_sub = Some(cx.subscribe_in(&conv, window, |this, _, e: &ConversationEvent, window, cx| match e {
            ConversationEvent::Navigate(t) => this.open(t.clone(), window, cx),
            ConversationEvent::Compose { target, topic, body } => {
                this.open(target.clone(), window, cx);
                if let Some(c) = &this.conversation {
                    let (topic, body) = (topic.clone(), body.clone());
                    c.update(cx, |c, cx| c.composer().update(cx, |comp, cx| comp.prefill(topic, body, window, cx)));
                }
            }
            ConversationEvent::OpenImage(m) => {
                this.lightbox = Some(m.clone());
                this.lightbox_focus.focus(window, cx);
                cx.notify();
            }
        }));
        conv.update(cx, |c, cx| c.composer().update(cx, |comp, cx| comp.focus(window, cx)));
        self.conversation = Some(conv);
        cx.notify();
    }
}

impl Workspace {
    /// Whether this server is the one showing. Only the active workspace sets the theme.
    pub fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            Self::choose_theme(&self.session, cx);
            self.refresh_theme(window, cx);
            self.focus.focus(window, cx);
        }
    }

    /// Points the theme at this server's effective palette and mode, and applies the
    /// settings that aren't read on demand.
    fn choose_theme(session: &Entity<Session>, cx: &mut App) {
        let realm = session.read(cx).account.realm.clone();
        let e = crate::settings::effective(cx, Some(&realm));
        Theme::choose(e.palette, e.mode);
        crate::settings::apply_globals(&e, cx);
    }

    fn refresh_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = Rc::new(Theme::for_appearance(window.appearance()));
        if *theme == *self.theme {
            return;
        }
        theme.apply_to_components(cx);
        self.theme = theme.clone();
        self.sidebar.update(cx, |s, cx| s.set_theme(theme.clone(), cx));
        if let Some(b) = &self.browser {
            b.update(cx, |b, cx| b.set_theme(theme.clone(), cx));
        }
        if let Some((sv, _)) = &self.search {
            sv.update(cx, |sv, cx| sv.set_theme(theme.clone(), cx));
        }
        if let Some((st, _)) = &self.settings {
            st.update(cx, |st, cx| st.set_theme(theme.clone(), cx));
        }
        if let Some((w, _)) = &self.welcome {
            w.update(cx, |w, cx| w.set_theme(theme.clone(), cx));
        }
        if let Some(c) = &self.conversation {
            c.update(cx, |c, cx| c.set_theme(theme, cx));
        }
        cx.notify();
    }

    fn quick_switch(&mut self, _: &QuickSwitch, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.is_some() {
            return self.close_switcher(window, cx);
        }
        let (session, theme) = (self.session.clone(), self.theme.clone());
        let sw = cx.new(|cx| super::switcher::Switcher::new(session, theme, window, cx));
        let sub = cx.subscribe_in(&sw, window, |this, _, e: &super::switcher::SwitcherEvent, window, cx| match e {
            super::switcher::SwitcherEvent::Open(t) => {
                let t = t.clone();
                this.switcher = None;
                this.open(t, window, cx);
            }
            super::switcher::SwitcherEvent::Dismiss => this.close_switcher(window, cx),
        });
        self.switcher = Some((sw, sub));
        cx.notify();
    }

    fn open_search(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((s, _)) = &self.search {
            // Already open: back to its field.
            let input = s.clone();
            input.update(cx, |_, cx| cx.notify());
            return;
        }
        self.switcher = None;
        self.settings = None;
        let (session, theme) = (self.session.clone(), self.theme.clone());
        let view = cx.new(|cx| super::search::SearchView::new(session, theme, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, e: &super::search::SearchEvent, window, cx| match e {
            super::search::SearchEvent::Open(t, id) => {
                let (t, id) = (t.clone(), *id);
                this.open(t, window, cx);
                if let Some(c) = &this.conversation {
                    c.update(cx, |c, cx| c.reveal(id, cx));
                }
            }
        });
        self.search = Some((view, sub));
        cx.notify();
    }

    fn show_settings(&mut self, section: super::settings_view::Section, window: &mut Window, cx: &mut Context<Self>) {
        self.search = None;
        self.browser = None;
        self.switcher = None;
        let (session, theme) = (self.session.clone(), self.theme.clone());
        let view = cx.new(|cx| super::settings_view::SettingsView::new(session, theme, section, window, cx));
        self.settings = Some((view, ()));
        cx.notify();
    }

    fn close_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher = None;
        if let Some(c) = &self.conversation {
            c.update(cx, |c, cx| c.composer().update(cx, |comp, cx| comp.focus(window, cx)));
        }
        cx.notify();
    }

    fn close_lightbox(&mut self, _: &CloseLightbox, window: &mut Window, cx: &mut Context<Self>) {
        self.lightbox = None;
        if let Some(c) = &self.conversation {
            c.update(cx, |c, cx| c.composer().update(cx, |comp, cx| comp.focus(window, cx)));
        }
        cx.notify();
    }

    /// The full-size image over a dimmed window. Shows the thumbnail until the original
    /// arrives; Esc or a click outside the image closes it.
    fn render_lightbox(&self, m: &Media, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let images = self.session.read(cx).images.clone();
        let client = self.session.read(cx).client.clone();
        let full = m.href.clone().filter(|h| is_image_path(h));
        let mut get = |u: &Option<String>| match u {
            Some(u) => match images.update(cx, |i, cx| i.get(u, cx)) {
                ImgState::Ready(img) => Some(img),
                _ => None,
            },
            None => None,
        };
        let shown = get(&full).or_else(|| get(&m.src));
        let loading_full = full.is_some() && images.read(cx).peek(full.as_deref().unwrap_or_default()).is_none();
        let vp = window.viewport_size();
        let (max_w, max_h) = (f32::from(vp.width) - 96., f32::from(vp.height) - 128.);
        let dims = m.dims.or_else(|| shown.as_ref().map(|i| (i.size(0).width.0 as u32, i.size(0).height.0 as u32)));
        let (w, h) = dims.map(|d| fit(d, max_w, max_h)).unwrap_or((max_w.min(480.), max_h.min(320.)));
        let title = m.title.clone().unwrap_or_default();
        let open = m.href.clone().or(m.src.clone()).map(|h| client.absolute(&h));
        let key = m.src.clone().or(m.href.clone()).unwrap_or_default();
        let lightbox = div()
            .id("lightbox")
            .key_context("Lightbox")
            .track_focus(&self.lightbox_focus)
            .on_action(cx.listener(Self::close_lightbox))
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(hsla(0., 0., 0., 0.85))
            .on_click(cx.listener(|this, _, w, cx| this.close_lightbox(&CloseLightbox, w, cx)))
            .child(
                super::motion::arrive(
                    SharedString::from(format!("lightbox-image-{key}")),
                    div().id("lightbox-image").w(px(w)).h(px(h)).on_click(|_, _, cx| cx.stop_propagation()).child(match shown {
                    Some(i) => img(ImageSource::Render(i)).object_fit(ObjectFit::Contain).size_full().into_any_element(),
                    None => div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(white())
                        .child("Loading…")
                        .into_any_element(),
                    }),
                    10.,
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_4()
                    .text_color(white())
                    .text_sm()
                    .child(title)
                    .when(loading_full, |d| d.child(div().text_color(hsla(0., 0., 1., 0.6)).child("loading full size…")))
                    .when_some(open, |d, url| {
                        d.child(
                            div()
                                .id("lightbox-open")
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(hsla(0., 0., 1., 0.15))
                                .cursor_pointer()
                                .child("Open in browser")
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    cx.open_url(&url)
                                }),
                        )
                    }),
            );
        // The backdrop fades in under the image settling into place.
        super::motion::fade_in(SharedString::from(format!("lightbox-{key}")), lightbox, 150)
    }
}

fn is_image_path(h: &str) -> bool {
    let ext = h.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let switcher = self.switcher.as_ref().map(|(sw, _)| {
            div()
                .id("switcher-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .pt(px(120.))
                .bg(hsla(0., 0., 0., if self.theme.dark { 0.6 } else { 0.35 }))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_switcher(window, cx)))
                .child(super::motion::pop_in("switcher-in", div().child(sw.clone())))
        });
        let lightbox = self.lightbox.clone().map(|m| self.render_lightbox(&m, window, cx).into_any_element());
        let t = &self.theme;
        div()
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .font_family(super::rich::UI_FONT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::quick_switch))
            .on_action(cx.listener(Self::open_search))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.show_settings(super::settings_view::Section::Appearance, window, cx)))
            .on_action(cx.listener(|this, _: &ShowAbout, window, cx| this.show_settings(super::settings_view::Section::Advanced, window, cx)))
            .child(self.sidebar.clone())
            .child(match (&self.browser, &self.conversation) {
                _ if self.welcome.is_some() => {
                    div().flex_1().min_w_0().h_full().children(self.welcome.as_ref().map(|(w, _)| w.clone()))
                }
                _ if self.settings.is_some() => {
                    div().flex_1().min_w_0().h_full().children(self.settings.as_ref().map(|(s, _)| s.clone()))
                }
                _ if self.search.is_some() => {
                    div().flex_1().min_w_0().h_full().children(self.search.as_ref().map(|(s, _)| s.clone()))
                }
                (Some(b), _) => div().flex_1().min_w_0().h_full().child(b.clone()),
                (None, Some(c)) => div().flex_1().min_w_0().h_full().child(c.clone()),
                (None, None) => div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(88.))
                            .rounded_full()
                            .bg(t.sidebar)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::icons::icon("messages-square", 40., t.faint)),
                    )
                    .child(
                        div()
                            .font_family(super::rich::DISPLAY_FONT)
                            .text_size(px(17.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.fg_strong)
                            .child("Nothing open"),
                    )
                    .child(div().text_color(t.muted).child("Pick a topic or a direct message from the sidebar.")),
            })
            .children(switcher)
            .children(lightbox)
    }
}
