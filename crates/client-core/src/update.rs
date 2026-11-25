//! GitHub release lookup for the in-app update check.

pub const REPO: &str = "HMHamster88/rust-chat";
const API: &str = "https://api.github.com/repos";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub notes: String,
}

/// Returns the latest release when it is newer than `current`, or `None`.
pub fn check(current: &str) -> Result<Option<Release>, String> {
    let url = format!("{API}/{REPO}/releases/latest");
    let mut response = match ureq::get(&url)
        .header(
            "User-Agent",
            concat!("rust-chat/", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(error) => return Err(format!("update check failed: {error}")),
    };
    let value: serde_json::Value = response
        .body_mut()
        .read_json()
        .map_err(|error| format!("update response was not JSON: {error}"))?;
    let tag = value
        .get("tag_name")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim_start_matches('v')
        .trim();
    if tag.is_empty() {
        return Err("the release has no tag".to_owned());
    }
    if !is_newer(tag, current) {
        return Ok(None);
    }
    Ok(Some(Release {
        version: tag.to_owned(),
        url: value
            .get("html_url")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned(),
        notes: value
            .get("body")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .chars()
            .take(4000)
            .collect(),
    }))
}

/// Numeric comparison of `major.minor.patch`; pre-release suffixes are ignored.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_compare_numerically() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v1.0.10", "1.0.9"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("not-a-version", "0.1.0"));
        assert!(is_newer("0.2.0-rc.1", "0.1.0"));
    }
}
