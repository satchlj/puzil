//! Small controls shared by the dialogs and panels: buttons, fields, check boxes, segmented
//! choices, toggles, menus. They match the settings page and the sidebar menus.

use gpui_kit::{
    component::input::{Input, InputState},
    prelude::FluentBuilder as _,
    *,
};

use std::rc::Rc;

use crate::{icons, theme::Theme};

/// A filled button (add `on_click`). Disabled ones are dimmed and ignore the pointer.
pub fn primary(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, enabled: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .px_3p5()
        .h(px(32.))
        .rounded(px(5.))
        .bg(t.accent)
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(white())
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.9)))
        .when(!enabled, |d| d.opacity(0.45))
        .child(label.into())
}

/// A quiet button: Cancel, Discard.
pub fn secondary(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .px_3p5()
        .h(px(32.))
        .rounded(px(5.))
        .bg(t.input_bg)
        .border_1()
        .border_color(t.divider)
        .cursor_pointer()
        .hover(|s| s.bg(t.bg_selected))
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.fg)
        .child(label.into())
}

/// A button for something destructive (archive, delete).
pub fn danger(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, filled: bool) -> Stateful<Div> {
    let d = div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .px_3p5()
        .h(px(32.))
        .rounded(px(5.))
        .cursor_pointer()
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .child(label.into());
    if filled {
        d.bg(t.danger).text_color(white()).hover(|s| s.opacity(0.9))
    } else {
        d.bg(t.input_bg).border_1().border_color(t.divider).text_color(t.danger).hover(|s| s.bg(t.bg_selected))
    }
}

/// A form field's label.
pub fn label(t: &Theme, text: impl Into<SharedString>) -> Div {
    div().pb(px(5.)).text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(text.into())
}

/// A hint under a field.
pub fn hint(t: &Theme, text: impl Into<SharedString>) -> Div {
    div().pt(px(5.)).text_size(px(12.5)).text_color(t.muted).child(text.into())
}

/// A text input in a bordered box.
pub fn field(t: &Theme, input: &Entity<InputState>, focused: bool) -> Div {
    field_box(t, focused, Input::new(input).appearance(false))
}

/// The box of [`field`] around any one-line field (a spellchecked one).
pub fn field_box(t: &Theme, focused: bool, field: impl IntoElement) -> Div {
    div()
        .h(px(36.))
        .px_2p5()
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(if focused { t.accent } else { t.divider })
        .bg(t.panel)
        .child(div().flex_1().min_w_0().child(field))
}

/// A box that looks like a field but opens a menu: the current choice and a chevron.
pub fn picker(t: &Theme, id: impl Into<ElementId>, text: impl Into<SharedString>, enabled: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(px(36.))
        .px_2p5()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .rounded(px(6.))
        .border_1()
        .border_color(t.divider)
        .bg(t.panel)
        .text_color(t.fg)
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.border_color(t.accent)))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text.into()))
        .when(enabled, |d| d.child(icons::icon("chevron-down", 14., t.muted)))
}

/// A smaller picker for a settings row's right side.
pub fn dropdown(t: &Theme, id: impl Into<ElementId>, text: impl Into<SharedString>, enabled: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(px(30.))
        .max_w(px(260.))
        .px_2p5()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .rounded(px(5.))
        .border_1()
        .border_color(if enabled { t.divider } else { gpui_kit::transparent_black() })
        .when(enabled, |d| d.bg(t.panel).cursor_pointer().hover(|s| s.border_color(t.accent)))
        .text_size(px(13.))
        .text_color(if enabled { t.fg } else { t.muted })
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text.into()))
        .when(enabled, |d| d.child(icons::icon("chevron-down", 12., t.muted)))
}

/// A check box with its label (add `on_click`).
pub fn checkbox(t: &Theme, id: impl Into<ElementId>, on: bool, text: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .py_1()
        .cursor_pointer()
        .text_size(px(14.))
        .child(
            div()
                .size(px(16.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .border_1()
                .border_color(if on { t.accent } else { t.divider })
                .bg(if on { t.accent } else { t.panel })
                .when(on, |d| d.child(icons::icon("check", 12., white()))),
        )
        .child(text.into())
}

/// An on/off switch (add `on_click`).
pub fn toggle(t: &Theme, id: impl Into<ElementId>, on: bool, enabled: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex_none()
        .w(px(40.))
        .h(px(24.))
        .p(px(3.))
        .rounded_full()
        .bg(if on { t.accent } else { t.bg_selected })
        .flex()
        .when(on, |d| d.justify_end())
        .when(enabled, |d| d.cursor_pointer())
        .when(!enabled, |d| d.opacity(0.5))
        .child(div().size(px(18.)).rounded_full().bg(white()).shadow_sm())
}

/// One option of a segmented control, full width: the selected one is filled.
pub fn segment(t: &Theme, id: impl Into<ElementId>, text: impl Into<SharedString>, selected: bool, enabled: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex_1()
        .h(px(32.))
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .when(selected, |d| d.bg(t.accent).text_color(white()))
        .when(!selected && enabled, |d| d.cursor_pointer().text_color(t.fg).hover(|s| s.bg(t.bg_hover)))
        .when(!selected && !enabled, |d| d.text_color(t.faint))
        .child(text.into())
}

/// The frame around [`segment`]s.
pub fn segments(t: &Theme) -> Div {
    div().flex().flex_row().rounded(px(6.)).border_1().border_color(t.divider).overflow_hidden().bg(t.panel)
}

/// A settings row: title and description on the left, `control` on the right.
pub fn row(t: &Theme, title: impl Into<SharedString>, desc: Option<SharedString>, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_4()
        .py(px(11.))
        .border_b_1()
        .border_color(t.border)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(div().text_color(t.fg_strong).font_weight(FontWeight::MEDIUM).child(title.into()))
                .children(desc.map(|d| div().text_size(px(12.5)).text_color(t.muted).child(d))),
        )
        .child(div().flex_none().child(control))
}

/// A popup menu's frame, like the sidebar's right-click menu.
pub fn menu(t: &Theme, id: impl Into<ElementId>, width: f32) -> Stateful<Div> {
    div()
        .id(id.into())
        .occlude()
        .w(px(width))
        .p_1p5()
        .flex()
        .flex_col()
        .rounded(px(6.))
        .border_1()
        .border_color(t.border)
        .bg(t.popover)
        .shadow_lg()
}

/// A menu item (add `on_click`); `checked` puts a check at the end.
pub fn menu_item(t: &Theme, id: impl Into<ElementId>, text: impl Into<SharedString>, checked: bool) -> Stateful<Div> {
    let id = id.into();
    let group = SharedString::from(format!("{id:?}"));
    div()
        .id(id)
        .group(group.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .min_h(px(32.))
        .px_2()
        .rounded(px(4.))
        .cursor_pointer()
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.fg)
        .hover(|s| s.bg(t.accent).text_color(white()))
        .child(div().flex_1().min_w_0().child(text.into()))
        .when(checked, |d| d.child(icons::icon("check", 14., t.fg).group_hover(group, |s| s.text_color(white()))))
}

pub fn separator(t: &Theme) -> Div {
    div().h(px(1.)).mx_1().my_1().bg(t.divider)
}

/// A note in a tinted box: read-only explanations, time-limit warnings.
pub fn note(t: &Theme, warn: bool, text: impl IntoElement) -> Div {
    div()
        .px(px(11.))
        .py(px(9.))
        .rounded(px(6.))
        .text_size(px(13.))
        .when(warn, |d| d.bg(t.mention_me_bg).text_color(t.fg_strong))
        .when(!warn, |d| d.bg(t.bg_hover).text_color(t.muted))
        .child(text)
}

/// A small label shown while the pointer rests on something, for `.tooltip(…)`: who
/// reacted, or a button's name and shortcut.
pub fn tip(text: impl Into<SharedString>, theme: Rc<Theme>, cx: &mut App) -> AnyView {
    let text = text.into();
    cx.new(|_| Tip { text, theme }).into()
}

struct Tip {
    text: SharedString,
    theme: Rc<Theme>,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = &self.theme;
        div()
            .max_w(px(320.))
            .px_2()
            .py_1()
            .rounded(px(6.))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_md()
            .text_size(px(13.))
            .text_color(t.fg_strong)
            .child(self.text.clone())
    }
}
