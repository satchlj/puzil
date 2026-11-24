//! What the current user may do to topics and channels, from the organization's and each
//! channel's permission settings. The server has the final word; these decide what Puzil
//! offers, so a refusal is rare rather than routine.
//!
//! The rules follow Zulip's help center ("Channel permissions", "Restrict moving messages"):
//! a channel's administrators are its `can_administer_channel_group` plus the organization's
//! administrators, and channel-level grants add to the organization-level ones.

use std::collections::HashSet;

use crate::{
    model::State,
    types::{GroupSetting, Stream, StreamId, UserId},
};

/// A system role group, by its Zulip name.
pub const EVERYONE: &str = "role:everyone";
pub const MEMBERS: &str = "role:members";
pub const FULL_MEMBERS: &str = "role:fullmembers";
pub const MODERATORS: &str = "role:moderators";
pub const ADMINISTRATORS: &str = "role:administrators";
pub const OWNERS: &str = "role:owners";
pub const NOBODY: &str = "role:nobody";

/// The roles offered in permission pickers, broadest first, with their labels.
pub const ROLES: [(&str, &str); 7] = [
    (EVERYONE, "Everyone"),
    (MEMBERS, "Members"),
    (FULL_MEMBERS, "Full members"),
    (MODERATORS, "Moderators"),
    (ADMINISTRATORS, "Administrators"),
    (OWNERS, "Owners"),
    (NOBODY, "Nobody"),
];

/// Privacy of a channel, as the settings offer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privacy {
    Public,
    WebPublic,
    /// Invite only; new subscribers see past messages.
    PrivateShared,
    /// Invite only; new subscribers see only what comes after.
    PrivateProtected,
}

impl Privacy {
    pub fn of(st: &Stream) -> Self {
        match (st.invite_only, st.is_web_public, st.history_public_to_subscribers) {
            (false, true, _) => Self::WebPublic,
            (false, false, _) => Self::Public,
            (true, _, true) => Self::PrivateShared,
            (true, _, false) => Self::PrivateProtected,
        }
    }

    /// The `invite_only`, `is_web_public` and `history_public_to_subscribers` values.
    pub fn flags(self) -> (bool, bool, bool) {
        match self {
            Self::Public => (false, false, true),
            Self::WebPublic => (false, true, true),
            Self::PrivateShared => (true, false, true),
            Self::PrivateProtected => (true, false, false),
        }
    }
}

/// Moving messages: within the channel (a new topic) or out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Topic,
    Channel,
}

impl State {
    pub fn group_id(&self, name: &str) -> Option<u64> {
        self.realm.user_groups.iter().find(|g| g.name == name).map(|g| g.id)
    }

    /// Whether `user` belongs to group `id`, directly or through its subgroups.
    pub fn in_group_id(&self, id: u64, user: UserId) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![id];
        while let Some(g) = stack.pop() {
            if !seen.insert(g) {
                continue;
            }
            let Some(g) = self.realm.user_groups.iter().find(|x| x.id == g) else { continue };
            if g.members.contains(&user) {
                return true;
            }
            stack.extend(&g.direct_subgroup_ids);
        }
        false
    }

    pub fn in_setting(&self, s: &GroupSetting, user: UserId) -> bool {
        match s {
            GroupSetting::Group(id) => self.in_group_id(*id, user),
            GroupSetting::Custom { direct_members, direct_subgroups } => {
                direct_members.contains(&user) || direct_subgroups.iter().any(|g| self.in_group_id(*g, user))
            }
        }
    }

    fn mine(&self, s: &GroupSetting) -> bool {
        self.in_setting(s, self.me)
    }

    fn in_role(&self, role: &str) -> bool {
        self.group_id(role).is_some_and(|g| self.in_group_id(g, self.me))
    }

    pub fn is_org_owner(&self) -> bool {
        self.in_role(OWNERS)
    }

    pub fn is_org_admin(&self) -> bool {
        self.in_role(ADMINISTRATORS)
    }

    /// Moderators and up: the organization's time limits on moving don't apply to them.
    pub fn is_moderator(&self) -> bool {
        self.in_role(MODERATORS)
    }

    pub fn channel_admin(&self, sid: StreamId) -> bool {
        self.is_org_admin() || self.streams.get(&sid).is_some_and(|st| self.mine(&st.can_administer_channel_group))
    }

    /// Whether we can read the channel's messages: subscribed, or free to subscribe.
    pub fn content_access(&self, sid: StreamId) -> bool {
        let Some(st) = self.streams.get(&sid) else { return false };
        self.subs.contains_key(&sid)
            || (!st.invite_only && !self.users.get(&self.me).is_some_and(|u| u.is_guest))
            || self.mine(&st.can_subscribe_group)
            || self.mine(&st.can_add_subscribers_group)
    }

    pub fn can_move(&self, sid: StreamId, kind: Move) -> bool {
        let Some(st) = self.streams.get(&sid) else { return false };
        if st.is_archived {
            return false;
        }
        let p = &self.realm.perms;
        match kind {
            Move::Topic => {
                self.channel_admin(sid)
                    || self.mine(&p.can_move_messages_between_topics_group)
                    || self.mine(&st.can_move_messages_within_channel_group)
            }
            Move::Channel => {
                self.content_access(sid)
                    && (self.channel_admin(sid)
                        || self.mine(&p.can_move_messages_between_channels_group)
                        || self.mine(&st.can_move_messages_out_of_channel_group))
            }
        }
    }

    pub fn can_resolve(&self, sid: StreamId) -> bool {
        let Some(st) = self.streams.get(&sid) else { return false };
        !st.is_archived
            && (self.can_move(sid, Move::Topic)
                || self.mine(&self.realm.perms.can_resolve_topics_group)
                || self.mine(&st.can_resolve_topics_group))
    }

    /// Changing a channel's "general chat" policy: its administrators, if the organization lets them.
    pub fn can_set_topics_policy(&self, sid: StreamId) -> bool {
        self.channel_admin(sid) && self.mine(&self.realm.perms.can_set_topics_policy_group)
    }

    /// Whether channel permission `key` may be set to role group `role` (the server refuses
    /// Everyone for some, Nobody for others).
    pub fn role_allowed(&self, key: &str, role: &str) -> bool {
        let Some(rule) = self.realm.perms.channel_setting_rules.get(key) else { return true };
        match role {
            EVERYONE => rule.allow_everyone_group,
            NOBODY => rule.allow_nobody_group,
            _ => true,
        }
    }

    /// Deleting a whole topic is for organization administrators.
    pub fn can_delete_topic(&self, sid: StreamId) -> bool {
        self.is_org_admin() && self.streams.get(&sid).is_some_and(|st| !st.is_archived)
    }

    pub fn can_add_subscribers(&self, sid: StreamId) -> bool {
        let Some(st) = self.streams.get(&sid) else { return false };
        !st.is_archived
            && (self.mine(&st.can_add_subscribers_group)
                || (self.content_access(sid)
                    && (self.mine(&self.realm.perms.can_add_subscribers_group) || self.channel_admin(sid))))
    }

    pub fn can_remove_subscribers(&self, sid: StreamId) -> bool {
        let Some(st) = self.streams.get(&sid) else { return false };
        self.channel_admin(sid) || self.mine(&st.can_remove_subscribers_group) || self.mine(&self.realm.perms.can_remove_subscribers_group)
    }

    /// Whether we may create a channel of this privacy.
    pub fn can_create_channel(&self, privacy: Privacy) -> bool {
        let p = &self.realm.perms;
        match privacy {
            Privacy::Public => self.mine(&p.can_create_public_channel_group),
            Privacy::WebPublic => p.enable_spectator_access && self.mine(&p.can_create_web_public_channel_group),
            Privacy::PrivateShared | Privacy::PrivateProtected => self.mine(&p.can_create_private_channel_group),
        }
    }

    pub fn can_create_any_channel(&self) -> bool {
        [Privacy::Public, Privacy::PrivateShared].into_iter().any(|p| self.can_create_channel(p))
    }

    /// A setting's role group name, when it is exactly one role group.
    pub fn role_of(&self, s: &GroupSetting) -> Option<&'static str> {
        let id = match s {
            GroupSetting::Group(id) => *id,
            GroupSetting::Custom { direct_members, direct_subgroups } if direct_members.is_empty() && direct_subgroups.len() == 1 => {
                direct_subgroups[0]
            }
            GroupSetting::Custom { direct_members, direct_subgroups } if direct_members.is_empty() && direct_subgroups.is_empty() => {
                return Some(NOBODY);
            }
            _ => return None,
        };
        let name = self.realm.user_groups.iter().find(|g| g.id == id)?.name.as_str();
        ROLES.iter().map(|(n, _)| *n).find(|n| *n == name)
    }

    /// A short description of who a setting allows: "Members", "Ada Lovelace", "Ada, 2 others".
    pub fn describe(&self, s: &GroupSetting) -> String {
        if let Some(role) = self.role_of(s) {
            return ROLES.iter().find(|(n, _)| *n == role).map(|(_, l)| l.to_string()).unwrap_or_default();
        }
        let (users, groups): (Vec<UserId>, Vec<u64>) = match s {
            GroupSetting::Group(id) => (Vec::new(), vec![*id]),
            GroupSetting::Custom { direct_members, direct_subgroups } => (direct_members.clone(), direct_subgroups.clone()),
        };
        let mut names: Vec<String> = groups.iter().map(|g| self.group_label(*g)).collect();
        names.extend(users.iter().map(|u| self.user_name(*u)));
        match names.len() {
            0 => "Nobody".into(),
            1 => names.remove(0),
            n => format!("{}, {} other{}", names[0], n - 1, if n == 2 { "" } else { "s" }),
        }
    }

    /// A group's name as people see it: a role's label, or the group's own name.
    pub fn group_label(&self, id: u64) -> String {
        let Some(g) = self.realm.user_groups.iter().find(|g| g.id == id) else { return format!("group {id}") };
        ROLES.iter().find(|(n, _)| *n == g.name).map(|(_, l)| l.to_string()).unwrap_or_else(|| g.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Subscription, UserGroup};

    fn group(id: u64, name: &str, members: &[u64], sub: &[u64]) -> UserGroup {
        UserGroup {
            id,
            name: name.into(),
            description: String::new(),
            members: members.to_vec(),
            direct_subgroup_ids: sub.to_vec(),
            is_system_group: name.starts_with("role:"),
            deactivated: false,
        }
    }

    fn stream(id: u64, invite_only: bool) -> Stream {
        serde_json::from_value(serde_json::json!({"stream_id": id, "name": format!("s{id}"), "invite_only": invite_only})).unwrap()
    }

    /// Me (1) is a member; 2 is an admin. Role groups nest as Zulip's do.
    fn state() -> State {
        let mut s = State { me: 1, ..Default::default() };
        s.realm.user_groups = vec![
            group(10, OWNERS, &[], &[]),
            group(11, ADMINISTRATORS, &[2], &[10]),
            group(12, MODERATORS, &[], &[11]),
            group(13, FULL_MEMBERS, &[], &[12]),
            group(14, MEMBERS, &[1], &[13]),
            group(15, EVERYONE, &[], &[14]),
            group(16, NOBODY, &[], &[]),
            group(20, "researchers", &[3], &[]),
        ];
        s.streams.insert(100, stream(100, false));
        s.streams.insert(101, stream(101, true));
        s
    }

    #[test]
    fn membership_follows_subgroups() {
        let s = state();
        assert!(s.in_group_id(15, 1), "members are in everyone");
        assert!(!s.in_group_id(11, 1));
        assert!(s.in_group_id(14, 2), "admins are members");
        assert!(s.is_org_admin() == false && !s.is_moderator());
    }

    #[test]
    fn moves_need_an_org_or_channel_grant() {
        let mut s = state();
        assert!(!s.can_move(100, Move::Topic), "nobody by default");
        s.realm.perms.can_move_messages_between_topics_group = GroupSetting::Group(15);
        assert!(s.can_move(100, Move::Topic));
        assert!(!s.can_move(100, Move::Channel));
        s.streams.get_mut(&100).unwrap().can_move_messages_out_of_channel_group =
            GroupSetting::Custom { direct_members: vec![1], direct_subgroups: vec![] };
        assert!(s.can_move(100, Move::Channel));
        // A private channel we can't read: no moving out of it, even when granted.
        s.streams.get_mut(&101).unwrap().can_move_messages_out_of_channel_group = GroupSetting::Group(15);
        assert!(!s.can_move(101, Move::Channel));
        s.subs.insert(101, serde_json::from_value::<Subscription>(serde_json::json!({"stream_id": 101})).unwrap());
        assert!(s.can_move(101, Move::Channel));
        s.streams.get_mut(&100).unwrap().is_archived = true;
        assert!(!s.can_move(100, Move::Topic), "archived channels are frozen");
    }

    #[test]
    fn channel_admins_and_time_limits() {
        let mut s = state();
        s.streams.get_mut(&100).unwrap().can_administer_channel_group = GroupSetting::Custom { direct_members: vec![1], direct_subgroups: vec![] };
        assert!(s.channel_admin(100) && !s.channel_admin(101));
        assert!(s.can_move(100, Move::Topic) && s.can_remove_subscribers(100));
        s.me = 2;
        assert!(s.can_delete_topic(100), "organization admins delete topics");
    }

    #[test]
    fn describes_settings() {
        let s = state();
        assert_eq!(s.describe(&GroupSetting::Group(14)), "Members");
        assert_eq!(s.describe(&GroupSetting::default()), "Nobody");
        assert_eq!(s.describe(&GroupSetting::Custom { direct_members: vec![], direct_subgroups: vec![11] }), "Administrators");
        assert_eq!(s.role_of(&GroupSetting::Group(20)), None);
        assert_eq!(s.describe(&GroupSetting::Custom { direct_members: vec![9], direct_subgroups: vec![20] }), "researchers, 1 other");
        assert_eq!(Privacy::of(&s.streams[&101]), Privacy::PrivateProtected);
    }

    #[test]
    fn settings_and_groups_follow_events() {
        use crate::{events::parse_batch, types::RealmPerms};
        // Register names them realm_*; the custom form and plain ids both parse.
        let reg = serde_json::json!({
            "realm_can_move_messages_between_topics_group": 15,
            "realm_can_move_messages_between_channels_group": {"direct_members": [1], "direct_subgroups": []},
            "realm_move_messages_between_streams_limit_seconds": 604800,
            "realm_topics_policy": "allow_empty_topic",
            // A shape this build doesn't expect: only that setting falls back.
            "realm_message_retention_days": "unlimited",
        });
        let mut reg = reg;
        reg["server_supported_permission_settings"] = serde_json::json!({"stream": {
            "can_administer_channel_group": {"allow_everyone_group": false, "allow_nobody_group": true}}});
        let p = RealmPerms::from_register(&reg);
        let mut probe = state();
        probe.realm.perms = p.clone();
        assert!(!probe.role_allowed("can_administer_channel_group", EVERYONE));
        assert!(probe.role_allowed("can_administer_channel_group", NOBODY) && probe.role_allowed("can_send_message_group", EVERYONE));
        assert_eq!(p.can_move_messages_between_topics_group, GroupSetting::Group(15));
        assert_eq!(p.move_messages_between_streams_limit_seconds, Some(604800));
        let mut s = state();
        s.realm.perms = p;
        assert!(s.can_move(100, Move::Topic) && s.can_move(100, Move::Channel));

        let ev = |v: serde_json::Value| parse_batch(&[v]).remove(0);
        // An organization setting changes: moving between topics goes to admins only.
        s.apply(&ev(serde_json::json!({"type": "realm", "op": "update_dict", "property": "default", "data": {"can_move_messages_between_topics_group": 11}})));
        assert!(!s.can_move(100, Move::Topic));
        s.apply(&ev(serde_json::json!({"type": "realm", "op": "update", "property": "move_messages_between_streams_limit_seconds", "value": null})));
        assert_eq!(s.realm.perms.move_messages_between_streams_limit_seconds, None);
        // We're made an administrator: the role group gains us.
        s.apply(&ev(serde_json::json!({"type": "user_group", "op": "add_members", "group_id": 11, "user_ids": [1]})));
        assert!(s.is_org_admin() && s.can_move(100, Move::Topic));
        s.apply(&ev(serde_json::json!({"type": "user_group", "op": "remove_members", "group_id": 11, "user_ids": [1]})));
        assert!(!s.is_org_admin());
        // A channel permission changes.
        s.apply(&ev(serde_json::json!({"type": "stream", "op": "update", "stream_id": 100, "property": "can_administer_channel_group",
            "value": {"direct_members": [1], "direct_subgroups": []}, "name": "s100"})));
        assert!(s.channel_admin(100));
        // A privacy change brings its related flags along.
        s.apply(&ev(serde_json::json!({"type": "stream", "op": "update", "stream_id": 100, "property": "invite_only", "value": true,
            "history_public_to_subscribers": false, "is_web_public": false, "name": "s100"})));
        assert_eq!(Privacy::of(&s.streams[&100]), Privacy::PrivateProtected);
    }
}
