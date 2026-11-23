//! First run, after signing in: pick a theme and a mode, with the app recolouring live
//! behind the cards. Shown once; Settings › Appearance has the same choices later.

use std::rc::Rc;

use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    icons,
    settings::{self, Mode, Scope},
    theme::{Palette, Theme},
};

pub enum WelcomeEvent {
    Done,
}

pub struct Welcome {
    theme: Rc<Theme>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WelcomeEvent> for Welcome {}

impl Welcome {
    pub fn new(theme: Rc<Theme>, cx: &mut Context<Self>) -> Self {
        Self { theme, _subs: vec![cx.observe(&settings::handle(cx), |_, _, cx| cx.notify())] }
    }

    pub fn set_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    fn set(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut settings::Prefs)) {
        settings::handle(cx).update(cx, |s, cx| s.update(&Scope::App, f, cx));
    }
}

impl Render for Welcome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let eff = settings::effective(cx, None);
        let card = |p: Palette, cx: &mut Context<Self>| {
            let selected = eff.palette == p;
            let th = Theme::of(p, t.dark);
            div()
                .id(SharedString::from(format!("welcome-{}", p.name())))
                .w(px(180.))
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .rounded(px(10.))
                .border_2()
                .border_color(if selected { t.accent } else { t.border })
                .bg(t.panel)
                .cursor_pointer()
                .hover(|s| s.border_color(if selected { t.accent } else { t.faint }))
                .child(
                    div()
                        .h(px(96.))
                        .rounded(px(6.))
                        .overflow_hidden()
                        .flex()
                        .flex_row()
                        .child(div().w(px(42.)).h_full().bg(th.sidebar).p_1p5().flex().flex_col().gap_1().children(
                            (0..4).map(|i| div().h(px(5.)).w(px(if i == 1 { 24. } else { 30. })).rounded_full().bg(if i == 1 { th.fg_strong } else { th.muted })),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .h_full()
                                .bg(th.bg)
                                .p_2()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(div().flex().flex_row().gap_1p5().items_center().child(div().size(px(14.)).rounded_full().bg(th.accent)).child(div().h(px(6.)).w(px(48.)).rounded_full().bg(th.fg_strong)))
                                .child(div().h(px(5.)).w(px(96.)).rounded_full().bg(th.muted))
                                .child(div().h(px(5.)).w(px(70.)).rounded_full().bg(th.link))
                                .child(div().mt_auto().h(px(14.)).w_full().rounded(px(4.)).bg(th.composer_bg).border_1().border_color(th.border)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .px_0p5()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.fg_strong)
                        .child(p.label())
                        .when(selected, |d| d.child(icons::icon("check", 15., t.accent))),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set(cx, |l| l.palette = Some(p.name().to_string()))))
        };
        let cards: Vec<_> = Palette::ALL.iter().map(|p| card(*p, cx).into_any_element()).collect();
        let modes = div().flex().flex_row().p_0p5().gap_0p5().rounded(px(8.)).bg(t.input_bg).children(
            [("System", Mode::System, "monitor"), ("Light", Mode::Light, "sun"), ("Dark", Mode::Dark, "moon")].into_iter().map(|(name, m, ic)| {
                let selected = eff.mode == m;
                div()
                    .id(SharedString::from(format!("welcome-mode-{name}")))
                    .px_4()
                    .h(px(34.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .font_weight(FontWeight::MEDIUM)
                    .when(selected, |d| d.bg(t.panel).text_color(t.fg_strong).shadow_sm())
                    .when(!selected, |d| d.text_color(t.muted))
                    .child(icons::icon(ic, 15., if selected { t.fg_strong } else { t.icon }))
                    .child(name)
                    .on_click(cx.listener(move |this, _, _, cx| this.set(cx, |l| l.mode = Some(m))))
            }),
        );
        div()
            .id("welcome")
            .size_full()
            .overflow_y_scroll()
            .bg(t.bg)
            .text_color(t.fg)
            .child(
                div()
                    .max_w(px(640.))
                    .mx_auto()
                    .px_6()
                    .py_12()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(gpui_kit::img(ImageSource::Render(icons::app_icon())).size(px(72.)))
                    .child(div().font_family(super::rich::DISPLAY_FONT).text_size(px(26.)).font_weight(FontWeight::BOLD).text_color(t.fg_strong).child("Make Puzil yours"))
                    .child(div().text_center().text_color(t.muted).child("Pick a theme. You can change it any time in Settings (⌘,)."))
                    .child(div().pt_4().flex().flex_row().flex_wrap().justify_center().gap_3().children(cards))
                    .child(div().pt_4().child(modes))
                    .child(
                        div()
                            .id("welcome-done")
                            .mt_6()
                            .px_6()
                            .h(px(42.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .bg(t.accent)
                            .text_color(white())
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.9))
                            .child("Get started")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set(cx, |l| l.onboarded = Some(true));
                                cx.emit(WelcomeEvent::Done);
                            })),
                    )
                    .child(
                        div()
                            .pt_2()
                            .text_size(px(13.))
                            .text_color(t.faint)
                            .child("⌘K jumps anywhere · ⌘F searches · right-click a message for more"),
                    ),
            )
    }
}
