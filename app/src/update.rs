//! Install / self-update from GitHub Releases: download the .deb for this CPU, install it with pkexec, restart.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Redirects to /releases/tag/vX.Y.Z; unlike the REST API it has no 60-requests-an-hour limit.
const LATEST: &str = "https://github.com/datlt4/Kolphin/releases/latest";
const DOWNLOAD: &str = "https://github.com/datlt4/Kolphin/releases/download";
const INSTALLED_EXE: &str = "/usr/bin/kolphin";

pub struct Release {
    pub version: String,
    pub deb_url: String,
    /// Kolphin is not installed system-wide yet (running from a tarball or a dev build).
    pub fresh_install: bool,
}

/// The newest release if it should be offered: not installed yet, or newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().max_redirects(0).build().into();
    let resp = agent.get(LATEST).header("User-Agent", "Kolphin").call().map_err(|e| e.to_string())?;
    let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("");
    let Some(version) = version_from_location(location) else { return Ok(None) }; // no release published yet
    let deb_url = format!("{DOWNLOAD}/v{version}/kolphin_{version}_{}.deb", deb_arch());
    let fresh_install = !Path::new(INSTALLED_EXE).exists();
    Ok((fresh_install || newer(&version, env!("CARGO_PKG_VERSION"))).then_some(Release { version, deb_url, fresh_install }))
}

fn version_from_location(location: &str) -> Option<String> {
    let v = location.rsplit_once("/tag/v")?.1;
    (!v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| v.to_string())
}

/// Download to the cache dir, reporting (bytes so far, total).
pub fn download(url: &str, progress: impl Fn(u64, u64)) -> Result<PathBuf, String> {
    let dir = gtk::glib::user_cache_dir().join("kolphin");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(url.rsplit('/').next().unwrap_or("kolphin.deb"));
    let mut resp = ureq::get(url).header("User-Agent", "Kolphin").call().map_err(|e| e.to_string())?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()).unwrap_or(0);
    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let (mut buf, mut done) = (vec![0u8; 256 * 1024], 0u64);
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        progress(done, total);
    }
    Ok(path)
}

/// Install the .deb as root (polkit asks for the password); apt pulls missing dependencies.
pub fn install(deb: &Path) -> Result<(), String> {
    let out = Command::new("pkexec")
        .args(["apt-get", "install", "-y", "--allow-downgrades"])
        .arg(deb)
        .output()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(deb);
    match out.status.code() {
        Some(0) => Ok(()),
        Some(126 | 127) => Err("Authentication cancelled".into()),
        _ => Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("apt-get failed").to_string()),
    }
}

/// Start the installed Kolphin once this process has exited (GApplication would otherwise hand off to us).
pub fn restart_later() {
    let script = format!("while kill -0 {} 2>/dev/null; do sleep 0.1; done; exec {INSTALLED_EXE}", std::process::id());
    let _ = Command::new("sh").args(["-c", &script]).spawn();
}

fn deb_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// `a` is a newer x.y.z version than `b`.
fn newer(a: &str, b: &str) -> bool {
    let v = |s: &str| s.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    v(a) > v(b)
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_from_redirect() {
        assert_eq!(super::version_from_location("https://github.com/datlt4/Kolphin/releases/tag/v0.6.0").as_deref(), Some("0.6.0"));
        assert_eq!(super::version_from_location("https://github.com/datlt4/Kolphin/releases"), None);
    }

    #[test]
    fn version_order() {
        assert!(super::newer("0.1.1", "0.1.0"));
        assert!(super::newer("0.10.0", "0.9.9"));
        assert!(!super::newer("0.1.0", "0.1.0"));
        assert!(!super::newer("0.9.9", "0.10.0"));
    }
}
