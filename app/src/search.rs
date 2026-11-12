//! Recursive name search: worker threads walk the tree (2 on a spinning disk) and stream matches in batches.
use crate::model::{self, Entry, Msg};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Case-insensitive substring, or a whole-name wildcard pattern with `*`/`?`.
pub fn matches(query: &str, name: &str) -> bool {
    if query.contains(['*', '?']) {
        model::glob_match(query, name)
    } else {
        name.to_lowercase().contains(&query.to_lowercase())
    }
}

struct Queue {
    dirs: Vec<PathBuf>,
    busy: usize,
}

/// Walk `root` for names matching `query`; entries are named by their path relative to `root`.
pub fn run(root: PathBuf, query: String, show_hidden: bool, tx: async_channel::Sender<Msg>) {
    let dev = std::fs::metadata(&root).map(|m| m.dev()).unwrap_or(0);
    let threads = if crate::thumbs::rotational(dev) { 2 } else { std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8) };
    let shared = Arc::new((Mutex::new(Queue { dirs: vec![root.clone()], busy: 0 }), Condvar::new()));
    let found = Arc::new(Mutex::new(Vec::<Entry>::new()));
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let (shared, found, root, query, tx) = (shared.clone(), found.clone(), root.clone(), query.clone(), tx.clone());
            std::thread::spawn(move || worker(&shared, &found, &root, &query, show_hidden, &tx))
        })
        .collect();
    // Flush matches every 150 ms while the walkers run.
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let done = workers.iter().all(|w| w.is_finished());
        let mut batch = std::mem::take(&mut *found.lock().unwrap());
        if !batch.is_empty() {
            batch.sort_unstable_by(|a, b| a.key.cmp(&b.key));
            if tx.send_blocking(Msg::Entries(batch)).is_err() {
                let (q, cv) = &*shared;
                q.lock().unwrap().dirs.clear();
                cv.notify_all();
                return;
            }
        }
        if done {
            break;
        }
    }
    let _ = tx.send_blocking(Msg::Done);
}

fn worker(shared: &(Mutex<Queue>, Condvar), found: &Mutex<Vec<Entry>>, root: &Path, query: &str, show_hidden: bool, tx: &async_channel::Sender<Msg>) {
    let (q, cv) = shared;
    loop {
        let dir = {
            let mut st = q.lock().unwrap();
            loop {
                if tx.is_closed() {
                    return;
                }
                if let Some(d) = st.dirs.pop() {
                    st.busy += 1;
                    break d;
                }
                if st.busy == 0 {
                    cv.notify_all();
                    return;
                }
                st = cv.wait_timeout(st, Duration::from_millis(100)).unwrap().0;
            }
        };
        let (mut subdirs, mut hits) = (Vec::new(), Vec::new());
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name();
                let s = name.to_string_lossy();
                if !show_hidden && s.starts_with('.') {
                    continue;
                }
                let ft = e.file_type().ok();
                let is_dir = ft.is_some_and(|t| t.is_dir());
                if is_dir {
                    subdirs.push(e.path());
                }
                if matches(query, &s) {
                    let path = e.path();
                    let rel = path.strip_prefix(root).unwrap_or(&path).as_os_str().to_os_string();
                    let mut entry = Entry::new(rel, is_dir || (ft.is_some_and(|t| t.is_symlink()) && path.is_dir()));
                    entry.symlink = ft.is_some_and(|t| t.is_symlink());
                    entry.set_stat(model::stat(&path));
                    hits.push(entry);
                }
            }
        }
        found.lock().unwrap().extend(hits);
        let mut st = q.lock().unwrap();
        st.dirs.extend(subdirs);
        st.busy -= 1;
        cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nested_matches() {
        let root = std::env::temp_dir().join(format!("kolphin-search-{}", std::process::id()));
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        for p in ["cat.jpg", "a/dog.jpg", "a/b/cat.png", "a/b/c/catalog.txt", ".hidden/cat.jpg"] {
            std::fs::write(root.join(p), "x").unwrap();
        }
        let (tx, rx) = async_channel::unbounded();
        run(root.clone(), "cat".into(), false, tx);
        let mut names = Vec::new();
        while let Ok(m) = rx.recv_blocking() {
            match m {
                Msg::Entries(b) => names.extend(b.into_iter().map(|e| e.name.to_string_lossy().into_owned())),
                Msg::Done => break,
                _ => {}
            }
        }
        names.sort();
        assert_eq!(names, ["a/b/c/catalog.txt", "a/b/cat.png", "cat.jpg"]);
        assert!(matches("*.JPG", "dog.jpg") && !matches("*.png", "dog.jpg"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
