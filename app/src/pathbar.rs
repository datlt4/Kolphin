//! Location bar: one button per folder level; the full editable path when focused (Ctrl+L or click on empty space).
use gtk::{glib, prelude::*};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

type PathCallback = RefCell<Option<Box<dyn Fn(PathBuf)>>>;

pub struct PathBar {
    pub root: gtk::Stack,
    crumbs: gtk::Box,
    scroller: gtk::ScrolledWindow,
    entry: gtk::Entry,
    path: RefCell<PathBuf>,
    on_go: PathCallback,
    on_cancel: RefCell<Option<Box<dyn Fn()>>>,
}

impl PathBar {
    pub fn new() -> Rc<Self> {
        let crumbs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&crumbs)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hscrollbar_policy(gtk::PolicyType::External)
            .hexpand(true)
            .build();
        let entry = gtk::Entry::builder().hexpand(true).build();
        let root = gtk::Stack::builder().hhomogeneous(false).vhomogeneous(true).build();
        root.add_css_class("pathbar");
        root.add_named(&scroller, Some("crumbs"));
        root.add_named(&entry, Some("entry"));
        let bar = Rc::new(PathBar {
            root,
            crumbs,
            scroller,
            entry,
            path: Default::default(),
            on_go: Default::default(),
            on_cancel: Default::default(),
        });

        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(#[weak] bar, move |_, _, _, _| bar.edit()));
        bar.scroller.add_controller(click);
        // The mouse wheel scrolls long paths sideways.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        wheel.connect_scroll(glib::clone!(
            #[weak] bar,
            #[upgrade_or] glib::Propagation::Proceed,
            move |_, dx, dy| {
                let adj = bar.scroller.hadjustment();
                adj.set_value(adj.value() + (dx + dy) * 40.0);
                glib::Propagation::Stop
            }
        ));
        bar.scroller.add_controller(wheel);

        bar.entry.connect_activate(glib::clone!(#[weak] bar, move |e| {
            let text = e.text();
            let path = match text.strip_prefix('~') {
                Some(rest) => glib::home_dir().join(rest.trim_start_matches('/')),
                None => PathBuf::from(text.as_str()),
            };
            if path.is_dir() {
                e.remove_css_class("error");
                bar.show_crumbs();
                bar.go(path);
            } else {
                e.add_css_class("error");
            }
        }));
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak] bar,
            #[upgrade_or] glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key != gtk::gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                bar.show_crumbs();
                if let Some(f) = bar.on_cancel.borrow().as_ref() {
                    f();
                }
                glib::Propagation::Stop
            }
        ));
        bar.entry.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(glib::clone!(#[weak] bar, move |_| bar.show_crumbs()));
        bar.entry.add_controller(focus);
        bar
    }

    pub fn connect_go(&self, f: impl Fn(PathBuf) + 'static) {
        *self.on_go.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_cancel(&self, f: impl Fn() + 'static) {
        *self.on_cancel.borrow_mut() = Some(Box::new(f));
    }

    fn go(&self, path: PathBuf) {
        if let Some(f) = self.on_go.borrow().as_ref() {
            f(path);
        }
    }

    pub fn edit(&self) {
        self.entry.set_text(&self.path.borrow().to_string_lossy());
        self.entry.remove_css_class("error");
        self.root.set_visible_child_name("entry");
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    fn show_crumbs(&self) {
        self.root.set_visible_child_name("crumbs");
    }

    pub fn set_path(self: &Rc<Self>, path: &Path) {
        *self.path.borrow_mut() = path.to_path_buf();
        while let Some(c) = self.crumbs.first_child() {
            self.crumbs.remove(&c);
        }
        let home = glib::home_dir();
        let (mut acc, rest, first) = match path.strip_prefix(&home) {
            Ok(rest) => (home.clone(), rest, ("user-home-symbolic", "Home")),
            Err(_) => (PathBuf::from("/"), path.strip_prefix("/").unwrap_or(path), ("drive-harddisk-symbolic", "")),
        };
        self.add_crumb(Some(first.0), first.1, acc.clone());
        for c in rest.components() {
            acc.push(c);
            self.crumbs.append(&gtk::Label::builder().label("›").css_classes(["dim-label"]).build());
            self.add_crumb(None, &c.as_os_str().to_string_lossy(), acc.clone());
        }
        if let Some(last) = self.crumbs.last_child() {
            last.add_css_class("current");
        }
        // Keep the deepest folder visible on long paths.
        let adj = self.scroller.hadjustment();
        glib::idle_add_local_once(move || adj.set_value(adj.upper()));
        self.show_crumbs();
    }

    fn add_crumb(self: &Rc<Self>, icon: Option<&str>, label: &str, path: PathBuf) {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        if let Some(icon) = icon {
            content.append(&gtk::Image::from_icon_name(icon));
        }
        if !label.is_empty() {
            content.append(&gtk::Label::new(Some(label)));
        }
        let button = gtk::Button::builder().child(&content).css_classes(["flat"]).build();
        button.connect_clicked(glib::clone!(#[weak(rename_to = bar)] self, move |_| bar.go(path.clone())));
        self.crumbs.append(&button);
    }
}
