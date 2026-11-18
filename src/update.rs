//! Update checks and feedback links. The app makes no network requests of its own:
//! an update check runs the system `curl` once, only when the user asks for it
//! (or has opted in to automatic checks), and sends nothing but the app version
//! in the User-Agent header. Feedback only opens a pre-filled page in the browser.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::process::Command;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Where the source, issues and releases live.
pub const REPO_URL: &str = "https://github.com/ahmedhussainmv/usage-lens-mac";
/// The download page, which also serves the update feed.
pub const SITE_URL: &str = "https://ahmedhussainmv.github.io/usage-lens-site";
pub const FEED_URL: &str = "https://ahmedhussainmv.github.io/usage-lens-site/latest.json";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
    #[serde(default)]
    pub notes: String,
}

fn numbers(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map_while(|p| p.parse::<u64>().ok())
        .collect()
}
/// True when `latest` is a higher dotted version than `current`.
pub fn is_newer(current: &str, latest: &str) -> bool {
    let (mut a, mut b) = (numbers(current), numbers(latest));
    if b.is_empty() {
        return false;
    }
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    b > a
}
/// Parses the feed and only accepts links that point at this project.
pub fn parse_feed(json: &str) -> Result<Release> {
    let release: Release = serde_json::from_str(json).context("Unreadable update feed")?;
    if !(release.url.starts_with(REPO_URL) || release.url.starts_with(SITE_URL)) {
        bail!("The update feed points somewhere unexpected.");
    }
    if numbers(&release.version).is_empty() {
        bail!("The update feed has no valid version.");
    }
    Ok(release)
}
/// One HTTPS request via the system curl. Returns the release if it is newer than this build.
pub fn check() -> Result<Option<Release>> {
    let output = Command::new("/usr/bin/curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "10",
            "--max-filesize",
            "65536",
            "--user-agent",
            &format!("UsageLens/{VERSION}"),
            FEED_URL,
        ])
        .output()
        .context("Could not run the system curl")?;
    if !output.status.success() {
        bail!(
            "Could not reach the update server ({})",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let release = parse_feed(&String::from_utf8_lossy(&output.stdout))?;
    Ok(is_newer(VERSION, &release.version).then_some(release))
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
pub fn macos_version() -> String {
    Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".into())
}
/// A pre-filled "new issue" page. It carries the app and macOS versions only;
/// the user reviews and edits it in the browser before anything is sent.
pub fn feedback_url(os_version: &str) -> String {
    let body = format!(
        "**What happened or what would you like?**\n\n\n\n**Steps to reproduce (if a bug)**\n\n\n\n---\nUsage Lens {VERSION} on macOS {os_version}\n_Please do not paste prompts, code or private project names._\n"
    );
    format!(
        "{REPO_URL}/issues/new?title={}&body={}",
        encode("Feedback: "),
        encode(&body)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compares_versions() {
        assert!(is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.9.9", "v1.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "garbage"));
    }
    #[test]
    fn accepts_only_project_links() {
        let ok = format!(r#"{{"version":"0.3.0","url":"{REPO_URL}/releases/tag/v0.3.0"}}"#);
        assert_eq!(parse_feed(&ok).unwrap().version, "0.3.0");
        assert!(parse_feed(r#"{"version":"0.3.0","url":"https://evil.example/x"}"#).is_err());
        assert!(parse_feed(r#"{"version":"x","url":"https://github.com/ahmedhussainmv/usage-lens-mac"}"#).is_err());
    }
    #[test]
    fn feedback_link_is_encoded_and_private() {
        let url = feedback_url("15.0");
        assert!(url.starts_with(REPO_URL));
        assert!(!url.contains(' ') && !url.contains('\n'));
        assert!(url.contains("macOS%2015.0"));
    }
}
