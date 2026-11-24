//! A search field for people (and optionally user groups), with suggestions under it: adding
//! subscribers, choosing who a permission covers, the people of a new channel.

use std::{collections::HashSet, rc::Rc};

use gpui_kit::{
    component::input::{InputEvent, InputState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::types::UserId;

use super::{
    composer::{ComposerDown, ComposerEscape, ComposerUp},
    kit,
};
use crate::{icons, session::Session, theme::Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Picked {
    User(UserId),
    Group(u64),
}

pub enum PeopleEvent {
    Picked(Picked),
    Dismiss,
}

pub struct PeoplePicker {
    session: Entity<Session>,
    theme: Rc<Theme>,
    input: Entity<InputState>,
    /// Offer user groups too (permissions), not just people.
    groups: bool,
    /// Already chosen: left out of the suggestions.
    pub exclude: HashSet<Picked>,
    sel: usize,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PeopleEvent> for PeoplePicker {}

impl PeoplePicker {
    pub fn new(session: Entity<Session>, theme: Rc<Theme>, groups: bool, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = placeholder.to_string();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subs = vec![cx.subscribe_in(&input, window, |this, _, e: &InputEvent, window, cx| match e {
            InputEvent::Change => {
                this.sel = 0;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.accept(this.sel, window, cx),
            _ => {}
        })];
        Self { session, theme, input, groups, exclude: HashSet::new(), sel: 0, _subs: subs }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.focus(window, cx));
    }

    /// Matches for what's typed: groups first (by name), then people (by name or email).
    fn matches(&self, cx: &App) -> Vec<(Picked, String, String)> {
        let q = self.input.read(cx).value().trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let s = &self.session.read(cx).state;
        let mut out: Vec<(Picked, String, String)> = Vec::new();
        if self.groups {
            for g in s.realm.user_groups.iter().filter(|g| !g.deactivated && !g.is_system_group) {
                if g.name.to_lowercase().contains(&q) && !self.exclude.contains(&Picked::Group(g.id)) {
                    out.push((Picked::Group(g.id), g.name.clone(), format!("{} members", g.members.len())));
                }
            }
        }
        let mut people: Vec<_> = s
            .users
            .values()
            .filter(|u| !u.deactivated && !self.exclude.contains(&Picked::User(u.user_id)))
            .filter(|u| u.full_name.to_lowercase().contains(&q) || u.email.to_lowercase().contains(&q))
            .map(|u| {
                let starts = u.full_name.to_lowercase().split_whitespace().any(|w| w.starts_with(&q));
                (!starts, u.full_name.to_lowercase(), Picked::User(u.user_id), u.full_name.clone(), if u.is_bot { "Bot".to_string() } else { String::new() })
            })
            .collect();
        people.sort();
        out.extend(people.into_iter().map(|(_, _, p, n, d)| (p, n, d)));
        out.truncate(8);
        out
    }

    fn accept(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((p, _, _)) = self.matches(cx).get(i).cloned() {
            self.input.update(cx, |inp, cx| inp.set_value("", window, cx));
            self.sel = 0;
            cx.emit(PeopleEvent::Picked(p));
            cx.notify();
        }
    }
}

impl Render for PeoplePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        let matches = self.matches(cx);
        let images = self.session.read(cx).images.clone();
        let avatars: Vec<Option<String>> = {
            let s = &self.session.read(cx).state;
            matches
                .iter()
                .map(|(p, _, _)| match p {
                    Picked::User(u) => s.users.get(u).and_then(|u| u.avatar_url.clone()),
                    Picked::Group(_) => None,
                })
                .collect()
        };
        let rows: Vec<AnyElement> = matches
            .iter()
            .zip(avatars)
            .enumerate()
            .map(|(i, ((p, name, detail), avatar))| {
                let lead: AnyElement = match p {
                    Picked::User(u) => {
                        let img = images.update(cx, |im, cx| im.avatar(*u, avatar.as_deref(), cx));
                        super::avatar::avatar(img, 22., t.user_color(*u), name).into_any_element()
                    }
                    Picked::Group(_) => icons::icon("users", 18., t.icon).into_any_element(),
                };
                div()
                    .id(("person", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(34.))
                    .px_2()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(i == self.sel, |d| d.bg(t.bg_selected))
                    .hover(|d| d.bg(t.bg_selected.opacity(0.6)))
                    .child(div().w(px(22.)).flex().justify_center().child(lead))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(t.fg_strong).child(name.clone()))
                    .child(div().flex_none().text_size(px(12.)).text_color(t.muted).child(detail.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| this.accept(i, window, cx)))
                    .into_any_element()
            })
            .collect();
        let n = rows.len();
        div()
            .relative()
            .on_action(cx.listener(move |this, _: &ComposerUp, _, cx| {
                if n > 0 {
                    this.sel = (this.sel + n - 1) % n;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(move |this, _: &ComposerDown, _, cx| {
                if n > 0 {
                    this.sel = (this.sel + 1) % n;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|_, _: &ComposerEscape, _, cx| cx.emit(PeopleEvent::Dismiss)))
            .child(kit::field(&t, &self.input, focused))
            .when(focused && n > 0, |d| {
                d.child(
                    deferred(anchored().snap_to_window_with_margin(px(8.)).child(kit::menu(&t, "people-suggestions", 320.).mt_1().children(rows)))
                        .with_priority(2),
                )
            })
    }
}
