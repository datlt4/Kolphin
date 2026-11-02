//! Drop Shelf (like Dropover): shake the pointer while dragging and a floating shelf appears next to it; drop files
//! there, keep working, drag them out later. X11 watches the pointer globally; everywhere there is Ctrl+Shift+D.
use crate::icons;
use gtk::{gdk, gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use x11_dl::xlib;

type Transfer = Box<dyn Fn(Vec<PathBuf>, bool)>;

pub struct Shelf {
    win: gtk::Window,
    list: gtk::Box,
    count: gtk::Label,
    items: RefCell<Vec<PathBuf>>,
    /// Shown by the shortcut: don't auto-hide while empty.
    pinned: Cell<bool>,
    /// Copy/Move into Kolphin's active folder.
    pub on_transfer: RefCell<Option<Transfer>>,
}

thread_local! {
    static SHELF: RefCell<Option<Rc<Shelf>>> = const { RefCell::new(None) };
}

pub fn get(app: &gtk::Application) -> Rc<Shelf> {
    SHELF.with(|s| s.borrow_mut().get_or_insert_with(|| Shelf::new(app)).clone())
}

impl Shelf {
    fn new(app: &gtk::Application) -> Rc<Self> {
        let count = gtk::Label::builder().label("Drop files here").hexpand(true).xalign(0.0).css_classes(["dim-label"]).build();
        let close = gtk::Button::builder().icon_name("window-close-symbolic").css_classes(["flat"]).build();
        let head = gtk::Box::builder().spacing(6).build();
        head.append(&gtk::Label::builder().label("Drop Shelf").css_classes(["heading"]).build());
        head.append(&count);
        head.append(&close);
        let list = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
        let scroll = gtk::ScrolledWindow::builder().child(&list).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).css_classes(["shelf-list"]).build();
        let buttons = gtk::Box::builder().spacing(6).homogeneous(true).build();
        let copy = gtk::Button::with_label("Copy here");
        let mv = gtk::Button::with_label("Move here");
        let clear = gtk::Button::with_label("Clear");
        for b in [&copy, &mv, &clear] {
            buttons.append(b);
        }
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).css_classes(["shelf"]).build();
        body.append(&head);
        body.append(&scroll);
        body.append(&buttons);
        let win = gtk::Window::builder().application(app).title("Drop Shelf").decorated(false).default_width(260).default_height(300).child(&body).build();
        win.set_hide_on_close(true);

        let shelf = Rc::new(Shelf { win, list, count, items: Default::default(), pinned: Default::default(), on_transfer: Default::default() });
        let weak = Rc::downgrade(&shelf);
        close.connect_clicked(glib::clone!(#[strong] weak, move |_| {
            if let Some(s) = weak.upgrade() {
                s.hide();
            }
        }));
        clear.connect_clicked(glib::clone!(#[strong] weak, move |_| {
            if let Some(s) = weak.upgrade() {
                s.clear();
            }
        }));
        for (b, cut) in [(&copy, false), (&mv, true)] {
            b.connect_clicked(glib::clone!(#[strong] weak, move |_| {
                let Some(s) = weak.upgrade() else { return };
                let items = s.items.take();
                if let Some(f) = s.on_transfer.borrow().as_ref() {
                    f(items, cut);
                }
                s.refresh();
                s.hide();
            }));
        }

        // Files dropped on the shelf are kept by reference.
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK);
        target.connect_drop(glib::clone!(
            #[strong] weak,
            move |_, value, _, _| {
                let (Some(s), Ok(list)) = (weak.upgrade(), value.get::<gdk::FileList>()) else { return false };
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
        shelf.win.add_controller(target);

        // Drag everything out at once; a completed drag empties the shelf.
        let src = gtk::DragSource::new();
        src.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
        src.connect_prepare(glib::clone!(
            #[strong] weak,
            move |src, _, _| {
                let s = weak.upgrade()?;
                let items = s.items.borrow();
                let first = items.first()?;
                let name = first.file_name().unwrap_or_default().to_string_lossy();
                if let Some(tex) = icons::texture(icons::icon_name(&name, first.is_dir()), 64) {
                    src.set_icon(Some(&tex), 32, 32);
                }
                let uris: Vec<String> = items.iter().map(|p| gio::File::for_path(p).uri().to_string()).collect();
                Some(gdk::ContentProvider::for_bytes("text/uri-list", &glib::Bytes::from_owned((uris.join("\r\n") + "\r\n").into_bytes())))
            }
        ));
        let cancelled = Rc::new(Cell::new(false));
        src.connect_drag_begin(glib::clone!(#[strong] cancelled, move |_, _| cancelled.set(false)));
        src.connect_drag_cancel(glib::clone!(
            #[strong] cancelled,
            move |_, _, _| {
                cancelled.set(true);
                false
            }
        ));
        src.connect_drag_end(glib::clone!(#[strong] weak, move |_, _, _| {
            if let (Some(s), false) = (weak.upgrade(), cancelled.get()) {
                s.clear();
                s.hide();
            }
        }));
        shelf.list.add_controller(src);
        shelf.refresh();
        shelf
    }

    fn refresh(&self) {
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        let items = self.items.borrow();
        self.count.set_text(&match items.len() {
            0 => "Drop files here".to_string(),
            n => format!("{n} item(s) · drag them out"),
        });
        for p in items.iter() {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let row = gtk::Box::builder().spacing(8).build();
            let img = gtk::Image::builder().pixel_size(24).build();
            img.set_paintable(icons::texture(icons::icon_name(&name, p.is_dir()), 48).as_ref());
            row.append(&img);
            row.append(&gtk::Label::builder().label(&name).ellipsize(gtk::pango::EllipsizeMode::Middle).xalign(0.0).build());
            self.list.append(&row);
        }
    }

    fn clear(&self) {
        self.items.borrow_mut().clear();
        self.refresh();
    }

    fn hide(&self) {
        self.pinned.set(false);
        self.win.set_visible(false);
    }

    pub fn is_empty(&self) -> bool {
        self.items.borrow().is_empty()
    }

    /// Show the shelf (top-left corner near screen point x, y on X11; wherever the WM puts it elsewhere).
    pub fn show_at(&self, pos: Option<(i32, i32)>, pinned: bool) {
        self.pinned.set(pinned || self.pinned.get());
        self.win.present();
        if let Some((x, y)) = pos {
            x11_place(&self.win, x, y);
        }
    }

    pub fn toggle(&self) {
        if self.win.is_visible() {
            self.hide();
        } else {
            self.show_at(None, true);
        }
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

/// Watch the pointer on X11 and pop the shelf up on a shake; an empty, unpinned shelf hides again once the
/// button is released.
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
        if shake.feed(rx, pressed, now) {
            get(&app).show_at(Some((rx + 24, ry + 24)), false);
        }
        released_at = if pressed { None } else { released_at.or(Some(now)) };
        let s = get(&app);
        if s.win.is_visible() && s.is_empty() && !s.pinned.get() && released_at.is_some_and(|t| now.duration_since(t) > Duration::from_millis(1500)) {
            s.hide();
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
