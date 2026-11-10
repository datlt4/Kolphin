//! Install / self-update from GitHub Releases: download the .deb for this CPU and install it with pkexec, or, when
//! running as an AppImage, replace the AppImage file; then restart.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Redirects to /releases/tag/vX.Y.Z; unlike the REST API it has no 60-requests-an-hour limit.
const LATEST: &str = "https://github.com/datlt4/Kolphin/releases/latest";
const DOWNLOAD: &str = "https://github.com/datlt4/Kolphin/releases/download";
const INSTALLED_EXE: &str = "/usr/bin/kolphin";

pub struct Release {
    pub version: String,
    /// The .deb, or the AppImage when running as one.
    pub url: String,
    /// Kolphin is not installed system-wide yet (running from a tarball or a dev build).
    pub fresh_install: bool,
}

/// The newest release if it should be offered: not installed yet, or newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().max_redirects(0).build().into();
    let resp = agent.get(LATEST).header("User-Agent", "Kolphin").call().map_err(|e| e.to_string())?;
    let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("");
    let Some(version) = version_from_location(location) else { return Ok(None) }; // no release published yet
    let (url, fresh_install) = match appimage() {
        Some(_) => (format!("{DOWNLOAD}/v{version}/kolphin_{version}_{}.AppImage", std::env::consts::ARCH), false),
        None => (format!("{DOWNLOAD}/v{version}/kolphin_{version}_{}.deb", deb_arch()), !Path::new(INSTALLED_EXE).exists()),
    };
    Ok((fresh_install || newer(&version, env!("CARGO_PKG_VERSION"))).then_some(Release { version, url, fresh_install }))
}

/// The AppImage file this process runs from, if any.
fn appimage() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file())
}

fn version_from_location(location: &str) -> Option<String> {
    // Only accept same-host /releases/tag/vX.Y.Z redirects; reject off-host or query URLs.
    let v = location.rsplit_once("/tag/v")?.1;
    let v = v.split(['?', '#', '/']).next().unwrap_or("");
    if v.is_empty() || v.len() > 32 || !v.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    if !location.starts_with("https://github.com/datlt4/Kolphin/releases/tag/v") {
        return None;
    }
    Some(v.to_string())
}

const MAX_DOWNLOAD: u64 = 200 << 20;

/// Download to the cache dir, reporting (bytes so far, total).
pub fn download(url: &str, progress: impl Fn(u64, u64)) -> Result<PathBuf, String> {
    // Only our own release assets over HTTPS.
    let ok_prefix = url.starts_with(&format!("{DOWNLOAD}/v")) && (url.ends_with(".deb") || url.ends_with(".AppImage"));
    if !ok_prefix {
        return Err("Refusing to download from an unexpected URL".into());
    }
    let dir = gtk::glib::user_cache_dir().join("kolphin");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(url.rsplit('/').next().unwrap_or("kolphin.deb"));
    let mut resp = ureq::get(url).header("User-Agent", "Kolphin").call().map_err(|e| e.to_string())?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()).unwrap_or(0);
    if total > MAX_DOWNLOAD {
        return Err("Release file is unexpectedly large".into());
    }
    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let (mut buf, mut done) = (vec![0u8; 256 * 1024], 0u64);
    // Magic bytes: .deb is `!<arch>`, AppImage/ELF is `\x7fELF`.
    let mut head: Vec<u8> = Vec::with_capacity(8);
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > MAX_DOWNLOAD {
            let _ = std::fs::remove_file(&path);
            return Err("Release file is unexpectedly large".into());
        }
        if head.len() < 8 {
            head.extend_from_slice(&buf[..n.min(8 - head.len())]);
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        progress(done, total);
    }
    if done < 1024 {
        let _ = std::fs::remove_file(&path);
        return Err("Release file is too small".into());
    }
    let is_deb = path.extension().is_some_and(|e| e == "deb");
    let magic_ok = if is_deb { head.starts_with(b"!<arch>") } else { head.starts_with(b"\x7fELF") };
    if !magic_ok {
        let _ = std::fs::remove_file(&path);
        return Err("Release file has the wrong format".into());
    }
    if let Err(e) = verify_sha256(&path, url) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// Manifest holding `<sha256>  <filename>` for this asset, if CI published one.
/// `.../kolphin_1.2.3_amd64.deb` -> `.../kolphin_1.2.3_amd64.sha256`.
fn manifest_url_for(url: &str) -> Option<String> {
    let base = url.strip_suffix(".deb").or_else(|| url.strip_suffix(".AppImage"))?;
    Some(format!("{base}.sha256"))
}

/// Expected hex digest for `filename` inside a `sha256sum` manifest.
fn parse_sha_manifest(text: &str, filename: &str) -> Option<String> {
    text.lines().filter_map(|l| {
        let (hash, name) = l.split_once(char::is_whitespace)?;
        let name = name.trim_start_matches([' ', '*']).rsplit('/').next().unwrap_or("");
        (name == filename).then(|| hash.trim().to_lowercase())
    }).find(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Check the downloaded file against its CI `.sha256` manifest.
/// The manifest is same-source with the file, so this only guards corruption,
/// not a compromised release: old releases have no manifest and skip this.
fn verify_sha256(path: &Path, url: &str) -> Result<(), String> {
    let Some(manifest_url) = manifest_url_for(url) else { return Ok(()) };
    let filename = url.rsplit('/').next().unwrap_or("");
    let resp = ureq::get(&manifest_url).header("User-Agent", "Kolphin").call();
    let mut resp = match resp {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let mut text = String::new();
    // Bound the read: a manifest is a few hundred bytes.
    if resp.body_mut().as_reader().take(64 * 1024).read_to_string(&mut text).is_err() {
        return Ok(());
    }
    let Some(want) = parse_sha_manifest(&text, filename) else {
        return Err("Release checksum manifest has no entry for this file".into());
    };
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    (got == want).then_some(()).ok_or_else(|| "Release file failed SHA-256 check".into())
}

/// Install the download: a new AppImage replaces the running one; a .deb goes in as root (polkit asks for the
/// password) and apt pulls missing dependencies.
pub fn install(deb: &Path) -> Result<(), String> {
    // Re-check magic at install time (TOCTOU guard if the cache file changed).
    let mut head = [0u8; 8];
    let magic_ok = std::fs::File::open(deb)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head).map(|_| head))
        .map(|h| {
            if deb.extension().is_some_and(|e| e == "AppImage") {
                h.starts_with(b"\x7fELF")
            } else {
                h.starts_with(b"!<arch>")
            }
        })
        .unwrap_or(false);
    if !magic_ok {
        return Err("Refusing to install: wrong file format".into());
    }
    if let Some(target) = appimage().filter(|_| deb.extension().is_some_and(|e| e == "AppImage")) {
        // Next to the target first, so the final rename is atomic.
        let tmp = target.with_extension("AppImage.new");
        let r = std::fs::copy(deb, &tmp)
            .and_then(|_| std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o755)))
            .and_then(|_| std::fs::rename(&tmp, &target));
        let _ = std::fs::remove_file(deb);
        let _ = std::fs::remove_file(&tmp);
        return r.map_err(|e| format!("{}: {e}", target.display()));
    }
    let out = Command::new("pkexec")
        .args(["apt-get", "install", "-y"])
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
    let exe = appimage().map_or(INSTALLED_EXE.to_string(), |p| p.display().to_string());
    let script = format!("while kill -0 {} 2>/dev/null; do sleep 0.1; done; exec \"$0\"", std::process::id());
    let _ = Command::new("sh").args(["-c", &script, &exe]).spawn();
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
        // Off-host or query-stripped redirects are rejected.
        assert_eq!(super::version_from_location("https://evil.com/releases/tag/v9.9.9"), None);
        assert_eq!(super::version_from_location("https://github.com/datlt4/Kolphin/releases/tag/v1.0?evil=1").as_deref(), Some("1.0"));
    }

    #[test]
    fn version_order() {
        assert!(super::newer("0.1.1", "0.1.0"));
        assert!(super::newer("0.10.0", "0.9.9"));
        assert!(!super::newer("0.1.0", "0.1.0"));
        assert!(!super::newer("0.9.9", "0.10.0"));
    }

    #[test]
    fn sha_manifest() {
        let base = "https://github.com/datlt4/Kolphin/releases/download/v0.16.6";
        let deb = format!("{base}/kolphin_0.16.6_amd64.deb");
        let deb_sha = format!("{base}/kolphin_0.16.6_amd64.sha256");
        assert_eq!(super::manifest_url_for(&deb).as_deref(), Some(deb_sha.as_str()));
        let img = format!("{base}/kolphin_0.16.6_x86_64.AppImage");
        let img_sha = format!("{base}/kolphin_0.16.6_x86_64.sha256");
        assert_eq!(super::manifest_url_for(&img).as_deref(), Some(img_sha.as_str()));
        assert!(super::manifest_url_for(&format!("{base}/kolphin.tar.gz")).is_none());
        let text = "abc123  kolphin_0.16.6_amd64.deb\n";
        assert!(super::parse_sha_manifest(text, "kolphin_0.16.6_amd64.deb").is_none());
        let good = format!("{}  kolphin_0.16.6_amd64.deb\n", "a".repeat(64));
        assert_eq!(super::parse_sha_manifest(&good, "kolphin_0.16.6_amd64.deb").as_deref(), Some("a".repeat(64).as_str()));
    }
}
