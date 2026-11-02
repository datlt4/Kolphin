//! Properties of one file or folder, with checksums (MD5, SHA-256, SHA-512, xxHash3) computed in a single read.
use gtk::{glib, prelude::*};
use md5::Digest;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ALGOS: [&str; 5] = ["MD5", "SHA-256", "SHA-512", "xxHash3-64", "xxHash3-128"];

pub fn show(parent: &gtk::Window, path: PathBuf) {
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return };
    let name = path.file_name().map_or("/".into(), |n| n.to_string_lossy().into_owned());
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).css_classes(["dialog-body"]).build();
    let header = gtk::HeaderBar::builder().title_widget(&gtk::Label::builder().label(format!("Properties — {name}")).css_classes(["heading"]).build()).build();
    let win = gtk::Window::builder().title(format!("Properties — {name}")).transient_for(parent).modal(true).default_width(560).default_height(640).titlebar(&header).build();
    win.set_child(Some(&gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).build()));
    let cancel = Arc::new(AtomicBool::new(false));

    let row = |label: &str, value: &str| {
        let r = gtk::Box::builder().spacing(12).build();
        r.append(&gtk::Label::builder().label(label).xalign(0.0).width_chars(12).css_classes(["dim-label"]).build());
        let v = gtk::Label::builder().label(value).xalign(0.0).hexpand(true).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).selectable(true).build();
        r.append(&v);
        body.append(&r);
        v
    };
    // A row whose value has a copy button.
    let copy_row = |label: &str, value: &str| {
        let v = row(label, value);
        let copy = gtk::Button::builder().icon_name("edit-copy-symbolic").css_classes(["flat"]).valign(gtk::Align::Center).tooltip_text(format!("Copy {}", label.to_lowercase())).build();
        copy.connect_clicked(glib::clone!(#[weak] v, move |b| b.clipboard().set_text(&v.text())));
        if let Some(r) = v.parent().and_downcast::<gtk::Box>() {
            r.append(&copy);
        }
    };
    let date = |t: i64| glib::DateTime::from_unix_local(t).ok().and_then(|d| d.format("%Y-%m-%d %H:%M:%S").ok()).map(|s| s.to_string()).unwrap_or_default();
    let is_dir = meta.is_dir();
    let (ct, _) = gtk::gio::content_type_guess(Some(&path), None::<&[u8]>);
    body.append(&gtk::Label::builder().label("GENERAL").xalign(0.0).css_classes(["side-title-label"]).build());
    copy_row("Name", &name);
    copy_row("Location", &path.parent().map(|p| p.display().to_string()).unwrap_or_default());
    copy_row("Path", &path.display().to_string());
    row("Type", &if is_dir { "Folder".into() } else { gtk::gio::content_type_get_description(&ct).to_string() });
    if meta.file_type().is_symlink() {
        row("Link to", &std::fs::read_link(&path).map(|t| t.display().to_string()).unwrap_or_default());
    }
    let size = row("Size", &if is_dir { "Computing…".into() } else { format!("{} ({} bytes)", glib::format_size(meta.len()), meta.len()) });
    row("Modified", &date(meta.mtime()));
    row("Accessed", &date(meta.atime()));
    row("Changed", &date(meta.ctime()));
    let mode = meta.permissions().mode();
    let rwx: String = (0..9).rev().map(|i| if mode & (1 << i) == 0 { '-' } else { b"xwr"[i % 3] as char }).collect();
    row("Permissions", &format!("{rwx} ({:o})", mode & 0o7777));
    row("Owner", &format!("{} : {}", user_name(meta.uid()), meta.gid()));
    if meta.nlink() > 1 && !is_dir {
        row("Hard links", &meta.nlink().to_string());
    }

    if is_dir {
        let (tx, rx) = async_channel::unbounded::<(u64, u64, bool)>();
        let (root, stop) = (path.clone(), cancel.clone());
        std::thread::spawn(move || crate::stats::walk(&root, &stop, &tx));
        glib::spawn_future_local(glib::clone!(#[weak] size, async move {
            while let Ok((n, bytes, done)) = rx.recv().await {
                size.set_text(&format!("{}{} · {n} files", glib::format_size(bytes), if done { "" } else { "…" }));
            }
        }));
    } else if meta.is_file() {
        checksums(&body, &path, &cancel);
    }
    win.connect_close_request(move |_| {
        cancel.store(true, Ordering::Relaxed);
        glib::Propagation::Proceed
    });
    win.present();
    // Selectable labels would otherwise grab focus and highlight the name.
    gtk::prelude::GtkWindowExt::set_focus(&win, None::<&gtk::Widget>);
}

fn user_name(uid: u32) -> String {
    std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|s| s.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.get(2) == Some(&uid.to_string().as_str())).then(|| f[0].to_string())
        }))
        .unwrap_or_else(|| uid.to_string())
}

fn checksums(body: &gtk::Box, path: &Path, cancel: &Arc<AtomicBool>) {
    body.append(&gtk::Label::builder().label("CHECKSUMS").xalign(0.0).css_classes(["side-title-label"]).build());
    let verify = gtk::Entry::builder().placeholder_text("Paste a checksum to verify").build();
    crate::dialogs::ime_safe(&verify);
    let verdict = gtk::Label::builder().xalign(0.0).build();
    let bar = gtk::ProgressBar::new();
    let start = gtk::Button::builder().label("Compute checksums").halign(gtk::Align::Start).build();
    body.append(&verify);
    body.append(&verdict);
    body.append(&start);
    body.append(&bar);
    let values: Vec<gtk::Label> = ALGOS
        .iter()
        .map(|algo| {
            let r = gtk::Box::builder().spacing(8).build();
            r.append(&gtk::Label::builder().label(*algo).xalign(0.0).width_chars(12).css_classes(["dim-label"]).build());
            let v = gtk::Label::builder().xalign(0.0).hexpand(true).wrap(true).wrap_mode(gtk::pango::WrapMode::Char).selectable(true).css_classes(["monospace"]).build();
            let copy = gtk::Button::builder().icon_name("edit-copy-symbolic").css_classes(["flat"]).tooltip_text("Copy").build();
            copy.connect_clicked(glib::clone!(#[weak] v, move |b| b.clipboard().set_text(&v.text())));
            r.append(&v);
            r.append(&copy);
            body.append(&r);
            v
        })
        .collect();

    let check = glib::clone!(#[weak] verify, #[weak] verdict, #[strong] values, move || {
        let want = verify.text().trim().to_lowercase();
        if want.is_empty() {
            return verdict.set_text("");
        }
        match ALGOS.iter().zip(&values).find(|(_, v)| !v.text().is_empty() && v.text() == want) {
            Some((algo, _)) => verdict.set_markup(&format!("<span foreground='#4ade80'>✓ Matches {algo}</span>")),
            None if values.iter().any(|v| v.text().is_empty()) => verdict.set_text("Compute checksums to verify"),
            None => verdict.set_markup("<span foreground='#ff5c72'>✗ No checksum matches</span>"),
        }
    });
    let c = check.clone();
    verify.connect_changed(move |_| c());
    // A hash already on the clipboard is probably the one to check.
    let clip = verify.clipboard();
    glib::spawn_future_local(glib::clone!(#[weak] verify, async move {
        if let Ok(Some(text)) = clip.read_text_future().await {
            let t = text.trim();
            if (32..=128).contains(&t.len()) && t.chars().all(|c| c.is_ascii_hexdigit()) {
                verify.set_text(t);
            }
        }
    }));

    let path = path.to_path_buf();
    let cancel = cancel.clone();
    start.connect_clicked(move |b| {
        b.set_sensitive(false);
        let (tx, rx) = async_channel::unbounded::<Result<(f64, Option<Vec<String>>), String>>();
        let (path, stop) = (path.clone(), cancel.clone());
        std::thread::spawn(move || {
            let r = hash_file(&path, &stop, &mut |f| tx.send_blocking(Ok((f, None))).is_ok());
            let _ = tx.send_blocking(r.map(|v| (1.0, Some(v))));
        });
        let check = check.clone();
        glib::spawn_future_local(glib::clone!(#[weak] bar, #[strong] values, async move {
            while let Ok(msg) = rx.recv().await {
                match msg {
                    Ok((f, None)) => bar.set_fraction(f),
                    Ok((_, Some(hashes))) => {
                        bar.set_fraction(1.0);
                        for (v, h) in values.iter().zip(hashes) {
                            v.set_text(&h);
                        }
                        check();
                    }
                    Err(e) => bar.set_text(Some(&e)),
                }
            }
        }));
    });
}

/// All checksums from one pass over the file; `progress` gets the fraction read.
fn hash_file(path: &Path, stop: &AtomicBool, progress: &mut dyn FnMut(f64) -> bool) -> Result<Vec<String>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let total = f.metadata().map_or(0, |m| m.len()).max(1);
    let (mut md5, mut sha256, mut sha512) = (md5::Md5::new(), sha2::Sha256::new(), sha2::Sha512::new());
    let (mut x64, mut x128) = (xxhash_rust::xxh3::Xxh3::new(), xxhash_rust::xxh3::Xxh3::new());
    let mut buf = vec![0u8; 4 << 20];
    let (mut done, mut last) = (0u64, Instant::now());
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        md5.update(chunk);
        sha256.update(chunk);
        sha512.update(chunk);
        x64.update(chunk);
        x128.update(chunk);
        done += n as u64;
        if stop.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if last.elapsed() > Duration::from_millis(100) {
            last = Instant::now();
            if !progress(done as f64 / total as f64) {
                return Err("cancelled".into());
            }
        }
    }
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    Ok(vec![hex(&md5.finalize()), hex(&sha256.finalize()), hex(&sha512.finalize()), format!("{:016x}", x64.digest()), format!("{:032x}", x128.digest128())])
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_hashes() {
        let p = std::env::temp_dir().join(format!("kolphin-hash-{}", std::process::id()));
        std::fs::write(&p, "abc").unwrap();
        let h = super::hash_file(&p, &Default::default(), &mut |_| true).unwrap();
        std::fs::remove_file(&p).unwrap();
        assert_eq!(h[0], "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(h[1], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(h[3], format!("{:016x}", xxhash_rust::xxh3::xxh3_64(b"abc")));
    }
}

