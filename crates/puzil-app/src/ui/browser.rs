//! Every channel in the organization, grouped by folder, with Join and Leave: how to see and
//! change what the sidebar shows.

use std::{collections::HashMap, rc::Rc};

use gpui_kit::{
    component::input::{Input, InputEvent, InputState, Textarea, TextareaState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::{
    client::NewChannel,
    perms::Privacy,
    types::{StreamId, UserId},
};

use super::{
    kit,
    people_picker::{PeopleEvent, PeoplePicker, Picked},
};
use crate::{icons, runtime::on_tokio, session::Session, theme::Theme};

pub enum BrowserEvent {
    /// A channel was created: go write its first message.
    Created(StreamId),
    /// Open a channel's settings (an archived one, to unarchive it).
    Settings(StreamId),
}

/// The New channel form.
struct Form {
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    desc_spell: super::spell_field::Spell,
    privacy: Privacy,
    folder: Option<i64>,
    folder_menu: Option<Point<Pixels>>,
    /// When a click outside closed the folder menu: the same click on its button mustn't reopen it.
    folder_closed: Option<std::time::Instant>,
    people: Vec<UserId>,
    picker: Entity<PeoplePicker>,
    busy: bool,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

pub struct ChannelBrowser {
    session: Entity<Session>,
    theme: Rc<Theme>,
    filter: Entity<InputState>,
    /// Joins (true) and leaves (false) sent but not yet reflected in our subscriptions.
    pending: HashMap<StreamId, bool>,
    show_archived: bool,
    form: Option<Form>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<BrowserEvent> for ChannelBrowser {}

impl ChannelBrowser {
    pub fn new(session: Entity<Session>, theme: Rc<Theme>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search channels"));
        let subs = vec![
            cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify()),
            // Subscription events land through the session; redraw when they do.
            cx.observe(&session, |this, session, cx| {
                let subs = &session.read(cx).state.subs;
                this.pending.retain(|sid, join| subs.contains_key(sid) != *join);
                cx.notify();
            }),
        ];
        filter.update(cx, |f, cx| f.focus(window, cx));
        Self { session, theme, filter, pending: HashMap::new(), show_archived: false, form: None, _subs: subs }
    }

    fn new_channel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = self.session.read(cx).state.me;
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Channel name"));
        // One line, as a textarea so misspellings can be underlined.
        let description = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 1).submit_on_enter(true).placeholder("Optional"));
        let desc_spell = super::spell_field::Spell::new(&description, |b: &mut Self| &mut b.form.as_mut().expect("form").desc_spell, &self.theme, window, cx);
        let picker = cx.new(|cx| PeoplePicker::new(self.session.clone(), self.theme.clone(), false, "Add people…", window, cx));
        picker.update(cx, |p, _| {
            p.exclude.insert(Picked::User(me));
        });
        let subs = vec![
            cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe_in(&name, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.create(cx);
                }
            }),
            cx.subscribe(&picker, |this, picker, e: &PeopleEvent, cx| {
                if let (PeopleEvent::Picked(Picked::User(u)), Some(f)) = (e, &mut this.form) {
                    if !f.people.contains(u) {
                        f.people.push(*u);
                    }
                    picker.update(cx, |p, _| {
                        p.exclude.insert(Picked::User(*u));
                    });
                    cx.notify();
                }
            }),
        ];
        let privacy = if self.session.read(cx).state.can_create_channel(Privacy::Public) { Privacy::Public } else { Privacy::PrivateShared };
        name.update(cx, |n, cx| n.focus(window, cx));
        self.form = Some(Form { name, description, desc_spell, privacy, folder: None, folder_menu: None, folder_closed: None, people: vec![me], picker, busy: false, error: None, _subs: subs });
        cx.notify();
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let Some(f) = &mut self.form else { return };
        let name = f.name.read(cx).value().trim().to_string();
        if name.is_empty() || f.busy {
            return;
        }
        let (invite_only, is_web_public, history_public_to_subscribers) = f.privacy.flags();
        let req = NewChannel {
            name,
            description: f.description.read(cx).value().replace('\n', " ").trim().to_string(),
            subscribers: f.people.clone(),
            invite_only,
            is_web_public,
            history_public_to_subscribers,
            folder_id: f.folder,
        };
        f.busy = true;
        f.error = None;
        cx.notify();
        let client = self.session.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { client.create_channel(&req).await }).await;
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(sid) => {
                        this.form = None;
                        cx.emit(BrowserEvent::Created(sid));
                    }
                    Err(e) => {
                        if let Some(f) = &mut this.form {
                            f.busy = false;
                            f.error = Some(match e {
                                puzil_zulip::ApiError::Api { msg, .. } if !msg.is_empty() => msg,
                                e => e.to_string(),
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_form(&self, desc_field: Option<AnyElement>, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.form.as_ref()?;
        let desc_field = desc_field?;
        let t = self.theme.clone();
        let s = self.session.read(cx);
        let options: Vec<(Privacy, &str)> = [(Privacy::Public, "Public"), (Privacy::PrivateShared, "Private, shared history"), (Privacy::PrivateProtected, "Private, protected")]
            .into_iter()
            .filter(|(p, _)| s.state.can_create_channel(*p))
            .collect();
        let folders: Vec<(i64, String)> = s.state.realm.channel_folders.iter().filter(|f| !f.is_archived).map(|f| (f.id, f.name.clone())).collect();
        let folder = f.folder.and_then(|id| folders.iter().find(|(i, _)| *i == id)).map(|(_, n)| n.clone()).unwrap_or("None".into());
        let images = s.images.clone();
        let people: Vec<(UserId, String, Option<String>)> = f
            .people
            .iter()
            .map(|u| (*u, if *u == s.state.me { "You".to_string() } else { s.state.user_name(*u) }, s.state.users.get(u).and_then(|x| x.avatar_url.clone())))
            .collect();
        let me = s.state.me;
        let name_focused = f.name.read(cx).focus_handle(cx).is_focused(window);
        let desc_focused = f.description.read(cx).focus_handle(cx).is_focused(window);
        let can_create = !f.name.read(cx).value().trim().is_empty() && !f.busy;

        let mut seg = kit::segments(&t);
        for (i, (p, label)) in options.iter().enumerate() {
            if i > 0 {
                seg = seg.child(div().w(px(1.)).bg(t.divider));
            }
            let p = *p;
            seg = seg.child(kit::segment(&t, ("new-privacy", i), label.to_string(), f.privacy == p, true).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(f) = &mut this.form {
                    f.privacy = p;
                    cx.notify();
                }
            })));
        }
        let mut chips = div().flex().flex_row().flex_wrap().gap_1().pb_1p5();
        for (u, name, avatar) in people {
            let img = images.update(cx, |i, cx| i.avatar(u, avatar.as_deref(), cx));
            chips = chips.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .pl(px(3.))
                    .pr_1p5()
                    .h(px(24.))
                    .rounded_full()
                    .border_1()
                    .border_color(t.divider)
                    .bg(t.panel)
                    .text_size(px(12.5))
                    .child(super::avatar::avatar(img, 18., t.user_color(u), &name))
                    .child(name)
                    .when(u != me, |d| {
                        d.child(div().id(("new-x", u as usize)).cursor_pointer().child(icons::icon("x", 12., t.muted)).on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(f) = &mut this.form {
                                f.people.retain(|x| *x != u);
                                f.picker.update(cx, |p, _| {
                                    p.exclude.remove(&Picked::User(u));
                                });
                                cx.notify();
                            }
                        })))
                    }),
            );
        }
        let folder_menu = f.folder_menu.map(|at| {
            let mut menu = kit::menu(&t, "new-folder-menu", 220.).on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if let Some(f) = &mut this.form {
                    f.folder_menu = None;
                    f.folder_closed = Some(std::time::Instant::now());
                    cx.notify();
                }
            }));
            let pick = |id: Option<i64>| {
                move |this: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                    if let Some(f) = &mut this.form {
                        f.folder = id;
                        f.folder_menu = None;
                        cx.notify();
                    }
                }
            };
            menu = menu.child(kit::menu_item(&t, "nf-none", "None", f.folder.is_none()).on_click(cx.listener(pick(None))));
            for (id, name) in &folders {
                menu = menu.child(kit::menu_item(&t, ("nf", *id as usize), name.clone(), f.folder == Some(*id)).on_click(cx.listener(pick(Some(*id)))));
            }
            deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(1)
        });
        Some(
            div()
                .max_w(px(560.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .py_2()
                .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(t.fg_strong).child("New channel"))
                .child(div().child(kit::label(&t, "Name")).child(kit::field(&t, &f.name, name_focused)))
                .child(div().child(kit::label(&t, "Description")).child(kit::field_box(&t, desc_focused, desc_field)))
                .child(div().child(kit::label(&t, "Who can see it")).child(seg))
                .when(!folders.is_empty(), |d| {
                    d.child(div().child(kit::label(&t, "Folder")).child(div().w(px(240.)).child(kit::picker(&t, "new-folder", folder, true).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if let Some(f) = &mut this.form {
                                let just_closed = f.folder_closed.take().is_some_and(|t| t.elapsed().as_millis() < 250);
                                f.folder_menu = (!just_closed).then_some(e.position);
                                cx.notify();
                            }
                        }),
                    ))))
                })
                .child(div().child(kit::label(&t, "People")).child(chips).child(f.picker.clone()))
                .child(kit::hint(&t, "Permissions start at the organization's defaults; change them in Channel settings afterwards."))
                .when_some(f.error.clone(), |d, e| d.child(div().text_size(px(13.)).text_color(t.danger).child(e)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(kit::primary(&t, "new-create", if f.busy { "Creating…" } else { "Create channel" }, can_create).on_click(cx.listener(|this, _, _, cx| this.create(cx))))
                        .child(kit::secondary(&t, "new-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.form = None;
                            cx.notify();
                        }))),
                )
                .children(folder_menu)
                .into_any_element(),
        )
    }

    pub fn set_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    fn toggle(&mut self, sid: StreamId, name: String, join: bool, cx: &mut Context<Self>) {
        self.pending.insert(sid, join);
        cx.notify();
        let this = cx.entity().downgrade();
        self.session.update(cx, |s, cx| {
            s.call(
                cx,
                move |c| async move { if join { c.subscribe(&[name]).await } else { c.unsubscribe(&[name]).await } },
                move |_, e, cx| {
                    tracing::warn!("changing subscription failed: {e}");
                    let _ = this.update(cx, |b, cx| {
                        b.pending.remove(&sid);
                        cx.notify();
                    });
                },
            )
        });
    }
}

impl Render for ChannelBrowser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let desc_field = {
            let t = self.theme.clone();
            self.form.as_mut().map(|f| {
                let field = Textarea::new(&f.description).appearance(false);
                f.desc_spell.wrap(field, |b: &mut Self| &mut b.form.as_mut().expect("form").desc_spell, &t, window, cx).into_any_element()
            })
        };
        let form = self.render_form(desc_field, window, cx);
        let q = self.filter.read(cx).value().to_lowercase();
        let show_archived = self.show_archived;
        let can_create = self.session.read(cx).state.can_create_any_channel();
        let s = self.session.read(cx);
        let mut channels: Vec<_> = s
            .state
            .streams
            .values()
            .filter(|st| show_archived || !st.is_archived)
            .filter(|st| q.is_empty() || st.name.to_lowercase().contains(&q) || st.description.to_lowercase().contains(&q))
            .collect();
        channels.sort_by_key(|st| st.name.to_lowercase());
        let total = channels.len();
        let joined = channels.iter().filter(|st| s.state.subs.contains_key(&st.stream_id)).count();
        let folders: Vec<_> = s.state.realm.channel_folders.iter().filter(|f| !f.is_archived).collect();
        let mut groups: Vec<(String, Vec<_>)> = folders
            .iter()
            .map(|f| (f.name.clone(), channels.iter().filter(|st| st.folder_id == Some(f.id)).copied().collect::<Vec<_>>()))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let rest: Vec<_> =
            channels.iter().filter(|st| !st.folder_id.is_some_and(|id| folders.iter().any(|f| f.id == id))).copied().collect();
        if !rest.is_empty() {
            groups.push((if groups.is_empty() { "Channels".into() } else { "Other channels".into() }, rest));
        }

        let mut rows: Vec<AnyElement> = Vec::new();
        for (label, members) in groups {
            rows.push(
                div().pt_5().pb_1().px_2().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(label).into_any_element(),
            );
            for st in members {
                let sid = st.stream_id;
                let archived = st.is_archived;
                let subscribed = s.state.subs.contains_key(&sid);
                let busy = self.pending.contains_key(&sid);
                let name = st.name.clone();
                let description = st.description.split_whitespace().collect::<Vec<_>>().join(" ");
                let button = div()
                    .id(("join", sid as usize))
                    .group("join")
                    .flex_none()
                    .w(px(84.))
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .rounded(px(4.))
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .map(|d| match (busy, subscribed) {
                        (true, _) => d.bg(t.bg_selected).text_color(t.muted).child("…"),
                        // "Joined", which reads "Leave" under the pointer.
                        (false, true) => d
                            .relative()
                            .bg(t.bg_selected.opacity(0.6))
                            .text_color(t.fg)
                            .hover(|s| s.bg(t.danger))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .group_hover("join", |s| s.invisible())
                                    .child(icons::icon("check", 14., t.fg))
                                    .child("Joined"),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(white())
                                    .invisible()
                                    .group_hover("join", |s| s.visible())
                                    .child("Leave"),
                            ),
                        (false, false) => d.bg(t.accent).text_color(white()).hover(|s| s.opacity(0.88)).child("Join"),
                    })
                    .when(!busy, |d| {
                        let name = name.clone();
                        d.on_click(cx.listener(move |this, _, _, cx| this.toggle(sid, name.clone(), !subscribed, cx)))
                    });
                // An archived channel can't be joined; its settings can unarchive it.
                let button = if archived {
                    kit::secondary(&t, ("archived-settings", sid as usize), "Settings")
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(BrowserEvent::Settings(sid))))
                        .into_any_element()
                } else {
                    button.into_any_element()
                };
                rows.push(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .px_2()
                        .py_2()
                        .rounded(px(6.))
                        .hover(|s| s.bg(t.bg_hover))
                        .when(archived, |d| d.opacity(0.7))
                        .child(icons::channel(st.invite_only, 20., t.faint, t.bg))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_2()
                                        .items_baseline()
                                        .child(div().text_color(t.fg_strong).font_weight(FontWeight::MEDIUM).child(name))
                                        .when(archived, |d| d.child(div().text_size(px(12.)).text_color(t.muted).child("Archived"))),
                                )
                                .when(!description.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(t.muted)
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .child(description),
                                    )
                                }),
                        )
                        .child(button)
                        .into_any_element(),
                );
            }
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .child(
                crate::theme::drag_region(div().id("browser-header"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(48.))
                    .px_4()
                    .flex_none()
                    .border_b_1()
                    .border_color(t.border)
                    .child(icons::icon("compass", 20., t.faint))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(t.fg_strong).child("Browse channels"))
                    .child(div().text_size(px(13.)).text_color(t.muted).child(format!("{total} channels · {joined} joined")))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("show-archived")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .text_size(px(13.))
                            .text_color(t.muted)
                            .child("Show archived")
                            .child(kit::toggle(&t, "show-archived-toggle", show_archived, true))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_archived = !this.show_archived;
                                cx.notify();
                            })),
                    )
                    .when(can_create && self.form.is_none(), |d| {
                        d.child(
                            kit::primary(&t, "new-channel", "+ New channel", true)
                                .h(px(28.))
                                .on_click(cx.listener(|this, _, window, cx| this.new_channel(window, cx))),
                        )
                    }),
            )
            .child(
                div()
                    .id("browser-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .max_w(px(760.))
                            .mx_auto()
                            .px_6()
                            .py_4()
                            .map(|d| match form {
                                Some(form) => d.child(form),
                                None => d
                                    .child(div().h(px(40.)).px_2().rounded(px(6.)).bg(t.composer_bg).border_1().border_color(t.composer_border).flex().items_center().child(Input::new(&self.filter).appearance(false)))
                                    .children(rows),
                            }),
                    ),
            )
    }
}
