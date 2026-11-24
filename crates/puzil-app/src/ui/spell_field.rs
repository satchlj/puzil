//! Spellchecking for a text field: misspelled words get a wavy underline, autocorrect fixes
//! a word as it's finished (Backspace straight after puts back what you typed), ⌘; (Ctrl+;
//! off macOS) jumps to a misspelled word and lists its suggestions (↑↓, Enter or Tab to
//! replace, ⌘; again for the next one, Esc to close), and right-clicking a word offers the
//! same, with Cut, Copy and Paste.
//!
//! A view owns a [`Spell`] per field and wraps the field's element with [`Spell::wrap`].
//! The words themselves come from [`crate::spell`].

use std::{collections::HashSet, ops::Range, rc::Rc};

use gpui_kit::{
    component::input::{Copy, Cut, Enter, InputEvent, Paste, TextDecoration, TextareaState},
    prelude::FluentBuilder as _,
    *,
};

use super::composer::{ComposerDown, ComposerEscape, ComposerTab, ComposerUp};
use crate::{spell, theme::Theme};

actions!(spell_field, [SpellNext]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(chord!(";"), SpellNext, Some("Input"))]);
}

/// Where a view keeps a field's [`Spell`], for the listeners.
pub type Getter<V> = fn(&mut V) -> &mut Spell;

#[derive(Clone)]
enum Choice {
    Replace(String),
    Learn,
    Ignore,
    Cut,
    Copy,
    Paste,
}

impl Choice {
    fn label(&self, word: &str) -> String {
        match self {
            Choice::Replace(s) => s.clone(),
            Choice::Learn => format!("Learn “{word}”"),
            Choice::Ignore => "Ignore".into(),
            Choice::Cut => "Cut".into(),
            Choice::Copy => "Copy".into(),
            Choice::Paste => "Paste".into(),
        }
    }
}

struct Popup {
    /// The misspelled word (none for a right-click elsewhere: just Cut, Copy, Paste).
    word: Option<(Range<usize>, String)>,
    items: Vec<Choice>,
    sel: usize,
    /// A right-click menu, at the pointer; otherwise ⌘;'s list, above the field.
    at: Option<Point<Pixels>>,
    /// Where the cursor was before ⌘; selected the word, to put it back.
    restore: Option<usize>,
}

/// An autocorrection just made, so Backspace can take it back.
struct Fix {
    start: usize,
    corrected: String,
    original: String,
    /// The text as it will be once Backspace deletes the separator typed after it.
    after_backspace: String,
}

pub struct Spell {
    field: Entity<TextareaState>,
    popup: Option<Popup>,
    fix: Option<Fix>,
    /// Words autocorrect was told (by Backspace) to leave alone.
    keep: HashSet<String>,
    /// The text as last seen, to tell typing from other changes.
    prev: String,
    color: Hsla,
    /// Off for fields whose text isn't prose (none yet), and while a value is set from code.
    pub enabled: bool,
    _sub: Subscription,
}

impl Spell {
    pub fn new<V: 'static>(field: &Entity<TextareaState>, get: Getter<V>, theme: &Theme, window: &mut Window, cx: &mut Context<V>) -> Self {
        let sub = cx.subscribe_in(field, window, move |v, _, e: &InputEvent, window, cx| {
            if let InputEvent::Change = e {
                get(v).changed(window, cx);
                cx.notify();
            }
        });
        let weak = cx.weak_entity();
        let input = field.downgrade();
        field.update(cx, |f, _| {
            f.on_context_menu(Rc::new(move |_, _, position, window, cx| {
                let Some(offset) = input.upgrade().and_then(|i| i.read(cx).context_menu_offset()) else { return };
                let _ = weak.update(cx, |v, cx| {
                    get(v).open_menu(offset, position, window, cx);
                    cx.notify();
                });
            }));
        });
        let prev = field.read(cx).value().to_string();
        let mut this = Self { field: field.clone(), popup: None, fix: None, keep: HashSet::new(), prev, color: theme.danger, enabled: true, _sub: sub };
        this.underline(cx);
        this
    }

    fn text(&self, cx: &App) -> String {
        self.field.read(cx).value().to_string()
    }

    /// After the text changed: undo an autocorrection (Backspace), make one (a word just
    /// finished), and redraw the underlines.
    fn changed(&mut self, window: &mut Window, cx: &mut App) {
        let text = self.text(cx);
        if text == self.prev {
            return;
        }
        self.popup = None;
        let cursor = self.field.read(cx).cursor();
        if !self.enabled {
            self.prev = text;
            return self.underline(cx);
        }
        if let Some(fix) = self.fix.take()
            && text == fix.after_backspace
            && cursor == fix.start + fix.corrected.len()
        {
            // Backspace right after a correction: what was typed comes back, and stays.
            let range = fix.start..fix.start + fix.corrected.len();
            self.replace(range, &fix.original, window, cx);
            self.keep.insert(fix.original.clone());
            let end = fix.start + fix.original.len();
            self.field.update(cx, |f, cx| f.set_selected_range(end..end, cx));
        } else {
            self.autocorrect(&text, cursor, window, cx);
        }
        self.prev = self.text(cx);
        self.underline(cx);
    }

    /// If one character was just typed and it ends a word (a space or punctuation), and
    /// that word is misspelled and the checker is sure of the fix, make it.
    fn autocorrect(&mut self, text: &str, cursor: usize, window: &mut Window, cx: &mut App) {
        let Some(sep) = text[..cursor.min(text.len())].chars().next_back() else { return };
        if !(sep.is_whitespace() || ".,!?;:)".contains(sep)) {
            return;
        }
        let at = cursor - sep.len_utf8();
        // Only typing: the text is the old text with this one character added.
        if text.len() != self.prev.len() + sep.len_utf8() || format!("{}{}", &text[..at], &text[cursor..]) != self.prev {
            return;
        }
        let Some(r) = spell::words(text).into_iter().find(|r| r.end == at) else { return };
        let word = text[r.clone()].to_string();
        if self.keep.contains(&word) {
            return;
        }
        spell::set_autocorrect(crate::settings::effective(cx, None).autocorrect);
        let Some(fixed) = spell::correction(&word) else { return };
        self.replace(r.clone(), &fixed, window, cx);
        let shift = fixed.len() as isize - word.len() as isize;
        let new_cursor = (cursor as isize + shift) as usize;
        self.field.update(cx, |f, cx| f.set_selected_range(new_cursor..new_cursor, cx));
        let now = self.text(cx);
        let after_backspace = format!("{}{}", &now[..r.start + fixed.len()], &now[r.start + fixed.len() + sep.len_utf8()..]);
        self.fix = Some(Fix { start: r.start, corrected: fixed, original: word, after_backspace });
    }

    fn replace(&mut self, range: Range<usize>, with: &str, window: &mut Window, cx: &mut App) {
        let with = with.to_string();
        self.field.update(cx, |f, cx| {
            f.set_selected_range(range, cx);
            f.replace(with, window, cx);
        });
    }

    /// Wavy underlines under the misspelled words, except one still being typed (the word
    /// the cursor is at the end of).
    fn underline(&mut self, cx: &mut App) {
        let text = self.text(cx);
        let cursor = self.field.read(cx).cursor();
        let style = HighlightStyle { underline: Some(UnderlineStyle { thickness: px(1.), color: Some(self.color), wavy: true }), ..Default::default() };
        let marks: Vec<TextDecoration> = if self.enabled {
            spell::mistakes(&text)
                .into_iter()
                .filter(|r| !(r.end == cursor && self.popup.is_none() && typing_at(&text, cursor)))
                .map(|r| TextDecoration::new(r, style))
                .collect()
        } else {
            vec![]
        };
        self.field.update(cx, |f, cx| f.set_decorations(marks, cx));
    }

    /// ⌘;: the misspelled word at or before the cursor (the next one when the list is
    /// already open), selected, with its suggestions.
    fn next(&mut self, window: &mut Window, cx: &mut App) {
        let text = self.text(cx);
        let all = spell::mistakes(&text);
        if all.is_empty() {
            return self.close(window, cx);
        }
        let (cursor, restore) = match &self.popup {
            Some(Popup { word: Some((r, _)), at: None, restore, .. }) => (r.end, *restore),
            _ => {
                let c = self.field.read(cx).cursor();
                (c, Some(c))
            }
        };
        let r = if self.popup.as_ref().is_some_and(|p| p.at.is_none()) {
            all.iter().find(|r| r.start >= cursor).or(all.first()).cloned()
        } else {
            all.iter().rev().find(|r| r.start <= cursor).or(all.first()).cloned()
        };
        let Some(r) = r else { return };
        let word = text[r.clone()].to_string();
        let mut items: Vec<Choice> = spell::suggestions(&word).into_iter().map(Choice::Replace).collect();
        items.extend([Choice::Learn, Choice::Ignore]);
        self.field.update(cx, |f, cx| f.set_selected_range(r.clone(), cx));
        self.popup = Some(Popup { word: Some((r, word)), items, sel: 0, at: None, restore });
    }

    /// A right-click at `offset`.
    fn open_menu(&mut self, offset: usize, position: Point<Pixels>, _: &mut Window, cx: &mut App) {
        let text = self.text(cx);
        let word = spell::mistakes(&text).into_iter().find(|r| r.start <= offset && offset <= r.end).map(|r| (r.clone(), text[r].to_string()));
        let mut items: Vec<Choice> = Vec::new();
        if let Some((_, w)) = &word {
            items.extend(spell::suggestions(w).into_iter().map(Choice::Replace));
            items.extend([Choice::Learn, Choice::Ignore]);
        }
        items.extend([Choice::Cut, Choice::Copy, Choice::Paste]);
        self.popup = Some(Popup { word, items, sel: 0, at: Some(position), restore: None });
    }

    fn close(&mut self, _: &mut Window, cx: &mut App) {
        if let Some(Popup { restore: Some(c), .. }) = self.popup.take() {
            self.field.update(cx, |f, cx| f.set_selected_range(c..c, cx));
        }
        self.underline(cx);
    }

    fn choose(&mut self, i: usize, window: &mut Window, cx: &mut App) {
        let Some(p) = self.popup.take() else { return };
        let Some(choice) = p.items.get(i).cloned() else { return };
        let word = p.word.clone();
        let restore = p.restore;
        match (choice, word) {
            (Choice::Replace(s), Some((r, w))) => {
                self.replace(r.clone(), &s, window, cx);
                let shift = s.len() as isize - w.len() as isize;
                let c = match restore {
                    Some(c) if c >= r.end => (c as isize + shift) as usize,
                    Some(c) if c <= r.start => c,
                    _ => r.start + s.len(),
                };
                self.field.update(cx, |f, cx| f.set_selected_range(c..c, cx));
                self.prev = self.text(cx);
            }
            (Choice::Learn, Some((_, w))) => {
                spell::learn(&w);
                self.popup = Some(p);
                self.close(window, cx);
            }
            (Choice::Ignore, Some((_, w))) => {
                spell::ignore(&w);
                self.popup = Some(p);
                self.close(window, cx);
            }
            (Choice::Cut, _) => window.dispatch_action(Box::new(Cut), cx),
            (Choice::Copy, _) => window.dispatch_action(Box::new(Copy), cx),
            (Choice::Paste, _) => window.dispatch_action(Box::new(Paste), cx),
            _ => {}
        }
        self.underline(cx);
    }

    fn step(&mut self, by: isize) {
        if let Some(p) = &mut self.popup {
            let n = p.items.len() as isize;
            p.sel = (p.sel as isize + by).rem_euclid(n.max(1)) as usize;
        }
    }

    /// The field's element, with the keys and the suggestion list or menu around it.
    pub fn wrap<V: 'static>(&mut self, field: impl IntoElement, get: Getter<V>, theme: &Rc<Theme>, window: &Window, cx: &mut Context<V>) -> Div {
        if self.color != theme.danger {
            self.color = theme.danger;
            self.underline(cx);
        }
        // Text set from code (`set_value` reports no change): a cleared composer, a chosen
        // suggestion. Typing is left to the change events, which autocorrect needs.
        let text = self.field.read(cx).value();
        if text != self.prev.as_str() && (text.is_empty() || !self.field.read(cx).focus_handle(cx).is_focused(window)) {
            self.reset(cx);
        }
        let t = theme.clone();
        let popup = self.popup.as_ref().map(|p| {
            let word = p.word.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
            let mut rows: Vec<AnyElement> = Vec::new();
            for (i, c) in p.items.iter().enumerate() {
                let gap = i > 0 && matches!(c, Choice::Learn | Choice::Cut) && !matches!(p.items[i - 1], Choice::Ignore | Choice::Learn) || (matches!(c, Choice::Cut) && i > 0);
                if gap {
                    rows.push(div().h(px(1.)).mx_2().my_1().bg(t.border).into_any_element());
                }
                let bold = matches!(c, Choice::Replace(_));
                rows.push(
                    div()
                        .id(("spell", i))
                        .h(px(30.))
                        .px_2()
                        .mx_1()
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .text_color(t.fg_strong)
                        .when(bold, |d| d.font_weight(FontWeight::MEDIUM))
                        .when(i == p.sel, |d| d.bg(t.bg_selected))
                        .hover(|d| d.bg(t.bg_selected.opacity(0.6)))
                        .child(c.label(&word))
                        .on_click(cx.listener(move |v, _, window, cx| {
                            get(v).choose(i, window, cx);
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            if p.word.is_some() && p.items.iter().all(|c| !matches!(c, Choice::Replace(_))) {
                rows.insert(0, div().px_3().py_1().text_size(px(13.)).text_color(t.faint).child("No suggestions").into_any_element());
            }
            let menu = p.at.is_some();
            let card = div()
                .id("spell-card")
                .min_w(px(200.))
                .max_w(px(320.))
                .py_1()
                .flex()
                .flex_col()
                .rounded(px(8.))
                .border_1()
                .border_color(t.border)
                .bg(t.popover)
                .shadow_lg()
                .text_size(px(14.))
                .occlude()
                .children(rows)
                .when(!menu, |d| {
                    d.child(div().px_3().pt_1().pb(px(2.)).text_size(px(12.)).text_color(t.faint).child(format!("↑↓ choose · Enter replaces · {} next · Esc", if cfg!(target_os = "macos") { "⌘;" } else { "Ctrl+;" })))
                })
                // A click elsewhere closes it and leaves the cursor where that click puts it.
                .on_mouse_down_out(cx.listener(move |v, _, _, cx| {
                    let s = get(v);
                    s.popup = None;
                    s.underline(cx);
                    cx.notify();
                }));
            match p.at {
                Some(at) => deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(card)).with_priority(2).into_any_element(),
                None => div().absolute().bottom_full().left_0().mb_1().child(card).into_any_element(),
            }
        });
        div()
            .relative()
            .capture_action(cx.listener(move |v, _: &Enter, window, cx| {
                let s = get(v);
                if let Some(p) = &s.popup {
                    cx.stop_propagation();
                    let sel = p.sel;
                    s.choose(sel, window, cx);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(move |v, _: &ComposerTab, window, cx| {
                let s = get(v);
                match &s.popup {
                    Some(p) if p.at.is_none() => {
                        let sel = p.sel;
                        s.choose(sel, window, cx);
                        cx.notify();
                    }
                    _ => cx.propagate(),
                }
            }))
            .on_action(cx.listener(move |v, _: &ComposerUp, _, cx| {
                let s = get(v);
                if s.popup.is_some() {
                    s.step(-1);
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(move |v, _: &ComposerDown, _, cx| {
                let s = get(v);
                if s.popup.is_some() {
                    s.step(1);
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(move |v, _: &ComposerEscape, window, cx| {
                let s = get(v);
                if s.popup.is_some() {
                    s.close(window, cx);
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(move |v, _: &SpellNext, window, cx| {
                get(v).next(window, cx);
                cx.notify();
            }))
            .child(field)
            .children(popup)
    }

    /// Set the field's text from code (a draft, an edit): no autocorrect, fresh underlines.
    pub fn reset(&mut self, cx: &mut App) {
        self.prev = self.text(cx);
        self.fix = None;
        self.popup = None;
        self.underline(cx);
    }
}

/// Whether the cursor is at the end of a word still being typed (nothing after it on the
/// line but the end of the text or a space).
fn typing_at(text: &str, cursor: usize) -> bool {
    text[cursor.min(text.len())..].chars().next().is_none_or(|c| c == '\n')
}
