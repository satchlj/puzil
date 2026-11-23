# Puzil

A fast native [Zulip](https://zulip.com) client for macOS (Linux to follow), written in Rust with [GPUI](https://gpui.rs).

- **Fast:** a GPU-rendered UI, a local SQLite cache and full-text index, cold start to your messages in a fraction of a second.
- **Familiar:** a Discord-style layout with Zulip's model underneath: channels, channel folders, topics (with `Parent / Child` nesting), and direct messages.
- **Complete rendering:** Markdown, code with highlighting, typeset math (Typst), images with a lightbox, file attachments, spoilers, reactions, mentions.
- **Keyboard first:** ⌘K jumps anywhere, ⌘F searches (local index plus the server), ↑ selects messages, Enter edits or replies.
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

You only do this once. After that Puzil updates itself: when a new version is out, a prompt appears in the corner of the window. **Update and restart** downloads it, checks it carries the same signature as the copy you have, swaps it in and restarts, all in one click. **Puzil › Check for Updates…** checks on demand.

Releases are signed with the same certificate every time, so after an update macOS doesn't ask again for access to Puzil's saved logins.

Sign in with your organization's URL (password, SSO, or a `zuliprc` file).

## Build

Rust stable (2024 edition) and Xcode command-line tools.

```bash
cargo run --release -p puzil-app       # run from source
tools/macos/bundle.sh                  # build dist/Puzil.app and a zip
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
