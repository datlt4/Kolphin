//! Main window: places sidebar, tabs, split view, header bar and window actions.
use crate::canvas::ViewMode;
use crate::actions;
use crate::bookmarks;
use crate::clipboard;
use crate::kermius;
use crate::newdoc;
use crate::palette;
use crate::shelf;
use crate::devices;
use crate::dialogs;
use crate::ops;
use crate::props;
use crate::quicklook;
use crate::stats;
use crate::model::{GroupBy, SortBy};
use crate::notify::{self, ToastExt};
use crate::viewprops;
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
    pub toasts: gtk::Overlay,
    tab_list: RefCell<Vec<Rc<Tab>>>,
    show_hidden: Cell<bool>,
    /// Last folder sent to (or reported by) the Kermius terminal.
    last_cd: RefCell<Option<PathBuf>>,
    content: gtk::Paned,
    sidebar: gtk::Box,
    main_col: gtk::Box,
    view_button: gtk::MenuButton,
    places: gtk::ListBox,
    place_paths: RefCell<Vec<Option<Place>>>,
    /// LAN hosts found by gvfs (name, URI).
    hosts: RefCell<Vec<(String, String)>>,
    /// (drive label, trash files dir) for the Trash ▾ menu.
    trashes: RefCell<Vec<(String, PathBuf)>>,
    /// Free space and trashes, gathered in the background (see `SideInfo`).
    side: RefCell<SideInfo>,
    side_busy: Cell<bool>,
}

thread_local! {
    static WINDOWS: RefCell<Vec<Rc<Win>>> = const { RefCell::new(Vec::new()) };
    static VOLUME_MONITOR: RefCell<Option<gio::VolumeMonitor>> = const { RefCell::new(None) };
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
        ("win.filter", &["<Control>i"]),
        ("win.search", &["<Control>f"]),
        ("win.new-folder", &["<Control><Shift>n"]),
        ("win.new-file('empty')", &["<Control><Alt>n"]),
        ("win.show-sidebar", &["F9"]),
        ("win.terminal", &["F4"]),
        ("win.palette", &["<Control><Shift>p"]),
        ("win.shelf", &["<Control><Shift>d"]),
    ] {
        app.set_accels_for_action(action, keys);
    }
}

pub fn open(app: &adw::Application, paths: &[PathBuf]) {
    let win = Win::new(app);
    // A plain start reopens the last session's tabs (and split views).
    let session = if paths.is_empty() && WINDOWS.with(|w| w.borrow().is_empty()) { viewprops::load_session() } else { None };
    match session {
        Some(s) => {
            if s.hidden {
                if let Some(a) = win.action("toggle-hidden") {
                    a.activate(None);
                }
            }
            for tab in &s.tabs {
                win.add_tab(tab[0].clone());
                if let Some(second) = tab.get(1) {
                    win.toggle_split();
                    win.pane().navigate(second.clone());
                }
            }
            if let Some(page) = win.tab_list.borrow().get(s.selected).map(|t| t.page.clone()) {
                win.tabs.set_selected_page(&page);
            }
        }
        None if paths.is_empty() => win.add_tab(glib::home_dir()),
        None => {}
    }
    for p in paths {
        win.add_tab(p.clone());
    }
    win.window.present();
    if viewprops::check_updates_on_start() && !UPDATE_CHECKED.replace(true) {
        crate::check_update(&win.toasts, false);
    }
    WINDOWS.with(|w| w.borrow_mut().push(win));
}

/// "Show in folder": open each item's folder and select the item (optionally its Properties).
pub fn reveal(app: &adw::Application, items: &[PathBuf], properties: bool) {
    let Some(first) = items.first() else { return };
    let Some(parent) = first.parent().map(PathBuf::from) else { return };
    let existing = WINDOWS.with(|w| w.borrow().last().cloned());
    let win = match existing {
        Some(win) => {
            win.add_tab(parent);
            win
        }
        None => {
            open(app, &[parent]);
            match WINDOWS.with(|w| w.borrow().last().cloned()) {
                Some(w) => w,
                None => return,
            }
        }
    };
    win.window.present();
    if let Some(name) = first.file_name() {
        win.pane().select_when_loaded(name.to_os_string());
    }
    if properties {
        props::show(win.window.upcast_ref(), first.clone());
    }
}

/// Remember every window's tabs (called when the app quits, e.g. to restart after an update).
/// Run a PDF job off the main thread with a progress toast, then report.
fn run_pdf(w: &Rc<Win>, busy: &str, job: impl FnOnce() -> Result<String, String> + Send + 'static) {
    let toast = notify::Toast::builder().title(busy).timeout(0).build();
    w.toasts.add_toast(toast.clone());
    let weak = w.weak();
    glib::spawn_future_local(async move {
        let res = gio::spawn_blocking(job).await.unwrap_or_else(|_| Err("PDF job crashed".into()));
        toast.dismiss();
        if let Some(w) = weak.upgrade() {
            w.toasts.add_toast(notify::Toast::new(&res.unwrap_or_else(|e| format!("PDF failed: {e}"))));
        }
    });
}

/// Sidebar facts that need the disks (free space, trashes on other drives). Gathered off the main thread: a
/// sleeping HDD or a network share must not hold up the window.
#[derive(Default, Clone, PartialEq)]
struct SideInfo {
    usage: std::collections::HashMap<PathBuf, (u64, u64)>,
    /// (drive label, trash files/ folder): the home trash first, then other drives' non-empty ones.
    trashes: Vec<(String, PathBuf)>,
    any_full: bool,
    /// Network shares whose folder is gone (unmounted behind gvfs' back).
    dead: Vec<PathBuf>,
}

/// `mounts`: mount points with their names.
fn side_info(mounts: Vec<(PathBuf, String)>) -> SideInfo {
    let (remote, mounts): (Vec<_>, Vec<_>) = mounts.into_iter().partition(|(p, _)| devices::is_remote(p));
    let dead = remote.into_iter().map(|(p, _)| p).filter(|p| !p.is_dir()).collect();
    let mut usage = std::collections::HashMap::new();
    for p in std::iter::once(PathBuf::from("/")).chain(mounts.iter().map(|(p, _)| p.clone())) {
        if let Some(u) = devices::usage(&p) {
            usage.insert(p, u);
        }
    }
    let full = |d: &PathBuf| std::fs::read_dir(d).is_ok_and(|mut r| r.next().is_some());
    let dirs = ops::trash_dirs(&mounts.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>());
    let trashes: Vec<(String, PathBuf)> = dirs
        .iter()
        .enumerate()
        .filter(|(i, d)| *i == 0 || full(d))
        .map(|(i, d)| {
            let label = if i == 0 { "Home".to_string() } else { mounts.iter().find(|(t, _)| d.starts_with(t)).map_or("drive".to_string(), |(_, n)| n.clone()) };
            (label, d.clone())
        })
        .collect();
    let any_full = trashes.iter().any(|(_, d)| full(d));
    SideInfo { usage, trashes, any_full, dead }
}

/// Copy/Move from a Drop Shelf into the active folder of the last focused window.
pub fn shelf_transfer(items: Vec<PathBuf>, cut: bool) {
    if let Some(win) = WINDOWS.with(|w| w.borrow().last().cloned()) {
        clipboard::run_transfer(&win.toasts, items, win.pane().path(), cut);
    }
}

pub fn save_sessions() {
    WINDOWS.with(|w| {
        if let Some(win) = w.borrow().last() {
            win.save_session();
        }
    });
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
        let right = viewprops::sidebar_right();
        // One full-width header: brand, back/forward | cut/copy/paste … view, split, menu.
        let header = adw::HeaderBar::new();
        let brand = gtk::Box::builder().spacing(10).css_classes(["brand"]).build();
        let logo = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_static(include_bytes!("../../assets/icons/kolphin-128.png"))).ok();
        brand.append(&gtk::Image::builder().pixel_size(28).build());
        if let (Some(img), Some(logo)) = (brand.first_child().and_downcast::<gtk::Image>(), logo) {
            img.set_paintable(Some(&logo));
        }
        brand.append(&gtk::Label::new(Some("Kolphin")));
        let tools = gtk::Box::builder().spacing(2).build();
        tools.append(&brand);
        for (icon, action, tip) in [("go-previous-symbolic", "win.back", "Back (Alt+Left)"), ("go-next-symbolic", "win.forward", "Forward (Alt+Right)")] {
            tools.append(&gtk::Button::builder().icon_name(icon).action_name(action).tooltip_text(tip).build());
        }
        tools.append(&gtk::Separator::builder().orientation(gtk::Orientation::Vertical).css_classes(["header-sep"]).build());
        for (icon, action, tip) in [("edit-cut-symbolic", "win.cut", "Cut (Ctrl+X)"), ("edit-copy-symbolic", "win.copy", "Copy (Ctrl+C)"), ("edit-paste-symbolic", "win.paste", "Paste (Ctrl+V)")] {
            tools.append(&gtk::Button::builder().icon_name(icon).action_name(action).tooltip_text(tip).build());
        }
        header.pack_start(&tools);
        let menu = gio::Menu::new();
        menu.append(Some("New Tab"), Some("win.new-tab"));
        menu.append(Some("Select by Pattern…"), Some("win.select-pattern"));
        menu.append(Some("Random Sample…"), Some("win.random-sample"));
        menu.append(Some("Folder Statistics…"), Some("win.stats"));
        menu.append(Some("Connect to Server…"), Some("win.connect-server"));
        menu.append(Some("Command Palette (Ctrl+Shift+P)"), Some("win.palette"));
        menu.append(Some("Drop Shelf (Ctrl+Shift+D)"), Some("win.shelf"));
        menu.append(Some("Edit Custom Actions…"), Some("win.edit-actions"));
        menu.append(Some("Show Sidebar (F9)"), Some("win.show-sidebar"));
        menu.append(Some("Sidebar on Right"), Some("win.sidebar-right"));
        menu.append(Some("Use Kolphin as Default File Manager"), Some("win.default-fm"));
        menu.append(Some("Verify Copies (xxHash)"), Some("win.verify-copies"));
        menu.append(Some("Check for Updates on Start"), Some("win.auto-update"));
        menu.append(Some("Check for Updates"), Some("win.check-update"));
        header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).build());
        header.pack_end(&gtk::ToggleButton::builder().icon_name("view-dual-symbolic").action_name("win.split").tooltip_text("Split View (F3)").build());
        header.pack_end(&gtk::Button::builder().icon_name("tab-new-symbolic").action_name("win.new-tab").tooltip_text("New Tab (Ctrl+T)").build());
        let view_button = gtk::MenuButton::builder().icon_name("view-grid-symbolic").menu_model(&view_menu()).tooltip_text("View, sort and group").build();
        header.pack_end(&view_button);

        let tabs = adw::TabView::new();
        // Tabs live in the header, in place of the window title (like Kermius).
        let tab_bar = adw::TabBar::builder().view(&tabs).autohide(false).expand_tabs(false).hexpand(true).valign(gtk::Align::Center).build();
        header.set_title_widget(Some(&tab_bar));
        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main.append(&tabs);
        tabs.set_vexpand(true);

        let main_col = main;

        // Sidebar like Kermius: places/devices, version at the bottom.
        let places = gtk::ListBox::builder().css_classes(["places"]).build();
        let side_scroll = gtk::ScrolledWindow::builder().child(&places).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
        let version = gtk::Label::builder().label(format!("v{}", env!("CARGO_PKG_VERSION"))).xalign(0.0).css_classes(["side-foot"]).build();
        let sidebar = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["sidebar-pane"]).width_request(200).build();
        sidebar.append(&side_scroll);
        sidebar.append(&version);

        let content = gtk::Paned::builder().shrink_start_child(false).shrink_end_child(false).build();
        content.add_css_class("root-split");
        content.set_wide_handle(false);
        content.set_vexpand(true);
        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.append(&header);
        frame.append(&content);
        let toasts = notify::overlay();
        toasts.set_child(Some(&frame));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Kolphin")
            .icon_name("kolphin")
            .default_width(1200)
            .default_height(760)
            .content(&toasts)
            .build();

        let win = Rc::new(Win {
            content,
            sidebar,
            main_col,
            view_button,
            window,
            tabs,
            toasts,
            tab_list: Default::default(),
            show_hidden: Default::default(),
            last_cd: Default::default(),
            places,
            place_paths: Default::default(),
            hosts: Default::default(),
            trashes: Default::default(),
            side: Default::default(),
            side_busy: Default::default(),
        });
        win.setup_actions();
        win.setup_places();
        win.place_sidebar(right);

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
        win.window.connect_close_request(glib::clone!(
            #[strong] weak,
            move |_| {
                if let Some(win) = weak.upgrade() {
                    win.save_session();
                }
                glib::Propagation::Proceed
            }
        ));
        // The last focused window is the one shelves copy into.
        win.window.connect_is_active_notify(glib::clone!(#[strong] weak, move |w| {
            if w.is_active() {
                WINDOWS.with(|ws| {
                    let mut ws = ws.borrow_mut();
                    if let Some(i) = ws.iter().position(|x| std::ptr::eq(Rc::as_ptr(x), weak.as_ptr())) {
                        let me = ws.remove(i);
                        ws.push(me);
                    }
                });
            }
        }));
        win.window.connect_destroy(glib::clone!(#[strong] weak, move |_| {
            // At process exit the list itself is being dropped (and drops this window): nothing to remove then.
            let _ = WINDOWS.try_with(|w| {
                if let Ok(mut w) = w.try_borrow_mut() {
                    w.retain(|x| !std::ptr::eq(Rc::as_ptr(x), weak.as_ptr()));
                }
            });
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
        *pane.on_view.borrow_mut() = Some(Box::new(glib::clone!(#[strong] w, move || {
            if let Some(win) = w.upgrade() {
                win.sync();
            }
        })));
        pane.close.connect_clicked(glib::clone!(#[strong] w, #[strong] t, #[strong] p, move |_| {
            if let (Some(win), Some(tab), Some(pane)) = (w.upgrade(), t.upgrade(), p.upgrade()) {
                win.close_pane(&tab, &pane);
            }
        }));
        *pane.on_drop.borrow_mut() = Some(Box::new(glib::clone!(#[strong] w, move |files, dir, cut| {
            if let Some(win) = w.upgrade() {
                clipboard::run_transfer(&win.toasts, files, dir, cut);
            }
        })));
        *pane.on_denied.borrow_mut() = Some(Box::new(glib::clone!(#[strong] w, move |path| {
            let Some(win) = w.upgrade() else { return };
            let weak = win.weak();
            let toast = notify::Toast::builder()
                .title("The share refused access: its password may have changed.")
                .timeout(0)
                .on_button("Sign in again", move |_| {
                    let Some(win) = weak.upgrade() else { return };
                    let w2 = win.weak();
                    devices::reconnect(path.clone(), move |res| {
                        let Some(win) = w2.upgrade() else { return };
                        match res {
                            Ok(uri) => win.connect_server(uri),
                            Err(e) => win.toasts.add_toast(notify::Toast::new(&format!("Sign in again failed: {e}"))),
                        }
                    });
                })
                .build();
            win.toasts.add_toast(toast);
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
            if tab.paned.width() > 0 {
                tab.paned.set_position(tab.paned.width() / 2);
            } else {
                // Restored before the window has a size: split in half once it is laid out.
                let handler: Rc<Cell<Option<glib::SignalHandlerId>>> = Default::default();
                let h = handler.clone();
                handler.set(Some(tab.paned.connect_max_position_notify(move |p| {
                    if p.max_position() > 0 {
                        p.set_position(p.max_position() / 2);
                        if let Some(id) = h.take() {
                            p.disconnect(id);
                        }
                    }
                })));
            }
            tab.panes.borrow_mut().push(pane);
            self.sync();
        } else {
            let other = tab.panes.borrow().iter().find(|p| !Rc::ptr_eq(p, &active)).cloned();
            if let Some(other) = other {
                self.close_pane(&tab, &other);
            }
        }
    }

    /// Leave split view by closing `gone`; the other side stays.
    fn close_pane(&self, tab: &Rc<Tab>, gone: &Rc<Pane>) {
        tab.panes.borrow_mut().retain(|p| !Rc::ptr_eq(p, gone));
        let Some(keep) = tab.panes.borrow().first().cloned() else { return };
        tab.paned.set_start_child(None::<&gtk::Widget>);
        tab.paned.set_end_child(None::<&gtk::Widget>);
        tab.paned.set_start_child(Some(&keep.root));
        tab.active.set(0);
        keep.canvas.grab_focus();
        self.sync();
    }

    /// Reflect the active pane in the title, the action states and the split highlight.
    fn sync(&self) {
        let Some(tab) = self.current_tab() else { return };
        let pane = tab.pane();
        // Kolphin's terminal follows the active view.
        let path = pane.path();
        if kermius::get().has_terminal() && self.last_cd.borrow().as_ref() != Some(&path) {
            *self.last_cd.borrow_mut() = Some(path.clone());
            kermius::get().cd(&path);
        }
        self.window.set_title(Some(&format!("{} — Kolphin", folder_name(&pane.path()))));
        let mode = pane.canvas.mode();
        if let Some(a) = self.action("view-mode") {
            a.set_state(&mode_name(mode).to_variant());
        }
        self.view_button.set_icon_name(match mode {
            ViewMode::Icons => "view-grid-symbolic",
            ViewMode::Compact => "view-continuous-symbolic",
            ViewMode::Details => "view-list-symbolic",
        });
        let (sort, desc, group) = {
            let m = pane.canvas.model();
            (m.sort, m.desc, m.group)
        };
        if let Some(a) = self.action("sort-by") {
            a.set_state(&format!("{sort:?}").to_lowercase().to_variant());
        }
        if let Some(a) = self.action("sort-desc") {
            a.set_state(&desc.to_variant());
        }
        if let Some(a) = self.action("group-by") {
            a.set_state(&format!("{group:?}").to_lowercase().to_variant());
        }
        if let Some(a) = self.action("split") {
            a.set_state(&(tab.panes.borrow().len() > 1).to_variant());
        }
        let split = tab.panes.borrow().len() > 1;
        for p in tab.panes.borrow().iter() {
            p.close.set_visible(split);
            if split && !Rc::ptr_eq(p, &pane) {
                p.root.add_css_class("pane-inactive");
            } else {
                p.root.remove_css_class("pane-inactive");
            }
        }
    }

    fn save_session(&self) {
        let tabs = self.tab_list.borrow();
        // Tabs in their on-screen order.
        let mut ordered: Vec<_> = tabs.iter().map(|t| (self.tabs.page_position(&t.page), t.clone())).collect();
        ordered.sort_by_key(|(i, _)| *i);
        let selected = self.tabs.selected_page().map_or(0, |p| self.tabs.page_position(&p).max(0) as usize);
        let tabs = ordered.into_iter().map(|(_, t)| t.panes.borrow().iter().map(|p| p.path()).collect()).collect();
        viewprops::save_session(&viewprops::Session { tabs, selected, hidden: self.show_hidden.get() });
    }

    /// Put the sidebar on the left or right.
    fn place_sidebar(&self, right: bool) {
        let c = &self.content;
        c.set_start_child(None::<&gtk::Widget>);
        c.set_end_child(None::<&gtk::Widget>);
        let (start, end): (&gtk::Widget, &gtk::Widget) = if right { (self.main_col.upcast_ref(), self.sidebar.upcast_ref()) } else { (self.sidebar.upcast_ref(), self.main_col.upcast_ref()) };
        c.set_start_child(Some(start));
        c.set_end_child(Some(end));
        c.set_resize_start_child(right);
        c.set_resize_end_child(!right);
        let width = self.window.default_width().max(self.window.width());
        c.set_position(if right { width - 240 } else { 240 });
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
        add("reload", |w| {
            w.pane().reload();
            w.fill_places();
        });
        add("location", |w| w.pane().pathbar.edit());
        add("new-tab", |w| w.add_tab(w.pane().path()));
        add("close-tab", |w| {
            if let Some(page) = w.tabs.selected_page() {
                w.tabs.close_page(&page);
            }
        });
        add("zoom-in", |w| w.pane().canvas.zoom_by(true));
        add("zoom-out", |w| w.pane().canvas.zoom_by(false));
        add("check-update", |w| crate::check_update(&w.toasts, true));
        add("filter", |w| w.pane().toggle_filter());
        add("search", |w| w.pane().start_search());
        add("copy", |w| clipboard::copy(&w.pane(), false, &w.toasts));
        add("cut", |w| clipboard::copy(&w.pane(), true, &w.toasts));
        add("paste", |w| clipboard::paste(&w.pane(), &w.toasts));
        add("open-selected", |w| w.pane().open_selected(false));
        add("open-with", |w| dialogs::open_with(w.window.upcast_ref(), w.pane().selected_paths()));
        add("quicklook", |w| quicklook::show(w.window.upcast_ref(), &w.pane()));
        add("properties", |w| {
            let sel = w.pane().selected_paths();
            props::show(w.window.upcast_ref(), if sel.len() == 1 { sel[0].clone() } else { w.pane().path() });
        });
        add("undo", |w| {
            let msg = ops::undo().unwrap_or_else(|e| e);
            w.toasts.add_toast(notify::Toast::new(&msg));
        });
        add("restore", |w| {
            let msg = match ops::restore_from_trash(&w.pane().selected_paths()) {
                Ok(n) => format!("Restored {n} item(s)"),
                Err(e) => format!("Restore failed: {e}"),
            };
            w.toasts.add_toast(notify::Toast::new(&msg));
            w.fill_places();
        });
        add("empty-trash", |w| {
            let weak = w.weak();
            let dir = w.pane().path();
            let text = "Permanently delete everything in this trash? This cannot be undone.";
            dialogs::confirm(w.window.upcast_ref(), "Empty Trash", text, "Empty Trash", move || {
                let Some(w) = weak.upgrade() else { return };
                let msg = match ops::empty_trash(std::slice::from_ref(&dir)) {
                    Ok(n) => format!("Deleted {n} item(s) from the trash"),
                    Err(e) => format!("Empty trash failed: {e}"),
                };
                w.toasts.add_toast(notify::Toast::new(&msg));
                w.fill_places();
            });
        });
        add("trash", |w| {
            let paths = w.pane().selected_paths();
            if paths.is_empty() {
                return;
            }
            // Inside a trash, Delete means delete for good.
            if ops::is_trash(&w.pane().path()) {
                return gio::prelude::ActionGroupExt::activate_action(&w.window, "delete", None);
            }
            let (toasts, weak) = (w.toasts.clone(), w.weak());
            glib::spawn_future_local(async move {
                let p = paths.clone();
                let res = gio::spawn_blocking(move || ops::trash(&p)).await.unwrap_or_else(|_| Err("trash crashed".into()));
                if let Some(w) = weak.upgrade() {
                    w.fill_places();
                }
                let msg = match res {
                    Ok(items) => {
                        let n = items.len();
                        ops::record(ops::Undo::Trashed(items));
                        format!("Moved {n} item(s) to the trash · Ctrl+Z to undo")
                    }
                    Err(e) => format!("Trash failed: {e}"),
                };
                toasts.add_toast(notify::Toast::new(&msg));
            });
        });
        add("delete", |w| {
            let paths = w.pane().selected_paths();
            if paths.is_empty() {
                return;
            }
            let toasts = w.toasts.clone();
            let text = format!("Permanently delete {} item(s)? This cannot be undone.", paths.len());
            dialogs::confirm(w.window.upcast_ref(), "Delete Permanently", &text, "Delete", move || {
                let (paths, toasts) = (paths.clone(), toasts.clone());
                glib::spawn_future_local(async move {
                    let n = paths.len();
                    let res = gio::spawn_blocking(move || ops::delete_permanently(&paths)).await.unwrap_or_else(|_| Err("delete crashed".into()));
                    toasts.add_toast(notify::Toast::new(&match res {
                        Ok(()) => format!("Deleted {n} item(s)"),
                        Err(e) => format!("Delete failed: {e}"),
                    }));
                });
            });
        });
        add("rename", |w| {
            let paths = w.pane().selected_paths();
            let weak = w.weak();
            match paths.len() {
                0 => {}
                1 => {
                    let from = paths[0].clone();
                    let name = from.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    dialogs::prompt(w.window.upcast_ref(), "Rename", "New name:", &name, move |new| {
                        let Some(w) = weak.upgrade() else { return };
                        match ops::rename(&from, new.trim()) {
                            Ok(to) => {
                                ops::record(ops::Undo::Moved(vec![(from.clone(), to.clone())]));
                                w.pane().select_when_loaded(to.file_name().unwrap_or_default().to_os_string());
                            }
                            Err(e) => w.toasts.add_toast(notify::Toast::new(&format!("Rename failed: {e}"))),
                        }
                    });
                }
                _ => dialogs::batch_rename(w.window.upcast_ref(), paths, move |plan| {
                    let Some(w) = weak.upgrade() else { return };
                    let (mut done, mut errors) = (Vec::new(), Vec::new());
                    for (from, name) in plan {
                        match ops::rename(&from, &name) {
                            Ok(to) => done.push((from, to)),
                            Err(e) => errors.push(format!("{name}: {e}")),
                        }
                    }
                    let msg = format!("Renamed {} item(s){}", done.len(), errors.first().map(|e| format!(" · {} failed ({e})", errors.len())).unwrap_or_default());
                    ops::record(ops::Undo::Moved(done));
                    w.toasts.add_toast(notify::Toast::new(&msg));
                }),
            }
        });
        add("new-folder", |w| {
            let dir = w.pane().path();
            let name = ops::unique(&dir.join("New Folder")).file_name().unwrap_or_default().to_string_lossy().into_owned();
            let weak = w.weak();
            dialogs::prompt(w.window.upcast_ref(), "New Folder", "Folder name:", &name, move |name| {
                let Some(w) = weak.upgrade() else { return };
                match ops::new_folder(&dir, name.trim()) {
                    Ok(p) => {
                        ops::record(ops::Undo::Created(vec![p.clone()]));
                        w.pane().select_when_loaded(p.file_name().unwrap_or_default().to_os_string());
                    }
                    Err(e) => w.toasts.add_toast(notify::Toast::new(&format!("New folder failed: {e}"))),
                }
            });
        });
        add("connect-server", |w| {
            let weak = w.weak();
            dialogs::prompt(w.window.upcast_ref(), "Connect to Server", "Address, e.g. smb://nas.local/share, sftp://user@host/path, ftp://host", "smb://", move |uri| {
                if let Some(win) = weak.upgrade() {
                    win.connect_server(uri);
                }
            });
        });
        add("palette", |w| palette::show(w.window.upcast_ref()));
        add("shelf", |w| {
            if let Some(app) = w.window.application() {
                shelf::open(&app);
            }
        });
        add("terminal", |w| {
            let k = kermius::get();
            // The shell's cd moves this window's active view (unless it is already there).
            let weak = w.weak();
            *k.on_cwd.borrow_mut() = Some(Box::new(move |cwd| {
                if let Some(win) = weak.upgrade() {
                    let pane = win.pane();
                    if pane.path() != cwd && cwd.is_dir() {
                        *win.last_cd.borrow_mut() = Some(cwd.clone());
                        pane.navigate(cwd);
                    }
                }
            }));
            let path = w.pane().path();
            *w.last_cd.borrow_mut() = Some(path.clone());
            k.open_terminal(&path);
        });
        add("share", |w| {
            let mut paths = w.pane().selected_paths();
            if paths.is_empty() {
                paths.push(w.pane().path());
            }
            if !kermius::get().share(paths) {
                w.toasts.add_toast(notify::Toast::new("Sharing needs Kermius (Nearby Share)"));
            }
        });
        add("edit-actions", |w| {
            let _ = actions::load();
            let uri = gio::File::for_path(actions::config_file()).uri();
            let _ = gio::AppInfo::launch_default_for_uri(&uri, Some(&WidgetExt::display(&w.window).app_launch_context()));
        });
        let open_trash = gio::SimpleAction::new("open-trash", Some(glib::VariantTy::INT32));
        let w = self.weak();
        open_trash.connect_activate(move |_, v| {
            let (Some(win), Some(i)) = (w.upgrade(), v.and_then(|v| v.get::<i32>())) else { return };
            let dir = win.trashes.borrow().get(i as usize).map(|(_, d)| d.clone());
            if let Some(dir) = dir {
                win.pane().navigate(dir);
            }
        });
        self.window.add_action(&open_trash);
        // New File / New Document: the target is a kind from newdoc::KINDS or "template:<path>".
        let new_file = gio::SimpleAction::new("new-file", Some(glib::VariantTy::STRING));
        let w = self.weak();
        new_file.connect_activate(move |_, v| {
            let (Some(win), Some(id)) = (w.upgrade(), v.and_then(|v| v.get::<String>())) else { return };
            let Some(name) = newdoc::default_name(&id) else { return };
            let dir = win.pane().path();
            let name = ops::unique(&dir.join(name)).file_name().unwrap_or_default().to_string_lossy().into_owned();
            let weak = win.weak();
            dialogs::prompt(win.window.upcast_ref(), "New File", "File name:", &name, move |name| {
                let Some(w) = weak.upgrade() else { return };
                match newdoc::create(&dir, name.trim(), &id) {
                    Ok(p) => {
                        ops::record(ops::Undo::Created(vec![p.clone()]));
                        w.pane().select_when_loaded(p.file_name().unwrap_or_default().to_os_string());
                    }
                    Err(e) => w.toasts.add_toast(notify::Toast::new(&format!("New file failed: {e}"))),
                }
            });
        });
        self.window.add_action(&new_file);
        let custom = gio::SimpleAction::new("custom-action", Some(glib::VariantTy::INT32));
        let w = self.weak();
        custom.connect_activate(move |_, v| {
            let (Some(win), Some(i)) = (w.upgrade(), v.and_then(|v| v.get::<i32>())) else { return };
            let Some(a) = actions::load().into_iter().nth(i as usize) else { return };
            if let Err(e) = actions::run(&a, &win.pane().path(), &win.pane().selected_paths()) {
                win.toasts.add_toast(notify::Toast::new(&format!("{}: {e}", a.name)));
            }
        });
        self.window.add_action(&custom);
        add("add-bookmark", |w| {
            let mut dirs: Vec<PathBuf> = w.pane().selected_paths().into_iter().filter(|p| p.is_dir()).collect();
            if dirs.is_empty() {
                dirs.push(w.pane().path());
            }
            let n = bookmarks::add(&dirs);
            w.fill_places();
            w.toasts.add_toast(notify::Toast::new(&format!("Bookmarked {n} folder(s)")));
        });
        add("open-archive", |w| {
            if let Some(a) = w.pane().selected_paths().into_iter().find(|p| crate::archive::is_archive(p)) {
                // Not a server: don't remember it under Network.
                let weak = w.weak();
                devices::connect(w.window.upcast_ref(), &crate::archive::uri(&a), move |res| {
                    let res = res.and_then(|c| match c {
                        devices::Connected::Path(p) => Ok(p),
                        devices::Connected::Shares(_) => Err("not an archive".into()),
                    });
                    win_open(&weak, res, None);
                });
            }
        });
        add("extract", |w| {
            let archives: Vec<PathBuf> = w.pane().selected_paths().into_iter().filter(|p| crate::archive::is_archive(p)).collect();
            let msg = match crate::archive::extract_here(&archives) {
                Ok(()) => format!("Extracting {} archive(s)…", archives.len()),
                Err(e) => format!("Extract failed: {e}"),
            };
            w.toasts.add_toast(notify::Toast::new(&msg));
        });
        add("pdf-convert", |w| {
            let files = w.pane().selected_in_pick_order();
            run_pdf(w, "Converting to PDF…", move || crate::pdf::convert_each(&files).map(|made| format!("Made {} PDF(s)", made.len())));
        });
        add("pdf-merge", |w| {
            let files = w.pane().selected_in_pick_order();
            let dir = w.pane().path();
            let name = ops::unique(&dir.join("Merged.pdf")).file_name().unwrap_or_default().to_string_lossy().into_owned();
            let weak = w.weak();
            dialogs::prompt(w.window.upcast_ref(), "Merge into PDF", &format!("{} files, in the order you picked them. PDF name:", files.len()), &name, move |name| {
                let Some(w) = weak.upgrade() else { return };
                let name = name.trim().to_string();
                let out = ops::unique(&dir.join(if name.to_lowercase().ends_with(".pdf") { name } else { format!("{name}.pdf") }));
                let files = files.clone();
                run_pdf(&w, "Merging into PDF…", move || {
                    crate::pdf::merge(&files, &out)?;
                    Ok(format!("Made {}", out.file_name().unwrap_or_default().to_string_lossy()))
                });
            });
        });
        add("compress", |w| {
            let files = w.pane().selected_paths();
            let Some(first) = files.first() else { return };
            let stem = if files.len() == 1 { first.file_stem().unwrap_or_default().to_string_lossy().into_owned() } else { "Archive".into() };
            let (dir, weak) = (w.pane().path(), w.weak());
            dialogs::compress(w.window.upcast_ref(), &stem, move |name| {
                let Some(w) = weak.upgrade() else { return };
                let target = ops::unique(&dir.join(name));
                let msg = match crate::archive::compress(&target, &files) {
                    Ok(()) => format!("Compressing into {}…", target.file_name().unwrap_or_default().to_string_lossy()),
                    Err(e) => format!("Compress failed: {e}"),
                };
                w.toasts.add_toast(notify::Toast::new(&msg));
            });
        });
        add("make-symlink", |w| make_links(w, false));
        add("make-hardlink", |w| make_links(w, true));
        add("open-tab-selected", |w| w.pane().open_selected(true));
        add("select-all", |w| {
            w.pane().canvas.update(|m| {
                for k in 0..m.order.len() {
                    let i = m.order[k] as usize;
                    m.selected[i] = true;
                }
            })
        });
        add("copy-path", |w| {
            let paths = w.pane().selected_paths();
            let text = paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n");
            w.window.clipboard().set_text(&text);
            w.toasts.add_toast(notify::Toast::new(&format!("Copied {} path(s)", paths.len())));
        });
        add("stats", |w| {
            let pane = w.pane();
            stats::show(w.window.upcast_ref(), &pane.path(), &pane.canvas.model());
        });
        add("select-pattern", |w| {
            let weak = w.weak();
            dialogs::prompt(w.window.upcast_ref(), "Select by Pattern", "Wildcards like *.jpg or img_??.png, or a regex between slashes: /^img_\\d+\\.png$/", "*", move |text| {
                let Some(w) = weak.upgrade() else { return };
                let msg = match w.pane().select_pattern(&text) {
                    Ok(n) => format!("Selected {n} item(s)"),
                    Err(e) => format!("Bad pattern: {e}"),
                };
                w.toasts.add_toast(notify::Toast::new(&msg));
            });
        });
        add("random-sample", |w| {
            let weak = w.weak();
            dialogs::prompt(w.window.upcast_ref(), "Random Sample", "How many files to pick at random from this folder (after filters)?", "100", move |text| {
                let Some(w) = weak.upgrade() else { return };
                let msg = match text.trim().parse::<usize>() {
                    Ok(n) => format!("Selected {} random file(s)", w.pane().random_sample(n)),
                    Err(_) => "Enter a number".to_string(),
                };
                w.toasts.add_toast(notify::Toast::new(&msg));
            });
        });
        add("change-icon", |w| {
            let paths = w.pane().selected_paths();
            if paths.is_empty() {
                return;
            }
            let weak = w.weak();
            dialogs::pick_icon(w.window.upcast_ref(), move |icon| {
                viewprops::set_custom_icon(&paths, Some(icon));
                if let Some(w) = weak.upgrade() {
                    w.pane().canvas.queue_draw();
                }
            });
        });
        add("reset-icon", |w| {
            viewprops::set_custom_icon(&w.pane().selected_paths(), None);
            w.pane().canvas.queue_draw();
        });

        let stateful = |name: &str, init: glib::Variant, f: fn(&Rc<Win>, &glib::Variant)| {
            let param = (init.type_() == glib::VariantTy::STRING).then_some(glib::VariantTy::STRING);
            let a = gio::SimpleAction::new_stateful(name, param, &init);
            let w = self.weak();
            a.connect_activate(move |a, v| {
                let Some(win) = w.upgrade() else { return };
                // Booleans toggle; strings take the chosen target.
                let next = match v {
                    Some(v) => v.clone(),
                    None => (!a.state().and_then(|s| s.get::<bool>()).unwrap_or(false)).to_variant(),
                };
                a.set_state(&next);
                f(&win, &next);
            });
            self.window.add_action(&a);
        };
        stateful("sort-by", "name".to_variant(), |w, v| {
            let key = match v.get::<String>().as_deref() {
                Some("size") => SortBy::Size,
                Some("modified") => SortBy::Modified,
                Some("type") => SortBy::Type,
                Some("extension") => SortBy::Extension,
                _ => SortBy::Name,
            };
            let desc = w.pane().canvas.model().desc;
            w.pane().sort_by(key, Some(desc));
        });
        stateful("sort-desc", false.to_variant(), |w, v| {
            let key = w.pane().canvas.model().sort;
            w.pane().sort_by(key, v.get::<bool>());
        });
        stateful("group-by", "none".to_variant(), |w, v| {
            w.pane().group_by(match v.get::<String>().as_deref() {
                Some("name") => GroupBy::Name,
                Some("type") => GroupBy::Type,
                Some("extension") => GroupBy::Extension,
                Some("size") => GroupBy::Size,
                Some("modified") => GroupBy::Modified,
                Some("dimensions") => GroupBy::Dimensions,
                _ => GroupBy::None,
            });
        });
        stateful("show-sidebar", true.to_variant(), |w, v| {
            w.sidebar.set_visible(v.get::<bool>().unwrap_or(true));
        });
        stateful("default-fm", crate::fm::is_default().to_variant(), |w, v| {
            let on = v.get::<bool>().unwrap_or(false);
            let msg = match crate::fm::set_default(on) {
                Ok(()) if on => {
                    if let Some(app) = w.window.application().and_downcast::<adw::Application>() {
                        crate::fm::serve(&app);
                    }
                    "Kolphin now opens folders and \"Show in folder\"".to_string()
                }
                Ok(()) => "Files (Nautilus) is the default file manager again".to_string(),
                Err(e) => format!("Could not change the default: {e}"),
            };
            w.toasts.add_toast(notify::Toast::new(&msg));
        });
        stateful("verify-copies", viewprops::verify_copies().to_variant(), |_, v| {
            viewprops::set_verify_copies(v.get::<bool>().unwrap_or(false));
        });
        stateful("auto-update", viewprops::check_updates_on_start().to_variant(), |_, v| {
            viewprops::set_check_updates_on_start(v.get::<bool>().unwrap_or(true));
        });
        stateful("sidebar-right", viewprops::sidebar_right().to_variant(), |w, v| {
            let right = v.get::<bool>().unwrap_or(false);
            viewprops::set_sidebar_right(right);
            w.place_sidebar(right);
        });

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
            win.pane().set_mode(m);
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
            let place = win.place_paths.borrow().get(row.index() as usize).cloned().flatten();
            match place {
                Some(Place::Path(path) | Place::Bookmark(path)) => win.pane().navigate(path),
                Some(Place::Share(path, _)) if path.is_dir() => win.pane().navigate(path),
                Some(Place::Share(_, uri)) => win.connect_server(uri),
                Some(Place::Volume(vol)) => {
                    let weak = win.weak();
                    devices::mount_volume(win.window.upcast_ref(), vol, move |res| win_open(&weak, res, None));
                }
                Some(Place::Server(uri)) => win.connect_server(uri),
                Some(Place::Connect) => {
                    let weak = win.weak();
                    dialogs::prompt(win.window.upcast_ref(), "Connect to Server", "Address, e.g. smb://nas.local/share, sftp://user@host/path, ftp://host", "smb://", move |uri| {
                        if let Some(win) = weak.upgrade() {
                            win.connect_server(uri);
                        }
                    });
                }
                None => {}
            }
        });
        // Right click: remove a bookmark / forget a server.
        let menu = gtk::GestureClick::builder().button(3).build();
        let w = self.weak();
        menu.connect_pressed(move |_, _, _, y| {
            let Some(win) = w.upgrade() else { return };
            let Some(row) = win.places.row_at_y(y as i32) else { return };
            let place = win.place_paths.borrow().get(row.index() as usize).cloned().flatten();
            let weak = win.weak();
            match place {
                Some(Place::Bookmark(p)) => {
                    let text = format!("Remove the bookmark for {}?", p.display());
                    dialogs::confirm(win.window.upcast_ref(), "Remove Bookmark", &text, "Remove", move || {
                        bookmarks::remove(&p);
                        if let Some(w) = weak.upgrade() {
                            w.fill_places();
                        }
                    });
                }
                Some(Place::Server(uri)) => {
                    let text = format!("Forget {uri}? Its saved password is removed too, so the next connection asks again.");
                    dialogs::confirm(win.window.upcast_ref(), "Forget Server", &text, "Forget", move || {
                        viewprops::forget_server(&uri);
                        devices::forget_password(&uri);
                        if let Some(w) = weak.upgrade() {
                            w.fill_places();
                        }
                    });
                }
                _ => {}
            }
        });
        self.places.add_controller(menu);
        // Drop folders on the sidebar to bookmark them.
        let drop = gtk::DropTarget::new(gtk::gdk::FileList::static_type(), gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::LINK);
        let w = self.weak();
        drop.connect_drop(move |_, value, _, _| {
            let (Some(win), Ok(list)) = (w.upgrade(), value.get::<gtk::gdk::FileList>()) else { return false };
            let dirs: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).filter(|p| p.is_dir()).collect();
            let n = bookmarks::add(&dirs);
            win.fill_places();
            win.toasts.add_toast(notify::Toast::new(&if n > 0 { format!("Bookmarked {n} folder(s)") } else { "Only folders can be bookmarked".to_string() }));
            n > 0
        });
        self.places.add_controller(drop);

        // GIO drops the monitor (and its signals) once nobody holds it: keep it for the app's life.
        let monitor = gio::VolumeMonitor::get();
        VOLUME_MONITOR.with(|m| *m.borrow_mut() = Some(monitor.clone()));
        let w = self.weak();
        let refresh = move || {
            if let Some(win) = w.upgrade() {
                win.fill_places();
            }
        };
        monitor.connect_mount_added(glib::clone!(#[strong] refresh, move |_, _| refresh()));
        monitor.connect_mount_removed(glib::clone!(#[strong] refresh, move |_, _| refresh()));
        monitor.connect_mount_changed(glib::clone!(#[strong] refresh, move |_, _| refresh()));
        monitor.connect_volume_added(glib::clone!(#[strong] refresh, move |_, _| refresh()));
        monitor.connect_volume_changed(glib::clone!(#[strong] refresh, move |_, _| refresh()));
        monitor.connect_volume_removed(glib::clone!(#[strong] refresh, move |_, _| refresh()));
        monitor.connect_drive_connected(glib::clone!(#[strong] refresh, move |_, _| refresh()));
        monitor.connect_drive_disconnected(move |_, _| refresh());
        self.fill_places();
        // Look for LAN hosts now and every minute.
        let w = self.weak();
        let scan = move || {
            let w = w.clone();
            devices::discover(move |hosts| {
                if let Some(win) = w.upgrade() {
                    if *win.hosts.borrow() != hosts {
                        *win.hosts.borrow_mut() = hosts;
                        win.fill_places();
                    }
                }
            });
        };
        scan();
        let w = self.weak();
        glib::timeout_add_seconds_local(60, move || {
            if w.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            scan();
            glib::ControlFlow::Continue
        });
    }

    fn connect_server(self: &Rc<Self>, uri: String) {
        let weak = self.weak();
        let uri = devices::normalize(&uri);
        // One-time use of an embedded password is allowed, but warn and never store it.
        let stripped = devices::strip_credentials(&uri);
        if stripped != uri {
            self.toasts.add_toast(notify::Toast::new("Password in the address will be used once and never stored. Prefer the password prompt."));
        }
        let u = stripped.clone();
        let display = devices::server_label(&stripped);
        self.toasts.add_toast(notify::Toast::new(&format!("Connecting to {display}…")));
        devices::connect(self.window.upcast_ref(), &uri, move |res| match res {
            Ok(devices::Connected::Path(p)) => win_open(&weak, Ok(p), Some(u)),
            Ok(devices::Connected::Shares(shares)) => {
                let Some(win) = weak.upgrade() else { return };
                let w2 = win.weak();
                let names = shares.iter().map(|(n, _)| n.clone()).collect();
                dialogs::choose(win.window.upcast_ref(), "Choose a Share", &format!("Shares on {u}"), names, move |i| {
                    if let Some(win) = w2.upgrade() {
                        win.connect_server(shares[i].1.clone());
                    }
                });
            }
            Err(e) => win_open(&weak, Err(e), None),
        });
    }

    /// Rebuild the sidebar now from what is known, then refresh free space and trashes in the background.
    fn fill_places(self: &Rc<Self>) {
        self.render_places();
        if self.side_busy.replace(true) {
            return;
        }
        let mounts: Vec<(PathBuf, String)> = gio::VolumeMonitor::get()
            .mounts()
            .iter()
            .filter_map(|m| Some((m.root().path()?, m.name().to_string())))
            .collect();
        let weak = self.weak();
        glib::spawn_future_local(async move {
            let info = gio::spawn_blocking(move || side_info(mounts)).await.unwrap_or_default();
            let Some(win) = weak.upgrade() else { return };
            win.side_busy.set(false);
            if *win.side.borrow() != info {
                *win.side.borrow_mut() = info;
                win.render_places();
            }
        });
    }

    fn render_places(self: &Rc<Self>) {
        while let Some(row) = self.places.row_at_index(0) {
            self.places.remove(&row);
        }
        let places = RefCell::new(Vec::new());
        let title = |text: &str| {
            let label = gtk::Label::builder().label(text).xalign(0.0).build();
            self.places.append(&gtk::ListBoxRow::builder().child(&label).selectable(false).activatable(false).css_classes(["side-title"]).build());
            places.borrow_mut().push(None);
        };
        // A row: icon + name (+ eject), and for mounted local drives a free-space bar.
        let add = |icon: gio::Icon, label: &str, place: Place, usage: Option<(u64, u64)>, eject: Option<gio::Mount>| {
            let line = gtk::Box::builder().spacing(11).build();
            line.append(&gtk::Image::from_gicon(&icon));
            line.append(&gtk::Label::builder().label(label).ellipsize(gtk::pango::EllipsizeMode::End).hexpand(true).xalign(0.0).build());
            if let Some(m) = eject {
                let b = gtk::Button::builder().icon_name("media-eject-symbolic").css_classes(["flat", "eject"]).tooltip_text("Eject / unmount").build();
                let weak = self.weak();
                b.connect_clicked(move |_| {
                    let Some(win) = weak.upgrade() else { return };
                    let w2 = win.weak();
                    let root = m.root().path();
                    let name = m.name().to_string();
                    let busy = notify::Toast::builder().title(format!("Unmounting {name}…")).timeout(0).build();
                    win.toasts.add_toast(busy.clone());
                    devices::eject(win.window.upcast_ref(), m.clone(), move |res| {
                        busy.dismiss();
                        let Some(win) = w2.upgrade() else { return };
                        win.fill_places();
                        match res {
                            // Views inside the ejected drive go home instead of showing a vanished folder.
                            Ok(()) => {
                                win.toasts.add_toast(notify::Toast::new(&format!("Unmounted {name}")));
                                for pane in win.all_panes() {
                                    if root.as_ref().is_some_and(|r| pane.path().starts_with(r)) {
                                        pane.navigate(glib::home_dir());
                                    }
                                }
                            }
                            Err(e) => win.toasts.add_toast(notify::Toast::new(&format!("Eject failed: {e}"))),
                        }
                    });
                });
                line.append(&b);
            }
            let row = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
            row.append(&line);
            if let Some((free, size)) = usage {
                let used = 1.0 - free as f64 / size as f64;
                let bar = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).value(used).css_classes(["usage"]).build();
                row.set_tooltip_text(Some(&format!("{:.0}% used · {} free of {}", used * 100.0, glib::format_size(free), glib::format_size(size))));
                row.append(&bar);
            }
            self.places.append(&row);
            places.borrow_mut().push(Some(place));
        };
        title("PLACES");
        let themed = |name: &str| gio::ThemedIcon::new(name).upcast::<gio::Icon>();
        add(themed("user-home-symbolic"), "Home", Place::Path(glib::home_dir()), None, None);
        for (dir, icon) in [
            (glib::UserDirectory::Desktop, "user-desktop-symbolic"),
            (glib::UserDirectory::Documents, "folder-documents-symbolic"),
            (glib::UserDirectory::Downloads, "folder-download-symbolic"),
            (glib::UserDirectory::Pictures, "folder-pictures-symbolic"),
            (glib::UserDirectory::Videos, "folder-videos-symbolic"),
            (glib::UserDirectory::Music, "folder-music-symbolic"),
        ] {
            if let Some(p) = special_dir(dir) {
                add(themed(icon), &folder_name(&p), Place::Path(p), None, None);
            }
        }

        title("BOOKMARKS");
        let marks = bookmarks::load();
        let builtin: Vec<PathBuf> = places.borrow().iter().flatten().filter_map(|p| if let Place::Path(p) = p { Some(p.clone()) } else { None }).collect();
        let marks: Vec<_> = marks.into_iter().filter(|(p, _)| !builtin.contains(p)).collect();
        if marks.is_empty() {
            let hint = gtk::Label::builder().label("Drag folders here").xalign(0.0).css_classes(["dim-label"]).build();
            self.places.append(&gtk::ListBoxRow::builder().child(&hint).selectable(false).activatable(false).build());
            places.borrow_mut().push(None);
        }
        for (path, label) in marks {
            add(themed("folder-symbolic"), &label, Place::Bookmark(path), None, None);
        }

        // One Trash row; other drives' non-empty trashes are in its ▾ menu.
        let monitor = gio::VolumeMonitor::get();
        let side = self.side.borrow().clone();
        let home_trash = glib::user_data_dir().join("Trash/files");
        let trashes = if side.trashes.is_empty() { vec![("Home".to_string(), home_trash)] } else { side.trashes.clone() };
        let any_full = side.any_full;
        let line = gtk::Box::builder().spacing(11).build();
        line.append(&gtk::Image::from_gicon(&themed(if any_full { "user-trash-full-symbolic" } else { "user-trash-symbolic" })));
        line.append(&gtk::Label::builder().label("Trash").hexpand(true).xalign(0.0).build());
        if trashes.len() > 1 {
            let menu = gio::Menu::new();
            for (i, (label, _)) in trashes.iter().enumerate() {
                let item = gio::MenuItem::new(Some(&format!("Trash on {label}")), None);
                item.set_action_and_target_value(Some("win.open-trash"), Some(&(i as i32).to_variant()));
                menu.append_item(&item);
            }
            line.append(&gtk::MenuButton::builder().icon_name("pan-down-symbolic").menu_model(&menu).css_classes(["flat", "eject"]).tooltip_text("Trash on other drives").build());
        }
        self.places.append(&line);
        places.borrow_mut().push(Some(Place::Path(trashes[0].1.clone())));
        *self.trashes.borrow_mut() = trashes;

        title("DEVICES");
        add(themed("drive-harddisk-symbolic"), "Root", Place::Path(PathBuf::from("/")), side.usage.get(std::path::Path::new("/")).copied(), None);
        let mut network = Vec::new();
        for m in monitor.mounts() {
            let Some(path) = m.root().path() else { continue };
            if devices::is_remote(&path) {
                network.push(m);
                continue;
            }
            let eject = devices::ejectable(&m).then(|| m.clone());
            add(m.symbolic_icon(), &m.name(), Place::Path(path.clone()), side.usage.get(&path).copied(), eject);
        }
        // Plugged in but not mounted yet (USB sticks, other partitions): click to mount.
        for v in monitor.volumes() {
            if v.get_mount().is_none() && v.can_mount() {
                add(v.symbolic_icon(), &v.name(), Place::Volume(v.clone()), None, None);
            }
        }

        title("NETWORK");
        let mut shown = Vec::new();
        for m in network {
            let uri = m.root().uri().to_string();
            // A share whose folder is gone was unmounted behind gvfs' back: offer the saved server instead.
            if let Some(path) = m.root().path().filter(|p| !side.dead.contains(p)) {
                shown.push(uri.clone());
                add(m.symbolic_icon(), &m.name(), Place::Share(path, uri), None, Some(m.clone()));
            }
        }
        // gvfs lower-cases share names in its URIs, so compare without case.
        let shown: Vec<String> = shown.iter().map(|s| s.to_lowercase()).collect();
        for uri in viewprops::recent_servers().into_iter().filter(|u| !shown.iter().any(|s| s.starts_with(&u.trim_end_matches('/').to_lowercase()))) {
            add(themed("network-server-symbolic"), &devices::server_label(&uri), Place::Server(uri.clone()), None, None);
        }
        for (name, uri) in self.hosts.borrow().iter() {
            add(themed("network-workgroup-symbolic"), name, Place::Server(uri.clone()), None, None);
        }
        add(themed("list-add-symbolic"), "Connect to Server…", Place::Connect, None, None);
        *self.place_paths.borrow_mut() = places.into_inner();
    }
}

/// An XDG folder (Downloads, …); when user-dirs points it at $HOME, fall back to ~/<English name> if that exists
/// (a symlink to another drive counts).
fn special_dir(dir: glib::UserDirectory) -> Option<PathBuf> {
    let home = glib::home_dir();
    let p = glib::user_special_dir(dir).filter(|p| p.is_dir() && p.as_path() != home.as_path() && p.as_path() != home.join("").as_path());
    p.or_else(|| {
        let name = match dir {
            glib::UserDirectory::Desktop => "Desktop",
            glib::UserDirectory::Documents => "Documents",
            glib::UserDirectory::Downloads => "Downloads",
            glib::UserDirectory::Pictures => "Pictures",
            glib::UserDirectory::Videos => "Videos",
            glib::UserDirectory::Music => "Music",
            _ => return None,
        };
        Some(home.join(name)).filter(|p| p.is_dir())
    })
}

/// A sidebar entry and what clicking it does.
#[derive(Clone)]
enum Place {
    Path(PathBuf),
    Bookmark(PathBuf),
    /// A network mount: its local path, and the URI to reconnect if it went away.
    Share(PathBuf, String),
    Volume(gio::Volume),
    Server(String),
    Connect,
}

/// After mounting: open the result in the active view (and remember a server), or report the error.
fn win_open(weak: &Weak<Win>, res: Result<PathBuf, String>, server: Option<String>) {
    let Some(win) = weak.upgrade() else { return };
    match res {
        Ok(path) => {
            if let Some(uri) = server {
                viewprops::add_recent_server(&uri);
            }
            win.pane().navigate(path);
            win.fill_places();
        }
        Err(e) => win.toasts.add_toast(notify::Toast::new(&format!("Could not open: {e}"))),
    }

}

fn make_links(w: &Rc<Win>, hard: bool) {
    let msg = match ops::make_links(&w.pane().selected_paths(), hard) {
        Ok(made) => {
            let n = made.len();
            ops::record(ops::Undo::Created(made));
            format!("Created {n} {} · Ctrl+Z to undo", if hard { "hard link(s)" } else { "symlink(s)" })
        }
        Err(e) => format!("Link failed: {e}"),
    };
    w.toasts.add_toast(notify::Toast::new(&msg));
}

/// View dropdown: mode, sort, group, hidden files, filter.
fn view_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = |title: &str, items: &[(&str, &str)], action: &str| {
        let s = gio::Menu::new();
        for (label, target) in items {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
            s.append_item(&item);
        }
        menu.append_section(Some(title), &s);
        s
    };
    section("View", &[("Icons", "icons"), ("Compact", "compact"), ("Details", "details")], "win.view-mode");
    let sort = section(
        "Sort by",
        &[("Name", "name"), ("Size", "size"), ("Modified", "modified"), ("Type", "type"), ("Extension", "extension")],
        "win.sort-by",
    );
    sort.append(Some("Descending"), Some("win.sort-desc"));
    section(
        "Group by",
        &[("None", "none"), ("Name", "name"), ("Type", "type"), ("Extension", "extension"), ("Size", "size"), ("Modified", "modified"), ("Image Size (W×H)", "dimensions")],
        "win.group-by",
    );
    let more = gio::Menu::new();
    more.append(Some("Show Hidden Files"), Some("win.toggle-hidden"));
    more.append(Some("Filter…"), Some("win.filter"));
    menu.append_section(None, &more);
    menu
}

fn folder_name(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
}
