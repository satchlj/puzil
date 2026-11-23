//! The servers Puzil is signed in to, for the views that list or change them (the rail, the
//! account panel, Settings). `Root` owns the sessions; it listens here for requests and keeps
//! `sessions` and `active` current.

use gpui_kit::*;

use crate::{accounts::Account, session::Session};

pub enum ServersEvent {
    Switch(usize),
    /// Show the sign-in screen to add another server.
    Add,
    SignOut(Account),
}

#[derive(Default)]
pub struct Servers {
    pub sessions: Vec<Entity<Session>>,
    pub active: usize,
}

impl EventEmitter<ServersEvent> for Servers {}

pub struct ServersHandle(pub Entity<Servers>);
impl Global for ServersHandle {}

pub fn init(cx: &mut App) {
    let e = cx.new(|_| Servers::default());
    cx.set_global(ServersHandle(e));
}

pub fn handle(cx: &App) -> Entity<Servers> {
    cx.global::<ServersHandle>().0.clone()
}

pub fn request(cx: &mut App, e: ServersEvent) {
    handle(cx).update(cx, |_, cx| cx.emit(e));
}
