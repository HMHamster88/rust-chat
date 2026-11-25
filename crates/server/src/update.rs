//! Self-update from GitHub releases (Linux server).

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use rust_chat_crypto::{sha256, to_hex};
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

pub const REPO: &str = "HMHamster88/rust-chat";
const API: &str = "https://api.github.com/repos";
const ASSET_SUFFIX: &str = "-linux-x86_64.tar.gz";
pub const UNIT: &str = "rust-chat-server";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub tarball_url: String,
    /// GitHub's own `sha256:` digest for the asset, when the API reports one.
    pub tarball_digest: Option<String>,
    pub html_url: String,
}

/// Latest stable release, or a specific tag when `tag` is given.
pub fn release(tag: Option<&str>) -> Result<Release> {
    let url = match tag {
        Some(tag) => format!("{API}/{REPO}/releases/tags/{tag}"),
        None => format!("{API}/{REPO}/releases/latest"),
    };
    let mut response = ureq::get(&url)
        .header(
            "User-Agent",
            concat!("rust-chat-server/", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("cannot query {url}"))?;
    let value: Value = response
        .body_mut()
        .read_json()
        .context("release response was not JSON")?;
    release_from_json(&value)
}

fn release_from_json(value: &Value) -> Result<Release> {
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .context("release has no tag")?
        .to_owned();
    let version = tag.trim_start_matches('v').to_owned();
    let html_url = value
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let asset = value
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| {
            assets.iter().find(|asset| {
                asset
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.ends_with(ASSET_SUFFIX))
            })
        })
        .with_context(|| format!("release {tag} has no {ASSET_SUFFIX} asset"))?;
    let tarball_url = asset
        .get("browser_download_url")
        .and_then(Value::as_str)
        .context("the release asset has no download URL")?
        .to_owned();
    let tarball_digest = asset
        .get("digest")
        .and_then(Value::as_str)
        .filter(|digest| !digest.is_empty())
        .map(str::to_owned);
    Ok(Release {
        tag,
        version,
        tarball_url,
        tarball_digest,
        html_url,
    })
}

/// Numeric `major.minor.patch` comparison; pre-release suffixes are ignored.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut parts = core.split('.');
    Some((
        parts.next()?.parse().ok()?,
        parts.next().unwrap_or("0").parse().ok()?,
        parts.next().unwrap_or("0").parse().ok()?,
    ))
}

fn download(url: &str) -> Result<Vec<u8>> {
    let mut response = ureq::get(url)
        .header(
            "User-Agent",
            concat!("rust-chat-server/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .with_context(|| format!("cannot download {url}"))?;
    response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .context("cannot read the release archive")
}

/// Replaces the running binary with the release's `rust-chat-server`.
pub fn install(release: &Release) -> Result<PathBuf> {
    if std::env::consts::OS != "linux" {
        bail!("self-update is only supported on Linux");
    }
    if !is_root() {
        bail!("run this update with sudo (it replaces the installed binary)");
    }
    let archive = download(&release.tarball_url)?;
    verify_download(&archive, release.tarball_digest.as_deref())?;
    let (binary, sums) = extract(&archive)?;
    verify_binary(&binary, &sums)?;

    let exe = std::env::current_exe().context("cannot locate the running binary")?;
    let dir = exe.parent().context("the binary has no parent directory")?;
    let staged = dir.join(format!(".rust-chat-server.new.{}", std::process::id()));
    fs::write(&staged, &binary).with_context(|| format!("cannot write {}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
    }

    let version = Command::new(&staged)
        .arg("--version")
        .output()
        .context("cannot run the downloaded binary")?;
    let version_text = String::from_utf8_lossy(&version.stdout);
    if !version.status.success() || !version_text.contains(&release.version) {
        let _ = fs::remove_file(&staged);
        bail!(
            "downloaded binary reports `{}`, expected {}",
            version_text.trim(),
            release.version
        );
    }

    let previous = dir.join("rust-chat-server.previous");
    if exe.exists() {
        fs::copy(&exe, &previous).with_context(|| format!("cannot back up {}", exe.display()))?;
    }
    fs::rename(&staged, &exe).with_context(|| format!("cannot install {}", exe.display()))?;
    Ok(exe)
}

/// Checks the archive against GitHub's `sha256:` asset digest when the API supplied one.
pub fn verify_download(archive: &[u8], digest: Option<&str>) -> Result<()> {
    let Some(digest) = digest else {
        return Ok(());
    };
    let expected = digest
        .strip_prefix("sha256:")
        .with_context(|| format!("unsupported release digest `{digest}`"))?;
    let actual = to_hex(&sha256(archive));
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("download checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

/// Extracts `rust-chat-server` and `SHA256SUMS.txt` from the release tarball.
fn extract(archive: &[u8]) -> Result<(Vec<u8>, String)> {
    let cursor = std::io::Cursor::new(archive);
    let mut tar = tar::Archive::new(GzDecoder::new(cursor));
    let mut binary = None;
    let mut sums = None;
    for entry in tar.entries().context("invalid release archive")? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if name == "rust-chat-server" {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            binary = Some(data);
        } else if name == "SHA256SUMS.txt" {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            sums = Some(text);
        }
    }
    Ok((
        binary.context("the release archive has no rust-chat-server")?,
        sums.context("the release archive has no SHA256SUMS.txt")?,
    ))
}

fn verify_binary(binary: &[u8], sums: &str) -> Result<()> {
    let expected = sums
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?;
            (name == "rust-chat-server").then_some(hash)
        })
        .context("SHA256SUMS.txt has no rust-chat-server entry")?;
    let actual = to_hex(&sha256(binary));
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

/// Swaps the `.previous` binary back into place.
pub fn rollback() -> Result<PathBuf> {
    if std::env::consts::OS != "linux" {
        bail!("self-update is only supported on Linux");
    }
    if !is_root() {
        bail!("run this rollback with sudo (it replaces the installed binary)");
    }
    let exe = std::env::current_exe().context("cannot locate the running binary")?;
    let dir = exe.parent().context("the binary has no parent directory")?;
    let previous = dir.join("rust-chat-server.previous");
    if !previous.exists() {
        bail!("no previous binary at {}", previous.display());
    }
    let staged = dir.join(format!(".rust-chat-server.rollback.{}", std::process::id()));
    fs::copy(&previous, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
    }
    fs::copy(&exe, &previous)?;
    fs::rename(&staged, &exe)?;
    Ok(exe)
}

/// Restarts the unit when it is active and waits for the listener.
///
/// Returns `false` when the unit is not active (manually installed binary), in which case there is
/// nothing to restart.
pub fn restart_and_wait(port: u16) -> Result<bool> {
    let active = Command::new("systemctl")
        .args(["is-active", "--quiet", UNIT])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !active {
        return Ok(false);
    }
    let status = Command::new("systemctl")
        .args(["restart", UNIT])
        .status()
        .context("cannot run systemctl restart")?;
    if !status.success() {
        bail!("systemctl restart {UNIT} failed");
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    for _ in 0..30 {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("the server did not start listening on 127.0.0.1:{port}");
}

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "0")
        .unwrap_or(false)
}

/// Asks for confirmation on stdin unless `yes` is true.
pub fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    use std::io::Write;
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn release_json_picks_the_linux_asset() {
        let release = release_from_json(&json!({
            "tag_name": "v0.2.0",
            "html_url": "https://example.test/r",
            "assets": [
                {"name": "rust-chat-v0.2.0-windows-x64.zip", "browser_download_url": "https://example.test/w"},
                {"name": "rust-chat-v0.2.0-linux-x86_64.tar.gz", "browser_download_url": "https://example.test/l",
                 "digest": "sha256:abc"}
            ]
        }))
        .unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.tarball_url, "https://example.test/l");
        assert_eq!(release.tarball_digest.as_deref(), Some("sha256:abc"));
    }

    #[test]
    fn download_digest_is_optional_and_verified() {
        let data = b"archive";
        let digest = format!("sha256:{}", to_hex(&sha256(data)));
        assert!(verify_download(data, Some(&digest)).is_ok());
        assert!(verify_download(data, None).is_ok());
        assert!(verify_download(data, Some(&format!("sha256:{}", "0".repeat(64)))).is_err());
        assert!(verify_download(data, Some("md5:abc")).is_err());
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.10", "1.0.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("bad", "0.1.0"));
    }

    #[test]
    fn checksum_verification_rejects_a_mismatch() {
        let data = b"binary";
        let good = format!("{}  rust-chat-server\n", to_hex(&sha256(data)));
        assert!(verify_binary(data, &good).is_ok());
        let bad = format!("{}  rust-chat-server\n", "0".repeat(64));
        assert!(verify_binary(data, &bad).is_err());
    }
}
