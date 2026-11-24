//! A channel's settings, in place of the conversation: General, Permissions, Subscribers and
//! Personal. Every change applies at once (text fields on Enter or when they lose focus).
//! What the user can't change is shown as plain text, with a note saying who can.

use std::{collections::HashSet, rc::Rc};

use gpui_kit::{
    component::input::{InputEvent, InputState, Textarea, TextareaState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::{
    client::json,
    perms::{self, Privacy},
    types::{GroupSetting, Stream, StreamId, UserId},
};
use serde_json::{Value, json};

use super::{
    kit,
    people_picker::{PeopleEvent, PeoplePicker, Picked},
};
use crate::{icons, runtime::on_tokio, session::Session, theme::Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Permissions,
    Subscribers,
    Personal,
}

/// The channel permissions, in the order shown: field, title, description. Those described as
/// "Besides the organization's rule" add people to an organization-wide permission.
const PERMS: [(&str, &str, &str); 11] = [
    ("can_administer_channel_group", "Administer this channel", "Change its settings, permissions and subscribers"),
    ("can_send_message_group", "Post", ""),
    ("can_create_topic_group", "Start new topics", ""),
    ("can_move_messages_within_channel_group", "Move messages within", ADDS),
    ("can_move_messages_out_of_channel_group", "Move messages out", ADDS),
    ("can_resolve_topics_group", "Resolve topics", ADDS),
    ("can_delete_any_message_group", "Delete any message", ADDS),
    ("can_delete_own_message_group", "Delete their own messages", ADDS),
    ("can_subscribe_group", "Join on their own", ""),
    ("can_add_subscribers_group", "Add subscribers", ADDS),
    ("can_remove_subscribers_group", "Remove subscribers", "Besides administrators"),
];
const ADDS: &str = "Besides the organization's rule";

/// Personal notification settings: subscription property, label, the account-wide default's name.
const NOTIFY: [(&str, &str, &str); 5] = [
    ("push_notifications", "Push", "enable_stream_push_notifications"),
    ("email_notifications", "Email", "enable_stream_email_notifications"),
    ("desktop_notifications", "Desktop", "enable_stream_desktop_notifications"),
    ("audible_notifications", "Sound", "enable_stream_audible_notifications"),
    ("wildcard_mentions_notify", "@all mentions", "wildcard_mentions_notify"),
];

/// Which dropdown menu is open.
#[derive(Debug, Clone, PartialEq)]
enum Menu {
    Folder,
    TopicsPolicy,
    Retention,
    Perm(&'static str),
    Notify(&'static str),
}

pub struct ChannelSettings {
    session: Entity<Session>,
    theme: Rc<Theme>,
    pub sid: StreamId,
    tab: Tab,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    desc_spell: super::spell_field::Spell,
    filter: Entity<InputState>,
    /// Add people, while open.
    adder: Option<(Entity<PeoplePicker>, Subscription)>,
    /// The custom set being edited for a permission, with its search field.
    custom: Option<(&'static str, Entity<PeoplePicker>, Subscription)>,
    menu: Option<(Menu, Point<Pixels>)>,
    /// Remove buttons pressed, until the server's event lands.
    removing: HashSet<UserId>,
    /// The name and description the fields were last filled with: only what's typed over
    /// them is sent, so a field left alone never undoes someone else's change.
    name_base: String,
    desc_base: String,
    /// When a menu last closed by a click outside it: that same click on its button mustn't reopen it.
    menu_closed: Option<(Menu, std::time::Instant)>,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

/// A permission's current value, read by field name.
fn setting(st: &Stream, key: &str) -> GroupSetting {
    serde_json::to_value(st).ok().and_then(|v| serde_json::from_value(v[key].clone()).ok()).unwrap_or_default()
}

fn setting_json(g: &GroupSetting) -> Value {
    serde_json::to_value(g).unwrap_or(Value::Null)
}

impl ChannelSettings {
    pub fn new(session: Entity<Session>, theme: Rc<Theme>, sid: StreamId, tab: Tab, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (name_base, desc_base) = {
            let st = session.read(cx).state.streams.get(&sid);
            (st.map(|s| s.name.clone()).unwrap_or_default(), st.map(|s| s.description.clone()).unwrap_or_default())
        };
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Channel name"));
        name.update(cx, |i, cx| i.set_value(name_base.clone(), window, cx));
        let description = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).submit_on_enter(true).placeholder("What it's for"));
        description.update(cx, |i, cx| i.set_value(desc_base.clone(), window, cx));
        let desc_spell = super::spell_field::Spell::new(&description, |c: &mut Self| &mut c.desc_spell, &theme, window, cx);
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter subscribers"));
        let subs = vec![
            cx.subscribe_in(&name, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.save_name(cx);
                }
            }),
            cx.subscribe_in(&description, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::PressEnter { shift: false, .. } | InputEvent::Blur) {
                    this.save_description(cx);
                }
            }),
            cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify()),
            // Someone else's rename or description edit shows in a field that isn't being edited.
            cx.observe_in(&session, window, |this, session, window, cx| {
                let (name, desc, members) = {
                    let s = session.read(cx);
                    let st = s.state.streams.get(&this.sid);
                    (st.map(|s| s.name.clone()), st.map(|s| s.description.clone()), s.members(this.sid).cloned())
                };
                if let Some(name) = name
                    && name != this.name_base
                    && !this.name.read(cx).focus_handle(cx).is_focused(window)
                {
                    this.name.update(cx, |i, cx| i.set_value(name.clone(), window, cx));
                    this.name_base = name;
                }
                if let Some(desc) = desc
                    && desc != this.desc_base
                    && !this.description.read(cx).focus_handle(cx).is_focused(window)
                {
                    this.description.update(cx, |i, cx| i.set_value(desc.clone(), window, cx));
                    this.desc_spell.reset(cx);
                    this.desc_base = desc;
                }
                // A removal is done once the member list no longer has them.
                if let Some(m) = &members {
                    this.removing.retain(|u| m.binary_search(u).is_ok());
                }
                // A new event queue drops member lists: fetch it again while it's showing.
                if this.tab == Tab::Subscribers && members.is_none() && !session.read(cx).members_failed(this.sid) {
                    let sid = this.sid;
                    session.update(cx, |s, cx| s.load_members(sid, cx));
                }
                cx.notify();
            }),
        ];
        let mut this = Self {
            session,
            theme,
            sid,
            tab: Tab::General,
            name,
            description,
            desc_spell,
            filter,
            adder: None,
            custom: None,
            menu: None,
            removing: HashSet::new(),
            name_base,
            desc_base,
            menu_closed: None,
            error: None,
            _subs: subs,
        };
        this.show(tab, cx);
        this
    }

    pub fn set_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub fn show(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.menu = None;
        if tab == Tab::Subscribers {
            let sid = self.sid;
            self.session.update(cx, |s, cx| s.load_members(sid, cx));
        }
        cx.notify();
    }

    fn stream(&self, cx: &App) -> Option<Stream> {
        self.session.read(cx).state.streams.get(&self.sid).cloned()
    }

    /// Sends a channel settings change; a failure shows at the top of the panel.
    fn update(&mut self, params: Vec<(&'static str, String)>, cx: &mut Context<Self>) {
        let (client, sid) = (self.session.read(cx).client.clone(), self.sid);
        self.error = None;
        self.menu = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { client.update_channel(sid, &params).await }).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = r {
                    this.error = Some(friendly(e));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Runs any other server call the same way.
    fn call<F, Fut>(&mut self, f: F, cx: &mut Context<Self>)
    where
        F: FnOnce(puzil_zulip::Client) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), puzil_zulip::ApiError>> + Send + 'static,
    {
        let client = self.session.read(cx).client.clone();
        self.error = None;
        self.menu = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = on_tokio(async move { f(client).await }).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = r {
                    this.error = Some(friendly(e));
                    this.removing.clear();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn save_name(&mut self, cx: &mut Context<Self>) {
        let new = self.name.read(cx).value().trim().to_string();
        if new.is_empty() || new == self.name_base.trim() || !self.session.read(cx).state.channel_admin(self.sid) {
            return;
        }
        self.name_base = new.clone();
        self.update(vec![("new_name", new)], cx);
    }

    fn save_description(&mut self, cx: &mut Context<Self>) {
        let new = self.description.read(cx).value().trim().to_string();
        if new == self.desc_base.trim() || !self.session.read(cx).state.channel_admin(self.sid) {
            return;
        }
        self.desc_base = new.clone();
        self.update(vec![("description", new)], cx);
    }

    fn set_privacy(&mut self, p: Privacy, window: &mut Window, cx: &mut Context<Self>) {
        let Some(st) = self.stream(cx) else { return };
        let was = Privacy::of(&st);
        if p == was {
            return;
        }
        let (private, web, history) = p.flags();
        let params = vec![
            ("is_private", private.to_string()),
            ("is_web_public", web.to_string()),
            ("history_public_to_subscribers", history.to_string()),
        ];
        // Anything that shows the channel's history to more people asks first.
        let exposure = |p: Privacy| match p {
            Privacy::PrivateProtected => 0,
            Privacy::PrivateShared => 1,
            Privacy::Public => 2,
            Privacy::WebPublic => 3,
        };
        if exposure(p) > exposure(was) {
            let (title, detail, button) = match p {
                Privacy::WebPublic => (
                    format!("Make #{} web-public?", st.name),
                    "Anyone on the internet will be able to read it, including its whole history, without an account.",
                    "Make web-public",
                ),
                Privacy::Public => (
                    format!("Make #{} public?", st.name),
                    "Everyone in the organization will be able to join it and read its whole history.",
                    "Make public",
                ),
                _ => (
                    format!("Share #{}'s history?", st.name),
                    "People added from now on will see everything said in it before they joined.",
                    "Share history",
                ),
            };
            let answer = window.prompt(PromptLevel::Warning, &title, Some(detail), &[button, "Cancel"], cx);
            cx.spawn(async move |this, cx| {
                if answer.await.ok() == Some(0) {
                    let _ = this.update(cx, |this, cx| this.update(params, cx));
                }
            })
            .detach();
        } else {
            self.update(params, cx);
        }
    }

    fn set_perm(&mut self, key: &'static str, value: GroupSetting, cx: &mut Context<Self>) {
        self.update(vec![(key, json(json!({ "new": setting_json(&value) })))], cx);
    }

    /// Adds someone or a group to a permission's custom set (turning a role into a set with that role in it).
    fn add_to_perm(&mut self, key: &'static str, p: Picked, cx: &mut Context<Self>) {
        let Some(st) = self.stream(cx) else { return };
        let (mut users, mut groups) = match setting(&st, key) {
            GroupSetting::Group(id) => (Vec::new(), vec![id]),
            GroupSetting::Custom { direct_members, direct_subgroups } => (direct_members, direct_subgroups),
        };
        let nobody = self.session.read(cx).state.group_id(perms::NOBODY);
        groups.retain(|g| Some(*g) != nobody);
        match p {
            Picked::User(u) if !users.contains(&u) => users.push(u),
            Picked::Group(g) if !groups.contains(&g) => groups.push(g),
            _ => return,
        }
        self.set_perm(key, GroupSetting::Custom { direct_members: users, direct_subgroups: groups }, cx);
    }

    fn remove_from_perm(&mut self, key: &'static str, p: Picked, cx: &mut Context<Self>) {
        let Some(st) = self.stream(cx) else { return };
        let (mut users, mut groups) = match setting(&st, key) {
            GroupSetting::Group(id) => (Vec::new(), vec![id]),
            GroupSetting::Custom { direct_members, direct_subgroups } => (direct_members, direct_subgroups),
        };
        match p {
            Picked::User(u) => users.retain(|x| *x != u),
            Picked::Group(g) => groups.retain(|x| *x != g),
        }
        self.set_perm(key, GroupSetting::Custom { direct_members: users, direct_subgroups: groups }, cx);
    }

    fn edit_custom(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let picker = cx.new(|cx| PeoplePicker::new(self.session.clone(), self.theme.clone(), true, "Add people or groups", window, cx));
        let sub = cx.subscribe_in(&picker, window, move |this, _, e: &PeopleEvent, _, cx| match e {
            PeopleEvent::Picked(p) => this.add_to_perm(key, *p, cx),
            PeopleEvent::Dismiss => {
                this.custom = None;
                cx.notify();
            }
        });
        picker.update(cx, |p, cx| p.focus(window, cx));
        self.custom = Some((key, picker, sub));
        cx.notify();
    }

    fn open_adder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picker = cx.new(|cx| PeoplePicker::new(self.session.clone(), self.theme.clone(), false, "Add people…", window, cx));
        let sub = cx.subscribe_in(&picker, window, |this, _, e: &PeopleEvent, _, cx| match e {
            PeopleEvent::Picked(Picked::User(u)) => this.add_subscriber(*u, cx),
            PeopleEvent::Picked(_) => {}
            PeopleEvent::Dismiss => {
                this.adder = None;
                cx.notify();
            }
        });
        picker.update(cx, |p, cx| p.focus(window, cx));
        self.adder = Some((picker, sub));
        cx.notify();
    }

    fn add_subscriber(&mut self, u: UserId, cx: &mut Context<Self>) {
        let Some(name) = self.stream(cx).map(|s| s.name) else { return };
        self.call(move |c| async move { c.add_subscribers(&name, &[u]).await }, cx);
    }

    fn remove_subscriber(&mut self, u: UserId, cx: &mut Context<Self>) {
        let Some(name) = self.stream(cx).map(|s| s.name) else { return };
        self.removing.insert(u);
        self.call(move |c| async move { c.remove_subscribers(&name, &[u]).await }, cx);
    }

    fn archive(&mut self, archive: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(st) = self.stream(cx) else { return };
        let sid = self.sid;
        if !archive {
            return self.update(vec![("is_archived", "false".into())], cx);
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Archive #{}?", st.name),
            Some("It leaves everyone's sidebar and takes no new messages. Its history stays searchable, and it can be unarchived."),
            &["Archive", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() == Some(0) {
                let _ = this.update(cx, |this, cx| this.call(move |c| async move { c.archive_channel(sid).await }, cx));
            }
        })
        .detach();
    }

    fn set_personal(&mut self, prop: &'static str, value: Value, cx: &mut Context<Self>) {
        let sid = self.sid;
        self.call(move |c| async move { c.set_subscription_property(sid, prop, value).await }, cx);
    }

    fn open_menu(&mut self, m: Menu, at: Point<Pixels>, cx: &mut Context<Self>) {
        // The click on a menu's own button closed it already (outside the menu): leave it closed.
        if self.menu_closed.take().is_some_and(|(o, t)| o == m && t.elapsed() < std::time::Duration::from_millis(250)) {
            return cx.notify();
        }
        self.menu = Some((m, at));
        cx.notify();
    }
}

/// A server refusal, as a sentence.
fn friendly(e: puzil_zulip::ApiError) -> String {
    match e {
        puzil_zulip::ApiError::Api { msg, .. } if !msg.is_empty() => format!("Couldn't save: {msg}"),
        e => format!("Couldn't save: {e}"),
    }
}

// ---- Rendering ----

impl ChannelSettings {
    fn general(&self, st: &Stream, admin: bool, desc_field: AnyElement, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme.clone();
        let s = self.session.read(cx);
        let mut out = Vec::new();
        let privacy = Privacy::of(st);
        let folders = &s.state.realm.channel_folders;
        let folder = st.folder_id.and_then(|id| folders.iter().find(|f| f.id == id)).map(|f| f.name.clone()).unwrap_or("None".into());

        let name_focused = self.name.read(cx).focus_handle(cx).is_focused(window);
        let desc_focused = self.description.read(cx).focus_handle(cx).is_focused(window);
        let left = div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(div().child(kit::label(&t, "Name")).child(if admin {
                kit::field(&t, &self.name, name_focused).into_any_element()
            } else {
                div().text_color(t.fg_strong).child(st.name.clone()).into_any_element()
            }))
            .child(div().child(kit::label(&t, "Description")).child(if admin {
                div()
                    .px_2p5()
                    .py_1p5()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if desc_focused { t.accent } else { t.divider })
                    .bg(t.panel)
                    .child(desc_field)
                    .into_any_element()
            } else {
                div()
                    .text_color(if st.description.is_empty() { t.faint } else { t.fg })
                    .child(if st.description.is_empty() { "No description".to_string() } else { st.description.clone() })
                    .into_any_element()
            }))
            .child(div().child(kit::label(&t, "Folder")).child(if admin && !folders.is_empty() {
                kit::picker(&t, "folder-picker", folder, true)
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.open_menu(Menu::Folder, e.position, cx)
                    }))
                    .into_any_element()
            } else {
                div().text_color(t.fg_strong).child(folder).into_any_element()
            }));

        let mut options = vec![
            (Privacy::Public, "Public", "Anyone in the organization can join and read its history"),
            (Privacy::PrivateShared, "Private, shared history", "Invite only; new members see past messages"),
            (Privacy::PrivateProtected, "Private, protected history", "Invite only; new members see only what comes after"),
        ];
        if privacy == Privacy::WebPublic || (s.state.realm.perms.enable_spectator_access && s.state.can_create_channel(Privacy::WebPublic)) {
            options.insert(1, (Privacy::WebPublic, "Web-public", "Anyone on the internet can read it, without an account"));
        }
        // Content access is needed to open a channel up; anyone who administers it can close it.
        let can_privacy = admin && s.state.content_access(self.sid);
        let mut right = div().flex().flex_col().child(kit::label(&t, "Who can see it"));
        for (i, (p, title, desc)) in options.into_iter().enumerate() {
            let on = p == privacy;
            right = right.child(
                div()
                    .id(("privacy", i))
                    .flex()
                    .flex_row()
                    .gap_2p5()
                    .items_start()
                    .mb_1p5()
                    .px_2p5()
                    .py_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if on { t.accent } else { t.divider })
                    .when(on, |d| d.bg(t.bg_hover))
                    .when(can_privacy && !on, |d| d.cursor_pointer().hover(|s| s.border_color(t.muted)))
                    .when(!can_privacy && !on, |d| d.opacity(0.55))
                    .child(
                        div()
                            .mt(px(2.))
                            .size(px(14.))
                            .flex_none()
                            .rounded_full()
                            .border_color(if on { t.accent } else { t.divider })
                            .map(|d| if on { d.border(px(4.)) } else { d.border(px(1.5)) }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(t.fg_strong).child(title))
                            .child(div().text_size(px(12.5)).text_color(t.muted).child(desc)),
                    )
                    .when(can_privacy && !on, |d| d.on_click(cx.listener(move |this, _, window, cx| this.set_privacy(p, window, cx)))),
            );
        }
        out.push(div().flex().flex_row().gap_4().pb_2().child(left.flex_1().min_w_0()).child(right.flex_1().min_w_0()).into_any_element());

        // "General chat" topic.
        let can_policy = s.state.can_set_topics_policy(self.sid);
        let realm_policy = policy_label(&s.state.realm.perms.topics_policy);
        let current = match st.topics_policy.as_str() {
            "" | "inherit" => format!("Organization default ({})", realm_policy.to_lowercase()),
            p => policy_label(p).to_string(),
        };
        out.push(
            kit::row(
                &t,
                "\"General chat\" topic",
                Some("Messages without a topic".into()),
                kit::dropdown(&t, "topics-policy", current, can_policy).when(can_policy, |d| {
                    d.on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.open_menu(Menu::TopicsPolicy, e.position, cx)
                    }))
                }),
            )
            .into_any_element(),
        );

        // Retention: the organization's administrators.
        // A channel's retention is for the organization's owners.
        let org_owner = s.state.is_org_owner();
        let org_admin = s.state.is_org_admin();
        let retention = match st.message_retention_days {
            None => "Organization default".to_string(),
            Some(d) if d < 0 => "Forever".to_string(),
            Some(d) => format!("{d} days"),
        };
        out.push(
            kit::row(
                &t,
                "Keep messages",
                (!org_owner).then(|| "Set by the organization's owners".into()),
                kit::dropdown(&t, "retention", retention, org_owner).when(org_owner, |d| {
                    d.on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.open_menu(Menu::Retention, e.position, cx)
                    }))
                }),
            )
            .into_any_element(),
        );

        // Private channels can't be default ones.
        if org_admin && !st.invite_only {
            let default = s.state.realm.default_streams.contains(&self.sid);
            let sid = self.sid;
            out.push(
                kit::row(
                    &t,
                    "Default channel",
                    Some("New members of the organization join it".into()),
                    kit::toggle(&t, "default-channel", default, true).on_click(cx.listener(move |this, _, _, cx| {
                        this.call(move |c| async move { c.set_default_channel(sid, !default).await }, cx)
                    })),
                )
                .into_any_element(),
            );
        }

        if admin {
            let archived = st.is_archived;
            out.push(
                kit::row(
                    &t,
                    if archived { "Unarchive channel" } else { "Archive channel" },
                    Some(if archived { "Puts it back in sidebars and lets it take messages again." } else { "Hides it and stops new messages. History stays searchable." }.into()),
                    kit::danger(&t, "archive", if archived { "Unarchive" } else { "Archive…" }, false)
                        .on_click(cx.listener(move |this, _, window, cx| this.archive(!archived, window, cx))),
                )
                .into_any_element(),
            );
        }
        out
    }

    fn permissions(&self, st: &Stream, admin: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme.clone();
        let mut out = Vec::new();
        for (key, title, desc) in PERMS {
            if key == "can_subscribe_group" && !st.invite_only {
                continue;
            }
            let value = setting(st, key);
            let mut text = self.session.read(cx).state.describe(&value);
            if text == "Nobody" && desc == ADDS {
                text = "No one extra".into();
            } else if text == "Nobody" && desc.starts_with("Besides") {
                text = "Administrators only".into();
            }
            let control = kit::dropdown(&t, SharedString::from(format!("perm-{key}")), text, admin).when(admin, |d| {
                d.on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_menu(Menu::Perm(key), e.position, cx)
                }))
            });
            out.push(kit::row(&t, title, (!desc.is_empty()).then(|| desc.into()), control).into_any_element());
            if let Some((k, picker, _)) = &self.custom
                && *k == key
            {
                // The custom set: chips (removable) and a search field to add more.
                let (users, groups) = match &value {
                    GroupSetting::Group(id) => (Vec::new(), vec![*id]),
                    GroupSetting::Custom { direct_members, direct_subgroups } => (direct_members.clone(), direct_subgroups.clone()),
                };
                let (group_chips, user_chips): (Vec<(u64, String)>, Vec<(UserId, String, Option<String>)>) = {
                    let s = &self.session.read(cx).state;
                    (
                        groups.iter().map(|g| (*g, s.group_label(*g))).collect(),
                        users.iter().map(|u| (*u, s.user_name(*u), s.users.get(u).and_then(|x| x.avatar_url.clone()))).collect(),
                    )
                };
                let mut chips = div().flex().flex_row().flex_wrap().gap_1().pb_2();
                for (g, label) in group_chips {
                    chips = chips.child(self.chip(&format!("g{key}{g}"), label, None, key, Picked::Group(g), cx));
                }
                for (u, name, avatar) in user_chips {
                    chips = chips.child(self.chip(&format!("u{key}{u}"), name, Some((u, avatar)), key, Picked::User(u), cx));
                }
                out.push(
                    div()
                        .py_2()
                        .px_3()
                        .mb_2()
                        .rounded(px(6.))
                        .bg(t.bg_hover)
                        .child(chips)
                        .child(div().flex().flex_row().gap_2().items_center().child(div().flex_1().child(picker.clone())).child(
                            kit::secondary(&t, "custom-done", "Done").on_click(cx.listener(|this, _, _, cx| {
                                this.custom = None;
                                cx.notify();
                            })),
                        ))
                        .into_any_element(),
                );
            }
        }
        out
    }

    fn chip(&self, id: &str, label: String, user: Option<(UserId, Option<String>)>, key: &'static str, p: Picked, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme.clone();
        let images = self.session.read(cx).images.clone();
        let lead: AnyElement = match &user {
            Some((u, avatar)) => {
                let img = images.update(cx, |i, cx| i.avatar(*u, avatar.as_deref(), cx));
                super::avatar::avatar(img, 18., t.user_color(*u), &label).into_any_element()
            }
            None => icons::icon("users", 14., t.icon).into_any_element(),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .pl(px(3.))
            .pr_1()
            .h(px(24.))
            .rounded_full()
            .border_1()
            .border_color(t.divider)
            .bg(t.panel)
            .text_size(px(12.5))
            .child(lead)
            .child(label)
            .child(
                div()
                    .id(SharedString::from(format!("chip-x-{id}")))
                    .cursor_pointer()
                    .child(icons::icon("x", 12., t.muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_from_perm(key, p, cx))),
            )
            .into_any_element()
    }

    fn subscribers(&self, st: &Stream, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme.clone();
        let s = self.session.read(cx);
        let can_add = s.state.can_add_subscribers(self.sid);
        let can_remove = s.state.can_remove_subscribers(self.sid);
        let me = s.state.me;
        let q = self.filter.read(cx).value().trim().to_lowercase();
        let mut out = Vec::new();
        let mut top = div().flex().flex_row().gap_2().pb_2().child(div().flex_1().child(kit::field(&t, &self.filter, false)));
        if can_add && self.adder.is_none() {
            top = top.child(kit::primary(&t, "add-people", "Add people…", true).on_click(cx.listener(|this, _, window, cx| this.open_adder(window, cx))));
        }
        out.push(top.into_any_element());
        if let Some((picker, _)) = &self.adder {
            out.push(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .pb_2()
                    .child(div().flex_1().child(picker.clone()))
                    .child(kit::secondary(&t, "adder-done", "Done").on_click(cx.listener(|this, _, _, cx| {
                        this.adder = None;
                        cx.notify();
                    })))
                    .into_any_element(),
            );
            out.push(kit::hint(&t, "They'll get a direct message from Zulip saying you added them.").pb_2().into_any_element());
        }
        let Some(members) = s.members(self.sid) else {
            let text = if s.members_failed(self.sid) { "Couldn't load subscribers." } else { "Loading subscribers…" };
            out.push(div().py_4().text_color(t.muted).child(text).into_any_element());
            return out;
        };
        let images = s.images.clone();
        let mut people: Vec<(String, UserId, Option<String>, bool)> = members
            .iter()
            .filter_map(|u| s.state.users.get(u))
            .filter(|u| q.is_empty() || u.full_name.to_lowercase().contains(&q) || u.email.to_lowercase().contains(&q))
            .map(|u| (u.full_name.clone(), u.user_id, u.avatar_url.clone(), u.is_bot))
            .collect();
        people.sort_by_key(|p| p.0.to_lowercase());
        let _ = st;
        // A row (and an avatar fetch) per subscriber is too much for a big channel: the first
        // ones, and a filter for the rest.
        const SHOWN: usize = 200;
        let hidden = people.len().saturating_sub(SHOWN);
        people.truncate(SHOWN);
        for (name, u, avatar, bot) in people {
            let img = images.update(cx, |i, cx| i.avatar(u, avatar.as_deref(), cx));
            let removing = self.removing.contains(&u);
            out.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2p5()
                    .py(px(7.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(super::avatar::avatar(img, 28., t.user_color(u), &name))
                    .child(div().text_color(t.fg_strong).child(name.clone()))
                    .when(bot, |d| d.child(div().text_size(px(12.)).text_color(t.muted).child("Bot")))
                    .when(u == me, |d| d.child(div().text_size(px(12.)).text_color(t.muted).child("You")))
                    .child(div().flex_1())
                    .when(can_remove && u != me, |d| {
                        d.child(
                            div()
                                .id(("remove", u as usize))
                                .text_size(px(13.))
                                .text_color(t.muted)
                                .when(!removing, |d| {
                                    d.cursor_pointer().hover(|s| s.text_color(t.danger)).on_click(cx.listener(move |this, _, _, cx| this.remove_subscriber(u, cx)))
                                })
                                .child(if removing { "Removing…" } else { "Remove" }),
                        )
                    })
                    .into_any_element(),
            );
        }
        if hidden > 0 {
            out.push(kit::hint(&t, format!("{hidden} more. Filter to find someone.")).pt_3().into_any_element());
        }
        out
    }

    fn personal(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme.clone();
        let s = self.session.read(cx);
        let mut out = Vec::new();
        let Some(sub) = s.state.subs.get(&self.sid).cloned() else {
            out.push(kit::note(&t, false, div().child("You're not subscribed to this channel. Join it from Browse channels to pin it or change its notifications.")).into_any_element());
            return out;
        };
        let (pin, muted) = (sub.pin_to_top, sub.is_muted);
        out.push(
            kit::row(
                &t,
                "Pin to top of sidebar",
                None,
                kit::toggle(&t, "pin", pin, true).on_click(cx.listener(move |this, _, _, cx| this.set_personal("pin_to_top", json!(!pin), cx))),
            )
            .into_any_element(),
        );
        out.push(
            kit::row(
                &t,
                "Mute",
                Some("Its messages don't count as unread or notify you, except in topics you follow or unmute".into()),
                kit::toggle(&t, "mute", muted, true).on_click(cx.listener(move |this, _, _, cx| this.set_personal("is_muted", json!(!muted), cx))),
            )
            .into_any_element(),
        );
        out.push(
            div().pt_5().pb_1().text_size(px(12.)).font_weight(FontWeight::BOLD).text_color(t.muted).child("NOTIFICATIONS FROM ZULIP (PHONE, EMAIL, WEB)").into_any_element(),
        );
        let subv = serde_json::to_value(&sub).unwrap_or_default();
        for (prop, label, global) in NOTIFY {
            let text = match subv[prop].as_bool() {
                Some(true) => "On".to_string(),
                Some(false) => "Off".to_string(),
                None => format!("Default ({})", if s.user_settings[global].as_bool().unwrap_or(false) { "on" } else { "off" }),
            };
            out.push(
                kit::row(
                    &t,
                    label,
                    None,
                    kit::dropdown(&t, SharedString::from(format!("notify-{prop}")), text, true).on_mouse_down(MouseButton::Left, cx.listener(
                        move |this, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_menu(Menu::Notify(prop), e.position, cx)
                        },
                    )),
                )
                .into_any_element(),
            );
        }
        out
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (m, at) = self.menu.clone()?;
        let t = self.theme.clone();
        let s = self.session.read(cx);
        let st = s.state.streams.get(&self.sid)?.clone();
        let mut menu = kit::menu(&t, "cs-menu", 260.).on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.menu_closed = this.menu.take().map(|(m, _)| (m, std::time::Instant::now()));
            cx.notify();
        }));
        match m {
            Menu::Folder => {
                menu = menu.child(kit::menu_item(&t, "folder-none", "None", st.folder_id.is_none()).on_click(cx.listener(|this, _, _, cx| {
                    this.update(vec![("folder_id", "null".into())], cx)
                })));
                for f in s.state.realm.channel_folders.iter().filter(|f| !f.is_archived) {
                    let id = f.id;
                    menu = menu.child(kit::menu_item(&t, ("folder", id as usize), f.name.clone(), st.folder_id == Some(id)).on_click(
                        cx.listener(move |this, _, _, cx| this.update(vec![("folder_id", id.to_string())], cx)),
                    ));
                }
            }
            Menu::TopicsPolicy => {
                let realm = policy_label(&s.state.realm.perms.topics_policy).to_lowercase();
                let options = [
                    ("inherit", format!("Organization default ({realm})")),
                    ("allow_empty_topic", "Allowed".into()),
                    ("disable_empty_topic", "Not allowed".into()),
                    ("empty_topic_only", "Only general chat".into()),
                ];
                let current = if st.topics_policy.is_empty() { "inherit" } else { st.topics_policy.as_str() };
                for (i, (v, label)) in options.into_iter().enumerate() {
                    menu = menu.child(kit::menu_item(&t, ("policy", i), label, current == v).on_click(cx.listener(move |this, _, _, cx| {
                        this.update(vec![("topics_policy", v.to_string())], cx)
                    })));
                }
            }
            Menu::Retention => {
                let options: [(&str, Value); 5] =
                    [("Organization default", json!("realm_default")), ("Forever", json!("unlimited")), ("1 year", json!(365)), ("90 days", json!(90)), ("30 days", json!(30))];
                for (i, (label, v)) in options.into_iter().enumerate() {
                    let checked = match (&v, st.message_retention_days) {
                        (Value::String(s), None) => s == "realm_default",
                        (Value::String(s), Some(d)) => s == "unlimited" && d < 0,
                        (Value::Number(n), Some(d)) => n.as_i64() == Some(d),
                        _ => false,
                    };
                    menu = menu.child(kit::menu_item(&t, ("ret", i), label, checked).on_click(cx.listener(move |this, _, window, cx| {
                        if checked {
                            this.menu = None;
                            return cx.notify();
                        }
                        // Anything but Forever can delete old messages for good: ask first.
                        let v = v.clone();
                        if v != json!("unlimited") {
                            let answer = window.prompt(
                                PromptLevel::Critical,
                                &if v.is_number() { format!("Keep messages for {label}?") } else { "Use the organization's default?".to_string() },
                                Some("Messages older than that in this channel will be deleted permanently."),
                                &["Change", "Cancel"],
                                cx,
                            );
                            cx.spawn(async move |this, cx| {
                                if answer.await.ok() == Some(0) {
                                    let _ = this.update(cx, |this, cx| this.update(vec![("message_retention_days", json(&v))], cx));
                                }
                            })
                            .detach();
                            this.menu = None;
                            cx.notify();
                        } else {
                            this.update(vec![("message_retention_days", json(&v))], cx)
                        }
                    })));
                }
            }
            Menu::Perm(key) => {
                let value = setting(&st, key);
                let role = s.state.role_of(&value);
                for (name, label) in perms::ROLES {
                    let Some(id) = s.state.group_id(name).filter(|_| s.state.role_allowed(key, name)) else { continue };
                    menu = menu.child(kit::menu_item(&t, SharedString::from(format!("role-{name}")), label, role == Some(name)).on_click(
                        cx.listener(move |this, _, _, cx| this.set_perm(key, GroupSetting::Group(id), cx)),
                    ));
                }
                menu = menu.child(kit::separator(&t)).child(
                    kit::menu_item(&t, "role-custom", "Specific people and groups…", role.is_none())
                        .on_click(cx.listener(move |this, _, window, cx| this.edit_custom(key, window, cx))),
                );
            }
            Menu::Notify(prop) => {
                let sub = s.state.subs.get(&self.sid)?;
                let v = serde_json::to_value(sub).ok()?[prop].as_bool();
                let global = NOTIFY.iter().find(|(p, _, _)| *p == prop).map(|(_, _, g)| *g).unwrap_or_default();
                if v.is_none() {
                    let on = s.user_settings[global].as_bool().unwrap_or(false);
                    menu = menu.child(kit::menu_item(&t, "n-default", format!("Default ({})", if on { "on" } else { "off" }), true));
                }
                menu = menu
                    .child(kit::menu_item(&t, "n-on", "On", v == Some(true)).on_click(cx.listener(move |this, _, _, cx| this.set_personal(prop, json!(true), cx))))
                    .child(kit::menu_item(&t, "n-off", "Off", v == Some(false)).on_click(cx.listener(move |this, _, _, cx| this.set_personal(prop, json!(false), cx))));
            }
        }
        let menu = super::motion::pop_in(SharedString::from(format!("cs-menu-{m:?}")), menu);
        Some(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(1).into_any_element())
    }
}

fn policy_label(p: &str) -> &'static str {
    match p {
        "disable_empty_topic" => "Not allowed",
        "empty_topic_only" => "Only general chat",
        _ => "Allowed",
    }
}

impl Render for ChannelSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let Some(st) = self.stream(cx) else {
            return div().size_full().bg(t.bg).flex().items_center().justify_center().text_color(t.muted).child("This channel is gone.");
        };
        let admin = self.session.read(cx).state.channel_admin(self.sid);
        let count = self.session.read(cx).members(self.sid).map(|m| m.len() as u64).unwrap_or(st.subscriber_count);
        let tabs = [(Tab::General, "General".to_string()), (Tab::Permissions, "Permissions".into()), (Tab::Subscribers, format!("Subscribers · {count}")), (Tab::Personal, "Personal".into())];
        let tab_row = div().flex().flex_row().gap_1().px_6().pt_2p5().border_b_1().border_color(t.border).children(tabs.into_iter().enumerate().map(|(i, (tab, label))| {
            let on = tab == self.tab;
            div()
                .id(("cs-tab", i))
                .px_3()
                .py_2()
                .mb(px(-1.))
                .border_b_2()
                .border_color(if on { t.accent } else { gpui_kit::transparent_black() })
                .font_weight(FontWeight::MEDIUM)
                .text_color(if on { t.fg_strong } else { t.muted })
                .cursor_pointer()
                .hover(|s| s.text_color(t.fg_strong))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.show(tab, cx)))
        }));
        let body: Vec<AnyElement> = match self.tab {
            Tab::General => {
                let t = self.theme.clone();
                let field = self.desc_spell.wrap(Textarea::new(&self.description).appearance(false), |c: &mut Self| &mut c.desc_spell, &t, window, cx).into_any_element();
                self.general(&st, admin, field, window, cx)
            }
            Tab::Permissions => self.permissions(&st, admin, cx),
            Tab::Subscribers => self.subscribers(&st, cx),
            Tab::Personal => self.personal(cx),
        };
        let read_only_note = match self.tab {
            Tab::General | Tab::Permissions if !admin => Some("Only this channel's administrators can change these."),
            _ => None,
        };
        let menu = self.render_menu(cx);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.bg)
            .text_color(t.fg)
            .child(
                crate::theme::drag_region(div().id("cs-header"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(48.))
                    .px_4()
                    .flex_none()
                    .border_b_1()
                    .border_color(t.border)
                    .child(icons::channel(st.invite_only, 20., t.faint, t.bg))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(t.fg_strong).child(st.name.clone()))
                    .child(div().text_size(px(13.)).text_color(t.muted).child(if st.is_archived { "· Archived · Channel settings" } else { "· Channel settings" })),
            )
            .child(tab_row)
            .child(
                div().id("cs-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                    div()
                        .max_w(px(820.))
                        .px_6()
                        .py(px(18.))
                        .flex()
                        .flex_col()
                        .when_some(self.error.clone(), |d, e| d.child(div().pb_3().text_size(px(13.)).text_color(t.danger).child(e)))
                        .when_some(read_only_note, |d, n| d.child(div().pb_3().child(kit::note(&t, false, div().child(n)))))
                        .children(body),
                ),
            )
            .children(menu)
    }
}
