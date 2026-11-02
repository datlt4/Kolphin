//! One folder view: location bar, file canvas, status bar, history, loading and live updates.
use crate::canvas::{Action, Canvas, ViewMode};
use crate::model::{self, Entry, Msg, UNKNOWN_SIZE};
use crate::pathbar::PathBar;
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
    status: gtk::Label,
    path: RefCell<PathBuf>,
    back: RefCell<Vec<PathBuf>>,
    forward: RefCell<Vec<PathBuf>>,
    generation: Cell<u64>,
    loading: Cell<bool>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    changes: RefCell<Changes>,
    pending_select: RefCell<Option<OsString>>,
    pub on_navigated: Callback,
    pub on_focus: Callback,
    pub on_open_tab: PathCallback,
}

impl Pane {
    pub fn new(path: PathBuf, mode: ViewMode, show_hidden: bool) -> Rc<Self> {
        let pathbar = PathBar::new();
        let canvas = Canvas::default();
        canvas.set_mode(mode);
        canvas.reset(show_hidden);
        let scrollbar = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&canvas.adjustment()));
        let view = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        view.append(&canvas);
        view.append(&scrollbar);
        let status = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["statusbar"]).build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&pathbar.root);
        root.append(&view);
        root.append(&status);

        let pane = Rc::new(Pane {
            root,
            pathbar,
            canvas,
            status,
            path: RefCell::new(path),
            back: Default::default(),
            forward: Default::default(),
            generation: Default::default(),
            loading: Default::default(),
            monitor: Default::default(),
            changes: Default::default(),
            pending_select: Default::default(),
            on_navigated: Default::default(),
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
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(#[strong] weak, move |_| {
            if let Some(pane) = weak.upgrade() {
                if let Some(f) = pane.on_focus.borrow().as_ref() {
                    f();
                }
            }
        }));
        pane.root.add_controller(focus);
        pane.load();
        pane
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
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
                            for (i, (size, mtime)) in stats.into_iter().enumerate() {
                                let e = &mut m.entries[start + i];
                                e.size = size;
                                e.mtime = mtime;
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
        }
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

    fn apply_changes(&self) {
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
                (e.size, e.mtime) = model::stat(&path);
                Some(e)
            })
            .collect();
        added.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        self.canvas.update(|m| {
            m.remove_names(&deleted);
            m.append(added);
        });
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
