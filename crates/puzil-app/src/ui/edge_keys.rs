//! ↑ on a text field's first line goes to the start of its text, ↓ on its last line to the
//! end (as in macOS text fields). The field's own ↑/↓ only move between lines, so when one
//! leaves the cursor where it was, the cursor goes to that end instead.
//!
//! Bound in the fields' context ahead of their own ↑/↓ but behind the composer's (lists that
//! use the arrows still get them first). The handler sits at the window's root, which can't
//! see the focused field, so every field is tracked here as it's made.

use gpui_kit::{
    component::input::{InputState, MoveToEnd, MoveToStart, TextareaState},
    *,
};

actions!(edge_keys, [EdgeUp, EdgeDown]);

#[derive(Clone)]
enum Field {
    Input(WeakEntity<InputState>),
    Area(WeakEntity<TextareaState>),
}

impl Field {
    /// The cursor and selection, or None once the field is gone.
    fn state(&self, cx: &App) -> Option<(usize, std::ops::Range<usize>)> {
        match self {
            Field::Input(w) => w.upgrade().map(|e| (e.read(cx).cursor(), e.read(cx).selected_range())),
            Field::Area(w) => w.upgrade().map(|e| (e.read(cx).cursor(), e.read(cx).selected_range())),
        }
    }

    fn focused(&self, window: &Window, cx: &App) -> bool {
        match self {
            Field::Input(w) => w.upgrade().is_some_and(|e| e.read(cx).focus_handle(cx).is_focused(window)),
            Field::Area(w) => w.upgrade().is_some_and(|e| e.read(cx).focus_handle(cx).is_focused(window)),
        }
    }
}

#[derive(Default)]
struct Fields(Vec<Field>);

impl Global for Fields {}

/// Call after the text fields' own bindings and before the composer's.
pub fn bind_keys(cx: &mut App) {
    cx.set_global(Fields::default());
    cx.observe_new::<InputState>(|_, _, cx| {
        let w = cx.entity().downgrade();
        cx.update_global::<Fields, _>(|f, _| f.0.push(Field::Input(w)));
    })
    .detach();
    cx.observe_new::<TextareaState>(|_, _, cx| {
        let w = cx.entity().downgrade();
        cx.update_global::<Fields, _>(|f, _| f.0.push(Field::Area(w)));
    })
    .detach();
    cx.bind_keys([KeyBinding::new("up", EdgeUp, Some("Input")), KeyBinding::new("down", EdgeDown, Some("Input"))]);
}

/// For the window's root: `.on_action(|_: &EdgeUp, w, cx| edge_keys::handle(false, w, cx))`.
pub fn handle(down: bool, window: &mut Window, cx: &mut App) {
    // The field's own ↑/↓ runs next either way.
    cx.propagate();
    let field = {
        let fields = cx.global::<Fields>();
        fields.0.iter().find(|f| f.focused(window, cx)).cloned()
    };
    cx.update_global::<Fields, _>(|f, cx| f.0.retain(|x| x.state(cx).is_some()));
    let Some(field) = field else { return };
    let Some((before, sel)) = field.state(cx) else { return };
    if !sel.is_empty() {
        return;
    }
    window.defer(cx, move |window, cx| {
        if field.state(cx).is_some_and(|(after, _)| after == before) {
            if down {
                window.dispatch_action(Box::new(MoveToEnd), cx);
            } else {
                window.dispatch_action(Box::new(MoveToStart), cx);
            }
        }
    });
}
