//! New versions: GitHub's latest release of the public repo, checked at launch and every six
//! hours. A newer one prompts over the window and installs itself in one click.
//!
//! On macOS:
//! 1. the release zip downloads (an app's own downloads carry no quarantine, so the new copy
//!    opens without Gatekeeper's prompt);
//! 2. it unpacks into a staging folder beside the running app, on the same volume;
//! 3. its code signature must satisfy the running app's own designated requirement, which
//!    names the signing certificate: a zip signed by anyone else is refused;
//! 4. the two apps swap places and Puzil relaunches. The old copy is deleted next launch.
//!
//! On Linux, as an AppImage: the AppImage for this architecture downloads with its `.sig`,
//! an ed25519 signature that must verify against [`RELEASE_KEY`]; the new file replaces the
//! old one in place and Puzil relaunches. A build from source isn't updated.
//!
//! An unbundled or ad-hoc-signed Puzil can't verify an update, so it only links to the
//! release page.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use gpui_kit::*;

const LATEST: &str = "https://api.github.com/repos/satchlj/puzil/releases/latest";
const STAGING: &str = ".puzil-update";

/// The public half of the key the release workflow signs AppImages with (the private half is
/// the repo's `PUZIL_UPDATE_KEY` secret).
const RELEASE_KEY: [u8; 32] = hex32("ebd05f0bb7964c1ec985e1a126a2b8daad4723a78db84ca6ede1e3fd8c94f95d");

const fn hex32(s: &str) -> [u8; 32] {
    const fn nib(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("not hex"),
        }
    }
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = nib(b[2 * i]) << 4 | nib(b[2 * i + 1]);
        i += 1;
    }
    out
}

/// `bytes` carries a valid release signature.
fn signed_by_release_key(bytes: &[u8], sig: &[u8]) -> bool {
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&RELEASE_KEY) else { return false };
    let Ok(sig) = ed25519_dalek::Signature::from_slice(sig) else { return false };
    key.verify_strict(bytes, &sig).is_ok()
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    /// The release page, for reading or downloading by hand.
    pub page: String,
    /// The macOS app zip attached to the release.
    pub zip: Option<String>,
    /// This architecture's AppImage and its signature.
    pub appimage: Option<(String, String)>,
}

/// What this copy of Puzil is, for updating it.
enum Installed {
    /// A macOS `.app`.
    Bundle(PathBuf),
    /// A Linux AppImage file (its own path, not the mount it runs from).
    AppImage(PathBuf),
}

fn installed() -> Option<Installed> {
    if cfg!(target_os = "macos") {
        bundle().map(Installed::Bundle)
    } else {
        std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()).map(Installed::AppImage)
    }
}

/// Whether this copy checks for updates at all (a build from source on Linux doesn't).
pub fn supported() -> bool {
    cfg!(target_os = "macos") || std::env::var_os("APPIMAGE").is_some()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    /// Percent downloaded.
    Downloading(u8),
    Installing,
    Failed(String),
}

pub struct Updates {
    pub available: Option<Release>,
    pub phase: Phase,
    /// The prompt was put off with "Later" (until the next launch, a newer release, or a
    /// check from the menu).
    pub dismissed: bool,
    /// A check from the menu is running, or just found nothing ("You're up to date").
    pub checking: bool,
    pub up_to_date_notice: bool,
}

pub struct UpdatesHandle(pub Entity<Updates>);
impl Global for UpdatesHandle {}

pub fn handle(cx: &App) -> Entity<Updates> {
    cx.global::<UpdatesHandle>().0.clone()
}

/// `a` is newer than `b` (dotted numeric versions; a leading `v` ignored).
fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| s.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(a) > parse(b)
}

/// The `.app` this process runs from, if any.
fn bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_path_buf())
}

impl Updates {
    /// Whether "Update and restart" can work here (else the release page is offered).
    pub fn can_install(&self) -> bool {
        match (self.available.as_ref(), installed()) {
            (Some(r), Some(Installed::Bundle(_))) => r.zip.is_some(),
            (Some(r), Some(Installed::AppImage(_))) => r.appimage.is_some(),
            _ => false,
        }
    }

    /// Whether the update prompt shows over the window.
    pub fn prompting(&self) -> bool {
        (self.available.is_some() && (!self.dismissed || self.phase != Phase::Idle)) || self.checking || self.up_to_date_notice
    }
}

/// Puts the prompt off until something new happens.
pub fn dismiss(cx: &mut App) {
    handle(cx).update(cx, |u, cx| {
        u.dismissed = true;
        u.up_to_date_notice = false;
        if matches!(u.phase, Phase::Failed(_)) {
            u.phase = Phase::Idle;
        }
        cx.notify();
    });
}

/// "Check for Updates…": checks now and always answers in the prompt.
pub fn check_now(cx: &mut App) {
    if !supported() {
        return;
    }
    let e = handle(cx);
    e.update(cx, |u, cx| {
        u.dismissed = false;
        u.checking = true;
        u.up_to_date_notice = false;
        cx.notify();
    });
    cx.spawn(async move |cx| {
        let found = check(&e, cx).await;
        let _ = e.update(cx, |u, cx| {
            u.checking = false;
            u.up_to_date_notice = !found && u.available.is_none();
            cx.notify();
        });
        cx.background_executor().timer(Duration::from_secs(4)).await;
        let _ = e.update(cx, |u, cx| {
            u.up_to_date_notice = false;
            cx.notify();
        });
    })
    .detach();
}

/// Asks GitHub for the latest release; true when it's newer than this copy.
async fn check(e: &Entity<Updates>, cx: &mut AsyncApp) -> bool {
    // PUZIL_UPDATE_FEED stands in a local release description, for testing updates.
    let feed = std::env::var("PUZIL_UPDATE_FEED").unwrap_or_else(|_| LATEST.to_string());
    match on_tokio_json(feed).await {
        Ok(v) => {
            let tag = v["tag_name"].as_str().unwrap_or("").to_string();
            if tag.is_empty() || !newer(&tag, env!("CARGO_PKG_VERSION")) {
                // Nothing newer (a release withdrawn, say): forget an earlier offer.
                let _ = e.update(cx, |u, cx| {
                    if u.phase == Phase::Idle || matches!(u.phase, Phase::Failed(_)) {
                        u.available = None;
                        u.phase = Phase::Idle;
                        cx.notify();
                    }
                });
                return false;
            }
            let urls: Vec<String> = v["assets"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x["browser_download_url"].as_str().map(String::from)).collect())
                .unwrap_or_default();
            let zip = urls.iter().find(|u| u.ends_with(".zip")).cloned();
            let suffix = format!("-{}.AppImage", std::env::consts::ARCH);
            let appimage = urls.iter().find(|u| u.ends_with(&suffix)).and_then(|u| {
                let sig = format!("{u}.sig");
                urls.contains(&sig).then(|| (u.clone(), sig))
            });
            let release = Release {
                version: tag.trim_start_matches('v').to_string(),
                page: v["html_url"].as_str().unwrap_or("https://github.com/satchlj/puzil/releases").to_string(),
                zip,
                appimage,
            };
            let _ = e.update(cx, |u, cx| {
                // A newer release than the one put off asks again.
                if u.available.as_ref().is_none_or(|a| a.version != release.version) {
                    u.dismissed = false;
                }
                u.available = Some(release);
                cx.notify();
            });
            true
        }
        Err(err) => {
            tracing::debug!("update check: {err}");
            false
        }
    }
}

pub fn init(cx: &mut App) {
    let e = cx.new(|_| Updates { available: None, phase: Phase::Idle, dismissed: false, checking: false, up_to_date_notice: false });
    cx.set_global(UpdatesHandle(e.clone()));
    // What the last update left behind.
    match installed() {
        Some(Installed::Bundle(b)) => {
            if let Some(p) = b.parent() {
                let _ = std::fs::remove_dir_all(p.join(STAGING));
            }
        }
        Some(Installed::AppImage(a)) => {
            let _ = std::fs::remove_file(appimage_temp(&a));
        }
        None => {}
    }
    if !supported() || std::env::var_os("PUZIL_NO_UPDATE_CHECK").is_some() {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            check(&e, cx).await;
            cx.background_executor().timer(Duration::from_secs(6 * 3600)).await;
        }
    })
    .detach();
}

async fn on_tokio_json(url: String) -> Result<serde_json::Value, reqwest::Error> {
    crate::runtime::on_tokio(async move {
        reqwest::Client::new()
            .get(&url)
            .header("User-Agent", concat!("Puzil/", env!("CARGO_PKG_VERSION")))
            .header("Accept", "application/vnd.github+json")
            .timeout(Duration::from_secs(20))
            .send()
            .await?
            .json()
            .await
    })
    .await
}

fn set_phase(e: &Entity<Updates>, phase: Phase, cx: &mut AsyncApp) {
    let _ = e.update(cx, |u, cx| {
        u.phase = phase;
        cx.notify();
    });
    let _ = cx.update(|cx| cx.refresh_windows());
}

/// Downloads, verifies and installs the available release, then relaunches.
pub fn install(cx: &mut App) {
    let e = handle(cx);
    let u = e.read(cx);
    if matches!(u.phase, Phase::Downloading(_) | Phase::Installing) {
        return;
    }
    let (Some(release), Some(target)) = (u.available.clone(), installed()) else { return };
    let (url, sig_url) = match &target {
        Installed::Bundle(_) => match release.zip.clone() {
            Some(z) => (z, None),
            None => return,
        },
        Installed::AppImage(_) => match release.appimage.clone() {
            Some((a, s)) => (a, Some(s)),
            None => return,
        },
    };
    let progress = Arc::new(AtomicU8::new(0));
    e.update(cx, |u, cx| {
        u.phase = Phase::Downloading(0);
        cx.notify();
    });
    cx.refresh_windows();
    cx.spawn(async move |cx| {
        // Progress, while the download runs.
        let p = progress.clone();
        let e2 = e.clone();
        let ticker = cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let n = p.load(Ordering::Relaxed);
                let still = e2.read_with(cx, |u, _| matches!(u.phase, Phase::Downloading(_)));
                if !still {
                    break;
                }
                set_phase(&e2, Phase::Downloading(n), cx);
            }
        });
        let p = progress.clone();
        let fetched = crate::runtime::on_tokio(async move {
            let sig = match sig_url {
                Some(s) => Some(download(&s, &AtomicU8::new(0)).await?),
                None => None,
            };
            Ok::<_, reqwest::Error>((download(&url, &p).await?, sig))
        })
        .await;
        let result = match fetched {
            Err(err) => Err(format!("Download failed: {err}")),
            Ok((bytes, sig)) => {
                set_phase(&e, Phase::Installing, cx);
                let path = match &target {
                    Installed::Bundle(p) | Installed::AppImage(p) => p.clone(),
                };
                let appimage = matches!(target, Installed::AppImage(_));
                cx.background_executor()
                    .spawn(async move {
                        if appimage {
                            return replace_appimage(&path, &bytes, sig.as_deref().unwrap_or_default());
                        }
                        let r = stage_and_swap(&path, &bytes);
                        // A refused or broken download goes now, not next launch; but only
                        // while the app is in place (a half-done swap leaves it in staging).
                        if r.is_err() && path.exists()
                            && let Some(dir) = path.parent()
                        {
                            let _ = std::fs::remove_dir_all(dir.join(STAGING));
                        }
                        r
                    })
                    .await
            }
        };
        drop(ticker);
        match result {
            Ok(()) => {
                tracing::info!("updated to {}; relaunching", release.version);
                relaunch(&target);
                let _ = cx.update(|cx| cx.quit());
            }
            Err(msg) => {
                tracing::warn!("update: {msg}");
                set_phase(&e, Phase::Failed(msg), cx);
            }
        }
    })
    .detach();
}

async fn download(url: &str, progress: &AtomicU8) -> Result<Vec<u8>, reqwest::Error> {
    let mut resp = reqwest::Client::new()
        .get(url)
        .header("User-Agent", concat!("Puzil/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(600))
        .send()
        .await?
        .error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let mut out = Vec::with_capacity(total as usize);
    while let Some(chunk) = resp.chunk().await? {
        out.extend_from_slice(&chunk);
        if total > 0 {
            progress.store((out.len() as u64 * 100 / total).min(100) as u8, Ordering::Relaxed);
        }
    }
    Ok(out)
}

/// Where a downloaded AppImage waits before replacing `current` (same folder, so the swap
/// is a rename).
fn appimage_temp(current: &Path) -> PathBuf {
    current.with_file_name(".puzil-update.AppImage")
}

/// Checks the signature, then puts the new AppImage where the running one is. The running
/// copy keeps its (now unlinked) file until it exits.
fn replace_appimage(current: &Path, bytes: &[u8], sig: &[u8]) -> Result<(), String> {
    if !signed_by_release_key(bytes, sig) {
        return Err("the update isn't signed with Puzil's release key; not installing it".into());
    }
    let tmp = appimage_temp(current);
    std::fs::write(&tmp, bytes).map_err(|e| format!("Puzil can't write next to {}: {e}", current.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&tmp, current).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("putting the new version in place: {e}")
    })
}

/// Unpacks the zip beside `app`, checks its signature, and swaps it in.
fn stage_and_swap(app: &Path, zip: &[u8]) -> Result<(), String> {
    let parent = app.parent().ok_or("no folder above the app")?;
    let staging = parent.join(STAGING);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging).map_err(|e| format!("Puzil can't write to {}: {e}", parent.display()))?;
    let zip_path = staging.join("update.zip");
    std::fs::write(&zip_path, zip).map_err(|e| format!("saving the download: {e}"))?;
    let unpacked = staging.join("new");
    run(Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(&zip_path).arg(&unpacked), "unpacking")?;
    let new_app = std::fs::read_dir(&unpacked)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|d| d.path())
        .find(|p| p.extension().is_some_and(|x| x == "app"))
        .ok_or("the download has no app in it")?;
    verify(app, &new_app)?;
    let old = staging.join("old.app");
    std::fs::rename(app, &old).map_err(|e| format!("moving the old version aside: {e}"))?;
    if let Err(e) = std::fs::rename(&new_app, app) {
        let _ = std::fs::rename(&old, app);
        return Err(format!("putting the new version in place: {e}"));
    }
    Ok(())
}

/// The new app must satisfy the running app's designated requirement: same identifier, same
/// signing certificate.
fn verify(current: &Path, new_app: &Path) -> Result<(), String> {
    let out = Command::new("/usr/bin/codesign").arg("-d").arg("-r-").arg(current).output().map_err(|e| e.to_string())?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let req = text
        .lines()
        .find_map(|l| l.strip_prefix("designated => "))
        .ok_or("this copy of Puzil isn't signed, so it can't check an update")?;
    if req.contains("cdhash") {
        return Err("this copy of Puzil is signed ad hoc, so it can't check an update".into());
    }
    run(
        Command::new("/usr/bin/codesign").arg("--verify").arg("--deep").arg("--strict").arg(format!("-R={req}")).arg(new_app),
        "checking the update's signature",
    )
    .map_err(|_| "the update isn't signed by Puzil's certificate; not installing it".to_string())
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Starts the updated Puzil once this process has exited, passing on Puzil's environment
/// overrides (a development data folder, say).
fn relaunch(target: &Installed) {
    let wait = r#"pid=$1; shift; while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done; exec "$@""#;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(wait).arg("sh").arg(std::process::id().to_string());
    match target {
        Installed::Bundle(app) => {
            cmd.arg("/usr/bin/open").arg(app);
            for (k, v) in std::env::vars() {
                if k.starts_with("PUZIL_") {
                    cmd.arg("--env").arg(format!("{k}={v}"));
                }
            }
        }
        Installed::AppImage(file) => {
            // The new AppImage sets these for itself; the old mount's would point at nothing.
            cmd.arg(file);
            for k in ["APPDIR", "APPIMAGE", "ARGV0", "OWD"] {
                cmd.env_remove(k);
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let _ = cmd.spawn();
}

#[cfg(test)]
mod tests {
    #[test]
    fn compares_versions() {
        assert!(super::newer("v0.2.0", "0.1.0"));
        assert!(super::newer("0.10.0", "0.9.3"));
        assert!(!super::newer("v0.1.0", "0.1.0"));
        assert!(!super::newer("0.1.0", "0.2.0"));
    }

    /// Signed by the release workflow's command (`openssl pkeyutl -sign -rawin`) with the
    /// real release key.
    #[test]
    fn checks_release_signatures() {
        let msg = b"puzil update test\n";
        let sig = hex(
            "35b60a7fa6776e26310acb0a4078ef52c86b0a8a8e4141195a0c46d3fc77184542b1aed1718457dadc0ffe9a0d9eb9691d120dfa9a4dac44af39483c29e25b08",
        );
        assert!(super::signed_by_release_key(msg, &sig));
        assert!(!super::signed_by_release_key(b"puzil update test, tampered\n", &sig));
        assert!(!super::signed_by_release_key(msg, &sig[..63]));
        assert!(!super::signed_by_release_key(msg, &[]));
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
}
