//! Directory entries, natural sort keys and the background directory scanner.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const UNKNOWN_SIZE: u64 = u64::MAX;
pub const UNKNOWN_TIME: i64 = i64::MIN;

pub struct Entry {
    pub name: Box<OsStr>,
    pub key: Box<[u8]>,
    pub is_dir: bool,
    pub deleted: bool,
    pub size: u64,
    pub mtime: i64,
    pub symlink: bool,
    /// Hard links to this file (1 = none besides itself).
    pub nlink: u32,
    /// Tag ids from `user.kolphin.tags` (symlinks show their target's too).
    pub tags: Vec<String>,
    /// Image size: None = not read yet, Some(None) = not an image or unreadable.
    pub dims: Option<Option<(u32, u32)>>,
}

impl Entry {
    pub fn new(name: OsString, is_dir: bool) -> Self {
        let key = sort_key(&name.to_string_lossy(), is_dir);
        Entry { name: name.into_boxed_os_str(), key, is_dir, deleted: false, size: UNKNOWN_SIZE, mtime: UNKNOWN_TIME, symlink: false, nlink: 1, dims: None, tags: Vec::new() }
    }

    pub fn hidden(&self) -> bool {
        self.name.as_bytes().first() == Some(&b'.')
    }
}

/// Byte key giving folders first, then case-insensitive natural order (`img_2` < `img_10`).
pub fn sort_key(name: &str, is_dir: bool) -> Box<[u8]> {
    let lower = name.to_lowercase();
    let b = lower.as_bytes();
    let mut k = Vec::with_capacity(b.len() + name.len() + 8);
    k.push(if is_dir { 0 } else { 1 });
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let run = &b[start..i];
            let digits = run.iter().position(|&c| c != b'0').map_or(&run[run.len() - 1..], |p| &run[p..]);
            // A shorter number (after leading zeros) is smaller: compare length first, then digits.
            k.push(b'0');
            k.push(digits.len().min(255) as u8);
            k.extend_from_slice(digits);
        } else {
            k.push(b[i]);
            i += 1;
        }
    }
    k.push(0);
    k.extend_from_slice(name.as_bytes());
    k.into_boxed_slice()
}

pub enum Msg {
    /// New entries, sorted by key; they get the next indices in arrival order.
    Entries(Vec<Entry>),
    /// Stats for the entries at `start..`.
    Stats { start: usize, stats: Vec<Stat> },
    Done,
    Error(String),
}

/// List `dir`: names first in growing batches, then `stat` every entry. Stops when the receiver is dropped.
pub fn scan(dir: PathBuf, tx: async_channel::Sender<Msg>) {
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => {
            let _ = tx.send_blocking(Msg::Error(e.to_string()));
            return;
        }
    };
    let mut names: Vec<OsString> = Vec::new();
    let mut batch = Vec::new();
    let (mut limit, mut last) = (256, Instant::now());
    let flush = |batch: &mut Vec<Entry>, names: &mut Vec<OsString>| {
        batch.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        names.extend(batch.iter().map(|e| e.name.to_os_string()));
        tx.send_blocking(Msg::Entries(std::mem::take(batch))).is_ok()
    };
    for de in rd.flatten() {
        let ft = de.file_type().ok();
        let symlink = ft.is_some_and(|t| t.is_symlink());
        let is_dir = if symlink { de.path().is_dir() } else { ft.is_some_and(|t| t.is_dir()) };
        let mut e = Entry::new(de.file_name(), is_dir);
        e.symlink = symlink;
        batch.push(e);
        if batch.len() >= limit || last.elapsed() > Duration::from_millis(50) {
            if !flush(&mut batch, &mut names) {
                return;
            }
            limit = (limit * 2).min(65536);
            last = Instant::now();
        }
    }
    if !batch.is_empty() && !flush(&mut batch, &mut names) {
        return;
    }

    let (mut stats, mut start) = (Vec::with_capacity(4096), 0);
    last = Instant::now();
    for (i, name) in names.iter().enumerate() {
        stats.push(stat(&dir.join(name)));
        if stats.len() >= 4096 || last.elapsed() > Duration::from_millis(100) {
            if tx.send_blocking(Msg::Stats { start, stats: std::mem::take(&mut stats) }).is_err() {
                return;
            }
            start = i + 1;
            last = Instant::now();
        }
    }
    if !stats.is_empty() {
        let _ = tx.send_blocking(Msg::Stats { start, stats });
    }
    let _ = tx.send_blocking(Msg::Done);
}

#[derive(Clone, Copy)]
pub struct Stat {
    pub size: u64,
    pub mtime: i64,
    pub nlink: u32,
}

/// Size, mtime and link count following symlinks; folders have no size.
pub fn stat(path: &std::path::Path) -> Stat {
    match std::fs::metadata(path).or_else(|_| std::fs::symlink_metadata(path)) {
        Ok(m) => Stat { size: if m.is_dir() { UNKNOWN_SIZE } else { m.len() }, mtime: m.mtime(), nlink: if m.is_dir() { 1 } else { m.nlink() as u32 } },
        Err(_) => Stat { size: UNKNOWN_SIZE, mtime: UNKNOWN_TIME, nlink: 1 },
    }
}

impl Entry {
    pub fn set_stat(&mut self, s: Stat) {
        self.size = s.size;
        self.mtime = s.mtime;
        self.nlink = s.nlink;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum SortBy {
    #[default]
    Name,
    Size,
    Modified,
    Type,
    Extension,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum GroupBy {
    #[default]
    None,
    Name,
    Type,
    Extension,
    Size,
    Modified,
    /// Image width × height (read from file headers in the background).
    Dimensions,
}

/// A run of `order` shown under one group header; collapsed groups keep their count but no items.
pub struct Section {
    pub title: String,
    pub start: usize,
    pub len: usize,
    pub count: usize,
    pub collapsed: bool,
}

/// UI-side list: all entries (append-only, deleted ones tombstoned), sorted indices and the visible order.
#[derive(Default)]
pub struct Model {
    pub entries: Vec<Entry>,
    sorted: Vec<u32>,
    /// Visible entries in display order (indices into `entries`).
    pub order: Vec<u32>,
    pub sections: Vec<Section>,
    pub show_hidden: bool,
    pub selected: Vec<bool>,
    pub sort: SortBy,
    pub desc: bool,
    pub group: GroupBy,
    pub collapsed: HashSet<String>,
    pub filter: String,
    types: HashMap<String, String>,
}

impl Model {
    /// Merge entries already sorted by key.
    pub fn append(&mut self, batch: Vec<Entry>) {
        let start = self.entries.len() as u32;
        self.entries.extend(batch);
        self.selected.resize(self.entries.len(), false);
        let new = start..self.entries.len() as u32;
        let mut merged = Vec::with_capacity(self.sorted.len() + new.len());
        let (mut old, mut new) = (self.sorted.iter().copied().peekable(), new.peekable());
        loop {
            let pick_old = match (old.peek(), new.peek()) {
                (Some(&a), Some(&b)) => self.entries[a as usize].key <= self.entries[b as usize].key,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            merged.push(if pick_old { old.next() } else { new.next() }.unwrap());
        }
        self.sorted = merged;
        self.rebuild_order();
    }

    pub fn remove_names(&mut self, names: &HashSet<OsString>) {
        for (i, e) in self.entries.iter_mut().enumerate() {
            if !e.deleted && names.contains(e.name.as_ref()) {
                e.deleted = true;
                self.selected[i] = false;
            }
        }
        let entries = &self.entries;
        self.sorted.retain(|&i| !entries[i as usize].deleted);
        self.rebuild_order();
    }

    pub fn find(&self, name: &OsStr) -> Option<u32> {
        self.order.iter().copied().find(|&i| self.entries[i as usize].name.as_ref() == name)
    }

    /// Filter, sort, then group `sorted` (name order) into `order` and `sections`.
    pub fn rebuild_order(&mut self) {
        let filter = Filter::new(&self.filter);
        let (entries, show) = (&self.entries, self.show_hidden);
        let mut items: Vec<u32> = self
            .sorted
            .iter()
            .copied()
            .filter(|&i| {
                let e = &entries[i as usize];
                (show || !e.hidden()) && filter.matches(&e.name.to_string_lossy())
            })
            .collect();
        self.sort_items(&mut items);

        self.sections.clear();
        if self.group == GroupBy::None {
            self.sections.push(Section { title: String::new(), start: 0, len: items.len(), count: items.len(), collapsed: false });
            self.order = items;
        } else {
            // Group in first-seen order, then order the groups by rank; items keep their sorted order inside.
            let mut ids: HashMap<String, usize> = HashMap::new();
            let mut groups: Vec<((i64, String), String, Vec<u32>)> = Vec::new();
            let now = Now::new();
            for i in items {
                let (title, rank) = self.group_of(i, &now);
                let id = *ids.entry(title.clone()).or_insert_with(|| {
                    groups.push((rank, title, Vec::new()));
                    groups.len() - 1
                });
                groups[id].2.push(i);
            }
            groups.sort_by(|a, b| a.0.cmp(&b.0));
            self.order = Vec::with_capacity(self.entries.len());
            for (_, title, list) in groups {
                let collapsed = self.collapsed.contains(&title);
                let start = self.order.len();
                if !collapsed {
                    self.order.extend_from_slice(&list);
                }
                self.sections.push(Section { title, start, len: self.order.len() - start, count: list.len(), collapsed });
            }
        }
        // Nothing hidden, filtered out or collapsed stays selected.
        let was = std::mem::replace(&mut self.selected, vec![false; self.entries.len()]);
        for &i in &self.order {
            self.selected[i as usize] = was[i as usize];
        }
    }

    /// Stable sort of name-ordered items: folders first, then by the chosen key.
    fn sort_items(&mut self, items: &mut [u32]) {
        if self.sort == SortBy::Type {
            for &i in items.iter() {
                let ext = extension(&self.entries[i as usize]);
                self.types.entry(ext).or_insert_with_key(|ext| type_label(ext));
            }
        }
        let (e, types) = (&self.entries, &self.types);
        match self.sort {
            SortBy::Name => {}
            SortBy::Size => items.sort_by_key(|&i| (!e[i as usize].is_dir, if e[i as usize].is_dir { 0 } else { e[i as usize].size })),
            SortBy::Modified => items.sort_by_key(|&i| (!e[i as usize].is_dir, e[i as usize].mtime)),
            SortBy::Extension => items.sort_by_cached_key(|&i| (!e[i as usize].is_dir, extension(&e[i as usize]))),
            SortBy::Type => items.sort_by_cached_key(|&i| {
                let en = &e[i as usize];
                (!en.is_dir, if en.is_dir { String::new() } else { types[&extension(en)].to_lowercase() })
            }),
        }
        if self.desc {
            // Folders stay on top: reverse each block on its own.
            let dirs = items.iter().take_while(|&&i| e[i as usize].is_dir).count();
            items[..dirs].reverse();
            items[dirs..].reverse();
        }
    }

    /// Group title and rank (groups are shown in rank order).
    fn group_of(&mut self, i: u32, now: &Now) -> (String, (i64, String)) {
        let e = &self.entries[i as usize];
        match self.group {
            GroupBy::None => (String::new(), (0, String::new())),
            GroupBy::Name => {
                let name = e.name.to_string_lossy();
                let c = name.trim_start_matches('.').chars().next().unwrap_or('#');
                let title = if c.is_ascii_digit() {
                    "0–9".to_string()
                } else if c.is_alphabetic() {
                    c.to_uppercase().collect()
                } else {
                    "#".to_string()
                };
                let rank = title.to_lowercase();
                (title, (0, rank))
            }
            GroupBy::Extension if e.is_dir => ("Folders".into(), (0, String::new())),
            GroupBy::Extension => match extension(e) {
                ext if ext.is_empty() => ("No extension".into(), (2, String::new())),
                ext => (ext.to_uppercase(), (1, ext)),
            },
            GroupBy::Type if e.is_dir => ("Folder".into(), (0, String::new())),
            GroupBy::Type => {
                let ext = extension(e);
                let label = self.types.entry(ext.clone()).or_insert_with(|| type_label(&ext)).clone();
                let rank = label.to_lowercase();
                (label, (1, rank))
            }
            GroupBy::Size => {
                let (title, rank) = match e.size {
                    _ if e.is_dir => ("Folders", 0),
                    UNKNOWN_SIZE => ("Unknown", 99),
                    0 => ("Empty", 1),
                    s if s < 16 << 10 => ("Tiny (< 16 KB)", 2),
                    s if s < 1 << 20 => ("Small (< 1 MB)", 3),
                    s if s < 128 << 20 => ("Medium (< 128 MB)", 4),
                    s if s < 1 << 30 => ("Large (< 1 GB)", 5),
                    _ => ("Huge (≥ 1 GB)", 6),
                };
                (title.into(), (rank, String::new()))
            }
            GroupBy::Modified => now.bucket(e.mtime),
            GroupBy::Dimensions => match e.dims {
                _ if e.is_dir => ("Folders".into(), (0, String::new())),
                Some(Some((w, h))) => (format!("{w} × {h}"), (1_000_000_000_000 - w as i64 * h as i64, String::new())),
                None if crate::thumbs::kind(&e.name.to_string_lossy()) == Some(crate::thumbs::Kind::Image) => ("Reading…".into(), (2_000_000_000_000, String::new())),
                _ => ("Not images".into(), (3_000_000_000_000, String::new())),
            },
        }
    }
}

/// Day boundaries for "Modified" groups, computed once per rebuild.
struct Now {
    today: i64,
    year: i32,
}

impl Now {
    fn new() -> Self {
        let now = gtk::glib::DateTime::now_local().ok();
        let today = now
            .as_ref()
            .and_then(|n| gtk::glib::DateTime::from_local(n.year(), n.month(), n.day_of_month(), 0, 0, 0.0).ok())
            .map_or(0, |d| d.to_unix());
        Now { today, year: now.map_or(1970, |n| n.year()) }
    }

    fn bucket(&self, mtime: i64) -> (String, (i64, String)) {
        const DAY: i64 = 86400;
        let (title, rank): (String, i64) = match mtime {
            UNKNOWN_TIME => ("Unknown".into(), 9999),
            t if t >= self.today => ("Today".into(), 0),
            t if t >= self.today - DAY => ("Yesterday".into(), 1),
            t if t >= self.today - 7 * DAY => ("Last 7 days".into(), 2),
            t if t >= self.today - 30 * DAY => ("Last 30 days".into(), 3),
            t => {
                let y = gtk::glib::DateTime::from_unix_local(t).map_or(1970, |d| d.year());
                if y == self.year {
                    ("Earlier this year".into(), 4)
                } else {
                    (y.to_string(), 10 + (self.year - y) as i64)
                }
            }
        };
        (title, (rank, String::new()))
    }
}

/// Lowercase extension of a file ("" for none or for dot-files without one).
pub fn extension(e: &Entry) -> String {
    let name = e.name.to_string_lossy();
    match name.trim_start_matches('.').rsplit_once('.') {
        Some((_, ext)) => ext.to_lowercase(),
        None => String::new(),
    }
}

fn type_label(ext: &str) -> String {
    let (ct, _) = gtk::gio::content_type_guess(Some(&format!("x.{ext}")), None::<&[u8]>);
    gtk::gio::content_type_get_description(&ct).to_string()
}

/// Whole-name, case-insensitive wildcard match (`*`, `?`).
pub fn glob_match(pattern: &str, name: &str) -> bool {
    wildcard(pattern.to_lowercase().as_bytes(), name.to_lowercase().as_bytes())
}

/// Name filter: case-insensitive substring, or a whole-name wildcard pattern when it has `*` or `?`.
struct Filter {
    text: String,
    wildcard: bool,
}

impl Filter {
    fn new(text: &str) -> Self {
        Filter { text: text.to_lowercase(), wildcard: text.contains(['*', '?']) }
    }

    fn matches(&self, name: &str) -> bool {
        if self.text.is_empty() {
            return true;
        }
        let name = name.to_lowercase();
        if self.wildcard {
            wildcard(self.text.as_bytes(), name.as_bytes())
        } else {
            name.contains(&self.text)
        }
    }
}

fn wildcard(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<_> = names.iter().map(|n| (sort_key(n, false), n.to_string())).collect();
        v.sort();
        v.into_iter().map(|(_, n)| n).collect()
    }

    #[test]
    fn natural_order() {
        assert_eq!(sorted(&["img_10.jpg", "img_2.jpg", "img_1.jpg"]), ["img_1.jpg", "img_2.jpg", "img_10.jpg"]);
        assert_eq!(sorted(&["B.txt", "a.txt", "c.txt"]), ["a.txt", "B.txt", "c.txt"]);
        assert_eq!(sorted(&["a10", "a9b", "a09"]), ["a09", "a9b", "a10"]);
    }

    fn model(names: &[(&str, bool, u64)]) -> Model {
        let mut v: Vec<_> = names
            .iter()
            .map(|&(n, d, size)| {
                let mut e = Entry::new(n.into(), d);
                e.size = if d { UNKNOWN_SIZE } else { size };
                e
            })
            .collect();
        v.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        let mut m = Model::default();
        m.append(v);
        m
    }

    fn shown(m: &Model) -> Vec<String> {
        m.order.iter().map(|&i| m.entries[i as usize].name.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn sort_and_group() {
        let mut m = model(&[("b.jpg", false, 10), ("a.txt", false, 30), ("dir", true, 0), ("c.jpg", false, 20)]);
        m.sort = SortBy::Size;
        m.rebuild_order();
        assert_eq!(shown(&m), ["dir", "b.jpg", "c.jpg", "a.txt"]);
        m.desc = true;
        m.rebuild_order();
        assert_eq!(shown(&m), ["dir", "a.txt", "c.jpg", "b.jpg"]);
        m.group = GroupBy::Extension;
        m.rebuild_order();
        let titles: Vec<_> = m.sections.iter().map(|s| (s.title.as_str(), s.count)).collect();
        assert_eq!(titles, [("Folders", 1), ("JPG", 2), ("TXT", 1)]);
        assert_eq!(shown(&m), ["dir", "c.jpg", "b.jpg", "a.txt"]);
        m.collapsed.insert("JPG".into());
        m.rebuild_order();
        assert_eq!(shown(&m), ["dir", "a.txt"]);
        assert_eq!(m.sections[1].len, 0);
    }

    #[test]
    fn group_by_dimensions() {
        let mut m = model(&[("a.jpg", false, 1), ("b.jpg", false, 1), ("c.png", false, 1), ("d.txt", false, 1)]);
        m.entries[0].dims = Some(Some((640, 480)));
        m.entries[1].dims = Some(Some((1920, 1080)));
        m.group = GroupBy::Dimensions;
        m.rebuild_order();
        let titles: Vec<_> = m.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["1920 × 1080", "640 × 480", "Reading…", "Not images"]);
        assert!(glob_match("*.JPG", "a.jpg") && !glob_match("a?", "abc"));
    }

    #[test]
    fn filters() {
        let mut m = model(&[("img_1.jpg", false, 1), ("img_2.png", false, 1), ("notes.txt", false, 1)]);
        m.filter = "IMG".into();
        m.rebuild_order();
        assert_eq!(shown(&m), ["img_1.jpg", "img_2.png"]);
        m.filter = "*.png".into();
        m.rebuild_order();
        assert_eq!(shown(&m), ["img_2.png"]);
    }

    #[test]
    fn folders_first() {
        assert!(sort_key("zzz", true) < sort_key("aaa", false));
    }

    #[test]
    fn merge_and_remove() {
        let batch = |names: &[&str]| {
            let mut v: Vec<_> = names.iter().map(|n| Entry::new(n.into(), false)).collect();
            v.sort_unstable_by(|a, b| a.key.cmp(&b.key));
            v
        };
        let mut m = Model::default();
        m.append(batch(&["b", "d"]));
        m.append(batch(&["a", "c", ".h"]));
        let names = |m: &Model| m.order.iter().map(|&i| m.entries[i as usize].name.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(names(&m), ["a", "b", "c", "d"]);
        m.show_hidden = true;
        m.rebuild_order();
        assert_eq!(names(&m), [".h", "a", "b", "c", "d"]);
        m.remove_names(&["b".into()].into_iter().collect());
        assert_eq!(names(&m), [".h", "a", "c", "d"]);
    }
}
