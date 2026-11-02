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
        let map = s.get_or_insert_with(|| std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
        f(map)
    })
}

pub fn load(dir: &Path) -> Option<ViewProps> {
    with_store(|m| m.get(dir).cloned())
}

pub fn save(dir: &Path, props: ViewProps) {
    let json = with_store(|m| {
        if m.get(dir) == Some(&props) {
            return None;
        }
        m.insert(dir.to_path_buf(), props);
        serde_json::to_vec(m).ok()
    });
    if let Some(json) = json {
        let path = file();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

fn settings_file() -> PathBuf {
    gtk::glib::user_data_dir().join("kolphin").join("settings.json")
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
    let path = settings_file();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(path, s.to_string());
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
        let map = s.get_or_insert_with(|| std::fs::read(icons_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
        f(map)
    })
}

/// Material icon chosen by the user for this file or folder.
pub fn custom_icon(path: &Path) -> Option<String> {
    with_icons(|m| if m.is_empty() { None } else { m.get(path).cloned() })
}

pub fn has_custom_icons() -> bool {
    with_icons(|m| !m.is_empty())
}

pub fn set_custom_icon(paths: &[PathBuf], icon: Option<&str>) {
    let json = with_icons(|m| {
        for p in paths {
            match icon {
                Some(i) => m.insert(p.clone(), i.to_string()),
                None => m.remove(p),
            };
        }
        serde_json::to_vec(m).ok()
    });
    if let Some(json) = json {
        let path = icons_file();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let _ = std::fs::write(path, json);
    }
}

/// Servers connected to before, newest first.
pub fn recent_servers() -> Vec<String> {
    settings()["servers"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect()
}

pub fn forget_server(uri: &str) {
    let list: Vec<String> = recent_servers().into_iter().filter(|u| u != uri).collect();
    let mut s = settings();
    s["servers"] = list.into();
    let _ = std::fs::write(settings_file(), s.to_string());
}

pub fn add_recent_server(uri: &str) {
    let mut list = recent_servers();
    list.retain(|u| u != uri);
    list.insert(0, uri.to_string());
    list.truncate(8);
    let mut s = settings();
    s["servers"] = list.into();
    let path = settings_file();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(path, s.to_string());
}
