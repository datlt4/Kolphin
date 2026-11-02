//! Folder statistics: counts and sizes by extension, a size histogram, and the recursive size computed in the background.
use crate::model::{self, Model, UNKNOWN_SIZE};
use gtk::{glib, prelude::*};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BUCKETS: [(&str, u64); 6] = [("Empty", 1), ("< 16 KB", 16 << 10), ("< 1 MB", 1 << 20), ("< 128 MB", 128 << 20), ("< 1 GB", 1 << 30), ("≥ 1 GB", u64::MAX)];

pub fn show(parent: &gtk::Window, dir: &Path, m: &Model) {
    let (mut files, mut dirs, mut bytes) = (0usize, 0usize, 0u64);
    let mut by_ext: HashMap<String, (usize, u64)> = HashMap::new();
    let mut hist = [0usize; BUCKETS.len()];
    for e in m.entries.iter().filter(|e| !e.deleted) {
        if e.is_dir {
            dirs += 1;
            continue;
        }
        let size = if e.size == UNKNOWN_SIZE { 0 } else { e.size };
        files += 1;
        bytes += size;
        let ext = model::extension(e);
        let slot = by_ext.entry(if ext.is_empty() { "(none)".into() } else { format!(".{ext}") }).or_default();
        slot.0 += 1;
        slot.1 += size;
        hist[BUCKETS.iter().position(|&(_, lim)| size < lim).unwrap_or(BUCKETS.len() - 1)] += 1;
    }

    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14).css_classes(["dialog-body"]).build();
    let title = dir.file_name().map_or("/".into(), |n| n.to_string_lossy().into_owned());
    let header = gtk::HeaderBar::builder().title_widget(&gtk::Label::builder().label(format!("Statistics — {title}")).css_classes(["heading"]).build()).build();
    let win = gtk::Window::builder().title(format!("Statistics — {title}")).transient_for(parent).modal(true).default_width(520).default_height(620).titlebar(&header).build();
    win.set_child(Some(&gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).build()));

    let line = |label: &str, value: &str| {
        let row = gtk::Box::builder().spacing(12).build();
        row.append(&gtk::Label::builder().label(label).xalign(0.0).hexpand(true).css_classes(["dim-label"]).build());
        let v = gtk::Label::builder().label(value).xalign(1.0).build();
        row.append(&v);
        (row, v)
    };
    let section = |text: &str| gtk::Label::builder().label(text).xalign(0.0).css_classes(["side-title-label"]).build();

    body.append(&section("THIS FOLDER"));
    body.append(&line("Files", &files.to_string()).0);
    body.append(&line("Folders", &dirs.to_string()).0);
    body.append(&line("Size of files", &glib::format_size(bytes)).0);
    let (rec_row, rec) = line("Total size, all subfolders", "Computing…");
    body.append(&rec_row);

    body.append(&section("BY EXTENSION"));
    let mut exts: Vec<_> = by_ext.into_iter().collect();
    exts.sort_by_key(|e| std::cmp::Reverse(e.1 .0));
    for (ext, (n, size)) in exts.iter().take(15) {
        body.append(&line(ext, &format!("{n} files · {}", glib::format_size(*size))).0);
    }

    body.append(&section("FILE SIZES"));
    let max = hist.iter().copied().max().unwrap_or(0).max(1);
    for ((label, _), n) in BUCKETS.iter().zip(hist) {
        let row = gtk::Box::builder().spacing(12).build();
        row.append(&gtk::Label::builder().label(*label).xalign(0.0).width_chars(9).css_classes(["dim-label"]).build());
        row.append(&gtk::ProgressBar::builder().fraction(n as f64 / max as f64).hexpand(true).valign(gtk::Align::Center).build());
        row.append(&gtk::Label::builder().label(n.to_string()).xalign(1.0).width_chars(8).build());
        body.append(&row);
    }

    // Recursive size: walker thread reports every 200 ms; closing the window stops it.
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = async_channel::unbounded::<(u64, u64, bool)>();
    let root = dir.to_path_buf();
    let stop = cancel.clone();
    std::thread::spawn(move || walk(&root, &stop, &tx));
    glib::spawn_future_local(glib::clone!(#[weak] rec, async move {
        while let Ok((n, size, done)) = rx.recv().await {
            rec.set_text(&format!("{}{} · {n} files", glib::format_size(size), if done { "" } else { "…" }));
        }
    }));
    win.connect_close_request(move |_| {
        cancel.store(true, Ordering::Relaxed);
        glib::Propagation::Proceed
    });
    win.present();
}

/// Sum file sizes under `root` without following symlinks; hard-linked files count once.
fn walk(root: &Path, stop: &AtomicBool, tx: &async_channel::Sender<(u64, u64, bool)>) {
    let (mut n, mut size, mut last) = (0u64, 0u64, Instant::now());
    let mut seen = HashSet::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.nlink() < 2 || seen.insert((meta.dev(), meta.ino())) {
                n += 1;
                size += meta.len();
            }
            if last.elapsed() > Duration::from_millis(200) {
                last = Instant::now();
                if tx.send_blocking((n, size, false)).is_err() {
                    return;
                }
            }
        }
    }
    let _ = tx.send_blocking((n, size, true));
}
