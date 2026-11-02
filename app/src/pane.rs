//! One folder view: location bar, file canvas, status bar, history, loading and live updates.
use crate::canvas::{Action, Canvas, ViewMode};
use crate::model::{self, Entry, Msg, UNKNOWN_SIZE};
use crate::pathbar::PathBar;
use crate::viewprops::{self, ViewProps};
use gtk::{gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

type Callback = RefCell<Option<Box<dyn Fn()>>>;
type PathCallback = RefCell<Option<Box<dyn Fn(PathBuf)>>>;

#[derive(Default)]
struct Changes {
    created: HashSet<OsString>,
    deleted: HashSet<OsString>,
    scheduled: bool,
}

pub struct Pane {
    pub root: gtk::Box,
    pub pathbar: Rc<PathBar>,
    pub canvas: Canvas,
    vscroll: gtk::Scrollbar,
    hscroll: gtk::Scrollbar,
    status: gtk::Label,
    slider: gtk::Scale,
    syncing: Cell<bool>,
    filter: gtk::SearchEntry,
    filter_bar: gtk::Revealer,
    path: RefCell<PathBuf>,
    back: RefCell<Vec<PathBuf>>,
    forward: RefCell<Vec<PathBuf>>,
    generation: Cell<u64>,
    loading: Cell<bool>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    changes: RefCell<Changes>,
    pending_select: RefCell<Option<OsString>>,
    pub on_navigated: Callback,
    /// Sort/group/mode changed (refresh the window's menus).
    pub on_view: Callback,
    pub on_focus: Callback,
    pub on_open_tab: PathCallback,
}

impl Pane {
    pub fn new(path: PathBuf, mode: ViewMode, show_hidden: bool) -> Rc<Self> {
        let pathbar = PathBar::new();
        let canvas = Canvas::default();
        canvas.reset(show_hidden);
        let vscroll = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&canvas.adjustment()));
        let hscroll = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&canvas.hadjustment()));
        // Show a scrollbar only when there is something to scroll.
        for bar in [&vscroll, &hscroll] {
            let adj = bar.adjustment();
            let bar = bar.downgrade();
            adj.connect_changed(move |a| {
                if let Some(bar) = bar.upgrade() {
                    bar.set_opacity(if a.upper() > a.page_size() + 0.5 { 1.0 } else { 0.0 });
                }
            });
        }
        let view = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        view.append(&canvas);
        view.append(&vscroll);
        let status = gtk::Label::builder().xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build();
        let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 16.0, 256.0, 1.0);
        slider.set_width_request(140);
        slider.set_tooltip_text(Some("Icon size (Ctrl+scroll)"));
        let status_bar = gtk::Box::builder().spacing(12).css_classes(["statusbar"]).build();
        status_bar.append(&status);
        status_bar.append(&gtk::Image::from_icon_name("zoom-out-symbolic"));
        status_bar.append(&slider);
        let filter = gtk::SearchEntry::builder().placeholder_text("Filter — text or pattern like *.jpg").hexpand(true).build();
        let filter_bar = gtk::Revealer::builder().child(&filter).css_classes(["filterbar"]).build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&pathbar.root);
        root.append(&filter_bar);
        root.append(&view);
        root.append(&hscroll);
        root.append(&status_bar);

        let pane = Rc::new(Pane {
            root,
            pathbar,
            canvas,
            vscroll,
            hscroll,
            status,
            slider,
            syncing: Default::default(),
            filter,
            filter_bar,
            path: RefCell::new(path),
            back: Default::default(),
            forward: Default::default(),
            generation: Default::default(),
            loading: Default::default(),
            monitor: Default::default(),
            changes: Default::default(),
            pending_select: Default::default(),
            on_navigated: Default::default(),
            on_view: Default::default(),
            on_focus: Default::default(),
            on_open_tab: Default::default(),
        });
        let weak = Rc::downgrade(&pane);
        pane.pathbar.connect_go(glib::clone!(#[strong] weak, move |p| {
            if let Some(pane) = weak.upgrade() {
                pane.navigate(p);
            }
        }));
        pane.pathbar.connect_cancel(glib::clone!(#[strong] weak, move || {
            if let Some(pane) = weak.upgrade() {
                pane.canvas.grab_focus();
            }
        }));
        pane.canvas.connect_action(glib::clone!(#[strong] weak, move |a| {
            if let Some(pane) = weak.upgrade() {
                pane.on_canvas(a);
            }
        }));
        pane.slider.connect_value_changed(glib::clone!(#[strong] weak, move |s| {
            let Some(pane) = weak.upgrade() else { return };
            if !pane.syncing.get() {
                pane.canvas.set_icon_size(s.value());
                pane.save_props();
            }
        }));
        pane.filter.connect_search_changed(glib::clone!(#[strong] weak, move |e| {
            if let Some(pane) = weak.upgrade() {
                let text = e.text().to_string();
                pane.canvas.update(|m| {
                    m.filter = text;
                    m.rebuild_order();
                });
            }
        }));
        pane.filter.connect_stop_search(glib::clone!(#[strong] weak, move |_| {
            if let Some(pane) = weak.upgrade() {
                pane.close_filter();
            }
        }));
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(#[strong] weak, move |_| {
            if let Some(pane) = weak.upgrade() {
                if let Some(f) = pane.on_focus.borrow().as_ref() {
                    f();
                }
            }
        }));
        pane.root.add_controller(focus);
        pane.show_mode(mode);
        pane.load();
        pane
    }

    pub fn set_mode(&self, mode: ViewMode) {
        self.show_mode(mode);
        self.save_props();
    }

    fn show_mode(&self, mode: ViewMode) {
        let compact = mode == ViewMode::Compact;
        self.vscroll.set_visible(!compact);
        self.hscroll.set_visible(compact);
        self.canvas.set_mode(mode);
        self.sync_slider();
    }

    fn sync_slider(&self) {
        let (lo, hi) = self.canvas.icon_range();
        self.syncing.set(true);
        self.slider.set_range(lo, hi);
        self.slider.set_value(self.canvas.icon_size());
        self.syncing.set(false);
    }

    /// Sort by `key`; the same key again flips the direction.
    pub fn sort_by(&self, key: model::SortBy, desc: Option<bool>) {
        self.canvas.update(|m| {
            m.desc = desc.unwrap_or(m.sort == key && !m.desc);
            m.sort = key;
            m.rebuild_order();
        });
        self.save_props();
    }

    pub fn group_by(self: &Rc<Self>, group: model::GroupBy) {
        self.canvas.update(|m| {
            m.group = group;
            m.rebuild_order();
        });
        self.save_props();
        self.read_dims();
    }

    /// Read image sizes from file headers in the background (only while grouping by dimensions).
    fn read_dims(self: &Rc<Self>) {
        if self.canvas.model().group != model::GroupBy::Dimensions {
            return;
        }
        let todo: Vec<(u32, PathBuf)> = {
            let m = self.canvas.model();
            let dir = self.path();
            m.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.is_dir && !e.deleted && e.dims.is_none())
                .filter(|(_, e)| crate::thumbs::kind(&e.name.to_string_lossy()) == Some(crate::thumbs::Kind::Image))
                .map(|(i, e)| (i as u32, dir.join(&*e.name)))
                .collect()
        };
        if todo.is_empty() {
            return;
        }
        let (tx, rx) = async_channel::unbounded::<Vec<(u32, Option<(u32, u32)>)>>();
        std::thread::spawn(move || {
            let (mut batch, mut last) = (Vec::new(), Instant::now());
            for (i, path) in todo {
                batch.push((i, imagesize::size(&path).ok().map(|s| (s.width as u32, s.height as u32))));
                if last.elapsed() > Duration::from_millis(300) {
                    last = Instant::now();
                    if tx.send_blocking(std::mem::take(&mut batch)).is_err() {
                        return;
                    }
                }
            }
            let _ = tx.send_blocking(batch);
        });
        let (weak, generation) = (Rc::downgrade(self), self.generation.get());
        glib::spawn_future_local(async move {
            while let Ok(batch) = rx.recv().await {
                let Some(pane) = weak.upgrade() else { return };
                if pane.generation.get() != generation {
                    return;
                }
                pane.canvas.update(|m| {
                    for (i, d) in batch {
                        m.entries[i as usize].dims = Some(d);
                    }
                    m.rebuild_order();
                });
            }
        });
    }

    /// Select names matching a wildcard pattern, or a regex written as /…/; returns how many.
    pub fn select_pattern(&self, pattern: &str) -> Result<usize, String> {
        let pattern = pattern.trim();
        let re = match pattern.strip_prefix('/').and_then(|p| p.strip_suffix('/')) {
            Some(r) => Some(regex::RegexBuilder::new(r).case_insensitive(true).build().map_err(|e| e.to_string())?),
            None => None,
        };
        let mut n = 0;
        self.canvas.update(|m| {
            m.selected.iter_mut().for_each(|s| *s = false);
            for k in 0..m.order.len() {
                let i = m.order[k] as usize;
                let name = m.entries[i].name.to_string_lossy();
                let hit = match &re {
                    Some(re) => re.is_match(&name),
                    None => model::glob_match(pattern, &name),
                };
                if hit {
                    m.selected[i] = true;
                    n += 1;
                }
            }
        });
        Ok(n)
    }

    /// Select `n` random visible files (folders are skipped); returns how many.
    pub fn random_sample(&self, n: usize) -> usize {
        let mut seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut picked = 0;
        self.canvas.update(|m| {
            let mut files: Vec<u32> = m.order.iter().copied().filter(|&i| !m.entries[i as usize].is_dir).collect();
            let k = n.min(files.len());
            for j in 0..k {
                let r = j + (rand() as usize) % (files.len() - j);
                files.swap(j, r);
            }
            m.selected.iter_mut().for_each(|s| *s = false);
            for &i in &files[..k] {
                m.selected[i as usize] = true;
            }
            picked = k;
        });
        picked
    }

    /// Enter on the selection: one item opens like a double click; several open folders in tabs and files in their apps.
    pub fn open_selected(self: &Rc<Self>, in_tabs: bool) {
        let sel = self.canvas.selection();
        if sel.len() == 1 && !in_tabs {
            return self.on_canvas(Action::Activate(sel[0]));
        }
        for idx in sel {
            let is_dir = self.canvas.model().entries[idx as usize].is_dir;
            if is_dir {
                self.on_canvas(Action::OpenInNewTab(idx));
            } else if !in_tabs {
                self.on_canvas(Action::Activate(idx));
            }
        }
    }

    pub fn toggle_filter(&self) {
        if self.filter_bar.reveals_child() && self.filter.has_focus() {
            self.close_filter();
        } else {
            self.filter_bar.set_reveal_child(true);
            self.filter.grab_focus();
        }
    }

    fn close_filter(&self) {
        self.filter.set_text("");
        self.filter_bar.set_reveal_child(false);
        self.canvas.grab_focus();
    }

    fn props(&self) -> ViewProps {
        let m = self.canvas.model();
        let (icon_size, list_size) = self.canvas.sizes();
        let mut collapsed: Vec<String> = m.collapsed.iter().cloned().collect();
        collapsed.sort();
        ViewProps { mode: self.canvas.mode(), icon_size, list_size, sort: m.sort, desc: m.desc, group: m.group, collapsed }
    }

    fn save_props(&self) {
        viewprops::save(&self.path(), self.props());
        if let Some(f) = self.on_view.borrow().as_ref() {
            f();
        }
    }

    fn apply_props(&self, p: &ViewProps) {
        self.canvas.set_sizes(p.icon_size, p.list_size);
        self.show_mode(p.mode);
        self.canvas.update(|m| {
            m.sort = p.sort;
            m.desc = p.desc;
            m.group = p.group;
            m.collapsed = p.collapsed.iter().cloned().collect();
        });
        if let Some(f) = self.on_view.borrow().as_ref() {
            f();
        }
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        let dir = self.path();
        let m = self.canvas.model();
        self.canvas.selection().into_iter().map(|i| dir.join(&*m.entries[i as usize].name)).collect()
    }

    pub fn navigate(self: &Rc<Self>, path: PathBuf) {
        if path == *self.path.borrow() {
            return self.load();
        }
        let old = self.path.replace(path);
        self.back.borrow_mut().push(old);
        self.forward.borrow_mut().clear();
        self.load();
    }

    pub fn go_back(self: &Rc<Self>) {
        if let Some(p) = self.back.borrow_mut().pop() {
            let old = self.path.replace(p);
            self.forward.borrow_mut().push(old);
            self.load();
        }
    }

    pub fn go_forward(self: &Rc<Self>) {
        if let Some(p) = self.forward.borrow_mut().pop() {
            let old = self.path.replace(p);
            self.back.borrow_mut().push(old);
            self.load();
        }
    }

    /// Go to the parent folder and select the folder we came from.
    pub fn go_up(self: &Rc<Self>) {
        let cur = self.path();
        if let (Some(parent), Some(name)) = (cur.parent(), cur.file_name()) {
            let name = name.to_os_string();
            self.navigate(parent.to_path_buf());
            self.select_when_loaded(name);
        }
    }

    pub fn reload(self: &Rc<Self>) {
        self.load();
    }

    pub fn set_show_hidden(&self, show: bool) {
        self.canvas.update(|m| {
            m.show_hidden = show;
            m.rebuild_order();
        });
    }

    fn select_when_loaded(self: &Rc<Self>, name: OsString) {
        *self.pending_select.borrow_mut() = Some(name);
    }

    fn load(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.monitor.take();
        *self.changes.borrow_mut() = Changes::default();
        self.pending_select.take();
        let path = self.path();
        let show_hidden = self.canvas.model().show_hidden;
        self.canvas.reset(show_hidden);
        self.canvas.set_dir(path.clone());
        self.filter.set_text("");
        self.filter_bar.set_reveal_child(false);
        self.apply_props(&viewprops::load(&path).unwrap_or_default());
        self.pathbar.set_path(&path);
        self.loading.set(true);
        if let Some(f) = self.on_navigated.borrow().as_ref() {
            f();
        }

        let (tx, rx) = async_channel::bounded(8);
        let dir = path.clone();
        std::thread::spawn(move || model::scan(dir, tx));
        let bench = std::env::var_os("KOLPHIN_BENCH").is_some();
        let t0 = Instant::now();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut first = true;
            let mut names_done = false;
            while let Ok(msg) = rx.recv().await {
                let Some(pane) = weak.upgrade() else { return };
                if pane.generation.get() != generation {
                    return;
                }
                match msg {
                    Msg::Entries(batch) => {
                        if bench && first {
                            eprintln!("[bench] first batch ({} entries) after {:.0} ms", batch.len(), t0.elapsed().as_secs_f64() * 1000.0);
                        }
                        first = false;
                        pane.canvas.update(|m| m.append(batch));
                        pane.try_select_pending();
                    }
                    Msg::Stats { start, stats } => {
                        if bench && !names_done {
                            eprintln!("[bench] {} names listed after {:.0} ms", pane.canvas.model().entries.len(), t0.elapsed().as_secs_f64() * 1000.0);
                        }
                        names_done = true;
                        pane.canvas.update(|m| {
                            for (i, st) in stats.into_iter().enumerate() {
                                m.entries[start + i].set_stat(st);
                            }
                            if m.sort != model::SortBy::Name || m.group != model::GroupBy::None {
                                m.rebuild_order();
                            }
                        });
                        pane.canvas.invalidate_text();
                    }
                    Msg::Done => {
                        if bench {
                            eprintln!("[bench] stat done after {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
                        }
                        pane.loading.set(false);
                        pane.canvas.update(|m| m.entries.shrink_to_fit());
                        pane.after_load();
                        pane.watch();
                        pane.update_status();
                        if bench {
                            run_bench(&pane);
                        }
                    }
                    Msg::Error(e) => {
                        pane.loading.set(false);
                        pane.status.set_text(&e);
                    }
                }
            }
        });
    }

    fn try_select_pending(&self) {
        let found = {
            let pending = self.pending_select.borrow();
            pending.as_ref().and_then(|n| self.canvas.model().find(n))
        };
        if let Some(idx) = found {
            self.pending_select.take();
            self.canvas.select_only(idx);
        }
    }

    fn on_canvas(self: &Rc<Self>, a: Action) {
        match a {
            Action::Activate(idx) => {
                let (name, is_dir) = {
                    let m = self.canvas.model();
                    let e = &m.entries[idx as usize];
                    (e.name.to_os_string(), e.is_dir)
                };
                let target = self.path().join(name);
                if is_dir {
                    self.navigate(target);
                } else {
                    let ctx = self.root.display().app_launch_context();
                    if let Err(e) = gio::AppInfo::launch_default_for_uri(&gio::File::for_path(&target).uri(), Some(&ctx)) {
                        self.status.set_text(&e.to_string());
                    }
                }
            }
            Action::OpenInNewTab(idx) => {
                let name = self.canvas.model().entries[idx as usize].name.to_os_string();
                if let Some(f) = self.on_open_tab.borrow().as_ref() {
                    f(self.path().join(name));
                }
            }
            Action::Up => self.go_up(),
            Action::Changed => self.update_status(),
            Action::SortBy(key) => self.sort_by(key, None),
            Action::ViewChanged => {
                self.sync_slider();
                self.save_props();
            }
            Action::ContextMenu { x, y, on_item } => self.context_menu(x, y, on_item),
        }
    }

    fn context_menu(&self, x: f64, y: f64, on_item: bool) {
        let menu = gio::Menu::new();
        let section = |items: &[(&str, &str)]| {
            let s = gio::Menu::new();
            for (label, action) in items {
                s.append(Some(label), Some(action));
            }
            menu.append_section(None, &s);
        };
        if on_item {
            section(&[("Open", "win.open-selected"), ("Open in New Tab", "win.open-tab-selected")]);
            section(&[("Cut", "win.cut"), ("Copy", "win.copy"), ("Paste", "win.paste"), ("Copy Path", "win.copy-path")]);
            section(&[("Change Icon…", "win.change-icon"), ("Reset Icon", "win.reset-icon")]);
        } else {
            section(&[("Paste", "win.paste"), ("Select All", "win.select-all")]);
            section(&[("Select by Pattern…", "win.select-pattern"), ("Random Sample…", "win.random-sample")]);
        }
        section(&[("Folder Statistics…", "win.stats")]);
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(&self.canvas);
        pop.set_has_arrow(false);
        pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    fn update_status(&self) {
        let m = self.canvas.model();
        let (mut dirs, mut files, mut bytes, mut sel, mut sel_bytes) = (0usize, 0usize, 0u64, 0usize, 0u64);
        for &i in &m.order {
            let e = &m.entries[i as usize];
            let size = if e.size == UNKNOWN_SIZE { 0 } else { e.size };
            if e.is_dir {
                dirs += 1;
            } else {
                files += 1;
                bytes += size;
            }
            if m.selected[i as usize] {
                sel += 1;
                sel_bytes += size;
            }
        }
        let mut text = format!("{dirs} folders, {files} files ({})", glib::format_size(bytes));
        if sel > 0 {
            text += &format!("  ·  {sel} selected ({})", glib::format_size(sel_bytes));
        }
        if self.loading.get() {
            text += "  ·  loading…";
        }
        self.status.set_text(&text);
    }

    /// Watch the folder; changes are batched and applied every 200 ms.
    fn watch(self: &Rc<Self>) {
        let file = gio::File::for_path(self.path());
        let Ok(monitor) = file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>) else { return };
        let weak = Rc::downgrade(self);
        monitor.connect_changed(move |_, file, other, event| {
            let Some(pane) = weak.upgrade() else { return };
            let name = |f: &gio::File| f.basename().map(PathBuf::into_os_string);
            {
                let mut c = pane.changes.borrow_mut();
                match event {
                    gio::FileMonitorEvent::Created | gio::FileMonitorEvent::MovedIn | gio::FileMonitorEvent::ChangesDoneHint => {
                        c.created.extend(name(file));
                    }
                    gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::MovedOut => c.deleted.extend(name(file)),
                    gio::FileMonitorEvent::Renamed => {
                        c.deleted.extend(name(file));
                        c.created.extend(other.and_then(name));
                    }
                    _ => return,
                }
                if c.scheduled {
                    return;
                }
                c.scheduled = true;
            }
            let weak = Rc::downgrade(&pane);
            glib::timeout_add_local_once(Duration::from_millis(200), move || {
                if let Some(pane) = weak.upgrade() {
                    pane.apply_changes();
                }
            });
        });
        *self.monitor.borrow_mut() = Some(monitor);
    }

    fn apply_changes(self: &Rc<Self>) {
        let Changes { created, mut deleted, .. } = std::mem::take(&mut *self.changes.borrow_mut());
        let dir = self.path();
        // Re-add created/changed names that still exist, with fresh stats.
        deleted.extend(created.iter().cloned());
        let mut added: Vec<Entry> = created
            .into_iter()
            .filter_map(|name| {
                let path = dir.join(&name);
                let meta = std::fs::symlink_metadata(&path).ok()?;
                let mut e = Entry::new(name, path.is_dir() || meta.is_dir());
                e.symlink = meta.file_type().is_symlink();
                e.set_stat(model::stat(&path));
                Some(e)
            })
            .collect();
        added.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        self.canvas.update(|m| {
            m.remove_names(&deleted);
            m.append(added);
        });
        self.read_dims();
    }

    fn after_load(self: &Rc<Self>) {
        self.read_dims();
    }
}

fn run_bench(pane: &Rc<Pane>) {
    let rss = || {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS")).map(|l| l.split_whitespace().nth(1).unwrap_or("0").parse::<u64>().unwrap_or(0) / 1024))
            .unwrap_or(0)
    };
    eprintln!("[bench] RSS after load: {} MB", rss());
    let canvas = pane.canvas.clone();
    canvas.set_mode(ViewMode::Icons);
    canvas.clone().bench_scroll(move |report| {
        eprintln!("[bench] icons: {report}");
        canvas.set_mode(ViewMode::Details);
        canvas.adjustment().set_value(0.0);
        canvas.bench_scroll(move |report| {
            eprintln!("[bench] details: {report}");
            eprintln!("[bench] RSS after scroll: {} MB", rss());
            if let Some(app) = gio::Application::default() {
                app.quit();
            }
        });
    });
}
