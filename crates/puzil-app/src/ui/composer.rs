//! The message composer: Enter sends, Shift+Enter is a newline, `@ # :` autocomplete,
//! per-conversation drafts, file uploads, and editing an earlier message.

use std::{path::PathBuf, rc::Rc};

use gpui_kit::{
    component::input::{Input, InputEvent, InputState, Textarea, TextareaState},
    prelude::FluentBuilder as _,
    *,
};
use puzil_zulip::{
    model::{Conv, TopicKey},
    types::MessageId,
};

use super::conversation::Target;
use crate::{icons, runtime::on_tokio, session::Session, theme::Theme};

actions!(composer, [ComposerUp, ComposerDown, ComposerTab, ComposerEscape]);

/// Binds the composer keys. They sit in the text field's context (`Input`), registered after
/// the field's own bindings so they are tried first.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", ComposerUp, Some("Input")),
        KeyBinding::new("down", ComposerDown, Some("Input")),
        KeyBinding::new("tab", ComposerTab, Some("Input")),
        KeyBinding::new("escape", ComposerEscape, Some("Input")),
    ]);
}

#[derive(Clone, Copy)]
enum Key {
    Up,
    Down,
    Tab,
    Escape,
}

pub enum ComposerEvent {
    /// A message went out to this target (a new topic navigates there).
    Sent(Target),
    /// Up on an empty composer: select the newest message (arrow keys move, Enter edits).
    SelectLast,
}

#[derive(Debug, Clone)]
struct Suggestion {
    label: String,
    detail: String,
    insert: String,
    /// A user's id and picture, for the row's avatar.
    user: Option<(u64, Option<String>)>,
}

struct Completion {
    /// Byte offset of the trigger character.
    start: usize,
    items: Vec<Suggestion>,
    sel: usize,
}

enum Mode {
    New,
    Edit { id: MessageId, loading: bool },
}

pub struct Composer {
    session: Entity<Session>,
    target: Target,
    theme: Rc<Theme>,
    input: Entity<TextareaState>,
    topic: Option<Entity<InputState>>,
    mode: Mode,
    completion: Option<Completion>,
    uploads: usize,
    error: Option<String>,
    /// Set while we replace the text ourselves, so the change isn't saved as a draft.
    quiet: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(session: Entity<Session>, target: Target, theme: Rc<Theme>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = Self::placeholder(&session, &target, cx);
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 12).submit_on_enter(true).placeholder(placeholder));
        let topic = matches!(target, Target::NewTopic(_)).then(|| cx.new(|cx| InputState::new(window, cx).placeholder("Topic")));
        let subs = vec![cx.subscribe_in(&input, window, Self::on_input)];
        let mut this = Self {
            session: session.clone(),
            target,
            theme,
            input,
            topic,
            mode: Mode::New,
            completion: None,
            uploads: 0,
            error: None,
            quiet: false,
            _subs: subs,
        };
        if let Target::Conv(c) = &this.target
            && let Some(d) = session.read(cx).draft(c)
        {
            this.set_text(&d, window, cx);
        }
        this
    }

    fn placeholder(session: &Entity<Session>, target: &Target, cx: &App) -> String {
        let s = session.read(cx);
        match target {
            Target::Conv(Conv::Topic(k)) => format!(
                "Message #{} > {}",
                s.state.streams.get(&k.stream_id).map(|x| x.name.as_str()).unwrap_or("?"),
                s.state.topic_display(k)
            ),
            Target::Conv(Conv::Dm(k)) => format!("Message {}", s.state.dm_display(k)),
            Target::NewTopic(sid) => format!("Message #{}", s.state.streams.get(sid).map(|x| x.name.as_str()).unwrap_or("?")),
        }
    }

    pub fn set_theme(&mut self, theme: Rc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.topic {
            Some(t) if t.read(cx).value().is_empty() => t.update(cx, |t, cx| t.focus(window, cx)),
            _ => self.input.update(cx, |i, cx| i.focus(window, cx)),
        }
    }

    fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.quiet = true;
        let text = text.to_string();
        self.input.update(cx, |i, cx| {
            i.set_value(text.clone(), window, cx);
            i.set_selected_range(text.len()..text.len(), cx);
        });
        self.quiet = false;
    }

    fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn on_input(&mut self, _: &Entity<TextareaState>, e: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match e {
            InputEvent::Change => {
                if self.quiet {
                    return;
                }
                self.error = None;
                self.update_completion(cx);
                if let (Mode::New, Target::Conv(c)) = (&self.mode, &self.target) {
                    let text = self.text(cx);
                    let empty = text.trim().is_empty();
                    self.session.update(cx, |s, cx| {
                        s.save_draft(c, text);
                        s.composing(c, empty, cx);
                    });
                }
                cx.notify();
            }
            InputEvent::PressEnter { shift: false, .. } => {
                if self.completion.is_some() {
                    self.accept(window, cx);
                } else {
                    self.submit(window, cx);
                }
            }
            _ => {}
        }
    }

    /// Keys the textarea would otherwise take: arrows and Tab while the suggestion list is
    /// open, Up on an empty composer, Escape while editing. They are bound in the textarea's
    /// own context ahead of its bindings; when one doesn't apply, the key goes on to the
    /// textarea. (A keystroke interceptor that swallows these keys leaves macOS text input
    /// unable to dispatch later key actions.)
    fn on_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let empty = self.text(cx).is_empty();
        let handled = match (key, &mut self.completion) {
            (Key::Up, Some(c)) => {
                c.sel = c.sel.checked_sub(1).unwrap_or(c.items.len() - 1);
                true
            }
            (Key::Down, Some(c)) => {
                c.sel = (c.sel + 1) % c.items.len();
                true
            }
            (Key::Tab, Some(_)) => {
                self.accept(window, cx);
                true
            }
            (Key::Escape, Some(_)) => {
                self.completion = None;
                true
            }
            (Key::Escape, None) if matches!(self.mode, Mode::Edit { .. }) => {
                self.cancel_edit(window, cx);
                true
            }
            (Key::Up, None) if matches!(self.mode, Mode::New) && empty => {
                cx.emit(ComposerEvent::SelectLast);
                true
            }
            _ => false,
        };
        if handled {
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    // ---- Autocomplete ----

    fn update_completion(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let cursor = self.input.read(cx).cursor().min(text.len());
        self.completion = trigger(&text, cursor).and_then(|(start, kind, query)| {
            let items = self.suggest(kind, &query, cx);
            (!items.is_empty()).then_some(Completion { start, items, sel: 0 })
        });
    }

    fn suggest(&self, kind: char, query: &str, cx: &mut Context<Self>) -> Vec<Suggestion> {
        let s = self.session.read(cx);
        let q = query.to_lowercase();
        const MAX: usize = 8;
        match kind {
            '@' => {
                let mut v: Vec<(u8, &puzil_zulip::types::User)> = s
                    .state
                    .users
                    .values()
                    .filter(|u| !u.deactivated)
                    .filter_map(|u| {
                        let n = u.full_name.to_lowercase();
                        let rank = if n.starts_with(&q) {
                            0
                        } else if n.split_whitespace().any(|w| w.starts_with(&q)) {
                            1
                        } else if n.contains(&q) || u.email.to_lowercase().starts_with(&q) {
                            2
                        } else {
                            return None;
                        };
                        Some((rank + if u.is_bot { 3 } else { 0 }, u))
                    })
                    .collect();
                v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.full_name.cmp(&b.1.full_name)));
                let mut out: Vec<Suggestion> = v
                    .into_iter()
                    .take(MAX)
                    .map(|(_, u)| {
                        let dup = s.state.users.values().filter(|o| o.full_name == u.full_name).count() > 1;
                        let insert =
                            if dup { format!("@**{}|{}** ", u.full_name, u.user_id) } else { format!("@**{}** ", u.full_name) };
                        // Zulip hides most addresses behind `user123@…` placeholders; those say nothing.
                        let placeholder = u
                            .email
                            .strip_prefix("user")
                            .is_some_and(|r| r.split('@').next().is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())));
                        let detail = if placeholder { String::new() } else { u.email.clone() };
                        Suggestion { label: u.full_name.clone(), detail, insert, user: Some((u.user_id, u.avatar_url.clone())) }
                    })
                    .collect();
                for w in ["all", "topic", "channel"] {
                    if w.starts_with(&q) && !q.is_empty() && out.len() < MAX {
                        out.push(Suggestion {
                            label: w.into(),
                            detail: "Notify everyone".into(),
                            insert: format!("@**{w}** "),
                            user: None,
                        });
                    }
                }
                out
            }
            '#' => {
                if let Some((chan, tq)) = q.split_once('>') {
                    let Some(st) = s.state.streams.values().find(|st| st.name.to_lowercase() == chan.trim()) else {
                        return vec![];
                    };
                    let topics = s
                        .topics
                        .get(&st.stream_id)
                        .map(|t| t.iter().map(|t| t.name.clone()).collect::<Vec<_>>())
                        .unwrap_or_default();
                    topics
                        .into_iter()
                        .filter(|t| t.to_lowercase().contains(tq.trim()))
                        .take(MAX)
                        .map(|t| Suggestion {
                            label: t.clone(),
                            detail: format!("#{}", st.name),
                            insert: format!("#**{}>{}** ", st.name, t),
                            user: None,
                        })
                        .collect()
                } else {
                    let mut v: Vec<_> = s
                        .state
                        .subs
                        .keys()
                        .filter_map(|id| s.state.streams.get(id))
                        .filter(|st| st.name.to_lowercase().contains(&q))
                        .collect();
                    v.sort_by_key(|st| (!st.name.to_lowercase().starts_with(&q), st.name.to_lowercase()));
                    v.into_iter()
                        .take(MAX)
                        .map(|st| Suggestion {
                            label: format!("#{}", st.name),
                            detail: String::new(),
                            insert: format!("#**{}** ", st.name),
                            user: None,
                        })
                        .collect()
                }
            }
            ':' => {
                let mut out: Vec<Suggestion> = s
                    .state
                    .realm_emoji
                    .values()
                    .filter(|e| !e.deactivated && e.name.contains(&q))
                    .take(MAX)
                    .map(|e| Suggestion {
                        label: format!(":{}:", e.name),
                        detail: "custom".into(),
                        insert: format!(":{}: ", e.name),
                        user: None,
                    })
                    .collect();
                let mut uni: Vec<_> = s.emoji.iter().filter(|e| e.name.contains(&q)).collect();
                uni.sort_by_key(|e| (!e.name.starts_with(&q), e.name.len()));
                out.extend(uni.into_iter().take(MAX - out.len().min(MAX)).map(|e| Suggestion {
                    label: format!("{} :{}:", e.glyph, e.name),
                    detail: String::new(),
                    insert: format!(":{}: ", e.name),
                    user: None,
                }));
                out
            }
            _ => vec![],
        }
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.completion.take() else {
            return;
        };
        let Some(item) = c.items.get(c.sel).cloned() else {
            return;
        };
        let cursor = self.input.read(cx).cursor();
        self.input.update(cx, |i, cx| {
            i.set_selected_range(c.start..cursor, cx);
            i.replace(item.insert, window, cx);
        });
        self.completion = None;
        cx.notify();
    }

    // ---- Sending and editing ----

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.text(cx).trim_end().to_string();
        if content.trim().is_empty() || self.uploads > 0 {
            return;
        }
        let max = self.session.read(cx).state.realm.max_message_length.max(1) as usize;
        if content.chars().count() > max {
            self.error = Some(format!("Message is longer than {max} characters"));
            cx.notify();
            return;
        }
        match self.mode {
            Mode::Edit { id, loading: false } => {
                self.session.update(cx, |s, cx| {
                    s.call(
                        cx,
                        move |c| async move { c.edit_message(id, &content).await },
                        |_, e, _| tracing::warn!("edit failed: {e}"),
                    )
                });
                self.cancel_edit(window, cx);
            }
            Mode::Edit { loading: true, .. } => {}
            Mode::New => {
                let (conv, topic) = match &self.target {
                    Target::Conv(c) => (c.clone(), self.session.read(cx).send_topic(c)),
                    Target::NewTopic(sid) => {
                        let topic = self.topic.as_ref().map(|t| t.read(cx).value().trim().to_string()).unwrap_or_default();
                        (Conv::Topic(TopicKey::new(*sid, &topic)), topic)
                    }
                };
                if let Conv::Topic(k) = &conv {
                    self.session.update(cx, |s, _| {
                        s.state.topic_names.entry(k.clone()).or_insert_with(|| topic.clone());
                    });
                }
                self.session.update(cx, |s, cx| s.send(conv.clone(), topic, content, cx));
                self.set_text("", window, cx);
                cx.emit(ComposerEvent::Sent(Target::Conv(conv)));
            }
        }
        cx.notify();
    }

    /// Puts `header` (an Overlay reply or thread link) at the top of the message unless it
    /// is already there, sets the new topic's name if given, and focuses the text.
    pub fn prefill(&mut self, topic: Option<String>, header: String, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::Edit { .. }) {
            self.cancel_edit(window, cx);
        }
        if let (Some(name), Some(input)) = (topic, &self.topic) {
            input.update(cx, |i, cx| i.set_value(name, window, cx));
        }
        let text = self.text(cx);
        if !text.starts_with(&header) {
            self.set_text(&format!("{header}{text}"), window, cx);
            if let Target::Conv(c) = &self.target {
                self.session.read(cx).save_draft(c, self.text(cx));
            }
        }
        self.input.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    pub fn start_edit(&mut self, id: MessageId, window: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |s, cx| s.stop_composing(None, cx));
        self.mode = Mode::Edit { id, loading: true };
        self.completion = None;
        self.set_text("", window, cx);
        if !self.input.read(cx).focus_handle(cx).is_focused(window) {
            self.input.update(cx, |i, cx| i.focus(window, cx));
        }
        let client = self.session.read(cx).client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let raw = on_tokio(async move { client.raw_message(id).await }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !matches!(this.mode, Mode::Edit { id: cur, .. } if cur == id) {
                    return;
                }
                match raw {
                    Ok(t) => {
                        this.mode = Mode::Edit { id, loading: false };
                        this.set_text(&t, window, cx);
                    }
                    Err(e) => {
                        this.error = Some(format!("Couldn't load the message: {e}"));
                        this.mode = Mode::New;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::New;
        let draft = match &self.target {
            Target::Conv(c) => self.session.read(cx).draft(c).unwrap_or_default(),
            Target::NewTopic(_) => String::new(),
        };
        self.set_text(&draft, window, cx);
        cx.notify();
    }

    // ---- Uploads ----

    fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update_in(cx, |this, window, cx| this.upload(paths, window, cx));
            }
        })
        .detach();
    }

    fn upload(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.session.read(cx).client.clone();
        let max = self.session.read(cx).state.realm.max_upload_mib as u64 * 1024 * 1024;
        for path in paths {
            self.uploads += 1;
            let client = client.clone();
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
            cx.spawn_in(window, async move |this, cx| {
                let n = name.clone();
                let r = on_tokio(async move {
                    let bytes = tokio::task::spawn_blocking(move || std::fs::read(&path))
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?;
                    if max > 0 && bytes.len() as u64 > max {
                        return Err(format!("{n} is larger than the server's {} MiB limit", max / 1024 / 1024));
                    }
                    client.upload(&n, bytes, mime_for(&n)).await.map_err(|e| e.to_string())
                })
                .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.uploads -= 1;
                    match r {
                        Ok(url) => this.input.update(cx, |i, cx| i.insert(format!("[{name}]({url}) "), window, cx)),
                        Err(e) => this.error = Some(format!("Upload failed: {e}")),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }
}

/// The autocomplete token ending at `cursor`: `(start, trigger, query)`.
fn trigger(text: &str, cursor: usize) -> Option<(usize, char, String)> {
    let before = &text[..cursor];
    let start =
        before.rfind(|c: char| c.is_whitespace()).map(|i| i + before[i..].chars().next().unwrap().len_utf8()).unwrap_or(0);
    let token = &before[start..];
    let kind = token.chars().next()?;
    let query = token[kind.len_utf8()..].trim_start_matches("**").to_string();
    match kind {
        '@' => Some((start, '@', query.trim_start_matches('_').to_string())),
        '#' => Some((start, '#', query)),
        ':' if query.chars().count() >= 2 && !query.contains(':') => Some((start, ':', query)),
        _ => None,
    }
}

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt" | "md" | "lean" | "py" | "rs") => "text/plain",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let editing = matches!(self.mode, Mode::Edit { .. });
        let images = self.session.read(cx).images.clone();
        let popup = self.completion.as_ref().map(|c| {
            let heading = match c.items.first() {
                Some(s) if s.user.is_some() => "Members",
                Some(s) if s.label.starts_with('#') || s.detail.starts_with('#') => "Channels",
                Some(s) if s.label.contains(':') => "Emoji",
                _ => "Suggestions",
            };
            let rows: Vec<_> = c
                .items
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let face = s.user.as_ref().map(|(id, url)| {
                        let img = images.update(cx, |im, cx| im.avatar(*id, url.as_deref(), cx));
                        super::avatar::avatar(img, 24., t.user_color(*id), &s.label)
                    });
                    div()
                        .id(("sugg", i))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .h(px(36.))
                        .px_2()
                        .mx_2()
                        .rounded(px(4.))
                        .when(i == c.sel, |d| d.bg(t.bg_selected))
                        .hover(|d| d.bg(t.bg_selected.opacity(0.6)))
                        .cursor_pointer()
                        .children(face)
                        .child(div().text_color(t.fg_strong).font_weight(FontWeight::MEDIUM).child(s.label.clone()))
                        .child(div().flex_1())
                        .child(div().text_color(t.faint).text_size(px(13.)).child(s.detail.clone()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(c) = &mut this.completion {
                                c.sel = i;
                            }
                            this.accept(window, cx);
                        }))
                })
                .collect();
            let kind = heading;
            let card = div()
                .flex()
                .flex_col()
                .rounded(px(8.))
                .border_1()
                .border_color(t.border)
                .bg(t.sidebar)
                .shadow_lg()
                .pb_2()
                .child(
                    div()
                        .px_4()
                        .pt_3()
                        .pb_2()
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.muted)
                        .child(heading.to_uppercase()),
                )
                .children(rows);
            div()
                .absolute()
                .bottom_full()
                .left(px(16.))
                .right(px(16.))
                .mb(px(-4.))
                .child(super::motion::pop_in(SharedString::from(format!("complete-{kind}")), card))
        });
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .px_4()
            .pb_6()
            .gap_1()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.upload(paths.paths().to_vec(), window, cx)))
            .on_action(cx.listener(|this, _: &ComposerUp, w, cx| this.on_key(Key::Up, w, cx)))
            .on_action(cx.listener(|this, _: &ComposerDown, w, cx| this.on_key(Key::Down, w, cx)))
            .on_action(cx.listener(|this, _: &ComposerTab, w, cx| this.on_key(Key::Tab, w, cx)))
            .on_action(cx.listener(|this, _: &ComposerEscape, w, cx| this.on_key(Key::Escape, w, cx)))
            .children(popup)
            .when(editing, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_1()
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .child(icons::icon("pencil", 12., t.muted))
                        .child("Editing message · Enter to save · Esc to cancel")
                        .when(matches!(self.mode, Mode::Edit { loading: true, .. }), |d| d.child("loading…")),
                )
            })
            .when_some(self.error.clone(), |d, e| d.child(div().px_1().text_size(px(12.)).text_color(t.danger).child(e)))
            .when_some(self.topic.clone(), |d, topic| {
                d.child(Input::new(&topic).appearance(false).bg(t.input_bg).rounded(px(8.)).px_2())
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_3()
                    .rounded(px(8.))
                    .bg(t.composer_bg)
                    .border_1()
                    .border_color(t.composer_border)
                    .pl_4()
                    .pr_3()
                    .py(px(5.))
                    .min_h(px(44.))
                    .child(
                        div()
                            .id("attach")
                            .flex_none()
                            .mt(px(5.))
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.opacity(0.7))
                            .cursor_pointer()
                            .child(icons::icon("plus", 22., t.icon))
                            .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx))),
                    )
                    .child(div().flex_1().min_w_0().child(Textarea::new(&self.input).appearance(false)))
                    .when(self.uploads > 0, |d| {
                        d.child(
                            div().text_size(px(12.)).text_color(t.muted).mt(px(9.)).child(format!("Uploading {}…", self.uploads)),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::trigger;

    #[test]
    fn finds_triggers() {
        assert_eq!(trigger("hi @ad", 6), Some((3, '@', "ad".into())));
        assert_eq!(trigger("@**Ada", 6), Some((0, '@', "Ada".into())));
        assert_eq!(trigger("go #gen", 7), Some((3, '#', "gen".into())));
        assert_eq!(trigger("x :ta", 5), Some((2, ':', "ta".into())));
        assert_eq!(trigger("x :t", 4), None, "one letter is too short for emoji");
        assert_eq!(trigger("time 3:45", 9), None);
        assert_eq!(trigger("plain", 5), None);
    }
}
