//! Main window: places sidebar, tabs, split view, header bar and window actions.
use crate::canvas::ViewMode;
use crate::pane::Pane;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

struct Tab {
    page: adw::TabPage,
    paned: gtk::Paned,
    panes: RefCell<Vec<Rc<Pane>>>,
    active: Cell<usize>,
}

impl Tab {
    fn pane(&self) -> Rc<Pane> {
        let panes = self.panes.borrow();
        panes[self.active.get().min(panes.len() - 1)].clone()
    }
}

pub struct Win {
    window: adw::ApplicationWindow,
    tabs: adw::TabView,
    pub toasts: adw::ToastOverlay,
    tab_list: RefCell<Vec<Rc<Tab>>>,
    show_hidden: Cell<bool>,
    places: gtk::ListBox,
    place_paths: RefCell<Vec<PathBuf>>,
}

thread_local! {
    static WINDOWS: RefCell<Vec<Rc<Win>>> = const { RefCell::new(Vec::new()) };
    static UPDATE_CHECKED: Cell<bool> = const { Cell::new(false) };
}

pub fn set_accels(app: &adw::Application) {
    for (action, keys) in [
        ("win.back", &["<Alt>Left"][..]),
        ("win.forward", &["<Alt>Right"]),
        ("win.up", &["<Alt>Up"]),
        ("win.home", &["<Alt>Home"]),
        ("win.reload", &["F5", "<Control>r"]),
        ("win.location", &["<Control>l", "F6"]),
        ("win.toggle-hidden", &["<Control>h"]),
        ("win.split", &["F3"]),
        ("win.new-tab", &["<Control>t"]),
        ("win.close-tab", &["<Control>w"]),
        ("win.view-mode('icons')", &["<Control>1"]),
        ("win.view-mode('compact')", &["<Control>2"]),
        ("win.view-mode('details')", &["<Control>3"]),
        ("win.zoom-in", &["<Control>plus", "<Control>equal"]),
        ("win.zoom-out", &["<Control>minus"]),
    ] {
        app.set_accels_for_action(action, keys);
    }
}

pub fn open(app: &adw::Application, paths: &[PathBuf]) {
    let win = Win::new(app);
    if paths.is_empty() {
        win.add_tab(glib::home_dir());
    }
    for p in paths {
        win.add_tab(p.clone());
    }
    win.window.present();
    if !UPDATE_CHECKED.replace(true) {
        crate::check_update(&win.toasts, false);
    }
    WINDOWS.with(|w| w.borrow_mut().push(win));
}

fn mode_name(m: ViewMode) -> &'static str {
    match m {
        ViewMode::Icons => "icons",
        ViewMode::Compact => "compact",
        ViewMode::Details => "details",
    }
}

impl Win {
    fn new(app: &adw::Application) -> Rc<Self> {
        let header = adw::HeaderBar::new();
        for (icon, action, tip) in [("go-previous-symbolic", "win.back", "Back"), ("go-next-symbolic", "win.forward", "Forward"), ("go-up-symbolic", "win.up", "Up")] {
            header.pack_start(&gtk::Button::builder().icon_name(icon).action_name(action).tooltip_text(tip).build());
        }
        let menu = gio::Menu::new();
        menu.append(Some("New Tab"), Some("win.new-tab"));
        menu.append(Some("Show Hidden Files"), Some("win.toggle-hidden"));
        menu.append(Some("Check for Updates"), Some("win.check-update"));
        header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).build());
        header.pack_end(&gtk::ToggleButton::builder().icon_name("view-dual-symbolic").action_name("win.split").tooltip_text("Split View (F3)").build());
        let modes = gtk::Box::builder().css_classes(["linked"]).build();
        for (icon, mode, tip) in [("view-grid-symbolic", "icons", "Icons"), ("view-continuous-symbolic", "compact", "Compact"), ("view-list-symbolic", "details", "Details")] {
            modes.append(
                &gtk::ToggleButton::builder()
                    .icon_name(icon)
                    .action_name("win.view-mode")
                    .action_target(&mode.to_variant())
                    .tooltip_text(tip)
                    .build(),
            );
        }
        header.pack_end(&modes);

        let tabs = adw::TabView::new();
        let tab_bar = adw::TabBar::builder().view(&tabs).autohide(true).build();
        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main.append(&tab_bar);
        main.append(&tabs);
        tabs.set_vexpand(true);

        let places = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
        let sidebar = gtk::ScrolledWindow::builder().child(&places).hscrollbar_policy(gtk::PolicyType::Never).width_request(180).build();
        let split = gtk::Paned::builder().start_child(&sidebar).end_child(&main).position(200).shrink_start_child(false).build();
        split.set_vexpand(true);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&split);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&content));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Kolphin")
            .icon_name("kolphin")
            .default_width(1200)
            .default_height(760)
            .content(&toasts)
            .build();

        let win = Rc::new(Win {
            window,
            tabs,
            toasts,
            tab_list: Default::default(),
            show_hidden: Default::default(),
            places,
            place_paths: Default::default(),
        });
        win.setup_actions();
        win.setup_places();

        let weak = Rc::downgrade(&win);
        win.tabs.connect_selected_page_notify(glib::clone!(#[strong] weak, move |_| {
            if let Some(win) = weak.upgrade() {
                win.sync();
            }
        }));
        win.tabs.connect_close_page(glib::clone!(
            #[strong] weak,
            move |view, page| {
                let Some(win) = weak.upgrade() else { return glib::Propagation::Proceed };
                win.tab_list.borrow_mut().retain(|t| t.page != *page);
                let last = view.n_pages() == 1;
                view.close_page_finish(page, true);
                if last {
                    win.window.close();
                }
                glib::Propagation::Stop
            }
        ));
        // Mouse back/forward buttons.
        let click = gtk::GestureClick::builder().button(0).propagation_phase(gtk::PropagationPhase::Capture).build();
        click.connect_pressed(glib::clone!(#[strong] weak, move |g, _, _, _| {
            let Some(win) = weak.upgrade() else { return };
            match g.current_button() {
                8 => win.pane().go_back(),
                9 => win.pane().go_forward(),
                _ => {}
            }
        }));
        win.window.add_controller(click);
        win.window.connect_destroy(glib::clone!(#[strong] weak, move |_| {
            WINDOWS.with(|w| w.borrow_mut().retain(|x| !std::ptr::eq(Rc::as_ptr(x), weak.as_ptr())));
        }));
        win
    }

    fn weak(self: &Rc<Self>) -> Weak<Self> {
        Rc::downgrade(self)
    }

    fn current_tab(&self) -> Option<Rc<Tab>> {
        let page = self.tabs.selected_page()?;
        self.tab_list.borrow().iter().find(|t| t.page == page).cloned()
    }

    fn pane(&self) -> Rc<Pane> {
        self.current_tab().expect("a tab is always open").pane()
    }

    fn all_panes(&self) -> Vec<Rc<Pane>> {
        self.tab_list.borrow().iter().flat_map(|t| t.panes.borrow().clone()).collect()
    }

    fn new_pane(self: &Rc<Self>, tab: &Weak<Tab>, path: PathBuf, mode: ViewMode) -> Rc<Pane> {
        let pane = Pane::new(path, mode, self.show_hidden.get());
        let (w, t, p) = (self.weak(), tab.clone(), Rc::downgrade(&pane));
        *pane.on_navigated.borrow_mut() = Some(Box::new(glib::clone!(#[strong] w, #[strong] t, #[strong] p, move || {
            if let (Some(win), Some(tab), Some(pane)) = (w.upgrade(), t.upgrade(), p.upgrade()) {
                if Rc::ptr_eq(&tab.pane(), &pane) {
                    tab.page.set_title(&folder_name(&pane.path()));
                    win.sync();
                }
            }
        })));
        *pane.on_focus.borrow_mut() = Some(Box::new(glib::clone!(#[strong] w, #[strong] t, #[strong] p, move || {
            if let (Some(win), Some(tab), Some(pane)) = (w.upgrade(), t.upgrade(), p.upgrade()) {
                let i = tab.panes.borrow().iter().position(|x| Rc::ptr_eq(x, &pane)).unwrap_or(0);
                tab.active.set(i);
                tab.page.set_title(&folder_name(&pane.path()));
                win.sync();
            }
        })));
        *pane.on_open_tab.borrow_mut() = Some(Box::new(glib::clone!(#[strong] w, move |path| {
            if let Some(win) = w.upgrade() {
                win.add_tab_background(path);
            }
        })));
        pane
    }

    fn add_tab(self: &Rc<Self>, path: PathBuf) {
        let page = self.insert_tab(path);
        self.tabs.set_selected_page(&page);
    }

    fn add_tab_background(self: &Rc<Self>, path: PathBuf) {
        self.insert_tab(path);
    }

    fn insert_tab(self: &Rc<Self>, path: PathBuf) -> adw::TabPage {
        let mode = self.current_tab().map(|t| t.pane().canvas.mode()).unwrap_or_default();
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        let page = self.tabs.append(&paned);
        page.set_title(&folder_name(&path));
        let tab = Rc::new(Tab { page: page.clone(), paned, panes: Default::default(), active: Cell::new(0) });
        let pane = self.new_pane(&Rc::downgrade(&tab), path, mode);
        tab.paned.set_start_child(Some(&pane.root));
        tab.panes.borrow_mut().push(pane.clone());
        self.tab_list.borrow_mut().push(tab);
        glib::idle_add_local_once(move || {
            pane.canvas.grab_focus();
        });
        page
    }

    /// F3: open a second view of the same folder, or close the other one.
    fn toggle_split(self: &Rc<Self>) {
        let Some(tab) = self.current_tab() else { return };
        let active = tab.pane();
        if tab.panes.borrow().len() == 1 {
            let pane = self.new_pane(&Rc::downgrade(&tab), active.path(), active.canvas.mode());
            tab.paned.set_end_child(Some(&pane.root));
            tab.paned.set_position(tab.paned.width() / 2);
            tab.panes.borrow_mut().push(pane);
        } else {
            tab.panes.borrow_mut().retain(|p| Rc::ptr_eq(p, &active));
            tab.paned.set_start_child(None::<&gtk::Widget>);
            tab.paned.set_end_child(None::<&gtk::Widget>);
            tab.paned.set_start_child(Some(&active.root));
            tab.active.set(0);
            active.canvas.grab_focus();
        }
        self.sync();
    }

    /// Reflect the active pane in the title, the action states and the split highlight.
    fn sync(&self) {
        let Some(tab) = self.current_tab() else { return };
        let pane = tab.pane();
        self.window.set_title(Some(&format!("{} — Kolphin", folder_name(&pane.path()))));
        if let Some(a) = self.action("view-mode") {
            a.set_state(&mode_name(pane.canvas.mode()).to_variant());
        }
        if let Some(a) = self.action("split") {
            a.set_state(&(tab.panes.borrow().len() > 1).to_variant());
        }
        let split = tab.panes.borrow().len() > 1;
        for p in tab.panes.borrow().iter() {
            if split && !Rc::ptr_eq(p, &pane) {
                p.root.add_css_class("pane-inactive");
            } else {
                p.root.remove_css_class("pane-inactive");
            }
        }
    }

    fn action(&self, name: &str) -> Option<gio::SimpleAction> {
        self.window.lookup_action(name)?.downcast().ok()
    }

    fn setup_actions(self: &Rc<Self>) {
        let add = |name: &str, f: fn(&Rc<Win>)| {
            let a = gio::SimpleAction::new(name, None);
            let w = self.weak();
            a.connect_activate(move |_, _| {
                if let Some(win) = w.upgrade() {
                    f(&win);
                }
            });
            self.window.add_action(&a);
        };
        add("back", |w| w.pane().go_back());
        add("forward", |w| w.pane().go_forward());
        add("up", |w| w.pane().go_up());
        add("home", |w| w.pane().navigate(glib::home_dir()));
        add("reload", |w| w.pane().reload());
        add("location", |w| w.pane().pathbar.edit());
        add("new-tab", |w| w.add_tab(w.pane().path()));
        add("close-tab", |w| {
            if let Some(page) = w.tabs.selected_page() {
                w.tabs.close_page(&page);
            }
        });
        add("zoom-in", |w| w.pane().canvas.zoom_by(1.15));
        add("zoom-out", |w| w.pane().canvas.zoom_by(1.0 / 1.15));
        add("check-update", |w| crate::check_update(&w.toasts, true));

        let split = gio::SimpleAction::new_stateful("split", None, &false.to_variant());
        let w = self.weak();
        split.connect_activate(move |_, _| {
            if let Some(win) = w.upgrade() {
                win.toggle_split();
            }
        });
        self.window.add_action(&split);

        let hidden = gio::SimpleAction::new_stateful("toggle-hidden", None, &false.to_variant());
        let w = self.weak();
        hidden.connect_activate(move |a, _| {
            let Some(win) = w.upgrade() else { return };
            let show = !win.show_hidden.get();
            win.show_hidden.set(show);
            a.set_state(&show.to_variant());
            for p in win.all_panes() {
                p.set_show_hidden(show);
            }
        });
        self.window.add_action(&hidden);

        let mode = gio::SimpleAction::new_stateful("view-mode", Some(glib::VariantTy::STRING), &"icons".to_variant());
        let w = self.weak();
        mode.connect_activate(move |a, v| {
            let (Some(win), Some(name)) = (w.upgrade(), v.and_then(|v| v.get::<String>())) else { return };
            let m = match name.as_str() {
                "compact" => ViewMode::Compact,
                "details" => ViewMode::Details,
                _ => ViewMode::Icons,
            };
            win.pane().canvas.set_mode(m);
            a.set_state(&name.to_variant());
        });
        self.window.add_action(&mode);

        let install = gio::SimpleAction::new("install-update", Some(glib::VariantTy::STRING));
        let w = self.weak();
        install.connect_activate(move |_, url| {
            if let (Some(win), Some(url)) = (w.upgrade(), url.and_then(|u| u.get::<String>())) {
                crate::install(&win.toasts, url);
            }
        });
        self.window.add_action(&install);
    }

    fn setup_places(self: &Rc<Self>) {
        let w = self.weak();
        self.places.connect_row_activated(move |_, row| {
            let Some(win) = w.upgrade() else { return };
            let path = win.place_paths.borrow().get(row.index() as usize).cloned();
            if let Some(path) = path {
                win.pane().navigate(path);
            }
        });
        let monitor = gio::VolumeMonitor::get();
        let w = self.weak();
        let refresh = move || {
            if let Some(win) = w.upgrade() {
                win.fill_places();
            }
        };
        monitor.connect_mount_added(glib::clone!(#[strong] refresh, move |_, _| refresh()));
        monitor.connect_mount_removed(glib::clone!(#[strong] refresh, move |_, _| refresh()));
        monitor.connect_mount_changed(move |_, _| refresh());
        self.fill_places();
    }

    fn fill_places(&self) {
        while let Some(row) = self.places.row_at_index(0) {
            self.places.remove(&row);
        }
        let mut paths = Vec::new();
        let mut add = |icon: gio::Icon, label: &str, path: PathBuf| {
            let row = gtk::Box::builder().spacing(10).margin_start(6).margin_top(2).margin_bottom(2).build();
            row.append(&gtk::Image::from_gicon(&icon));
            row.append(&gtk::Label::builder().label(label).ellipsize(gtk::pango::EllipsizeMode::End).build());
            self.places.append(&row);
            paths.push(path);
        };
        let themed = |name: &str| gio::ThemedIcon::new(name).upcast::<gio::Icon>();
        add(themed("user-home-symbolic"), "Home", glib::home_dir());
        for (dir, icon) in [
            (glib::UserDirectory::Desktop, "user-desktop-symbolic"),
            (glib::UserDirectory::Documents, "folder-documents-symbolic"),
            (glib::UserDirectory::Downloads, "folder-download-symbolic"),
            (glib::UserDirectory::Pictures, "folder-pictures-symbolic"),
            (glib::UserDirectory::Videos, "folder-videos-symbolic"),
            (glib::UserDirectory::Music, "folder-music-symbolic"),
        ] {
            if let Some(p) = glib::user_special_dir(dir).filter(|p| p.is_dir() && *p != glib::home_dir()) {
                add(themed(icon), &folder_name(&p), p);
            }
        }
        add(themed("drive-harddisk-symbolic"), "Root", PathBuf::from("/"));
        for mount in gio::VolumeMonitor::get().mounts() {
            if let Some(path) = mount.root().path() {
                add(mount.symbolic_icon(), &mount.name(), path);
            }
        }
        *self.place_paths.borrow_mut() = paths;
    }
}

fn folder_name(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
}
