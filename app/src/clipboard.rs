//! Cut / copy / paste of files through the system clipboard (Nautilus/GNOME format).
use crate::ops;
use crate::notify::{self, ToastExt};
use crate::pane::Pane;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::PathBuf;
use std::rc::Rc;

const GNOME: &str = "x-special/gnome-copied-files";

pub fn copy(pane: &Pane, cut: bool, toasts: &gtk::Overlay) {
    let paths = pane.selected_paths();
    if paths.is_empty() {
        toasts.add_toast(notify::Toast::new("Nothing selected"));
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
        toasts.add_toast(notify::Toast::new(&format!("{} {} item(s)", if cut { "Cut" } else { "Copied" }, paths.len())));
    }
}

pub fn paste(pane: &Rc<Pane>, toasts: &gtk::Overlay) {
    let clip = pane.root.clipboard();
    let dest = pane.path();
    glib::spawn_future_local(glib::clone!(#[weak] toasts, async move {
        let Ok((stream, mime)) = clip.read_future(&[GNOME, "text/uri-list"], glib::Priority::DEFAULT).await else {
            toasts.add_toast(notify::Toast::new("The clipboard has no files"));
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
        if cut {
            let _ = clip.set_content(None::<&gdk::ContentProvider>);
        }
        run_transfer(&toasts, sources, dest, cut);
    }));
}

/// Start a copy/move job and show its progress in a toast; the result can be undone.
pub fn run_transfer(toasts: &gtk::Overlay, sources: Vec<PathBuf>, dest: PathBuf, cut: bool) {
    let status = notify::Toast::builder().title(if cut { "Moving…" } else { "Copying…" }).timeout(0).build();
    toasts.add_toast(status.clone());
    let rx = ops::transfer(sources, dest, cut);
    let started = std::time::Instant::now();
    glib::spawn_future_local(glib::clone!(#[weak] toasts, async move {
        while let Ok(p) = rx.recv().await {
            match p.finished {
                None => status.set_title(&ops::describe(&p, cut, started)),
                Some(result) => {
                    status.dismiss();
                    let msg = match result {
                        Ok(undo) => {
                            ops::record(undo);
                            format!("{} {} item(s) · Ctrl+Z to undo", if cut { "Moved" } else { "Copied" }, p.done_items)
                        }
                        Err(e) => format!("{} failed: {e}", if cut { "Move" } else { "Copy" }),
                    };
                    toasts.add_toast(notify::Toast::new(&msg));
                    return;
                }
            }
        }
    }));
}

/// (cut?, local paths) from clipboard text.
fn parse(mime: &str, text: &str) -> (bool, Vec<PathBuf>) {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let (cut, uris) = if mime == GNOME { (lines.first() == Some(&"cut"), lines.get(1..).unwrap_or_default()) } else { (false, &lines[..]) };
    (cut, uris.iter().filter_map(|u| gio::File::for_uri(u).path()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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
