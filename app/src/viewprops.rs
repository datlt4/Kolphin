//! Per-folder view settings (~/.local/share/kolphin/view-props.json, never inside the folder) and app settings.
use crate::canvas::ViewMode;
use crate::model::{GroupBy, SortBy};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewProps {
    pub mode: ViewMode,
    pub icon_size: f64,
    pub list_size: f64,
    pub sort: SortBy,
    pub desc: bool,
    pub group: GroupBy,
    pub collapsed: Vec<String>,
}

impl Default for ViewProps {
    fn default() -> Self {
        ViewProps { mode: ViewMode::Icons, icon_size: 64.0, list_size: 22.0, sort: SortBy::Name, desc: false, group: GroupBy::None, collapsed: Vec::new() }
    }
}

thread_local! {
    static STORE: RefCell<Option<HashMap<PathBuf, ViewProps>>> = const { RefCell::new(None) };
}

fn file() -> PathBuf {
    gtk::glib::user_data_dir().join("kolphin").join("view-props.json")
}

fn with_store<T>(f: impl FnOnce(&mut HashMap<PathBuf, ViewProps>) -> T) -> T {
    STORE.with(|s| {
        let mut s = s.borrow_mut();
        let map = s.get_or_insert_with(|| {
            let map: HashMap<PathBuf, ViewProps> = std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            if settings()["props_by_real_path"].as_bool() == Some(true) {
                return map;
            }
            // Once: move entries saved under a symlinked path to the real folder (its own entry wins).
            let mut out: HashMap<PathBuf, ViewProps> = HashMap::new();
            for (k, v) in map {
                let real = key(&k);
                if real == k || !out.contains_key(&real) {
                    out.insert(real, v);
                }
            }
            if let Ok(json) = serde_json::to_vec(&out) {
                write_store(&json);
            }
            let mut v = settings();
            v["props_by_real_path"] = true.into();
            write_settings(&v);
            out
        });
        f(map)
    })
}

/// Settings follow the real folder: ~/Downloads → /mnt/data/Downloads share one entry (network shares as given).
fn key(dir: &Path) -> PathBuf {
    if crate::devices::is_remote(dir) {
        return dir.to_path_buf();
    }
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

pub fn load(dir: &Path) -> Option<ViewProps> {
    with_store(|m| m.get(&key(dir)).cloned())
}

pub fn save(dir: &Path, props: ViewProps) {
    let dir = &key(dir);
    let json = with_store(|m| {
        if m.get(dir) == Some(&props) {
            return None;
        }
        m.insert(dir.to_path_buf(), props);
        serde_json::to_vec(m).ok()
    });
    if let Some(json) = json {
        write_store(&json);
    }
}

fn write_secure(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = path.with_extension(format!("{}.{}.{}.tmp", std::process::id(), nanos, N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    // Mode 0600 at creation: no 0644 window where others could read passwords/servers.
    // No fsync: rename is enough for config files, and this runs on every view change.
    let wrote = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(bytes));
    if wrote.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    let _ = std::fs::rename(&tmp, path);
    // Rename keeps the tmp mode; enforce on the final file too (existing file may be 644).
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

fn write_store(json: &[u8]) {
    write_secure(&file(), json);
}

fn settings_file() -> PathBuf {
    gtk::glib::user_data_dir().join("kolphin").join("settings.json")
}

fn write_settings(v: &serde_json::Value) {
    write_secure(&settings_file(), v.to_string().as_bytes());
}

fn settings() -> serde_json::Value {
    std::fs::read(settings_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(serde_json::json!({}))
}

pub fn sidebar_right() -> bool {
    settings()["sidebar_right"].as_bool().unwrap_or(false)
}

pub fn set_sidebar_right(right: bool) {
    let mut s = settings();
    s["sidebar_right"] = right.into();
    write_settings(&s);
}

thread_local! {
    static ICONS: RefCell<Option<HashMap<PathBuf, String>>> = const { RefCell::new(None) };
}

fn icons_file() -> PathBuf {
    gtk::glib::user_data_dir().join("kolphin").join("custom-icons.json")
}

fn with_icons<T>(f: impl FnOnce(&mut HashMap<PathBuf, String>) -> T) -> T {
    ICONS.with(|s| {
        let mut s = s.borrow_mut();
        let map = s.get_or_insert_with(|| {
            let map: HashMap<PathBuf, String> = std::fs::read(icons_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            if settings()["icons_by_real_path"].as_bool() == Some(true) {
                return map;
            }
            // Once: move entries saved under a symlinked path to the real file (its own entry wins).
            let mut out: HashMap<PathBuf, String> = HashMap::new();
            for (k, v) in map {
                let real = icon_key(&k);
                if real == k || !out.contains_key(&real) {
                    out.insert(real, v);
                }
            }
            if let Ok(json) = serde_json::to_vec(&out) {
                write_secure(&icons_file(), &json);
            }
            let mut v = settings();
            v["icons_by_real_path"] = true.into();
            write_settings(&v);
            out
        });
        f(map)
    })
}

/// Custom icons follow the real file: ~/Downloads/x and /mnt/data/Downloads/x share one entry
/// (network shares as given).
fn icon_key(path: &Path) -> PathBuf {
    if crate::devices::is_remote(path) {
        return path.to_path_buf();
    }
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Material icon chosen by the user for this file or folder.
pub fn custom_icon(path: &Path) -> Option<String> {
    with_icons(|m| if m.is_empty() { None } else { m.get(&icon_key(path)).cloned() })
}

pub fn has_custom_icons() -> bool {
    with_icons(|m| !m.is_empty())
}

pub fn set_custom_icon(paths: &[PathBuf], icon: Option<&str>) {
    let json = with_icons(|m| {
        for p in paths {
            let real = icon_key(p);
            match icon {
                Some(i) => m.insert(real, i.to_string()),
                None => m.remove(&real),
            };
        }
        serde_json::to_vec(m).ok()
    });
    if let Some(json) = json {
        write_secure(&icons_file(), &json);
    }
}

/// Tabs of the last closed window: each tab's folders (two in split view), and the selected tab.
#[derive(Serialize, Deserialize, Default)]
pub struct Session {
    pub tabs: Vec<Vec<PathBuf>>,
    pub selected: usize,
    /// Hidden files were shown.
    #[serde(default)]
    pub hidden: bool,
}

pub fn save_session(s: &Session) {
    // Dev and benchmark runs share these settings with the installed app: never replace its session.
    if cfg!(debug_assertions) || std::env::var_os("KOLPHIN_BENCH").is_some() {
        return;
    }
    let mut v = settings();
    v["session"] = serde_json::to_value(s).unwrap_or_default();
    write_settings(&v);
}

/// The saved session, keeping only folders that still exist.
pub fn load_session() -> Option<Session> {
    let mut s: Session = serde_json::from_value(settings()["session"].clone()).ok()?;
    s.tabs = s.tabs.into_iter().map(|t| t.into_iter().filter(|p| p.is_dir()).collect::<Vec<_>>()).filter(|t| !t.is_empty()).collect();
    (!s.tabs.is_empty()).then_some(s)
}

pub fn check_updates_on_start() -> bool {
    settings()["check_updates"].as_bool().unwrap_or(true)
}

pub fn set_check_updates_on_start(on: bool) {
    let mut v = settings();
    v["check_updates"] = on.into();
    write_settings(&v);
}

/// Read copies back and compare xxHashes (off by default: it reads every file twice).
pub fn verify_copies() -> bool {
    settings()["verify_copies"].as_bool().unwrap_or(false)
}

pub fn set_verify_copies(on: bool) {
    let mut v = settings();
    v["verify_copies"] = on.into();
    write_settings(&v);
}

/// Servers connected to before, newest first (passwords are never stored: legacy entries are stripped on read).
pub fn recent_servers() -> Vec<String> {
    let list: Vec<String> = settings()["servers"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
    let clean: Vec<String> = list.iter().map(|u| crate::devices::strip_credentials(&crate::devices::normalize(u))).collect();
    if clean != list {
        let mut s = settings();
        s["servers"] = clean.clone().into();
        write_settings(&s);
    }
    clean
}

pub fn forget_server(uri: &str) {
    let list: Vec<String> = recent_servers().into_iter().filter(|u| u != uri).collect();
    let mut s = settings();
    s["servers"] = list.into();
    write_settings(&s);
}

pub fn add_recent_server(uri: &str) {
    // Never persist passwords: `smb://user:pass@h/s` -> `smb://user@h/s`.
    let uri = crate::devices::strip_credentials(&crate::devices::normalize(uri));
    let mut list = recent_servers();
    list.retain(|u| u != &uri);
    list.insert(0, uri);
    list.truncate(8);
    let mut s = settings();
    s["servers"] = list.into();
    write_settings(&s);
}

#[cfg(test)]
mod tests {
    #[test]
    fn symlinked_folder_shares_settings() {
        let d = std::env::temp_dir().join(format!("kolphin-vp-{}", std::process::id()));
        let real = d.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, d.join("link")).unwrap();
        assert_eq!(super::key(&d.join("link")), super::key(&real));
        std::fs::write(real.join("f.txt"), "x").unwrap();
        assert_eq!(super::icon_key(&d.join("link/f.txt")), super::icon_key(&real.join("f.txt")));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
