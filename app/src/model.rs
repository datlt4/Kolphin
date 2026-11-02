//! Directory entries, natural sort keys and the background directory scanner.
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
}

impl Entry {
    pub fn new(name: OsString, is_dir: bool) -> Self {
        let key = sort_key(&name.to_string_lossy(), is_dir);
        Entry { name: name.into_boxed_os_str(), key, is_dir, deleted: false, size: UNKNOWN_SIZE, mtime: UNKNOWN_TIME }
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
    /// (size, mtime) for the entries at `start..`.
    Stats { start: usize, stats: Vec<(u64, i64)> },
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
        let is_dir = match de.file_type() {
            Ok(t) if t.is_symlink() => de.path().is_dir(),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        batch.push(Entry::new(de.file_name(), is_dir));
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

/// (size, mtime) following symlinks; folders have no size.
pub fn stat(path: &std::path::Path) -> (u64, i64) {
    match std::fs::metadata(path).or_else(|_| std::fs::symlink_metadata(path)) {
        Ok(m) => (if m.is_dir() { UNKNOWN_SIZE } else { m.len() }, m.mtime()),
        Err(_) => (UNKNOWN_SIZE, UNKNOWN_TIME),
    }
}

/// UI-side list: all entries (append-only, deleted ones tombstoned), sorted indices and the visible order.
#[derive(Default)]
pub struct Model {
    pub entries: Vec<Entry>,
    sorted: Vec<u32>,
    /// Visible entries in display order (indices into `entries`).
    pub order: Vec<u32>,
    pub show_hidden: bool,
    pub selected: Vec<bool>,
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

    pub fn remove_names(&mut self, names: &std::collections::HashSet<OsString>) {
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

    pub fn rebuild_order(&mut self) {
        let (entries, show) = (&self.entries, self.show_hidden);
        self.order = self.sorted.iter().copied().filter(|&i| show || !entries[i as usize].hidden()).collect();
        for (i, e) in self.entries.iter().enumerate() {
            if e.hidden() && !show {
                self.selected[i] = false;
            }
        }
    }
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
