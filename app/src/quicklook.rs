//! Quick Look (Space): a large preview of the item under the cursor; arrows move through the folder.
use crate::pane::Pane;
use crate::thumbs::{self, Kind};
use gtk::{gdk, gio, glib, prelude::*};
use std::cell::Cell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

pub fn show(parent: &gtk::Window, pane: &Rc<Pane>) {
    let Some(start) = pane.canvas.cursor_position() else { return };
    let title = gtk::Label::builder().css_classes(["heading"]).ellipsize(gtk::pango::EllipsizeMode::Middle).build();
    let header = gtk::HeaderBar::builder().title_widget(&title).build();
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).vexpand(true).build();
    let info = gtk::Label::builder().xalign(0.0).css_classes(["statusbar"]).build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&content);
    body.append(&info);
    let win = gtk::Window::builder().title("Quick Look").transient_for(parent).default_width(960).default_height(720).titlebar(&header).child(&body).build();

    let pos = Rc::new(Cell::new(start));
    let weak: Weak<Pane> = Rc::downgrade(pane);
    let load = Rc::new(glib::clone!(#[weak] content, #[weak] title, #[weak] info, #[strong] weak, move |p: usize| {
        let Some(pane) = weak.upgrade() else { return };
        let Some(path) = pane.path_at(p) else { return };
        pane.canvas.select_position(p);
        title.set_text(&path.file_name().unwrap_or_default().to_string_lossy());
        info.set_text(&describe(&path));
        while let Some(c) = content.first_child() {
            content.remove(&c);
        }
        content.append(&preview(&path));
    }));
    load(start);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak] win,
        #[strong] weak,
        #[upgrade_or] glib::Propagation::Proceed,
        move |_, key, _, _| {
            let Some(pane) = weak.upgrade() else { return glib::Propagation::Proceed };
            let len = pane.canvas.model().order.len();
            let p = pos.get();
            let next = match key {
                gdk::Key::space | gdk::Key::Escape => {
                    win.close();
                    return glib::Propagation::Stop;
                }
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    if let Some(path) = pane.path_at(p) {
                        let _ = gio::AppInfo::launch_default_for_uri(&gio::File::for_path(path).uri(), Some(&WidgetExt::display(&win).app_launch_context()));
                    }
                    return glib::Propagation::Stop;
                }
                gdk::Key::Right | gdk::Key::Down => (p + 1).min(len.saturating_sub(1)),
                gdk::Key::Left | gdk::Key::Up => p.saturating_sub(1),
                _ => return glib::Propagation::Proceed,
            };
            if next != p {
                pos.set(next);
                load(next);
            }
            glib::Propagation::Stop
        }
    ));
    win.add_controller(keys);
    win.present();
}

fn describe(path: &Path) -> String {
    let Ok(m) = std::fs::metadata(path) else { return String::new() };
    let date = glib::DateTime::from_unix_local(std::os::unix::fs::MetadataExt::mtime(&m)).ok().and_then(|d| d.format("%Y-%m-%d %H:%M").ok());
    let mut s = if m.is_dir() { "Folder".to_string() } else { glib::format_size(m.len()).to_string() };
    if let Ok(d) = imagesize::size(path) {
        s += &format!("  ·  {} × {}", d.width, d.height);
    }
    if let Some(d) = date {
        s += &format!("  ·  {d}");
    }
    s + "  ·  ←/→ next · Enter open · Space close"
}

fn picture(tex: Option<gdk::Texture>) -> gtk::Picture {
    let pic = gtk::Picture::builder().can_shrink(true).keep_aspect_ratio(true).vexpand(true).hexpand(true).build();
    pic.set_paintable(tex.as_ref());
    pic
}

fn preview(path: &Path) -> gtk::Widget {
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if path.is_dir() {
        let n = std::fs::read_dir(path).map_or(0, |r| r.count());
        return message(&format!("{name}\n\n{n} items"));
    }
    match thumbs::kind(&name) {
        Some(kind) => {
            // Decode off the UI thread; large images are scaled to fit 2048 px.
            let pic = picture(None);
            let path = path.to_path_buf();
            glib::spawn_future_local(glib::clone!(#[weak] pic, async move {
                let img = gio::spawn_blocking(move || match kind {
                    Kind::Image => image::ImageReader::open(&path).ok()?.with_guessed_format().ok()?.decode().ok().map(|i| i.thumbnail(2048, 2048).to_rgba8()),
                    Kind::Video => video_frame(&path),
                })
                .await
                .ok()
                .flatten();
                if let Some(img) = img {
                    let (w, h) = img.dimensions();
                    let bytes = glib::Bytes::from_owned(img.into_raw());
                    pic.set_paintable(Some(&gdk::MemoryTexture::new(w as i32, h as i32, gdk::MemoryFormat::R8g8b8a8, &bytes, w as usize * 4)));
                }
            }));
            pic.upcast()
        }
        None => match read_text(path) {
            Some(text) => {
                let view = gtk::TextView::builder().editable(false).monospace(true).wrap_mode(gtk::WrapMode::WordChar).left_margin(16).top_margin(12).build();
                view.buffer().set_text(&text);
                gtk::ScrolledWindow::builder().child(&view).vexpand(true).build().upcast()
            }
            None => message(&format!("{name}\n\nNo preview for this file type")),
        },
    }
}

fn message(text: &str) -> gtk::Widget {
    gtk::Label::builder().label(text).justify(gtk::Justification::Center).vexpand(true).css_classes(["dim-label"]).build().upcast()
}

/// First 256 KB of a text file, or None for binary data.
fn read_text(path: &Path) -> Option<String> {
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(256 << 10).read_to_end(&mut buf).ok()?;
    if buf.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn video_frame(path: &Path) -> Option<image::RgbaImage> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // Unique per call so concurrent previews never share a /tmp name.
    let out: PathBuf = std::env::temp_dir().join(format!(
        "kolphin-ql-{}-{}-{}.png",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
    ));
    let ok = std::process::Command::new("ffmpegthumbnailer")
        .arg("-i")
        .arg(path)
        .arg("-o")
        .arg(&out)
        .args(["-s", "1280", "-c", "png"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let img = ok.then(|| image::open(&out).ok().map(|i| i.to_rgba8())).flatten();
    let _ = std::fs::remove_file(&out);
    img
}
