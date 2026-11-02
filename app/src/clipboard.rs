//! Cut / copy / paste of files through the system clipboard (Nautilus/GNOME format), transfers in the background.
use crate::pane::Pane;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const GNOME: &str = "x-special/gnome-copied-files";

pub fn copy(pane: &Pane, cut: bool, toasts: &adw::ToastOverlay) {
    let paths = pane.selected_paths();
    if paths.is_empty() {
        toasts.add_toast(adw::Toast::new("Nothing selected"));
        return;
    }
    let uris: Vec<String> = paths.iter().map(|p| gio::File::for_path(p).uri().to_string()).collect();
    let gnome = format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"));
    let text = paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n");
    let provider = gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_bytes(GNOME, &glib::Bytes::from_owned(gnome.into_bytes())),
        gdk::ContentProvider::for_bytes("text/uri-list", &glib::Bytes::from_owned((uris.join("\r\n") + "\r\n").into_bytes())),
        gdk::ContentProvider::for_value(&text.to_value()),
    ]);
    if pane.root.clipboard().set_content(Some(&provider)).is_ok() {
        toasts.add_toast(adw::Toast::new(&format!("{} {} item(s)", if cut { "Cut" } else { "Copied" }, paths.len())));
    }
}

pub fn paste(pane: &Rc<Pane>, toasts: &adw::ToastOverlay) {
    let clip = pane.root.clipboard();
    let dest = pane.path();
    glib::spawn_future_local(glib::clone!(#[weak] toasts, async move {
        let Ok((stream, mime)) = clip.read_future(&[GNOME, "text/uri-list"], glib::Priority::DEFAULT).await else {
            toasts.add_toast(adw::Toast::new("The clipboard has no files"));
            return;
        };
        let out = gio::MemoryOutputStream::new_resizable();
        let flags = gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET;
        if out.splice_future(&stream, flags, glib::Priority::DEFAULT).await.is_err() {
            return;
        }
        let (cut, sources) = parse(&mime, &String::from_utf8_lossy(&out.steal_as_bytes()));
        if sources.is_empty() {
            return;
        }
        let status = adw::Toast::builder().title(format!("{} {} item(s)…", if cut { "Moving" } else { "Copying" }, sources.len())).timeout(0).build();
        toasts.add_toast(status.clone());
        let res = gio::spawn_blocking(move || transfer(&sources, &dest, cut)).await.unwrap_or_else(|_| Err("transfer crashed".into()));
        status.dismiss();
        match res {
            Ok(n) => {
                if cut {
                    let _ = clip.set_content(None::<&gdk::ContentProvider>);
                }
                toasts.add_toast(adw::Toast::new(&format!("{} {n} item(s)", if cut { "Moved" } else { "Pasted" })));
            }
            Err(e) => toasts.add_toast(adw::Toast::new(&format!("Paste failed: {e}"))),
        }
    }));
}

/// (cut?, local paths) from clipboard text.
fn parse(mime: &str, text: &str) -> (bool, Vec<PathBuf>) {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let (cut, uris) = if mime == GNOME { (lines.first() == Some(&"cut"), lines.get(1..).unwrap_or_default()) } else { (false, &lines[..]) };
    (cut, uris.iter().filter_map(|u| gio::File::for_uri(u).path()).collect())
}

/// Copy or move `sources` into `dest`; existing names get " (copy)", " (copy 2)", …
fn transfer(sources: &[PathBuf], dest: &Path, cut: bool) -> Result<usize, String> {
    let mut done = 0;
    for src in sources {
        let name = src.file_name().ok_or("bad path")?;
        if cut && src.parent() == Some(dest) {
            continue;
        }
        if dest.starts_with(src) {
            return Err(format!("cannot put {} inside itself", src.display()));
        }
        let target = unique(&dest.join(name));
        let moved = cut && std::fs::rename(src, &target).is_ok();
        if !moved {
            copy_tree(src, &target).map_err(|e| format!("{}: {e}", src.display()))?;
            if cut {
                let removed = if src.is_dir() && !src.is_symlink() { std::fs::remove_dir_all(src) } else { std::fs::remove_file(src) };
                removed.map_err(|e| format!("{}: {e}", src.display()))?;
            }
        }
        done += 1;
    }
    Ok(done)
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if meta.is_dir() {
        std::fs::create_dir(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
        std::fs::set_permissions(dst, meta.permissions())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

fn unique(path: &Path) -> PathBuf {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_move_and_names() {
        let root = std::env::temp_dir().join(format!("kolphin-clip-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("f.txt"), "x").unwrap();
        std::fs::write(a.join("sub/g.txt"), "y").unwrap();
        std::os::unix::fs::symlink("f.txt", a.join("link")).unwrap();

        assert_eq!(transfer(&[a.join("f.txt"), a.join("sub"), a.join("link")], &b, false), Ok(3));
        assert!(b.join("sub/g.txt").exists() && b.join("link").is_symlink());
        assert_eq!(transfer(&[a.join("f.txt")], &b, false), Ok(1));
        assert!(b.join("f (copy).txt").exists());
        assert_eq!(transfer(&[a.join("f.txt")], &a, false), Ok(1));
        assert!(a.join("f (copy).txt").exists());

        assert_eq!(transfer(&[a.join("sub")], &b, true), Ok(1));
        assert!(!a.join("sub").exists() && b.join("sub (copy)/g.txt").exists());
        assert!(transfer(std::slice::from_ref(&b), &b.join("sub"), false).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parses_clipboard() {
        let (cut, p) = parse(GNOME, "cut\nfile:///tmp/a%20b\nfile:///tmp/c");
        assert!(cut);
        assert_eq!(p, [PathBuf::from("/tmp/a b"), PathBuf::from("/tmp/c")]);
        let (cut, p) = parse("text/uri-list", "# comment\r\nfile:///x\r\n");
        assert!(!cut);
        assert_eq!(p, [PathBuf::from("/x")]);
    }
}
