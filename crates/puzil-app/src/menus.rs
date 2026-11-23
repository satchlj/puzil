//! The macOS menu bar, and window behaviour: closing hides the app (the Dock icon brings it
//! back), and the window comes back where it was left.

use gpui_kit::{
    component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo},
    *,
};

use crate::ui::workspace::{OpenSearch, OpenSettings, QuickSwitch, ShowAbout};

actions!(puzil_app, [Hide, HideOthers, ShowAll, Minimize, Zoom, RevealLogs, CheckForUpdates]);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &CheckForUpdates, cx| crate::updates::check_now(cx));
    cx.on_action(|_: &RevealLogs, cx| cx.reveal_path(&crate::accounts::log_dir().join("puzil.log")));
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-h", Hide, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-m", Minimize, None),
        // ⌘, on macOS, Ctrl+, elsewhere.
        KeyBinding::new(chord!(","), OpenSettings, None),
    ]);
    cx.set_menus([
        Menu::new("Puzil").items([
            MenuItem::action("About Puzil", ShowAbout),
            #[cfg(target_os = "macos")]
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Puzil", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Puzil", crate::Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Go").items([MenuItem::action("Jump To…", QuickSwitch), MenuItem::action("Search…", OpenSearch)]),
        Menu::new("Window").items([MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)]),
        Menu::new("Help").items([MenuItem::action(crate::platform::SHOW_LOGS, RevealLogs)]),
    ]);
}

/// Window actions and close behaviour for the main window.
pub fn attach(window: &mut Window, cx: &mut App) {
    // On macOS closing the window hides Puzil instead; it keeps syncing, and the Dock icon
    // reopens it. Elsewhere there is no app-level hide, so closing the window quits.
    #[cfg(target_os = "macos")]
    window.on_window_should_close(cx, |_, cx| {
        cx.hide();
        false
    });
    #[cfg(not(target_os = "macos"))]
    let _ = (window, cx);
}

/// Where the window was, so the next launch opens it there.
pub fn saved_bounds() -> Option<Bounds<Pixels>> {
    let j = std::fs::read_to_string(crate::accounts::data_dir().join("window.json")).ok()?;
    let v: [f32; 4] = serde_json::from_str(&j).ok()?;
    (v[2] >= 600. && v[3] >= 400.).then(|| Bounds::new(point(px(v[0]), px(v[1])), size(px(v[2]), px(v[3]))))
}

pub fn save_bounds(b: Bounds<Pixels>) {
    let v = [f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)];
    if let Ok(j) = serde_json::to_string(&v) {
        let _ = std::fs::write(crate::accounts::data_dir().join("window.json"), j);
    }
}
