//! Drop Shelf (like Dropover): shake the pointer while dragging and a floating shelf appears next to it; drop files
//! there, keep working, drag them out later. X11 watches the pointer globally; everywhere there is Ctrl+Shift+D.
use crate::{icons, thumbs};
use gtk::{cairo, gdk, gio, glib, prelude::*};
use image::RgbaImage;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use x11_dl::xlib;

pub struct Shelf {
    win: gtk::Window,
    card: gtk::Overlay,
    stack: gtk::DrawingArea,
    hint: gtk::Label,
    pill: gtk::MenuButton,
    pill_label: gtk::Label,
    list: gtk::Box,
    items: RefCell<Vec<PathBuf>>,
    /// Pictures for the stack: thumbnails of images/videos, icons otherwise.
    pics: RefCell<HashMap<PathBuf, cairo::ImageSurface>>,
    requested: RefCell<Vec<PathBuf>>,
    /// Paths whose picture is a real thumbnail.
    thumbed: RefCell<HashSet<PathBuf>>,
    gen: Cell<u32>,
    thumb_owner: u64,
    thumb_tx: thumbs::Sender,
    /// One of our own drags is running: dropping it back on this shelf does nothing.
    dragging: Cell<bool>,
    /// Opened by the shortcut: don't auto-close while empty.
    pinned: Cell<bool>,
}

thread_local! {
    static SHELVES: RefCell<Vec<Rc<Shelf>>> = const { RefCell::new(Vec::new()) };
}

/// A new shelf (Ctrl+Shift+D), next to the pointer.
pub fn open(app: &gtk::Application) {
    Shelf::new(app).show_at(x11_pointer().map(|(x, y)| (x - 100, y - 100)), true);
}

fn uri_list(paths: &[PathBuf]) -> gdk::ContentProvider {
    let uris: Vec<String> = paths.iter().map(|p| gio::File::for_path(p).uri().to_string()).collect();
    gdk::ContentProvider::for_bytes("text/uri-list", &glib::Bytes::from_owned((uris.join("\r\n") + "\r\n").into_bytes()))
}

/// Drags from `widget` carry `paths()`; a completed drag hands them to `done`.
fn drag_out(widget: &impl IsA<gtk::Widget>, shelf: &Rc<Shelf>, paths: impl Fn(&Shelf) -> Vec<PathBuf> + 'static, done: impl Fn(&Rc<Shelf>, Vec<PathBuf>) + 'static) {
    let src = gtk::DragSource::new();
    src.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
    let weak = Rc::downgrade(shelf);
    let taken = Rc::new(RefCell::new(Vec::new()));
    src.connect_prepare(glib::clone!(
        #[strong] weak,
        #[strong] taken,
        move |src, _, _| {
            let s = weak.upgrade()?;
            let items = paths(&s);
            let first = items.first()?;
            let name = first.file_name().unwrap_or_default().to_string_lossy();
            if let Some(tex) = icons::texture(icons::icon_name(&name, first.is_dir()), 64) {
                src.set_icon(Some(&tex), 32, 32);
            }
            let content = uri_list(&items);
            *taken.borrow_mut() = items;
            Some(content)
        }
    ));
    let cancelled = Rc::new(Cell::new(false));
    src.connect_drag_begin(glib::clone!(
        #[strong] weak,
        #[strong] cancelled,
        move |_, _| {
            cancelled.set(false);
            if let Some(s) = weak.upgrade() {
                s.dragging.set(true);
            }
        }
    ));
    src.connect_drag_cancel(glib::clone!(
        #[strong] cancelled,
        move |_, _, _| {
            cancelled.set(true);
            false
        }
    ));
    src.connect_drag_end(move |_, _, _| {
        let Some(s) = weak.upgrade() else { return };
        s.dragging.set(false);
        if !cancelled.get() {
            done(&s, taken.take());
        }
    });
    widget.add_controller(src);
}

fn round_button(icon: &str) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).css_classes(["shelf-round"]).halign(gtk::Align::Start).valign(gtk::Align::Start).build()
}

impl Shelf {
    fn new(app: &gtk::Application) -> Rc<Self> {
        static OWNER: AtomicU64 = AtomicU64::new(1 << 62);
        let stack = gtk::DrawingArea::builder().content_width(150).content_height(124).halign(gtk::Align::Center).margin_top(30).margin_start(20).margin_end(20).margin_bottom(6).build();
        let hint = gtk::Label::builder().label("Drop files here").css_classes(["shelf-hint"]).build();
        let pill_label = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::Middle).max_width_chars(16).build();
        let pill_box = gtk::Box::builder().spacing(4).build();
        pill_box.append(&pill_label);
        pill_box.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
        let list = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).width_request(260).build();
        let scroll = gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).max_content_height(320).build();
        let pill = gtk::MenuButton::builder().popover(&gtk::Popover::builder().child(&scroll).build()).halign(gtk::Align::Center).css_classes(["shelf-pill"]).build();
        pill.set_child(Some(&pill_box));
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        body.append(&stack);
        body.append(&pill);

        let menu = gio::Menu::new();
        menu.append(Some("Copy to Current Folder"), Some("shelf.copy"));
        menu.append(Some("Move to Current Folder"), Some("shelf.move"));
        menu.append(Some("Clear"), Some("shelf.clear"));
        let more = gtk::MenuButton::builder().icon_name("view-more-symbolic").menu_model(&menu).halign(gtk::Align::End).valign(gtk::Align::Start).css_classes(["shelf-round"]).build();
        let close = round_button("window-close-symbolic");
        let card = gtk::Overlay::builder().child(&body).css_classes(["shelf"]).build();
        card.add_overlay(&hint);
        card.add_overlay(&close);
        card.add_overlay(&more);
        // Dragging the empty parts of the card moves the window.
        let handle = gtk::WindowHandle::builder().child(&card).build();
        let win = gtk::Window::builder().application(app).title("Drop Shelf").decorated(false).resizable(false).css_classes(["shelf-window"]).child(&handle).build();

        let (thumb_tx, rx) = async_channel::unbounded::<thumbs::Done>();
        let shelf = Rc::new(Shelf {
            win,
            card,
            stack,
            hint,
            pill,
            pill_label,
            list,
            items: Default::default(),
            pics: Default::default(),
            requested: Default::default(),
            thumbed: Default::default(),
            gen: Default::default(),
            thumb_owner: OWNER.fetch_add(1, Ordering::Relaxed),
            thumb_tx,
            dragging: Default::default(),
            pinned: Default::default(),
        });
        let weak = Rc::downgrade(&shelf);
        glib::spawn_future_local(glib::clone!(#[strong] weak, async move {
            while let Ok(done) = rx.recv().await {
                let Some(s) = weak.upgrade() else { return };
                s.on_thumb(done);
            }
        }));
        shelf.stack.set_draw_func(glib::clone!(#[strong] weak, move |_, cr, w, h| {
            if let Some(s) = weak.upgrade() {
                s.draw(cr, w, h);
            }
        }));
        close.connect_clicked(glib::clone!(#[strong] weak, move |_| {
            if let Some(s) = weak.upgrade() {
                s.close();
            }
        }));

        let actions = gio::SimpleActionGroup::new();
        for (name, cut) in [("copy", false), ("move", true)] {
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(glib::clone!(#[strong] weak, move |_, _| {
                let Some(s) = weak.upgrade() else { return };
                let items = s.items.take();
                crate::window::shelf_transfer(items, cut);
                s.close();
            }));
            actions.add_action(&a);
        }
        let clear = gio::SimpleAction::new("clear", None);
        clear.connect_activate(glib::clone!(#[strong] weak, move |_, _| {
            if let Some(s) = weak.upgrade() {
                s.items.borrow_mut().clear();
                s.refresh();
            }
        }));
        actions.add_action(&clear);
        shelf.win.insert_action_group("shelf", Some(&actions));

        // Files dropped on the shelf are kept by reference.
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK);
        target.connect_accept(glib::clone!(#[strong] weak, move |_, _| weak.upgrade().is_some_and(|s| !s.dragging.get())));
        target.connect_enter(glib::clone!(#[strong] weak, move |_, _, _| {
            if let Some(s) = weak.upgrade() {
                s.card.add_css_class("drop-hover");
            }
            gdk::DragAction::COPY
        }));
        target.connect_leave(glib::clone!(#[strong] weak, move |_| {
            if let Some(s) = weak.upgrade() {
                s.card.remove_css_class("drop-hover");
            }
        }));
        target.connect_drop(glib::clone!(
            #[strong] weak,
            move |_, value, _, _| {
                let (Some(s), Ok(list)) = (weak.upgrade(), value.get::<gdk::FileList>()) else { return false };
                s.card.remove_css_class("drop-hover");
                let mut items = s.items.borrow_mut();
                for p in list.files().iter().filter_map(|f| f.path()) {
                    if !items.contains(&p) {
                        items.push(p);
                    }
                }
                drop(items);
                s.refresh();
                true
            }
        ));
        shelf.card.add_controller(target);

        // Drag the whole stack out; a completed drag closes the shelf.
        drag_out(&shelf.stack, &shelf, |s| s.items.borrow().clone(), |s, _| s.close());
        shelf.refresh();
        SHELVES.with(|l| l.borrow_mut().push(shelf.clone()));
        shelf
    }

    fn refresh(self: &Rc<Self>) {
        let items = self.items.borrow();
        self.hint.set_visible(items.is_empty());
        self.pill.set_visible(!items.is_empty());
        self.pill_label.set_text(&summary(&items));
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        for p in items.iter() {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let row = gtk::Box::builder().spacing(8).css_classes(["shelf-row"]).build();
            let img = gtk::Image::builder().pixel_size(24).build();
            img.set_paintable(icons::texture(icons::icon_name(&name, p.is_dir()), 48).as_ref());
            row.append(&img);
            row.append(&gtk::Label::builder().label(&name).ellipsize(gtk::pango::EllipsizeMode::Middle).xalign(0.0).hexpand(true).build());
            // Takes it off the shelf; the file stays where it is.
            let remove = gtk::Button::builder().icon_name("window-close-symbolic").tooltip_text("Remove from shelf").css_classes(["flat", "circular", "shelf-remove"]).build();
            let weak = Rc::downgrade(self);
            let one = p.clone();
            remove.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade() {
                    s.remove(std::slice::from_ref(&one));
                }
            });
            row.append(&remove);
            // One file at a time out of the list.
            let one = p.clone();
            drag_out(&row, self, move |_| vec![one.clone()], |s, taken| s.remove(&taken));
            self.list.append(&row);
        }

        // Pictures for the top three.
        let gen = self.gen.get() + 1;
        self.gen.set(gen);
        let mut jobs = Vec::new();
        let mut requested = Vec::new();
        let mut pics = self.pics.borrow_mut();
        pics.retain(|p, _| items.contains(p));
        self.thumbed.borrow_mut().retain(|p| items.contains(p));
        for p in items.iter().rev().take(3) {
            if pics.contains_key(p) {
                continue;
            }
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            if let Some(s) = icons::texture(icons::icon_name(&name, p.is_dir()), 128).and_then(|t| texture_surface(&t)) {
                pics.insert(p.clone(), s);
            }
            if let Some(kind) = thumbs::kind(&name) {
                let mtime = std::fs::metadata(p).map_or(0, |m| m.mtime());
                jobs.push(thumbs::Job { key: (gen as u64) << 32 | requested.len() as u64, path: p.clone(), kind, px: thumbs::MAX_PX, mtime });
                requested.push(p.clone());
            }
        }
        *self.requested.borrow_mut() = requested;
        thumbs::request(self.thumb_owner, jobs, &self.thumb_tx);
        self.stack.queue_draw();
    }

    fn remove(self: &Rc<Self>, paths: &[PathBuf]) {
        self.items.borrow_mut().retain(|p| !paths.contains(p));
        if self.is_empty() {
            self.close();
        } else {
            self.refresh();
        }
    }

    fn on_thumb(&self, done: thumbs::Done) {
        if (done.key >> 32) as u32 != self.gen.get() {
            return;
        }
        let Some(path) = self.requested.borrow().get(done.key as u32 as usize).cloned() else { return };
        if let Some(s) = done.image.as_ref().and_then(rgba_surface) {
            self.pics.borrow_mut().insert(path.clone(), s);
            self.thumbed.borrow_mut().insert(path);
            self.stack.queue_draw();
        }
    }

    /// Up to three pictures fanned out, the newest in front; photos get a white frame.
    fn draw(&self, cr: &cairo::Context, w: i32, h: i32) {
        let items = self.items.borrow();
        let (w, h) = (w as f64, h as f64);
        if items.is_empty() {
            rounded(cr, 2.0, 2.0, w - 4.0, h - 4.0, 12.0);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.22);
            cr.set_line_width(1.5);
            cr.set_dash(&[6.0, 4.0], 0.0);
            let _ = cr.stroke();
            return;
        }
        let pics = self.pics.borrow();
        let thumbed = self.thumbed.borrow();
        let top: Vec<&PathBuf> = items.iter().rev().take(3).collect();
        let fan = [(0.0, 0.0), (-0.16, -14.0), (0.14, 14.0)];
        let box_px = (w.min(h) - 24.0).max(40.0);
        for (depth, p) in top.iter().enumerate().rev() {
            let Some(s) = pics.get(*p) else { continue };
            let (angle, dx) = fan[depth];
            let (sw, sh) = (s.width() as f64, s.height() as f64);
            let photo = thumbed.contains(*p);
            let fit = if photo { box_px } else { box_px * 0.8 };
            let k = fit / sw.max(sh);
            let (dw, dh) = (sw * k, sh * k);
            cr.save().ok();
            cr.translate(w / 2.0 + dx, h / 2.0);
            cr.rotate(angle);
            if photo {
                let b = 4.0;
                for (spread, alpha) in [(4.0, 0.10), (2.0, 0.18)] {
                    cr.rectangle(-dw / 2.0 - b - spread + 1.0, -dh / 2.0 - b - spread + 3.0, dw + 2.0 * (b + spread), dh + 2.0 * (b + spread));
                    cr.set_source_rgba(0.0, 0.0, 0.0, alpha);
                    let _ = cr.fill();
                }
                cr.rectangle(-dw / 2.0 - b, -dh / 2.0 - b, dw + 2.0 * b, dh + 2.0 * b);
                cr.set_source_rgb(1.0, 1.0, 1.0);
                let _ = cr.fill();
            }
            cr.translate(-dw / 2.0, -dh / 2.0);
            cr.scale(k, k);
            let _ = cr.set_source_surface(s, 0.0, 0.0);
            cr.source().set_filter(cairo::Filter::Good);
            cr.rectangle(0.0, 0.0, sw, sh);
            let _ = cr.fill();
            cr.restore().ok();
        }
    }

    fn close(&self) {
        SHELVES.with(|l| l.borrow_mut().retain(|s| !std::ptr::eq(Rc::as_ptr(s), self)));
        self.win.destroy();
    }

    fn is_empty(&self) -> bool {
        self.items.borrow().is_empty()
    }

    /// Show the shelf with its top-left corner at screen point x, y (X11; wherever the WM puts it elsewhere).
    fn show_at(&self, pos: Option<(i32, i32)>, pinned: bool) {
        self.pinned.set(pinned || self.pinned.get());
        self.win.present();
        if let Some((x, y)) = pos {
            x11_place(&self.win, x, y);
        }
    }
}

/// "2 Images", "3 Folders", "5 Items"; one item is shown by name.
fn summary(items: &[PathBuf]) -> String {
    if let [p] = items {
        return p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    }
    let kind = |p: &PathBuf| thumbs::kind(&p.file_name().unwrap_or_default().to_string_lossy());
    let n = items.len();
    if items.iter().all(|p| kind(p) == Some(thumbs::Kind::Image)) {
        format!("{n} Images")
    } else if items.iter().all(|p| kind(p) == Some(thumbs::Kind::Video)) {
        format!("{n} Videos")
    } else if items.iter().all(|p| p.is_dir()) {
        format!("{n} Folders")
    } else {
        format!("{n} Items")
    }
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

fn texture_surface(tex: &gdk::Texture) -> Option<cairo::ImageSurface> {
    let mut s = cairo::ImageSurface::create(cairo::Format::ARgb32, tex.width(), tex.height()).ok()?;
    let stride = s.stride() as usize;
    // GDK's default download format is cairo's ARGB32 (premultiplied, native endian).
    tex.download(&mut s.data().ok()?, stride);
    Some(s)
}

fn rgba_surface(img: &RgbaImage) -> Option<cairo::ImageSurface> {
    let (w, h) = img.dimensions();
    let mut s = cairo::ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32).ok()?;
    let stride = s.stride() as usize;
    {
        let mut d = s.data().ok()?;
        for (y, row) in img.rows().enumerate() {
            for (x, px) in row.enumerate() {
                let [r, g, b, a] = px.0;
                let m = |c: u8| (c as u16 * a as u16 / 255) as u8;
                let px = (a as u32) << 24 | (m(r) as u32) << 16 | (m(g) as u32) << 8 | m(b) as u32;
                d[y * stride + x * 4..][..4].copy_from_slice(&px.to_ne_bytes());
            }
        }
    }
    Some(s)
}

fn x11_pointer() -> Option<(i32, i32)> {
    let xl = xlib::Xlib::open().ok()?;
    // SAFETY: a short-lived display connection; XQueryPointer only writes the out-parameters.
    unsafe {
        let d = (xl.XOpenDisplay)(std::ptr::null());
        if d.is_null() {
            return None;
        }
        let (mut rr, mut cr, mut rx, mut ry, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0, 0, 0);
        let ok = (xl.XQueryPointer)(d, (xl.XDefaultRootWindow)(d), &mut rr, &mut cr, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask);
        (xl.XCloseDisplay)(d);
        (ok != 0).then_some((rx, ry))
    }
}

/// Move a window and keep it above others (X11 only; no-op elsewhere).
fn x11_place(win: &gtk::Window, x: i32, y: i32) {
    let Some(surface) = win.surface() else { return };
    let Ok(xs) = surface.downcast::<gdk4_x11::X11Surface>() else { return };
    let Ok(xl) = xlib::Xlib::open() else { return };
    // SAFETY: plain Xlib calls on our own display connection, closed before returning.
    unsafe {
        let d = (xl.XOpenDisplay)(std::ptr::null());
        if d.is_null() {
            return;
        }
        let xid = xs.xid();
        (xl.XMoveWindow)(d, xid, x, y);
        // _NET_WM_STATE_ADD _NET_WM_STATE_ABOVE
        let state = (xl.XInternAtom)(d, c"_NET_WM_STATE".as_ptr(), 0);
        let above = (xl.XInternAtom)(d, c"_NET_WM_STATE_ABOVE".as_ptr(), 0);
        let mut ev: xlib::XEvent = std::mem::zeroed();
        ev.client_message.type_ = xlib::ClientMessage;
        ev.client_message.window = xid;
        ev.client_message.message_type = state;
        ev.client_message.format = 32;
        ev.client_message.data.set_long(0, 1);
        ev.client_message.data.set_long(1, above as i64);
        let root = (xl.XDefaultRootWindow)(d);
        (xl.XSendEvent)(d, root, 0, xlib::SubstructureRedirectMask | xlib::SubstructureNotifyMask, &mut ev);
        (xl.XFlush)(d);
        (xl.XCloseDisplay)(d);
    }
}

/// Pointer shaken sideways while the left button is held: 4+ direction changes of 30+ px within 0.7 s.
#[derive(Default)]
pub struct ShakeDetector {
    extreme: i32,
    dir: i32,
    turns: Vec<Instant>,
    cooldown: Option<Instant>,
}

impl ShakeDetector {
    pub fn feed(&mut self, x: i32, pressed: bool, now: Instant) -> bool {
        if !pressed {
            self.turns.clear();
            self.dir = 0;
            self.extreme = x;
            return false;
        }
        let d = x - self.extreme;
        if self.dir == 0 {
            if d.abs() >= 30 {
                self.dir = d.signum();
                self.extreme = x;
            }
        } else if d.signum() == self.dir {
            self.extreme = x;
        } else if d.abs() >= 30 {
            self.dir = -self.dir;
            self.extreme = x;
            self.turns.push(now);
        }
        self.turns.retain(|t| now.duration_since(*t) < Duration::from_millis(700));
        let cool = self.cooldown.is_none_or(|c| now.duration_since(c) > Duration::from_secs(2));
        if self.turns.len() >= 4 && cool {
            self.turns.clear();
            self.cooldown = Some(now);
            return true;
        }
        false
    }
}

/// Watch the pointer on X11: a shake pops up an empty shelf (a new one unless one is already waiting); empty,
/// unpinned shelves close again once the button is released.
pub fn watch(app: &gtk::Application) {
    if gdk::Display::default().and_then(|d| d.downcast::<gdk4_x11::X11Display>().ok()).is_none() {
        return;
    }
    let Ok(xl) = xlib::Xlib::open() else { return };
    // SAFETY: a private display connection kept for the life of the app.
    let d = unsafe { (xl.XOpenDisplay)(std::ptr::null()) };
    if d.is_null() {
        return;
    }
    let root = unsafe { (xl.XDefaultRootWindow)(d) };
    let app = app.clone();
    let mut shake = ShakeDetector::default();
    let mut released_at: Option<Instant> = None;
    glib::timeout_add_local(Duration::from_millis(30), move || {
        let (mut rr, mut cr, mut rx, mut ry, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0, 0, 0);
        // SAFETY: XQueryPointer only writes the out-parameters.
        unsafe { (xl.XQueryPointer)(d, root, &mut rr, &mut cr, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask) };
        let pressed = mask & xlib::Button1Mask != 0;
        let now = Instant::now();
        let shelves = SHELVES.with(|l| l.borrow().clone());
        if shake.feed(rx, pressed, now) {
            let s = shelves.iter().find(|s| s.is_empty()).cloned().unwrap_or_else(|| Shelf::new(&app));
            s.show_at(Some((rx + 24, ry + 24)), false);
        }
        released_at =if pressed { None } else { released_at.or(Some(now)) };
        if released_at.is_some_and(|t| now.duration_since(t) > Duration::from_millis(1500)) {
            for s in shelves.iter().filter(|s| s.is_empty() && !s.pinned.get()) {
                s.close();
            }
        }
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::ShakeDetector;
    use std::time::{Duration, Instant};

    #[test]
    fn shake_needs_button_and_turns() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut s = ShakeDetector::default();
        // Left-right-left-right-left with the button held: fires.
        let xs = [0, 60, 0, 60, 0, 60];
        let fired: Vec<bool> = xs.iter().enumerate().map(|(i, &x)| s.feed(x, true, at(i as u64 * 80))).collect();
        assert!(fired.contains(&true));
        // Same motion without the button: never.
        let mut s = ShakeDetector::default();
        assert!(!xs.iter().enumerate().any(|(i, &x)| s.feed(x, false, at(i as u64 * 80))));
        // Too slow (one turn a second): never.
        let mut s = ShakeDetector::default();
        assert!(!xs.iter().enumerate().any(|(i, &x)| s.feed(x, true, at(i as u64 * 1000))));
        // Small jitter: never.
        let mut s = ShakeDetector::default();
        assert!(!(0..20).any(|i| s.feed(if i % 2 == 0 { 0 } else { 10 }, true, at(i * 40))));
    }
}
