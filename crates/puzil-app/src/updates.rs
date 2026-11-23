//! New versions: GitHub's latest release of the public repo, checked at launch and every six
//! hours. A newer one shows in the account panel and Settings, and installs itself:
//!
//! 1. the release zip downloads (an app's own downloads carry no quarantine, so the new copy
//!    opens without Gatekeeper's prompt);
//! 2. it unpacks into a staging folder beside the running app, on the same volume;
//! 3. its code signature must satisfy the running app's own designated requirement, which
//!    names the signing certificate: a zip signed by anyone else is refused;
//! 4. the two apps swap places and Puzil relaunches. The old copy is deleted next launch.
//!
//! An unbundled or ad-hoc-signed Puzil can't verify an update this way, so it only links to
//! the release page.

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

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    /// The release page, for reading or downloading by hand.
    pub page: String,
    /// The app zip attached to the release.
    pub zip: Option<String>,
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
        self.available.as_ref().is_some_and(|r| r.zip.is_some()) && bundle().is_some()
    }
}

pub fn init(cx: &mut App) {
    let e = cx.new(|_| Updates { available: None, phase: Phase::Idle });
    cx.set_global(UpdatesHandle(e.clone()));
    // What the last update left behind.
    if let Some(dir) = bundle().and_then(|b| b.parent().map(|p| p.join(STAGING))) {
        let _ = std::fs::remove_dir_all(dir);
    }
    if std::env::var_os("PUZIL_NO_UPDATE_CHECK").is_some() {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            // PUZIL_UPDATE_FEED stands in a local release description, for testing updates.
            let feed = std::env::var("PUZIL_UPDATE_FEED").unwrap_or_else(|_| LATEST.to_string());
            match on_tokio_json(feed).await {
                Ok(v) => {
                    let tag = v["tag_name"].as_str().unwrap_or("").to_string();
                    if !tag.is_empty() && newer(&tag, env!("CARGO_PKG_VERSION")) {
                        let zip = v["assets"].as_array().and_then(|a| {
                            a.iter().find_map(|x| x["browser_download_url"].as_str().filter(|u| u.ends_with(".zip")).map(String::from))
                        });
                        let release = Release {
                            version: tag.trim_start_matches('v').to_string(),
                            page: v["html_url"].as_str().unwrap_or("https://github.com/satchlj/puzil/releases").to_string(),
                            zip,
                        };
                        let _ = e.update(cx, |u, cx| {
                            u.available = Some(release);
                            cx.notify();
                        });
                        let _ = cx.update(|cx| cx.refresh_windows());
                    }
                }
                Err(err) => tracing::debug!("update check: {err}"),
            }
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
    let (Some(release), Some(app)) = (u.available.clone(), bundle()) else { return };
    let Some(zip_url) = release.zip.clone() else { return };
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
        let bytes = crate::runtime::on_tokio(async move { download(&zip_url, &p).await }).await;
        let result = match bytes {
            Err(err) => Err(format!("Download failed: {err}")),
            Ok(bytes) => {
                set_phase(&e, Phase::Installing, cx);
                let app2 = app.clone();
                cx.background_executor()
                    .spawn(async move {
                        let r = stage_and_swap(&app2, &bytes);
                        // A refused or broken download goes now, not next launch; but only
                        // while the app is in place (a half-done swap leaves it in staging).
                        if r.is_err() && app2.exists()
                            && let Some(dir) = app2.parent()
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
                relaunch(&app);
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

/// Opens `app` again once this process has exited, passing on Puzil's environment
/// overrides (a development data folder, say).
fn relaunch(app: &Path) {
    let mut args: Vec<String> = vec![std::process::id().to_string(), app.display().to_string()];
    for (k, v) in std::env::vars() {
        if k.starts_with("PUZIL_") {
            args.push("--env".into());
            args.push(format!("{k}={v}"));
        }
    }
    let _ = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"pid=$1; shift; while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open "$@""#)
        .arg("sh")
        .args(args)
        .spawn();
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
}
