//! Bookmarked folders, stored in GTK's bookmarks file (shared with Nautilus and the file chooser).
use gtk::{gio, glib, prelude::*};
use std::path::{Path, PathBuf};

fn file() -> PathBuf {
    glib::user_config_dir().join("gtk-3.0/bookmarks")
}

/// (folder, label) for each local bookmark.
pub fn load() -> Vec<(PathBuf, String)> {
    let text = std::fs::read_to_string(file()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let (uri, label) = l.split_once(' ').map_or((l, None), |(u, n)| (u, Some(n)));
            let path = gio::File::for_uri(uri).path()?;
            let label = label.map(String::from).unwrap_or_else(|| path.file_name().map_or("/".into(), |n| n.to_string_lossy().into_owned()));
            Some((path, label))
        })
        .collect()
}

fn save(list: &[String]) {
    let p = file();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::write(p, list.iter().map(|l| format!("{l}\n")).collect::<String>());
}

fn lines() -> Vec<String> {
    std::fs::read_to_string(file()).unwrap_or_default().lines().filter(|l| !l.trim().is_empty()).map(String::from).collect()
}

/// Add folders not bookmarked yet; returns how many were added.
pub fn add(dirs: &[PathBuf]) -> usize {
    let mut list = lines();
    let mut n = 0;
    for d in dirs.iter().filter(|d| d.is_dir()) {
        let uri = gio::File::for_path(d).uri().to_string();
        if !list.iter().any(|l| l.split(' ').next() == Some(uri.as_str())) {
            list.push(uri);
            n += 1;
        }
    }
    save(&list);
    n
}

pub fn remove(dir: &Path) {
    let uri = gio::File::for_path(dir).uri().to_string();
    let list: Vec<String> = lines().into_iter().filter(|l| l.split(' ').next() != Some(uri.as_str())).collect();
    save(&list);
}
