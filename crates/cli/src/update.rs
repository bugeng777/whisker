use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const GITHUB_LATEST: &str = "https://api.github.com/repos/bugeng777/whisker/releases/latest";
const INSTALL_COMMAND: &str =
    "curl -fsSL https://github.com/bugeng777/whisker/releases/latest/download/install.sh | bash";

#[derive(Debug, Default, Serialize, Deserialize)]
struct UpdateCache {
    checked_at: u64,
    #[serde(default)]
    latest: Option<String>,
    #[serde(default)]
    notified: Option<String>,
}

fn cache_path(home: &Path) -> PathBuf {
    home.join(".whisker").join("update-check.json")
}

fn parse_version(tag: &str) -> Option<Vec<u64>> {
    let tag = tag.trim().trim_start_matches('v');
    let core = tag.split(|ch| ch == '-' || ch == '+').next()?.trim();
    if core.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in core.split('.') {
        if part.is_empty() {
            return None;
        }
        parts.push(part.parse::<u64>().ok()?);
    }
    Some(parts)
}

fn is_newer(current: &str, latest: &str) -> bool {
    let (Some(current), Some(latest)) = (parse_version(current), parse_version(latest)) else {
        return false;
    };
    let len = current.len().max(latest.len());
    for index in 0..len {
        let current = current.get(index).copied().unwrap_or(0);
        let latest = latest.get(index).copied().unwrap_or(0);
        if latest != current {
            return latest > current;
        }
    }
    false
}

fn is_due(last_checked: u64, now: u64) -> bool {
    now.saturating_sub(last_checked) >= CHECK_INTERVAL_SECS
}

fn update_message(current: &str, latest: &str) -> String {
    format!("Update available: {latest} (current v{current})\nReinstall: {INSTALL_COMMAND}")
}

fn latest_release_url() -> String {
    std::env::var("WHISKER_UPDATE_CHECK_URL").unwrap_or_else(|_| GITHUB_LATEST.to_string())
}

async fn fetch_latest() -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .user_agent(concat!("whisker/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = client
        .get(latest_release_url())
        .send()
        .await?
        .error_for_status()?;
    let body: serde_json::Value = response.json().await?;
    body.get("tag_name")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .context("latest release is missing tag_name")
}

pub async fn check_for_update(home: &Path) -> Option<String> {
    let path = cache_path(home);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let mut cache = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<UpdateCache>(&raw).ok())
        .unwrap_or_default();
    if !is_due(cache.checked_at, now) {
        return None;
    }

    let current = env!("CARGO_PKG_VERSION");
    let latest = match fetch_latest().await {
        Ok(latest) => latest,
        Err(err) => {
            tracing::debug!("update check failed: {err}");
            return None;
        }
    };

    cache.checked_at = now;
    cache.latest = Some(latest.clone());
    let message =
        if is_newer(current, &latest) && cache.notified.as_deref() != Some(latest.as_str()) {
            cache.notified = Some(latest.clone());
            Some(update_message(current, &latest))
        } else {
            None
        };

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(raw) = serde_json::to_string_pretty(&cache) {
        let _ = std::fs::write(&path, raw);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_tags() {
        assert_eq!(parse_version("v0.6.1"), Some(vec![0, 6, 1]));
        assert_eq!(parse_version("0.6"), Some(vec![0, 6]));
        assert_eq!(parse_version("v1.2.3-rc.1"), Some(vec![1, 2, 3]));
        assert_eq!(parse_version("nightly"), None);
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.6.0", "v0.6.1"));
        assert!(is_newer("0.9.9", "v0.10.0"));
        assert!(!is_newer("0.6.1", "v0.6.1"));
        assert!(!is_newer("0.6.1", "v0.6.0"));
    }

    #[test]
    fn checks_at_most_once_per_day() {
        assert!(is_due(0, CHECK_INTERVAL_SECS));
        assert!(!is_due(100, 100 + CHECK_INTERVAL_SECS - 1));
    }

    #[test]
    fn reminder_names_versions_and_command() {
        let message = update_message("0.6.1", "v0.6.2");
        assert!(message.contains("v0.6.2"));
        assert!(message.contains("current v0.6.1"));
        assert!(message.contains("install.sh | bash"));
    }
}
