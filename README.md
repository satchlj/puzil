# Puzil

A fast native [Zulip](https://zulip.com) client for macOS and Linux, written in Rust with [GPUI](https://gpui.rs).

- **Fast:** a GPU-rendered UI, a local SQLite cache and full-text index, cold start to your messages in a fraction of a second.
- **Familiar:** a Discord-style layout with Zulip's model underneath: channels, channel folders, topics (with `Parent / Child` nesting), and direct messages.
- **Complete rendering:** Markdown, code with highlighting, typeset math (Typst), images with a lightbox, file attachments, spoilers, reactions, mentions.
- **Keyboard first:** ⌘K jumps anywhere, ⌘F searches (local index plus the server), ↑ selects messages, Enter edits or replies. On Linux, Ctrl stands in for ⌘.
- **Yours:** six themes in light and dark, app-wide settings that each server can override, and your Zulip notification, privacy and profile settings without opening the web app.
- **Several servers** side by side, each syncing in the background.

## Install

Download `Puzil-<version>.zip` from [Releases](../../releases), unzip, and move `Puzil.app` to Applications.

### Opening it the first time

Puzil isn't signed with an Apple Developer ID or notarized by Apple, so macOS refuses to open it at first ("Apple could not verify Puzil is free of malware"). Right-click → Open no longer gets past this on recent macOS. Instead, either:

**In System Settings**
1. Double-click Puzil and dismiss the warning with **Done**.
2. Open **System Settings → Privacy & Security** and scroll down to Security.
3. Next to "Puzil was blocked to protect your Mac", click **Open Anyway**, then confirm with your password or Touch ID.
4. In the dialog that follows, click **Open Anyway** again.

**Or in Terminal**, remove the download quarantine and open it normally:

```bash
xattr -dr com.apple.quarantine /Applications/Puzil.app
```

Each version you download is quarantined again, so after updating, repeat either one.

Releases are signed with the same certificate every time, so after an update macOS doesn't ask again for access to Puzil's saved logins.

Sign in with your organization's URL (password, SSO, or a `zuliprc` file).

### Linux

There is no packaged build yet; build from source. You need Rust stable and the development packages for fontconfig, freetype, Wayland, xkbcommon and xcb:

```bash
# Fedora
sudo dnf install fontconfig-devel freetype-devel wayland-devel libxkbcommon-devel libxkbcommon-x11-devel libxcb-devel
# Debian / Ubuntu
sudo apt install libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev

tools/linux/install.sh
```

That installs `~/.local/bin/puzil`, a desktop entry and the icon, and makes Puzil the handler for `zulip://` links, which is how an SSO sign-in in the browser gets back to the app. Without that handler, copy the `zulip://` link from the browser and paste it into the sign-in screen.

API keys go in the Secret Service (GNOME Keyring, KWallet, KeePassXC), or a `0600` file in `~/.local/share/puzil` when none is running. The window uses the desktop's own title bar, and closing it quits Puzil.

## Build

Rust stable (2024 edition), and Xcode command-line tools on macOS or the packages listed under Linux above.

```bash
cargo run --release -p puzil-app       # run from source
tools/macos/bundle.sh                  # build dist/Puzil.app and a zip
tools/linux/install.sh                 # Linux: build and install for the current user
```

The bundle is signed with a `Puzil Self-Signed` code-signing certificate when your keychain has one, otherwise ad hoc. `PUZIL_SIGN_ID="Developer ID Application: …"` signs with hardened runtime; adding `PUZIL_NOTARY_PROFILE=<notarytool profile>` also notarizes.

The workspace:

| Crate | |
|---|---|
| `puzil-app` | The GPUI app. |
| `puzil-zulip` | Zulip API client, event queue and domain model. |
| `puzil-store` | Per-account SQLite cache (WAL, FTS5). |
| `puzil-render` | Zulip HTML to a message AST. |
| `puzil-math` | KaTeX MathML / TeX to Typst-rendered SVG. |
| `puzil-avatar` | Generated avatars and the app icon. |
| `puzil-overlay` | Client for the Iliad Overlay service (enabled for one organization). |
| `puzil-cli` | A headless client for sync and search. |

## License

MIT or Apache-2.0, at your option. Bundled fonts (Figtree, Unbounded, JetBrains Mono) are under the SIL Open Font License; see `assets/fonts`.
