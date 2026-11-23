//! Puzil: a fast native Zulip client.
//!
//!     puzil-app                          open the app
//!     puzil-app --import-zuliprc PATH    verify and store a zuliprc's credentials, then exit
//!
//! `PUZIL_THEME=dark|light` overrides the system appearance. `PUZIL_NO_AUTO_READ=1` stops viewing from marking messages read (for testing against a
//! real account). Data (accounts, per-account SQLite, a file secret store in debug builds) lives in
//! `PUZIL_DATA_DIR`, by default the gitignored `reference/data/` for debug builds.

mod accounts;
mod dock;
mod icons;
mod instance;
mod math;
mod media;
mod menus;
mod overlay;
mod runtime;
mod servers;
mod session;
mod settings;
mod theme;
mod ui;
mod updates;

use std::borrow::Cow;

use gpui_kit::*;
use puzil_zulip::{Client, Credentials};

use crate::{
    session::Session,
    ui::{
        conversation::CopySelection,
        login::{Login, LoginEvent},
        workspace::Workspace,
    },
};

struct Server {
    session: Entity<Session>,
    workspace: Entity<Workspace>,
    _sub: Subscription,
}

struct Root {
    servers: Vec<Server>,
    active: usize,
    /// The sign-in screen: the only screen with no servers, or over them while adding one.
    login: Option<(Entity<Login>, Subscription)>,
    failed: Option<String>,
    _subs: Vec<Subscription>,
}

impl Root {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        theme::Theme::for_appearance(window.appearance()).apply_to_components(cx);
        // Remember the window frame (only while windowed: not fullscreen or zoomed).
        cx.observe_window_bounds(window, |_, window, _| {
            if let WindowBounds::Windowed(b) = window.window_bounds() {
                menus::save_bounds(b);
            }
        })
        .detach();
        let subs = vec![
            cx.observe_window_appearance(window, |_, window, cx| {
                theme::Theme::for_appearance(window.appearance()).apply_to_components(cx);
                cx.notify();
            }),
            cx.subscribe_in(&servers::handle(cx), window, |this, _, e: &servers::ServersEvent, window, cx| match e {
                servers::ServersEvent::Switch(i) => this.switch(*i, window, cx),
                servers::ServersEvent::Add => this.show_login(true, window, cx),
                servers::ServersEvent::SignOut(a) => this.sign_out(a.clone(), window, cx),
            }),
        ];
        let mut this = Self { servers: vec![], active: 0, login: None, failed: None, _subs: subs };
        let last = settings::handle(cx).read(cx).layer(&settings::Scope::App).last_server;
        for a in accounts::load() {
            match accounts::credentials(&a) {
                Ok(c) => this.add_server(a, c, window, cx),
                Err(e) => tracing::warn!("stored account unusable: {e:#}"),
            }
        }
        if let Some(i) = last.and_then(|r| this.servers.iter().position(|s| s.session.read(cx).account.realm == r)) {
            this.switch(i, window, cx);
        } else if !this.servers.is_empty() {
            this.switch(0, window, cx);
        }
        if this.servers.is_empty() {
            this.show_login(false, window, cx);
        }
        this
    }

    fn publish(&self, cx: &mut Context<Self>) {
        let (sessions, active) = (self.servers.iter().map(|s| s.session.clone()).collect(), self.active);
        servers::handle(cx).update(cx, |s, cx| {
            s.sessions = sessions;
            s.active = active;
            cx.notify();
        });
    }

    fn show_login(&mut self, cancellable: bool, window: &mut Window, cx: &mut Context<Self>) {
        let login = cx.new(|cx| Login::new(cancellable, window, cx));
        let sub = cx.subscribe_in(&login, window, |this, _, e: &LoginEvent, window, cx| match e {
            LoginEvent::SignedIn(a, c) => {
                this.login = None;
                // Signing in again to a server we already have replaces it.
                if let Some(i) = this.servers.iter().position(|s| s.session.read(cx).account.realm == a.realm) {
                    this.servers.remove(i);
                }
                this.add_server(a.clone(), c.clone(), window, cx);
                let last = this.servers.len() - 1;
                this.switch(last, window, cx);
            }
            LoginEvent::Cancel => {
                this.login = None;
                cx.notify();
            }
        });
        self.login = Some((login, sub));
        cx.notify();
    }

    fn add_server(&mut self, a: accounts::Account, c: Credentials, window: &mut Window, cx: &mut Context<Self>) {
        let store = match puzil_store::Store::open(&accounts::store_path(&a)) {
            Ok(s) => std::sync::Arc::new(s),
            Err(e) => {
                self.failed = Some(format!("Couldn't open the message cache: {e:#}"));
                cx.notify();
                return;
            }
        };
        let session = cx.new(|cx| Session::new(a, Client::new(c), store, cx));
        // A refused key is dead: forget it, and ask to sign in again if nothing's left.
        let sub = cx.observe_in(&session, window, |this, session, window, cx| {
            if let session::Status::SignedOut(why) = &session.read(cx).status {
                tracing::warn!("signed out by the server: {why}");
                let a = session.read(cx).account.clone();
                this.sign_out(a, window, cx);
            }
        });
        let workspace = cx.new(|cx| Workspace::new(session.clone(), window, cx));
        self.servers.push(Server { session, workspace, _sub: sub });
        self.publish(cx);
    }

    fn switch(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.servers.len() {
            return;
        }
        self.active = i;
        for (k, s) in self.servers.iter().enumerate() {
            s.workspace.update(cx, |w, cx| w.set_active(k == i, window, cx));
        }
        let realm = self.servers[i].session.read(cx).account.realm.clone();
        settings::handle(cx).update(cx, |s, cx| s.update(&settings::Scope::App, |l| l.last_server = Some(realm), cx));
        self.publish(cx);
        cx.notify();
    }

    fn sign_out(&mut self, a: accounts::Account, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = accounts::remove(&a) {
            tracing::warn!("forgetting the account failed: {e:#}");
        }
        let Some(i) = self.servers.iter().position(|s| s.session.read(cx).account == a) else { return };
        self.servers.remove(i);
        self.publish(cx);
        if self.servers.is_empty() {
            self.show_login(false, window, cx);
        } else {
            self.switch(self.active.min(self.servers.len() - 1), window, cx);
        }
    }

    fn on_url(&mut self, url: String, cx: &mut Context<Self>) {
        if let Some((l, _)) = &self.login {
            l.update(cx, |l, cx| l.handle_callback(&url, cx));
        }
    }
}

static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

impl Render for Root {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        static FIRST: std::sync::Once = std::sync::Once::new();
        FIRST.call_once(|| {
            window.on_next_frame(|_, _| {
                if let Some(t) = STARTED.get() {
                    tracing::info!("first frame {:.0?} after launch", t.elapsed());
                }
            });
        });
        let t = theme::Theme::for_appearance(window.appearance());
        let content = match (&self.login, self.servers.get(self.active), &self.failed) {
            (Some((l, _)), _, _) => div().flex_1().min_h_0().child(l.clone()),
            (None, Some(s), _) => div().flex_1().min_h_0().child(s.workspace.clone()),
            (None, None, e) => div().flex_1().p_8().text_color(t.fg).child(e.clone().unwrap_or_default()),
        };
        // No title bar: the traffic lights float in the sidebar's header strip.
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .font_family(ui::rich::UI_FONT)
            .on_action(|_: &menus::Minimize, window, _| window.minimize_window())
            .on_action(|_: &menus::Zoom, window, _| window.zoom_window())
            .child(content)
    }
}

fn import_zuliprc(path: &str) -> anyhow::Result<()> {
    let mut c = puzil_zulip::zuliprc::load(path.as_ref())?;
    let me = runtime::rt().block_on(puzil_zulip::auth::verify(&puzil_zulip::auth::http(), &c))?;
    c.user_id = Some(me.user_id);
    let a = accounts::add(&c, None)?;
    println!("added {} (user {}) at {}; secrets: {}", a.email, a.user_id, a.realm, accounts::secrets().describe());
    Ok(())
}

/// Logs go to stderr and to `puzil.log` in the log folder (a bundled app has no terminal).
/// The file starts over once it passes 5 MB, keeping one previous file.
fn init_logging() {
    use tracing_subscriber::{Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let filter = || tracing_subscriber::EnvFilter::try_from_env("PUZIL_LOG").unwrap_or_else(|_| "info".into());
    let dir = accounts::log_dir();
    let file = std::fs::create_dir_all(&dir).ok().and_then(|_| {
        let path = dir.join("puzil.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 << 20) {
            let _ = std::fs::rename(&path, dir.join("puzil.1.log"));
        }
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
    });
    let registry = tracing_subscriber::registry().with(fmt::layer().with_writer(std::io::stderr).with_filter(filter()));
    match file {
        Some(f) => registry.with(fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(f)).with_filter(filter())).init(),
        None => registry.init(),
    }
}

fn main() {
    STARTED.get_or_init(std::time::Instant::now);
    init_logging();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--import-zuliprc") {
        if let Err(e) = import_zuliprc(args.get(1).map(String::as_str).unwrap_or_default()) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    let launch_urls: Vec<String> = args.iter().filter(|a| a.starts_with("zulip://")).cloned().collect();
    // Linux delivers scheme URLs as argv of a fresh process: hand them to the running instance.
    let Some(incoming) = instance::claim_or_forward(&launch_urls) else { return };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tracing::debug!("creating app at {:.0?}", STARTED.get().map(|t| t.elapsed()).unwrap_or_default());
    let app = gpui_kit::application().with_assets(icons::AppAssets);
    // A click on the Dock icon brings the (hidden) window back.
    app.on_reopen(|cx| cx.activate(true));
    {
        let tx = tx.clone();
        app.on_open_urls(move |urls| {
            for u in urls {
                let _ = tx.send(u);
            }
        });
    }
    instance::pump(incoming, tx.clone());
    for u in launch_urls {
        let _ = tx.send(u);
    }
    let since = || STARTED.get().map(|t| t.elapsed()).unwrap_or_default();
    tracing::debug!("app built at {:.0?}", since());
    app.run(move |cx: &mut App| {
        tracing::debug!("run callback at {:.0?}", since());
        gpui_kit::init(cx);
        dock::set_icon();
        settings::init(cx);
        servers::init(cx);
        updates::init(cx);
        {
            let e = settings::effective(cx, None);
            theme::Theme::choose(e.palette, e.mode);
            settings::apply_globals(&e, cx);
        }
        tracing::debug!("gpui-kit init done at {:.0?}", since());
        // Figtree (UI and messages), Unbounded (display) and JetBrains Mono (code), all OFL;
        // Discord's own gg sans, Ginto Nord and gg mono aren't licensed for this.
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../../../assets/fonts/PuzilAtoms.ttf")),
                Cow::Borrowed(include_bytes!("../../../assets/fonts/Figtree-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../../../assets/fonts/Figtree-Medium.ttf")),
                Cow::Borrowed(include_bytes!("../../../assets/fonts/Figtree-SemiBold.ttf")),
                Cow::Borrowed(include_bytes!("../../../assets/fonts/Figtree-Bold.ttf")),
                Cow::Borrowed(include_bytes!("../../../assets/fonts/Figtree-Italic.ttf")),
                Cow::Borrowed(include_bytes!("../../../assets/fonts/Unbounded-Bold.ttf")),
                Cow::Borrowed(include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../../../assets/fonts/JetBrainsMono-Bold.ttf")),
            ])
            .expect("bundled fonts");
        cx.bind_keys([
            KeyBinding::new("cmd-c", CopySelection, Some("Conversation")),
            KeyBinding::new("ctrl-c", CopySelection, Some("Conversation")),
        ]);
        math::init(cx);
        ui::composer::bind_keys(cx);
        ui::workspace::bind_keys(cx);
        ui::conversation::bind_keys(cx);
        if std::env::var_os("PUZIL_LOG_KEYS").is_some() {
            cx.observe_keystrokes(|e, _, _| tracing::info!("key {:?} {:?} action={:?}", e.keystroke.key, e.keystroke.modifiers, e.action.as_ref().map(|a| a.name()))).detach();
        }
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        menus::init(cx);
        let bounds = menus::saved_bounds().unwrap_or_else(|| Bounds::centered(None, size(px(1280.), px(860.)), cx));
        let (_, root) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Puzil".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(theme::TRAFFIC_LIGHTS_AT.0), px(theme::TRAFFIC_LIGHTS_AT.1))),
                }),
                app_id: Some("earth.benson.puzil".into()),
                ..Default::default()
            },
            cx,
            |window, cx| {
                menus::attach(window, cx);
                cx.new(|cx| Root::new(window, cx))
            },
        )
        .expect("window");
        tracing::debug!("window opened at {:.0?}", since());
        cx.spawn(async move |cx| {
            while let Some(url) = rx.recv().await {
                let _ = root.update(cx, |r, cx| r.on_url(url, cx));
            }
        })
        .detach();
        cx.activate(true);
    });
}

actions!(puzil, [Quit]);
