//! New versions: GitHub's latest release of the public repo, checked at launch and every six
//! hours. A newer one shows in the account panel and Settings with a link to download it.

use std::time::Duration;

use gpui_kit::*;

const LATEST: &str = "https://api.github.com/repos/satchlj/puzil/releases/latest";

#[derive(Default)]
pub struct Updates {
    /// A newer release: its version and page.
    pub available: Option<(String, String)>,
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

pub fn init(cx: &mut App) {
    let e = cx.new(|_| Updates::default());
    cx.set_global(UpdatesHandle(e.clone()));
    if std::env::var_os("PUZIL_NO_UPDATE_CHECK").is_some() {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            let r = crate::runtime::on_tokio(async {
                let v: serde_json::Value = reqwest::Client::new()
                    .get(LATEST)
                    .header("User-Agent", concat!("Puzil/", env!("CARGO_PKG_VERSION")))
                    .header("Accept", "application/vnd.github+json")
                    .timeout(Duration::from_secs(20))
                    .send()
                    .await?
                    .json()
                    .await?;
                Ok::<_, reqwest::Error>(v)
            })
            .await;
            match r {
                Ok(v) => {
                    let tag = v["tag_name"].as_str().unwrap_or("").to_string();
                    let url = v["html_url"].as_str().unwrap_or("https://github.com/satchlj/puzil/releases").to_string();
                    if !tag.is_empty() && newer(&tag, env!("CARGO_PKG_VERSION")) {
                        let _ = e.update(cx, |u, cx| {
                            u.available = Some((tag.trim_start_matches('v').to_string(), url));
                            cx.notify();
                        });
                    }
                }
                Err(err) => tracing::debug!("update check: {err}"),
            }
            cx.background_executor().timer(Duration::from_secs(6 * 3600)).await;
        }
    })
    .detach();
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
