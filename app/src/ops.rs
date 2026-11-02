//! File operations with undo (trash, delete, rename, new folder, links) and the one-at-a-time transfer queue.
use gtk::prelude::{FileEnumeratorExt, FileExt};
use gtk::{gio, glib};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub enum Undo {
    /// (from, to) pairs: renames and moves.
    Moved(Vec<(PathBuf, PathBuf)>),
    /// New files or folders (copies, links, new folders); undo trashes them.
    Created(Vec<PathBuf>),
    /// Original paths of items sent to the trash; undo restores them.
    Trashed(Vec<PathBuf>),
}

thread_local! {
    static UNDO: RefCell<Vec<Undo>> = const { RefCell::new(Vec::new()) };
}

pub fn record(u: Undo) {
    UNDO.with(|s| s.borrow_mut().push(u));
}

/// Revert the last operation; returns a message for the user.
pub fn undo() -> Result<String, String> {
    let Some(u) = UNDO.with(|s| s.borrow_mut().pop()) else { return Err("Nothing to undo".into()) };
    match u {
        Undo::Moved(pairs) => {
            for (from, to) in pairs.iter().rev() {
                move_path(to, from).map_err(|e| format!("{}: {e}", to.display()))?;
            }
            Ok(format!("Undid move/rename of {} item(s)", pairs.len()))
        }
        Undo::Created(paths) => {
            trash(&paths)?;
            Ok(format!("Removed {} created item(s) (they are in the trash)", paths.len()))
        }
        Undo::Trashed(paths) => {
            let n = restore(&paths)?;
            Ok(format!("Restored {n} item(s) from the trash"))
        }
    }
}

pub fn trash(paths: &[PathBuf]) -> Result<(), String> {
    for p in paths {
        gio::File::for_path(p).trash(None::<&gio::Cancellable>).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

/// Put trashed items back where they were (the newest trash entry for each path).
fn restore(paths: &[PathBuf]) -> Result<usize, String> {
    let trash = gio::File::for_uri("trash:///");
    let list = trash
        .enumerate_children("standard::name,trash::orig-path,trash::deletion-date", gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>)
        .map_err(|e| e.to_string())?;
    let mut items: Vec<(PathBuf, String, gio::File)> = Vec::new();
    while let Ok(Some(info)) = list.next_file(None::<&gio::Cancellable>) {
        if let Some(orig) = info.attribute_byte_string("trash::orig-path") {
            let date = info.attribute_string("trash::deletion-date").map(|d| d.to_string()).unwrap_or_default();
            items.push((PathBuf::from(orig.as_str()), date, trash.child(info.name())));
        }
    }
    let mut n = 0;
    for p in paths {
        let found = items.iter().filter(|(orig, _, _)| orig == p).max_by(|a, b| a.1.cmp(&b.1));
        if let Some((_, _, file)) = found {
            file.move_(&gio::File::for_path(p), gio::FileCopyFlags::NONE, None::<&gio::Cancellable>, None)
                .map_err(|e| format!("{}: {e}", p.display()))?;
            n += 1;
        }
    }
    Ok(n)
}

pub fn delete_permanently(paths: &[PathBuf]) -> Result<(), String> {
    for p in paths {
        let r = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
        r.map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

/// Rename inside the same folder; refuses to overwrite.
pub fn rename(from: &Path, new_name: &str) -> Result<PathBuf, String> {
    if new_name.is_empty() || new_name.contains('/') || new_name == "." || new_name == ".." {
        return Err("Invalid name".into());
    }
    let to = from.with_file_name(new_name);
    if to == from {
        return Ok(to);
    }
    if std::fs::symlink_metadata(&to).is_ok() {
        return Err(format!("{new_name} already exists"));
    }
    std::fs::rename(from, &to).map_err(|e| e.to_string())?;
    Ok(to)
}

pub fn new_folder(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.contains('/') {
        return Err("Invalid name".into());
    }
    let p = dir.join(name);
    std::fs::create_dir(&p).map_err(|e| e.to_string())?;
    Ok(p)
}

/// "name (link)" next to each source: a symlink, or a hard link (files only).
pub fn make_links(paths: &[PathBuf], hard: bool) -> Result<Vec<PathBuf>, String> {
    let mut made = Vec::new();
    for src in paths {
        if hard && src.is_dir() {
            return Err(format!("{}: folders can't be hard-linked", src.display()));
        }
        let stem = src.file_stem().unwrap_or_default().to_string_lossy();
        let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let dst = unique(&src.with_file_name(format!("{stem} (link){ext}")));
        let r = if hard { std::fs::hard_link(src, &dst) } else { std::os::unix::fs::symlink(src, &dst) };
        r.map_err(|e| format!("{}: {e}", src.display()))?;
        made.push(dst);
    }
    Ok(made)
}

/// A free name: `path`, else "stem (copy).ext", "stem (copy 2).ext", …
pub fn unique(path: &Path) -> PathBuf {
    if std::fs::symlink_metadata(path).is_err() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..)
        .map(|n| path.with_file_name(if n == 1 { format!("{stem} (copy){ext}") } else { format!("{stem} (copy {n}){ext}") }))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .unwrap()
}

fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_tree(from, to, &mut |_| true)?;
    if from.is_dir() && !from.is_symlink() {
        std::fs::remove_dir_all(from)
    } else {
        std::fs::remove_file(from)
    }
}

// ---------- transfers ----------

pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub done_items: usize,
    pub total_items: usize,
    pub finished: Option<Result<Undo, String>>,
}

struct Job {
    sources: Vec<PathBuf>,
    dest: PathBuf,
    cut: bool,
    tx: async_channel::Sender<Progress>,
}

/// Copy or move into `dest`; jobs run one at a time (no two big copies fighting over a disk).
pub fn transfer(sources: Vec<PathBuf>, dest: PathBuf, cut: bool) -> async_channel::Receiver<Progress> {
    static QUEUE: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            for job in rx {
                run(job);
            }
        });
        tx
    });
    let (tx, rx) = async_channel::unbounded();
    let _ = queue.send(Job { sources, dest, cut, tx });
    rx
}

fn tree_size(p: &Path) -> u64 {
    let Ok(m) = std::fs::symlink_metadata(p) else { return 0 };
    if m.is_dir() {
        std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum()).unwrap_or(0)
    } else {
        m.len()
    }
}

fn run(job: Job) {
    let total_items = job.sources.len();
    let mut p = Progress { done_bytes: 0, total_bytes: 0, done_items: 0, total_items, finished: None };
    let result = (|| -> Result<Undo, String> {
        for src in &job.sources {
            if job.dest.starts_with(src) {
                return Err(format!("cannot put {} inside itself", src.display()));
            }
        }
        // Moves within one filesystem are renames: no bytes to count.
        p.total_bytes = job.sources.iter().filter(|s| !(job.cut && same_fs(s, &job.dest))).map(|s| tree_size(s)).sum();
        let (mut created, mut moved) = (Vec::new(), Vec::new());
        let mut last = Instant::now();
        for src in &job.sources {
            let name = src.file_name().ok_or("bad path")?;
            if job.cut && src.parent() == Some(job.dest.as_path()) {
                p.done_items += 1;
                continue;
            }
            let target = unique(&job.dest.join(name));
            let renamed = job.cut && std::fs::rename(src, &target).is_ok();
            if !renamed {
                let tx = &job.tx;
                let (total_bytes, done_items) = (p.total_bytes, p.done_items);
                let mut done_bytes = p.done_bytes;
                copy_tree(src, &target, &mut |n| {
                    done_bytes += n;
                    if last.elapsed() > Duration::from_millis(150) {
                        last = Instant::now();
                        let msg = Progress { done_bytes, total_bytes, done_items, total_items, finished: None };
                        return tx.send_blocking(msg).is_ok();
                    }
                    true
                })
                .map_err(|e| format!("{}: {e}", src.display()))?;
                p.done_bytes = done_bytes;
                if job.cut {
                    let r = if src.is_dir() && !src.is_symlink() { std::fs::remove_dir_all(src) } else { std::fs::remove_file(src) };
                    r.map_err(|e| format!("{}: {e}", src.display()))?;
                }
            }
            if job.cut {
                moved.push((src.clone(), target));
            } else {
                created.push(target);
            }
            p.done_items += 1;
        }
        Ok(if job.cut { Undo::Moved(moved) } else { Undo::Created(created) })
    })();
    p.finished = Some(result);
    let _ = job.tx.send_blocking(p);
}

fn same_fs(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    matches!((std::fs::symlink_metadata(a), std::fs::metadata(b)), (Ok(x), Ok(y)) if x.dev() == y.dev())
}

/// Copy a file or folder (symlinks stay links); `on_bytes` returns false to cancel.
fn copy_tree(src: &Path, dst: &Path, on_bytes: &mut dyn FnMut(u64) -> bool) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if meta.is_dir() {
        std::fs::create_dir(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()), on_bytes)?;
        }
        std::fs::set_permissions(dst, meta.permissions())
    } else {
        let (mut r, mut w) = (std::fs::File::open(src)?, std::fs::File::create(dst)?);
        let mut buf = vec![0u8; 4 << 20];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
            if !on_bytes(n as u64) {
                return Err(std::io::Error::other("cancelled"));
            }
        }
        std::fs::set_permissions(dst, meta.permissions())
    }
}

/// "Copying 3/10 · 45% · 120 MB/s" for a progress toast.
pub fn describe(p: &Progress, cut: bool, started: Instant) -> String {
    let verb = if cut { "Moving" } else { "Copying" };
    let mut s = format!("{verb} {}/{}", (p.done_items + 1).min(p.total_items), p.total_items);
    if let Some(pct) = (p.done_bytes * 100).checked_div(p.total_bytes) {
        let rate = p.done_bytes as f64 / started.elapsed().as_secs_f64().max(0.001);
        s += &format!(" · {pct}% · {}/s", glib::format_size(rate as u64));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(rx: async_channel::Receiver<Progress>) -> Result<Undo, String> {
        loop {
            if let Some(r) = rx.recv_blocking().unwrap().finished {
                return r;
            }
        }
    }

    #[test]
    fn transfers_and_names() {
        let root = std::env::temp_dir().join(format!("kolphin-ops-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("f.txt"), "x").unwrap();
        std::fs::write(a.join("sub/g.txt"), "y").unwrap();
        std::os::unix::fs::symlink("f.txt", a.join("link")).unwrap();

        assert!(matches!(wait(transfer(vec![a.join("f.txt"), a.join("sub"), a.join("link")], b.clone(), false)), Ok(Undo::Created(v)) if v.len() == 3));
        assert!(b.join("sub/g.txt").exists() && b.join("link").is_symlink());
        wait(transfer(vec![a.join("f.txt")], b.clone(), false)).unwrap();
        assert!(b.join("f (copy).txt").exists());
        assert!(matches!(wait(transfer(vec![a.join("sub")], b.clone(), true)), Ok(Undo::Moved(_))));
        assert!(!a.join("sub").exists() && b.join("sub (copy)/g.txt").exists());
        assert!(wait(transfer(vec![b.clone()], b.join("sub"), false)).is_err());

        let renamed = rename(&b.join("f.txt"), "h.txt").unwrap();
        assert!(renamed.exists() && rename(&renamed, "f (copy).txt").is_err());
        let links = make_links(std::slice::from_ref(&renamed), true).unwrap();
        assert_eq!(links[0].file_name().unwrap(), "h (link).txt");
        assert!(new_folder(&b, "x/y").is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
