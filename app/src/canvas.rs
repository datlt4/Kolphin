//! Virtualized file view: only the visible items are laid out and drawn (Icons / Compact / Details).
use crate::icons;
use crate::model::{GroupBy, Model, SortBy, UNKNOWN_SIZE, UNKNOWN_TIME};
use crate::thumbs;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk, pango, prelude::*};
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ViewMode {
    #[default]
    Icons,
    Compact,
    Details,
}

pub enum Action {
    Activate(u32),
    OpenInNewTab(u32),
    Up,
    /// Details column header clicked.
    SortBy(SortBy),
    /// Group collapsed/expanded or icon size changed (save the folder's view).
    ViewChanged,
    /// Right click at (x, y); `on_item` when over an entry (it is selected first).
    ContextMenu { x: f64, y: f64, on_item: bool },
    /// Files dropped on the view: copy or move them into `dir`.
    Drop { files: Vec<std::path::PathBuf>, dir: std::path::PathBuf, cut: bool },
    /// Selection or contents changed (refresh the status bar).
    Changed,
}

type ActionHandler = Box<dyn Fn(Action)>;

const HEADER_H: f64 = 28.0;
const GROUP_H: f64 = 38.0;
const SIZE_W: f64 = 100.0;
const DATE_W: f64 = 150.0;
const TYPE_W: f64 = 160.0;

/// Move (true) or copy: Shift forces move, Ctrl forces copy, otherwise move within one filesystem (like Nautilus).
fn drop_action(t: &gtk::DropTarget, files: &[gio::File], dir: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let state = t.current_event_state();
    if state.contains(gdk::ModifierType::SHIFT_MASK) {
        return true;
    }
    if state.contains(gdk::ModifierType::CONTROL_MASK) {
        return false;
    }
    let dev = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    files.first().and_then(|f| f.path()).is_some_and(|p| dev(&p).is_some() && dev(&p) == dev(dir))
}

/// Wrap long names without inserting "-" at the break.
fn no_hyphens(layout: &pango::Layout) {
    let attrs = layout.attributes().unwrap_or_default();
    attrs.insert(pango::AttrInt::new_insert_hyphens(false));
    layout.set_attributes(Some(&attrs));
}

/// Pango markup for a name (symlinks are marked on the icon instead).
fn name_markup(name: &str, _symlink: bool) -> String {
    glib::markup_escape_text(name).to_string()
}

/// One group laid out along the scroll axis.
struct Sec {
    start: usize,
    len: usize,
    /// Content offset along the scroll axis and the space it takes.
    off: f64,
    extent: f64,
}

struct Geometry {
    /// Columns per row (vertical modes).
    cols: usize,
    /// Items per column (Compact, which scrolls horizontally like Dolphin).
    rows: usize,
    horizontal: bool,
    cell_w: f64,
    cell_h: f64,
    icon: f64,
    /// Details column header.
    top: f64,
    /// Group header (0 when not grouped).
    hdr: f64,
    secs: Vec<Sec>,
    total: f64,
}

enum Hit {
    Item(usize),
    Header(usize),
}

impl Geometry {
    /// Content-space top-left corner of the item at position `p`.
    fn origin(&self, p: usize) -> (f64, f64) {
        let s = &self.secs[self.secs.partition_point(|s| s.start + s.len <= p).min(self.secs.len() - 1)];
        let local = p - s.start;
        if self.horizontal {
            (s.off + (local / self.rows) as f64 * self.cell_w, self.hdr + (local % self.rows) as f64 * self.cell_h)
        } else {
            ((local % self.cols) as f64 * self.cell_w, s.off + self.hdr + (local / self.cols) as f64 * self.cell_h)
        }
    }

    fn hit(&self, cx: f64, cy: f64) -> Option<Hit> {
        let a = if self.horizontal { cx } else { cy };
        let i = self.secs.partition_point(|s| s.off + s.extent <= a);
        let s = self.secs.get(i)?;
        let local = if self.horizontal {
            if cy < self.hdr {
                return Some(Hit::Header(i));
            }
            let (col, row) = (((cx - s.off) / self.cell_w) as usize, ((cy - self.hdr) / self.cell_h) as usize);
            (row < self.rows).then_some(col * self.rows + row)?
        } else {
            let ly = cy - s.off;
            if ly < self.hdr {
                return Some(Hit::Header(i));
            }
            let (col, row) = ((cx / self.cell_w) as usize, ((ly - self.hdr) / self.cell_h) as usize);
            (col < self.cols).then_some(row * self.cols + col)?
        };
        (local < s.len).then_some(Hit::Item(s.start + local))
    }

    /// Positions of the items intersecting the content rectangle, in order.
    fn items_in(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<usize> {
        let mut out = Vec::new();
        let (a0, a1) = if self.horizontal { (x0, x1) } else { (y0, y1) };
        let first = self.secs.partition_point(|s| s.off + s.extent <= a0);
        for s in &self.secs[first..] {
            if s.off > a1 {
                break;
            }
            if self.horizontal {
                let (ry0, ry1) = ((y0 - self.hdr).max(0.0), y1 - self.hdr);
                if ry1 < 0.0 {
                    continue;
                }
                let (c0, c1) = (((x0 - s.off).max(0.0) / self.cell_w) as usize, ((x1 - s.off) / self.cell_w) as usize);
                let (r0, r1) = ((ry0 / self.cell_h) as usize, ((ry1 / self.cell_h) as usize).min(self.rows - 1));
                for c in c0..=c1 {
                    out.extend((r0..=r1).map(|r| c * self.rows + r).filter(|&l| l < s.len).map(|l| s.start + l));
                }
            } else {
                let (ly0, ly1) = ((y0 - s.off - self.hdr).max(0.0), y1 - s.off - self.hdr);
                if ly1 < 0.0 {
                    continue;
                }
                let (c0, c1) = ((x0.max(0.0) / self.cell_w) as usize, ((x1 / self.cell_w) as usize).min(self.cols - 1));
                for r in (ly0 / self.cell_h) as usize..=(ly1 / self.cell_h) as usize {
                    out.extend((c0..=c1).map(|c| r * self.cols + c).filter(|&l| l < s.len).map(|l| s.start + l));
                }
            }
        }
        out
    }
}

struct Text {
    name: pango::Layout,
    /// Details columns: size, date, type.
    columns: Option<[pango::Layout; 3]>,
}

enum Thumb {
    Pending,
    Ready(u32, gdk::Texture),
    Failed,
}

/// Thumbnails kept in memory before off-screen ones are dropped.
const MAX_THUMBS: usize = 3000;

struct Band {
    start: (f64, f64),
    cur: (f64, f64),
    base: Vec<bool>,
}

mod imp {
    use super::*;

    pub struct Canvas {
        pub model: RefCell<Model>,
        pub mode: Cell<ViewMode>,
        pub zoom: Cell<f64>,
        pub small: Cell<f64>,
        pub vadj: gtk::Adjustment,
        pub hadj: gtk::Adjustment,
        pub cursor: Cell<Option<u32>>,
        pub anchor: Cell<Option<u32>>,
        pub hover: Cell<Option<u32>>,
        pub(super) band: RefCell<Option<Band>>,
        pub pending_select: Cell<Option<u32>>,
        pub(super) texts: RefCell<HashMap<u32, Text>>,
        pub text_width: Cell<f64>,
        pub line_h: Cell<f64>,
        pub target: Cell<Option<f64>>,
        pub velocity: Cell<f64>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub last_frame: Cell<i64>,
        pub typeahead: RefCell<(String, Option<Instant>)>,
        pub on_action: RefCell<Option<ActionHandler>>,
        pub draw_ms: RefCell<Option<Vec<f64>>>,
        pub(super) thumbs: RefCell<HashMap<u32, Thumb>>,
        pub thumb_gen: Cell<u32>,
        pub thumb_owner: Cell<u64>,
        pub thumb_tx: RefCell<Option<thumbs::Sender>>,
        pub thumb_requested: Cell<bool>,
        pub dir: RefCell<std::path::PathBuf>,
    }

    impl Default for Canvas {
        fn default() -> Self {
            Canvas {
                model: Default::default(),
                mode: Default::default(),
                zoom: Cell::new(64.0),
                small: Cell::new(22.0),
                vadj: gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 1.0, 0.0),
                hadj: gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 1.0, 0.0),
                cursor: Default::default(),
                anchor: Default::default(),
                hover: Default::default(),
                band: Default::default(),
                pending_select: Default::default(),
                texts: Default::default(),
                text_width: Default::default(),
                line_h: Default::default(),
                target: Default::default(),
                velocity: Default::default(),
                tick: Default::default(),
                last_frame: Default::default(),
                typeahead: Default::default(),
                on_action: Default::default(),
                draw_ms: Default::default(),
                thumbs: Default::default(),
                thumb_gen: Default::default(),
                thumb_owner: Default::default(),
                thumb_tx: Default::default(),
                thumb_requested: Default::default(),
                dir: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "KolphinCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            while let Some(c) = self.obj().first_child() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let t = Instant::now();
            self.obj().draw(snapshot);
            if let Some(v) = self.draw_ms.borrow_mut().as_mut() {
                v.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().update_adjustment();
            // Context menus are parented here; GTK needs the parent to place them.
            let mut child = self.obj().first_child();
            while let Some(c) = child {
                if let Some(p) = c.downcast_ref::<gtk::Popover>() {
                    p.present();
                }
                child = c.next_sibling();
            }
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Canvas {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Canvas {
    pub fn adjustment(&self) -> gtk::Adjustment {
        self.imp().vadj.clone()
    }

    pub fn hadjustment(&self) -> gtk::Adjustment {
        self.imp().hadj.clone()
    }

    /// The adjustment of the current scroll axis.
    fn scroll_adj(&self) -> &gtk::Adjustment {
        let imp = self.imp();
        if self.mode() == ViewMode::Compact {
            &imp.hadj
        } else {
            &imp.vadj
        }
    }

    /// Scroll offset (x, y) of the content.
    fn offset(&self) -> (f64, f64) {
        let imp = self.imp();
        (imp.hadj.value(), imp.vadj.value())
    }

    pub fn connect_action(&self, f: impl Fn(Action) + 'static) {
        *self.imp().on_action.borrow_mut() = Some(Box::new(f));
    }

    pub fn model(&self) -> Ref<'_, Model> {
        self.imp().model.borrow()
    }

    /// Change the model, then relayout and redraw.
    pub fn update(&self, f: impl FnOnce(&mut Model)) {
        f(&mut self.imp().model.borrow_mut());
        self.update_adjustment();
        self.queue_draw();
        self.emit(Action::Changed);
    }

    /// Size/date texts changed (stats arrived).
    pub fn invalidate_text(&self) {
        self.imp().texts.borrow_mut().clear();
        self.queue_draw();
    }

    pub fn reset(&self, show_hidden: bool) {
        let imp = self.imp();
        let old = std::mem::take(&mut *imp.model.borrow_mut());
        let big = old.entries.len() > 50_000;
        drop(old);
        // glibc keeps the freed memory of millions of small names; hand it back to the OS.
        #[cfg(target_env = "gnu")]
        if big {
            // SAFETY: malloc_trim only releases free heap pages.
            unsafe { libc::malloc_trim(0) };
        }
        #[cfg(not(target_env = "gnu"))]
        let _ = big;
        imp.model.borrow_mut().show_hidden = show_hidden;
        imp.texts.borrow_mut().clear();
        imp.cursor.set(None);
        imp.anchor.set(None);
        imp.hover.set(None);
        self.stop_animation();
        imp.vadj.set_value(0.0);
        imp.hadj.set_value(0.0);
        imp.thumbs.borrow_mut().clear();
        imp.thumb_gen.set(imp.thumb_gen.get().wrapping_add(1));
        self.update(|_| {});
    }

    pub fn mode(&self) -> ViewMode {
        self.imp().mode.get()
    }

    pub fn set_mode(&self, mode: ViewMode) {
        self.imp().mode.set(mode);
        self.relayout();
    }

    /// Icon size of the current mode (Icons: 32–256, Compact/Details: 16–64).
    pub fn icon_size(&self) -> f64 {
        if self.mode() == ViewMode::Icons { self.imp().zoom.get() } else { self.imp().small.get() }
    }

    pub fn icon_range(&self) -> (f64, f64) {
        if self.mode() == ViewMode::Icons { (32.0, 256.0) } else { (16.0, 64.0) }
    }

    pub fn set_icon_size(&self, px: f64) {
        let (lo, hi) = self.icon_range();
        let px = px.round().clamp(lo, hi);
        let cell = if self.mode() == ViewMode::Icons { &self.imp().zoom } else { &self.imp().small };
        if cell.replace(px) != px {
            self.relayout();
        }
    }

    /// Both sizes, for saving: (icons, list).
    pub fn sizes(&self) -> (f64, f64) {
        (self.imp().zoom.get(), self.imp().small.get())
    }

    pub fn set_sizes(&self, icons: f64, list: f64) {
        self.imp().zoom.set(icons.clamp(32.0, 256.0));
        self.imp().small.set(list.clamp(16.0, 64.0));
        self.relayout();
    }

    pub fn zoom_by(&self, grow: bool) {
        let s = self.icon_size();
        self.set_icon_size(if self.mode() == ViewMode::Icons { if grow { s * 1.15 } else { s / 1.15 } } else if grow { s + 2.0 } else { s - 2.0 });
        self.emit(Action::ViewChanged);
    }

    fn relayout(&self) {
        let cursor = self.cursor_pos();
        self.imp().texts.borrow_mut().clear();
        self.update_adjustment();
        if let Some(p) = cursor {
            self.ensure_visible(p);
        }
        self.queue_draw();
    }

    pub fn selection(&self) -> Vec<u32> {
        let m = self.model();
        m.order.iter().copied().filter(|&i| m.selected[i as usize]).collect()
    }

    /// Select one entry, make it the cursor and scroll to it.
    pub fn select_only(&self, idx: u32) {
        let imp = self.imp();
        {
            let mut m = imp.model.borrow_mut();
            m.selected.iter_mut().for_each(|s| *s = false);
            m.selected[idx as usize] = true;
        }
        imp.cursor.set(Some(idx));
        imp.anchor.set(Some(idx));
        if let Some(p) = self.cursor_pos() {
            self.ensure_visible(p);
        }
        self.queue_draw();
        self.emit(Action::Changed);
    }

    fn emit(&self, a: Action) {
        if let Some(f) = self.imp().on_action.borrow().as_ref() {
            f(a);
        }
    }

    // ---------- layout ----------

    fn line_height(&self) -> f64 {
        let imp = self.imp();
        if imp.line_h.get() == 0.0 {
            imp.line_h.set(self.create_pango_layout(Some("Ag")).pixel_size().1 as f64);
        }
        imp.line_h.get()
    }

    fn geometry(&self) -> Geometry {
        let (w, h) = (self.width().max(1) as f64, self.height().max(1) as f64);
        let m = self.model();
        let hdr = if m.group == GroupBy::None { 0.0 } else { GROUP_H };
        let small = self.imp().small.get();
        let list_h = (small + 8.0).max(26.0);
        let mut g = match self.mode() {
            ViewMode::Icons => {
                let icon = self.imp().zoom.get();
                let cols = ((w / (icon + 48.0).max(104.0)) as usize).max(1);
                let cell_h = icon + 2.0 * self.line_height() + 28.0;
                Geometry { cols, rows: 0, horizontal: false, cell_w: w / cols as f64, cell_h, icon, top: 0.0, hdr, secs: Vec::new(), total: 0.0 }
            }
            ViewMode::Compact => {
                let rows = (((h - 8.0 - hdr) / list_h) as usize).max(1);
                let cell_w = 260.0 + (small - 22.0) * 2.0;
                Geometry { cols: 0, rows, horizontal: true, cell_w, cell_h: list_h, icon: small, top: 0.0, hdr, secs: Vec::new(), total: 0.0 }
            }
            ViewMode::Details => Geometry { cols: 1, rows: 0, horizontal: false, cell_w: w, cell_h: list_h, icon: small, top: HEADER_H, hdr, secs: Vec::new(), total: 0.0 },
        };
        let mut off = g.top;
        for s in &m.sections {
            let extent = if g.horizontal {
                s.len.div_ceil(g.rows).max(1) as f64 * g.cell_w
            } else {
                g.hdr + s.len.div_ceil(g.cols) as f64 * g.cell_h
            };
            g.secs.push(Sec { start: s.start, len: s.len, off, extent });
            off += extent;
        }
        if g.secs.is_empty() {
            g.secs.push(Sec { start: 0, len: 0, off, extent: 0.0 });
        }
        g.total = off + if g.horizontal { 0.0 } else { 8.0 };
        g
    }

    fn update_adjustment(&self) {
        let g = self.geometry();
        let imp = self.imp();
        let (adj, other, page, step) = if g.horizontal {
            (&imp.hadj, &imp.vadj, self.width() as f64, g.cell_w)
        } else {
            (&imp.vadj, &imp.hadj, self.height() as f64, g.cell_h)
        };
        let value = adj.value().clamp(0.0, (g.total - page).max(0.0));
        adj.configure(value, 0.0, g.total, step, page * 0.9, page);
        other.configure(0.0, 0.0, 0.0, 1.0, 1.0, 0.0);
    }

    /// Position (in `order`) under widget point (x, y).
    fn hit(&self, x: f64, y: f64) -> Option<usize> {
        match self.hit_any(x, y)? {
            Hit::Item(p) => Some(p),
            Hit::Header(_) => None,
        }
    }

    fn hit_any(&self, x: f64, y: f64) -> Option<Hit> {
        let g = self.geometry();
        if y < g.top || x < 0.0 {
            return None;
        }
        let (sx, sy) = self.offset();
        g.hit(x + sx, y + sy)
    }

    pub fn cursor_position(&self) -> Option<usize> {
        self.cursor_pos().or_else(|| {
            let m = self.model();
            m.order.iter().position(|&i| m.selected[i as usize])
        })
    }

    pub fn select_position(&self, p: usize) {
        let idx = self.model().order.get(p).copied();
        if let Some(idx) = idx {
            self.select_only(idx);
        }
    }

    fn cursor_pos(&self) -> Option<usize> {
        let idx = self.imp().cursor.get()?;
        self.model().order.iter().position(|&i| i == idx)
    }

    fn ensure_visible(&self, p: usize) {
        let g = self.geometry();
        let (x, y) = g.origin(p);
        let (start, size, view) = if g.horizontal {
            (x, g.cell_w, self.width() as f64)
        } else {
            (y - g.top - g.hdr, g.cell_h + g.hdr, self.height() as f64 - g.top)
        };
        self.stop_animation();
        let adj = self.scroll_adj();
        let v = adj.value();
        if start < v {
            adj.set_value(start);
        } else if start + size > v + view {
            adj.set_value(start + size - view);
        }
    }

    // ---------- drawing ----------

    fn text_for(&self, idx: u32, g: &Geometry, details_name_w: f64) -> Ref<'_, Text> {
        let imp = self.imp();
        let width = match self.mode() {
            ViewMode::Icons => g.cell_w - 20.0,
            ViewMode::Compact => g.cell_w - g.icon - 32.0,
            ViewMode::Details => details_name_w,
        };
        if imp.text_width.get() != width {
            imp.texts.borrow_mut().clear();
            imp.text_width.set(width);
        }
        if !imp.texts.borrow().contains_key(&idx) {
            let m = self.model();
            let e = &m.entries[idx as usize];
            let name = self.create_pango_layout(None);
            name.set_markup(&name_markup(&e.name.to_string_lossy(), e.symlink));
            name.set_width((width.max(1.0) * pango::SCALE as f64) as i32);
            name.set_ellipsize(pango::EllipsizeMode::End);
            if self.mode() == ViewMode::Icons {
                name.set_wrap(pango::WrapMode::WordChar);
                name.set_height(-2);
                name.set_alignment(pango::Alignment::Center);
                no_hyphens(&name);
            }
            let columns = (self.mode() == ViewMode::Details).then(|| {
                let size = if e.size == UNKNOWN_SIZE { String::new() } else { glib::format_size(e.size).to_string() };
                let date = (e.mtime != UNKNOWN_TIME)
                    .then(|| glib::DateTime::from_unix_local(e.mtime).ok()?.format("%Y-%m-%d %H:%M").ok())
                    .flatten()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let kind = if e.is_dir {
                    "Folder".to_string()
                } else {
                    let (ct, _) = gio::content_type_guess(Some(&*e.name.to_string_lossy()), None::<&[u8]>);
                    gio::content_type_get_description(&ct).to_string()
                };
                let size = self.create_pango_layout(Some(&size));
                size.set_width(((SIZE_W - 12.0) * pango::SCALE as f64) as i32);
                size.set_alignment(pango::Alignment::Right);
                let date = self.create_pango_layout(Some(&date));
                let kind = self.create_pango_layout(Some(&kind));
                kind.set_width(((TYPE_W - 8.0) * pango::SCALE as f64) as i32);
                kind.set_ellipsize(pango::EllipsizeMode::End);
                [size, date, kind]
            });
            drop(m);
            let mut texts = imp.texts.borrow_mut();
            if texts.len() > 5000 {
                texts.clear();
            }
            texts.insert(idx, Text { name, columns });
        }
        Ref::map(imp.texts.borrow(), |t| &t[&idx])
    }

    fn draw(&self, snap: &gtk::Snapshot) {
        let imp = self.imp();
        let (w, h) = (self.width() as f64, self.height() as f64);
        let g = self.geometry();
        let (sx, sy) = self.offset();
        let style = self.style_context();
        let fg = style.color();
        let accent = style.lookup_color("accent_bg_color").unwrap_or(gdk::RGBA::new(0.36, 0.55, 1.0, 1.0));
        let muted = style.lookup_color("kolphin_muted").unwrap_or(fg);
        let hover = style.lookup_color("kolphin_hover").unwrap_or(gdk::RGBA::new(0.13, 0.15, 0.2, 1.0));
        let alpha = |c: &gdk::RGBA, a: f32| gdk::RGBA::new(c.red(), c.green(), c.blue(), c.alpha() * a);
        let rect = |x: f64, y: f64, w: f64, h: f64| graphene::Rect::new(x as f32, y as f32, w as f32, h as f32);
        let scale = self.scale_factor() as f64;

        // Details columns, right to left.
        let type_x = w - TYPE_W - 8.0;
        let date_x = type_x - DATE_W;
        let size_x = date_x - SIZE_W;
        let name_x = 12.0 + g.icon + 8.0;
        let name_w = (size_x - name_x - 8.0).max(40.0);

        snap.push_clip(&rect(0.0, 0.0, w, h));
        let visible = g.items_in(sx, sy, sx + w, sy + h);
        let focus = self.has_focus();
        let elev = style.lookup_color("kolphin_elev").unwrap_or(hover);
        let border = style.lookup_color("kolphin_border").unwrap_or(alpha(&fg, 0.12));
        let mut expand = None;
        let custom = crate::viewprops::has_custom_icons().then(|| self.dir());
        for &p in &visible {
            let (idx, selected, icon, symlink, nlink) = {
                let m = self.model();
                let idx = m.order[p];
                let e = &m.entries[idx as usize];
                let icon = custom
                    .as_ref()
                    .and_then(|d| crate::viewprops::custom_icon(&d.join(&*e.name)))
                    .and_then(|c| icons::find(&c))
                    .unwrap_or_else(|| icons::icon_name(&e.name.to_string_lossy(), e.is_dir));
                (idx, m.selected[idx as usize], icon, e.symlink, if e.is_dir { 1 } else { e.nlink })
            };
            let thumb = match imp.thumbs.borrow().get(&idx) {
                Some(Thumb::Ready(_, tex)) => Some(tex.clone()),
                _ => None,
            };
            let (ox, oy) = g.origin(p);
            let (x, y) = (ox - sx, oy - sy);
            let (bx, by, bw, bh, radius) = match self.mode() {
                ViewMode::Icons => (x + 4.0, y + 4.0, g.cell_w - 8.0, g.cell_h - 8.0, 8.0),
                _ => (x + 2.0, y + 1.0, g.cell_w - 4.0, g.cell_h - 2.0, 6.0),
            };
            let shape = gsk::RoundedRect::from_rect(rect(bx, by, bw, bh), radius);
            if selected {
                snap.push_rounded_clip(&shape);
                snap.append_color(&alpha(&accent, 0.2), &rect(bx, by, bw, bh));
                snap.pop();
                let c = alpha(&accent, 0.45);
                snap.append_border(&shape, &[1.0; 4], &[c, c, c, c]);
            } else if imp.hover.get() == Some(idx) {
                snap.push_rounded_clip(&shape);
                snap.append_color(&hover, &rect(bx, by, bw, bh));
                snap.pop();
            }
            if focus && imp.cursor.get() == Some(idx) {
                let c = alpha(&accent, 0.9);
                snap.append_border(&shape, &[1.0; 4], &[c, c, c, c]);
            }

            let (ix, iy) = match self.mode() {
                ViewMode::Icons => (x + (g.cell_w - g.icon) / 2.0, y + 10.0),
                _ => (x + 12.0, y + (g.cell_h - g.icon) / 2.0),
            };
            let drawn_icon = if let Some(tex) = thumb {
                // Fit the preview in the icon box, keeping its aspect ratio.
                let (tw, th) = (tex.width() as f64, tex.height() as f64);
                let s = g.icon / tw.max(th);
                let r = rect(ix + (g.icon - tw * s) / 2.0, iy + (g.icon - th * s) / 2.0, tw * s, th * s);
                let corner = if g.icon > 40.0 { 6.0 } else { 3.0 };
                snap.push_rounded_clip(&gsk::RoundedRect::from_rect(r, corner));
                snap.append_texture(&tex, &r);
                snap.pop();
                Some((tex, r, corner))
            } else if let Some(tex) = icons::texture(icon, (g.icon * scale).round() as u32) {
                let r = rect(ix, iy, g.icon, g.icon);
                snap.append_texture(&tex, &r);
                Some((tex, r, 0.0))
            } else {
                None
            };
            // Symlink: Ubuntu-style emblem, a purple disc with a white ↗ on the icon's bottom-right corner.
            if symlink {
                let d = (g.icon * 0.42).clamp(12.0, 44.0);
                let (cx, cy) = (ix + g.icon - d * 0.75, iy + g.icon - d * 0.75);
                let ring = gsk::RoundedRect::from_rect(rect(cx - 1.5, cy - 1.5, d + 3.0, d + 3.0), (d / 2.0 + 1.5) as f32);
                snap.push_rounded_clip(&ring);
                snap.append_color(&gdk::RGBA::new(0.08, 0.09, 0.11, 1.0), &rect(cx - 1.5, cy - 1.5, d + 3.0, d + 3.0));
                snap.pop();
                snap.push_rounded_clip(&gsk::RoundedRect::from_rect(rect(cx, cy, d, d), (d / 2.0) as f32));
                snap.append_color(&gdk::RGBA::new(0.55, 0.25, 0.55, 1.0), &rect(cx, cy, d, d));
                snap.pop();
                let l = self.create_pango_layout(None);
                l.set_markup(&format!("<span size='{}' weight='800'>↗</span>", (d * 0.62 * 0.75 * pango::SCALE as f64) as i32));
                let (aw, ah) = (l.pixel_size().0 as f64, l.pixel_size().1 as f64);
                snap.save();
                snap.translate(&graphene::Point::new((cx + (d - aw) / 2.0) as f32, (cy + (d - ah) / 2.0) as f32));
                snap.append_layout(&l, &gdk::RGBA::WHITE);
                snap.restore();
            }
            // Hard links: a dark ×N chip on the bottom-left.
            for (text, right) in [((nlink > 1).then(|| format!("×{nlink}")), false)] {
                let Some(text) = text else { continue };
                let l = self.create_pango_layout(None);
                l.set_markup(&format!("<span size='{}' weight='600'>{text}</span>", if g.icon >= 48.0 { "small" } else { "x-small" }));
                let (tw, lh) = (l.pixel_size().0 as f64, l.pixel_size().1 as f64 + 2.0);
                let lw = (tw + 8.0).max(lh);
                let bx = if right { ix + g.icon - lw + 3.0 } else { ix - 3.0 };
                let by = iy + g.icon - lh + 3.0;
                let chip = gsk::RoundedRect::from_rect(rect(bx, by, lw, lh), lh as f32 / 2.0);
                snap.push_rounded_clip(&chip);
                snap.append_color(&elev, &rect(bx, by, lw, lh));
                snap.pop();
                snap.append_border(&chip, &[1.0; 4], &[border, border, border, border]);
                snap.save();
                snap.translate(&graphene::Point::new((bx + (lw - tw) / 2.0) as f32, (by + 1.0) as f32));
                snap.append_layout(&l, &muted);
                snap.restore();
            }
            if self.mode() == ViewMode::Icons && selected && imp.cursor.get() == Some(idx) {
                expand = Some((idx, x, y, drawn_icon.clone()));
            }

            let text = self.text_for(idx, &g, name_w);
            let th = text.name.pixel_size().1 as f64;
            let (tx, ty) = match self.mode() {
                ViewMode::Icons => (x + 10.0, y + 10.0 + g.icon + 6.0),
                ViewMode::Compact => (x + 12.0 + g.icon + 8.0, y + (g.cell_h - th) / 2.0),
                ViewMode::Details => (name_x, y + (g.cell_h - th) / 2.0),
            };
            snap.save();
            snap.translate(&graphene::Point::new(tx as f32, ty as f32));
            snap.append_layout(&text.name, &fg);
            snap.restore();
            if let Some(cols) = &text.columns {
                let dim = muted;
                for (layout, cx) in cols.iter().zip([size_x, date_x + 12.0, type_x + 8.0]) {
                    snap.save();
                    snap.translate(&graphene::Point::new(cx as f32, ty as f32));
                    snap.append_layout(layout, &dim);
                    snap.restore();
                }
            }
        }

        // The focused selected icon shows its whole name, spilling over the items below (like Dolphin).
        if let Some((idx, x, y, drawn_icon)) = expand {
            let e_name = {
                let m = self.model();
                let e = &m.entries[idx as usize];
                name_markup(&e.name.to_string_lossy(), e.symlink)
            };
            let full = self.create_pango_layout(None);
            full.set_markup(&e_name);
            full.set_width(((g.cell_w - 20.0) * pango::SCALE as f64) as i32);
            full.set_wrap(pango::WrapMode::WordChar);
            full.set_alignment(pango::Alignment::Center);
            no_hyphens(&full);
            let short_h = self.text_for(idx, &g, name_w).name.pixel_size().1 as f64;
            let full_h = full.pixel_size().1 as f64;
            if full_h > short_h {
                // The whole cell grows downwards; the icon is drawn again on top of the fill.
                let ty = y + 10.0 + g.icon + 6.0;
                let r = rect(x + 4.0, y + 4.0, g.cell_w - 8.0, ty + full_h + 8.0 - (y + 4.0));
                let shape = gsk::RoundedRect::from_rect(r, 8.0);
                let bg = style.lookup_color("window_bg_color").unwrap_or(elev);
                snap.push_rounded_clip(&shape);
                snap.append_color(&bg, &r);
                snap.append_color(&alpha(&accent, 0.2), &r);
                snap.pop();
                let c = alpha(&accent, 0.9);
                snap.append_border(&shape, &[1.0; 4], &[c, c, c, c]);
                if let Some((tex, ir, corner)) = drawn_icon {
                    snap.push_rounded_clip(&gsk::RoundedRect::from_rect(ir, corner));
                    snap.append_texture(&tex, &ir);
                    snap.pop();
                }
                snap.save();
                snap.translate(&graphene::Point::new((x + 10.0) as f32, ty as f32));
                snap.append_layout(&full, &fg);
                snap.restore();
            }
        }

        // Group headers: chevron, title, count and a rule.
        if g.hdr > 0.0 {
            let border = style.lookup_color("kolphin_border").unwrap_or(alpha(&fg, 0.12));
            let m = self.model();
            let (a0, a1) = if g.horizontal { (sx, sx + w) } else { (sy, sy + h) };
            for (sec, info) in g.secs.iter().zip(&m.sections) {
                if sec.off + sec.extent < a0 || sec.off > a1 {
                    continue;
                }
                let (hx, hy, hw) = if g.horizontal { (sec.off - sx, 0.0, sec.extent) } else { (0.0, sec.off - sy, w) };
                let l = self.create_pango_layout(None);
                let chevron = if info.collapsed { "▸" } else { "▾" };
                l.set_markup(&format!(
                    "<span foreground='#8b91a1'>{chevron}</span>  <b>{}</b>  <span foreground='#5f6677' size='small'>{}</span>",
                    glib::markup_escape_text(&info.title),
                    info.count
                ));
                l.set_width(((hw - 24.0).max(1.0) * pango::SCALE as f64) as i32);
                l.set_ellipsize(pango::EllipsizeMode::End);
                snap.save();
                snap.translate(&graphene::Point::new((hx + 12.0) as f32, (hy + (g.hdr - l.pixel_size().1 as f64) / 2.0) as f32));
                snap.append_layout(&l, &fg);
                snap.restore();
                snap.append_color(&border, &rect(hx + 12.0, hy + g.hdr - 4.0, hw - 24.0, 1.0));
            }
        }

        if self.mode() == ViewMode::Details {
            let bg = style.lookup_color("view_bg_color").unwrap_or(gdk::RGBA::new(1.0, 1.0, 1.0, 1.0));
            snap.append_color(&bg, &rect(0.0, 0.0, w, HEADER_H));
            let border = style.lookup_color("kolphin_border").unwrap_or(alpha(&fg, 0.12));
            snap.append_color(&border, &rect(0.0, HEADER_H - 1.0, w, 1.0));
            let (sort, desc) = (self.model().sort, self.model().desc);
            for (title, x, key) in [("NAME", name_x, SortBy::Name), ("SIZE", size_x, SortBy::Size), ("MODIFIED", date_x + 12.0, SortBy::Modified), ("TYPE", type_x + 8.0, SortBy::Type)] {
                let l = self.create_pango_layout(None);
                let arrow = if key == sort { if desc { " ▼" } else { " ▲" } } else { "" };
                l.set_markup(&format!("<span size='small' weight='600' letter_spacing='800'>{title}{arrow}</span>"));
                if title == "SIZE" {
                    l.set_width(((SIZE_W - 12.0) * pango::SCALE as f64) as i32);
                    l.set_alignment(pango::Alignment::Right);
                }
                snap.save();
                snap.translate(&graphene::Point::new(x as f32, ((HEADER_H - l.pixel_size().1 as f64) / 2.0) as f32));
                snap.append_layout(&l, &muted);
                snap.restore();
            }
        }

        if let (Some(&first), Some(&last)) = (visible.first(), visible.last()) {
            self.request_thumbs(first, last + 1, (g.icon * scale).round() as u32);
        }

        if let Some(b) = imp.band.borrow().as_ref() {
            let (x0, y0) = (b.start.0.min(b.cur.0) - sx, b.start.1.min(b.cur.1) - sy);
            let r = rect(x0, y0, (b.start.0 - b.cur.0).abs(), (b.start.1 - b.cur.1).abs());
            snap.append_color(&alpha(&accent, 0.18), &r);
            let c = alpha(&accent, 0.8);
            snap.append_border(&gsk::RoundedRect::from_rect(r, 0.0), &[1.0; 4], &[c, c, c, c]);
        }
        snap.pop();
    }

    // ---------- thumbnails ----------

    /// Queue previews for the visible items, then one more screen ahead; replaces the previous request.
    fn request_thumbs(&self, first: usize, last: usize, px: u32) {
        let imp = self.imp();
        let px = px.clamp(16, thumbs::MAX_PX);
        let ahead = (last + (last - first)).min(self.model().order.len());
        let gen = imp.thumb_gen.get() as u64;
        let dir = self.dir();
        let mut jobs = Vec::new();
        {
            let m = self.model();
            let mut cache = imp.thumbs.borrow_mut();
            for p in first..ahead {
                let idx = m.order[p];
                let e = &m.entries[idx as usize];
                if e.is_dir || e.mtime == UNKNOWN_TIME {
                    continue;
                }
                match cache.get(&idx) {
                    Some(Thumb::Ready(have, _)) if *have >= px || *have >= thumbs::MAX_PX => continue,
                    Some(Thumb::Failed) => continue,
                    _ => {}
                }
                let name = e.name.to_string_lossy();
                let Some(kind) = thumbs::kind(&name) else { continue };
                if !matches!(cache.get(&idx), Some(Thumb::Ready(..))) {
                    cache.insert(idx, Thumb::Pending);
                }
                jobs.push(thumbs::Job { key: gen << 32 | idx as u64, path: dir.join(&*e.name), kind, px, mtime: e.mtime });
            }
            if cache.len() > MAX_THUMBS {
                let keep: std::collections::HashSet<u32> = m.order[first.saturating_sub(last - first)..ahead].iter().copied().collect();
                cache.retain(|k, _| keep.contains(k));
            }
        }
        if jobs.is_empty() && !imp.thumb_requested.get() {
            return;
        }
        imp.thumb_requested.set(!jobs.is_empty());
        let tx = imp.thumb_tx.borrow().clone();
        if let Some(tx) = tx {
            thumbs::request(imp.thumb_owner.get(), jobs, &tx);
        }
    }

    fn on_thumb(&self, done: thumbs::Done) {
        let imp = self.imp();
        if (done.key >> 32) as u32 != imp.thumb_gen.get() {
            return;
        }
        let idx = done.key as u32;
        let thumb = match done.image {
            Some(img) => {
                let (w, h) = img.dimensions();
                let bytes = glib::Bytes::from_owned(img.into_raw());
                Thumb::Ready(done.px, gdk::MemoryTexture::new(w as i32, h as i32, gdk::MemoryFormat::R8g8b8a8, &bytes, w as usize * 4).into())
            }
            None => Thumb::Failed,
        };
        imp.thumbs.borrow_mut().insert(idx, thumb);
        self.queue_draw();
    }

    /// Folder whose entries are shown (thumbnail paths are built from it).
    pub fn set_dir(&self, dir: std::path::PathBuf) {
        *self.imp().dir.borrow_mut() = dir;
    }

    fn dir(&self) -> std::path::PathBuf {
        self.imp().dir.borrow().clone()
    }

    // ---------- input ----------

    fn setup(&self) {
        static OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let (tx, rx) = async_channel::unbounded::<thumbs::Done>();
        self.imp().thumb_owner.set(OWNER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        *self.imp().thumb_tx.borrow_mut() = Some(tx);
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(done) = rx.recv().await {
                let Some(c) = weak.upgrade() else { return };
                c.on_thumb(done);
            }
        });
        self.set_focusable(true);
        self.set_overflow(gtk::Overflow::Hidden);
        self.set_hexpand(true);
        self.set_vexpand(true);
        let imp = self.imp();
        imp.vadj.connect_value_changed(glib::clone!(#[weak(rename_to = c)] self, move |_| c.queue_draw()));
        imp.hadj.connect_value_changed(glib::clone!(#[weak(rename_to = c)] self, move |_| c.queue_draw()));

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::KINETIC);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = c)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |ctl, dx, dy| c.on_scroll(ctl, dx, dy)
        ));
        scroll.connect_decelerate(glib::clone!(#[weak(rename_to = c)] self, move |_, vx, vy| {
            let unit = c.scroll_adj().page_size().powf(2.0 / 3.0);
            let v = if c.mode() == ViewMode::Compact { vx + vy } else { vy };
            c.imp().velocity.set(v * unit);
            c.start_animation();
        }));
        scroll.connect_scroll_begin(glib::clone!(#[weak(rename_to = c)] self, move |_| c.stop_animation()));
        self.add_controller(scroll);

        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.connect_pressed(glib::clone!(#[weak(rename_to = c)] self, move |g, n, x, y| c.on_press(g, n, x, y)));
        click.connect_released(glib::clone!(#[weak(rename_to = c)] self, move |_, _, _, _| {
            if let Some(idx) = c.imp().pending_select.take() {
                c.select_only(idx);
            }
        }));
        self.add_controller(click);

        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(#[weak(rename_to = c)] self, move |g, x, y| {
            if c.hit(x, y).is_none() {
                let ctrl = g.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
                let base = if ctrl { c.model().selected.clone() } else { vec![false; c.model().selected.len()] };
                let (sx, sy) = c.offset();
                let p = (x + sx, y + sy);
                *c.imp().band.borrow_mut() = Some(Band { start: p, cur: p, base });
            }
        }));
        drag.connect_drag_update(glib::clone!(#[weak(rename_to = c)] self, move |g, ox, oy| {
            if c.imp().band.borrow().is_none() {
                return;
            }
            c.imp().pending_select.set(None);
            let (sx, sy) = g.start_point().unwrap_or_default();
            let (x, y) = (sx + ox, sy + oy);
            // Auto-scroll while the pointer is past the edge along the scroll axis.
            let (pos, len) = if c.mode() == ViewMode::Compact { (x, c.width() as f64) } else { (y, c.height() as f64) };
            if pos < 0.0 || pos > len {
                let adj = c.scroll_adj();
                adj.set_value(adj.value() + if pos < 0.0 { pos } else { pos - len } * 0.5);
            }
            let (ox, oy) = c.offset();
            c.update_band(((x + ox).max(0.0), (y + oy).max(0.0)));
        }));
        drag.connect_drag_end(glib::clone!(#[weak(rename_to = c)] self, move |_, _, _| {
            if c.imp().band.take().is_some() {
                c.queue_draw();
            }
        }));
        self.add_controller(drag);

        self.setup_dnd();

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(#[weak(rename_to = c)] self, move |_, x, y| {
            let idx = c.hit(x, y).map(|p| c.model().order[p]);
            if c.imp().hover.replace(idx) != idx {
                c.queue_draw();
            }
        }));
        motion.connect_leave(glib::clone!(#[weak(rename_to = c)] self, move |_| {
            if c.imp().hover.take().is_some() {
                c.queue_draw();
            }
        }));
        self.add_controller(motion);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = c)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| c.on_key(key, state)
        ));
        self.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(#[weak(rename_to = c)] self, move |_| c.queue_draw()));
        focus.connect_leave(glib::clone!(#[weak(rename_to = c)] self, move |_| c.queue_draw()));
        self.add_controller(focus);
    }

    /// Drag selected files out (as a URI list) and accept files dropped in, onto a folder or the view.
    fn setup_dnd(&self) {
        let src = gtk::DragSource::new();
        src.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
        src.connect_prepare(glib::clone!(
            #[weak(rename_to = c)]
            self,
            #[upgrade_or]
            None,
            move |s, x, y| {
                let p = c.hit(x, y)?;
                let idx = c.model().order[p];
                if !c.model().selected[idx as usize] {
                    c.select_only(idx);
                }
                c.imp().pending_select.set(None);
                let dir = c.dir();
                let (uris, icon) = {
                    let m = c.model();
                    let uris: Vec<String> = c.selection().iter().map(|&i| gio::File::for_path(dir.join(&*m.entries[i as usize].name)).uri().to_string()).collect();
                    let e = &m.entries[idx as usize];
                    (uris, icons::icon_name(&e.name.to_string_lossy(), e.is_dir))
                };
                if let Some(tex) = icons::texture(icon, 64) {
                    s.set_icon(Some(&tex), 32, 32);
                }
                Some(gdk::ContentProvider::for_bytes("text/uri-list", &glib::Bytes::from_owned((uris.join("\r\n") + "\r\n").into_bytes())))
            }
        ));
        self.add_controller(src);

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY | gdk::DragAction::MOVE);
        drop.set_preload(true);
        drop.connect_motion(glib::clone!(
            #[weak(rename_to = c)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |t, x, y| {
                let (dir, folder) = c.drop_dir(x, y);
                if c.imp().hover.replace(folder) != folder {
                    c.queue_draw();
                }
                let files = t.value().and_then(|v| v.get::<gdk::FileList>().ok()).map(|l| l.files()).unwrap_or_default();
                if drop_action(t, &files, &dir) { gdk::DragAction::MOVE } else { gdk::DragAction::COPY }
            }
        ));
        drop.connect_leave(glib::clone!(#[weak(rename_to = c)] self, move |_| {
            c.imp().hover.set(None);
            c.queue_draw();
        }));
        drop.connect_drop(glib::clone!(
            #[weak(rename_to = c)]
            self,
            #[upgrade_or]
            false,
            move |t, value, x, y| {
                let Ok(list) = value.get::<gdk::FileList>() else { return false };
                let files = list.files();
                let (dir, _) = c.drop_dir(x, y);
                let cut = drop_action(t, &files, &dir);
                let paths: Vec<std::path::PathBuf> = files.iter().filter_map(|f| f.path()).filter(|p| p.parent() != Some(dir.as_path()) && *p != dir).collect();
                if paths.is_empty() {
                    return false;
                }
                c.emit(Action::Drop { files: paths, dir, cut });
                true
            }
        ));
        self.add_controller(drop);
    }

    /// Where a drop at (x, y) lands: a folder under the pointer, else the shown folder.
    fn drop_dir(&self, x: f64, y: f64) -> (std::path::PathBuf, Option<u32>) {
        let dir = self.dir();
        if let Some(p) = self.hit(x, y) {
            let m = self.model();
            let idx = m.order[p];
            let e = &m.entries[idx as usize];
            if e.is_dir {
                return (dir.join(&*e.name), Some(idx));
            }
        }
        (dir, None)
    }

    fn on_scroll(&self, ctl: &gtk::EventControllerScroll, dx: f64, dy: f64) -> glib::Propagation {
        if ctl.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
            self.zoom_by(dy < 0.0);
            return glib::Propagation::Stop;
        }
        let adj = self.scroll_adj();
        // Compact scrolls sideways: the vertical wheel moves it too.
        let d = if self.mode() == ViewMode::Compact { dx + dy } else { dy };
        let delta = d * adj.page_size().powf(2.0 / 3.0);
        let wheel = ctl.current_event_device().is_some_and(|d| d.source() == gdk::InputSource::Mouse);
        if wheel {
            // Mouse wheel: glide to the target instead of jumping.
            let max = adj.upper() - adj.page_size();
            let from = self.imp().target.get().unwrap_or(adj.value());
            self.imp().target.set(Some((from + delta).clamp(0.0, max.max(0.0))));
            self.imp().velocity.set(0.0);
            self.start_animation();
        } else {
            self.stop_animation();
            adj.set_value(adj.value() + delta);
        }
        glib::Propagation::Stop
    }

    fn start_animation(&self) {
        let imp = self.imp();
        if imp.tick.borrow().is_some() {
            return;
        }
        imp.last_frame.set(0);
        let id = self.add_tick_callback(|c, clock| {
            let imp = c.imp();
            let now = clock.frame_time();
            let dt = if imp.last_frame.get() == 0 { 1.0 / 60.0 } else { ((now - imp.last_frame.get()) as f64 / 1e6).min(0.05) };
            imp.last_frame.set(now);
            let adj = c.scroll_adj();
            let mut moving = false;
            if let Some(target) = imp.target.get() {
                let v = adj.value();
                let next = v + (target - v) * (1.0 - (-dt * 16.0).exp());
                if (target - next).abs() < 0.5 {
                    adj.set_value(target);
                    imp.target.set(None);
                } else {
                    adj.set_value(next);
                    moving = true;
                }
            }
            let vel = imp.velocity.get();
            if vel.abs() > 15.0 {
                let before = adj.value();
                adj.set_value(before + vel * dt);
                imp.velocity.set(if adj.value() == before { 0.0 } else { vel * (-dt * 3.0).exp() });
                moving = true;
            }
            if moving {
                glib::ControlFlow::Continue
            } else {
                imp.tick.take();
                glib::ControlFlow::Break
            }
        });
        *imp.tick.borrow_mut() = Some(id);
    }

    fn stop_animation(&self) {
        let imp = self.imp();
        imp.target.set(None);
        imp.velocity.set(0.0);
        if let Some(id) = imp.tick.take() {
            id.remove();
        }
    }

    fn on_press(&self, g: &gtk::GestureClick, n: i32, x: f64, y: f64) {
        self.grab_focus();
        let imp = self.imp();
        let state = g.current_event_state();
        let (ctrl, shift) = (state.contains(gdk::ModifierType::CONTROL_MASK), state.contains(gdk::ModifierType::SHIFT_MASK));
        if g.current_button() == 1 && self.mode() == ViewMode::Details && y < HEADER_H {
            let w = self.width() as f64;
            let type_x = w - TYPE_W - 8.0;
            let key = match x {
                x if x >= type_x => SortBy::Type,
                x if x >= type_x - DATE_W => SortBy::Modified,
                x if x >= type_x - DATE_W - SIZE_W => SortBy::Size,
                _ => SortBy::Name,
            };
            self.emit(Action::SortBy(key));
            return;
        }
        if let (1, Some(Hit::Header(i))) = (g.current_button(), self.hit_any(x, y)) {
            let title = self.model().sections.get(i).map(|s| s.title.clone());
            if let Some(title) = title {
                self.update(|m| {
                    if !m.collapsed.remove(&title) {
                        m.collapsed.insert(title);
                    }
                    m.rebuild_order();
                });
                self.emit(Action::ViewChanged);
            }
            return;
        }
        let hit = self.hit(x, y).map(|p| (p, self.model().order[p]));
        imp.pending_select.set(None);
        match (g.current_button(), hit) {
            (1, Some((_, idx))) if n == 2 => self.emit(Action::Activate(idx)),
            (1, Some((p, idx))) => {
                if ctrl {
                    let mut m = imp.model.borrow_mut();
                    m.selected[idx as usize] ^= true;
                } else if shift {
                    let from = imp.anchor.get().and_then(|a| self.model().order.iter().position(|&i| i == a)).unwrap_or(p);
                    self.select_range(from, p);
                    imp.cursor.set(Some(idx));
                    self.queue_draw();
                    self.emit(Action::Changed);
                    return;
                } else if self.model().selected[idx as usize] {
                    imp.pending_select.set(Some(idx));
                } else {
                    let mut m = imp.model.borrow_mut();
                    m.selected.iter_mut().for_each(|s| *s = false);
                    m.selected[idx as usize] = true;
                }
                imp.cursor.set(Some(idx));
                imp.anchor.set(Some(idx));
                self.queue_draw();
                self.emit(Action::Changed);
            }
            (1, None) if !ctrl => {
                imp.model.borrow_mut().selected.iter_mut().for_each(|s| *s = false);
                self.queue_draw();
                self.emit(Action::Changed);
            }
            (2, Some((_, idx))) if self.model().entries[idx as usize].is_dir => self.emit(Action::OpenInNewTab(idx)),
            (3, hit) => {
                match hit {
                    Some((_, idx)) if !self.model().selected[idx as usize] => self.select_only(idx),
                    Some(_) => {}
                    None => {
                        imp.model.borrow_mut().selected.iter_mut().for_each(|s| *s = false);
                        self.queue_draw();
                        self.emit(Action::Changed);
                    }
                }
                self.emit(Action::ContextMenu { x, y, on_item: hit.is_some() });
            }
            _ => {}
        }
    }

    fn select_range(&self, a: usize, b: usize) {
        let mut m = self.imp().model.borrow_mut();
        m.selected.iter_mut().for_each(|s| *s = false);
        for p in a.min(b)..=a.max(b) {
            let i = m.order[p] as usize;
            m.selected[i] = true;
        }
    }

    fn update_band(&self, cur: (f64, f64)) {
        let g = self.geometry();
        let imp = self.imp();
        let mut band = imp.band.borrow_mut();
        let Some(b) = band.as_mut() else { return };
        b.cur = cur;
        let hits = g.items_in(b.start.0.min(cur.0), b.start.1.min(cur.1), b.start.0.max(cur.0), b.start.1.max(cur.1));
        let mut m = imp.model.borrow_mut();
        m.selected.copy_from_slice(&b.base);
        for p in hits {
            let i = m.order[p] as usize;
            m.selected[i] = true;
        }
        drop(m);
        drop(band);
        self.queue_draw();
        self.emit(Action::Changed);
    }

    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let imp = self.imp();
        let (ctrl, shift) = (state.contains(gdk::ModifierType::CONTROL_MASK), state.contains(gdk::ModifierType::SHIFT_MASK));
        let len = self.model().order.len();
        let g = self.geometry();
        // Steps for (left/right, up/down, page) in positions.
        let (side, vert, per_page) = if g.horizontal {
            (g.rows, 1, ((self.width() as f64 / g.cell_w) as usize).max(1) * g.rows)
        } else {
            (1, g.cols, (((self.height() as f64 - g.top) / g.cell_h) as usize).max(1) * g.cols)
        };
        let grid = self.mode() != ViewMode::Details;
        let cur = self.cursor_pos();
        let target = |delta: isize| -> Option<usize> {
            if len == 0 {
                return None;
            }
            let p = cur.map_or(0, |p| (p as isize + delta).clamp(0, len as isize - 1) as usize);
            Some(p)
        };
        let moved = match key {
            gdk::Key::Up => target(-(vert as isize)),
            gdk::Key::Down => target(vert as isize),
            gdk::Key::Left if grid => target(-(side as isize)),
            gdk::Key::Right if grid => target(side as isize),
            gdk::Key::Page_Up => target(-(per_page as isize)),
            gdk::Key::Page_Down => target(per_page as isize),
            gdk::Key::Home => (len > 0).then_some(0),
            gdk::Key::End => len.checked_sub(1),
            gdk::Key::Return | gdk::Key::KP_Enter if !state.contains(gdk::ModifierType::ALT_MASK) => {
                if let Some(idx) = imp.cursor.get() {
                    self.emit(Action::Activate(idx));
                }
                return glib::Propagation::Stop;
            }
            gdk::Key::BackSpace => {
                self.emit(Action::Up);
                return glib::Propagation::Stop;
            }
            gdk::Key::a if ctrl => {
                self.update(|m| {
                    for i in 0..m.order.len() {
                        let idx = m.order[i] as usize;
                        m.selected[idx] = true;
                    }
                });
                return glib::Propagation::Stop;
            }
            gdk::Key::c | gdk::Key::x | gdk::Key::v | gdk::Key::z if ctrl => {
                let action = match key {
                    gdk::Key::c => "win.copy",
                    gdk::Key::x => "win.cut",
                    gdk::Key::z => "win.undo",
                    _ => "win.paste",
                };
                let _ = self.activate_action(action, None);
                return glib::Propagation::Stop;
            }
            gdk::Key::F2 | gdk::Key::Delete | gdk::Key::KP_Delete | gdk::Key::space => {
                let action = match key {
                    gdk::Key::F2 => "win.rename",
                    gdk::Key::space => "win.quicklook",
                    _ if shift => "win.delete",
                    _ => "win.trash",
                };
                let _ = self.activate_action(action, None);
                return glib::Propagation::Stop;
            }
            gdk::Key::Return | gdk::Key::KP_Enter if state.contains(gdk::ModifierType::ALT_MASK) => {
                let _ = self.activate_action("win.properties", None);
                return glib::Propagation::Stop;
            }
            gdk::Key::Escape => {
                self.update(|m| m.selected.iter_mut().for_each(|s| *s = false));
                return glib::Propagation::Stop;
            }
            _ => None,
        };
        if let Some(p) = moved {
            let idx = self.model().order[p];
            if shift {
                let from = imp.anchor.get().and_then(|a| self.model().order.iter().position(|&i| i == a)).unwrap_or(p);
                self.select_range(from, p);
                imp.cursor.set(Some(idx));
                self.ensure_visible(p);
                self.queue_draw();
                self.emit(Action::Changed);
            } else if ctrl {
                imp.cursor.set(Some(idx));
                self.ensure_visible(p);
                self.queue_draw();
            } else {
                self.select_only(idx);
            }
            return glib::Propagation::Stop;
        }
        if ctrl || state.contains(gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        match key.to_unicode().filter(|c| !c.is_control()) {
            Some(ch) => {
                self.type_ahead(ch);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    }

    /// Jump to the first entry whose name starts with what was typed in the last second.
    fn type_ahead(&self, ch: char) {
        let imp = self.imp();
        let prefix = {
            let mut t = imp.typeahead.borrow_mut();
            if t.1.is_none_or(|at| at.elapsed().as_millis() > 1000) {
                t.0.clear();
            }
            t.0.extend(ch.to_lowercase());
            t.1 = Some(Instant::now());
            t.0.clone()
        };
        let found = {
            let m = self.model();
            m.order.iter().copied().find(|&i| {
                let name = m.entries[i as usize].name.to_string_lossy();
                let mut chars = name.chars().flat_map(char::to_lowercase);
                prefix.chars().all(|p| chars.next() == Some(p))
            })
        };
        if let Some(idx) = found {
            self.select_only(idx);
        }
    }

    /// Scroll from top to bottom at a fixed speed, then report draw and frame times.
    pub fn bench_scroll(&self, done: impl FnOnce(String) + 'static) {
        self.stop_animation();
        *self.imp().draw_ms.borrow_mut() = Some(Vec::new());
        let frames = std::rc::Rc::new(RefCell::new(Vec::<f64>::new()));
        let done = RefCell::new(Some(done));
        let started = Cell::new(0i64);
        self.add_tick_callback(move |c, clock| {
            let now = clock.frame_time();
            let mut f = frames.borrow_mut();
            if started.get() == 0 {
                started.set(now);
            } else {
                f.push((now - started.get()) as f64 / 1000.0);
            }
            let adj = c.adjustment();
            let elapsed = (now - started.get()) as f64 / 1e6;
            adj.set_value(elapsed * 20000.0);
            if adj.value() + adj.page_size() < adj.upper() && elapsed < 8.0 {
                return glib::ControlFlow::Continue;
            }
            let mut gaps: Vec<f64> = f.windows(2).map(|w| w[1] - w[0]).collect();
            let mut draws = c.imp().draw_ms.take().unwrap_or_default();
            let pct = |v: &mut Vec<f64>, q: f64| {
                v.sort_by(f64::total_cmp);
                v.get(((v.len() as f64 - 1.0) * q) as usize).copied().unwrap_or(0.0)
            };
            let report = format!(
                "scrolled {:.0} px in {:.2} s, {} frames | frame interval ms p50 {:.1} p95 {:.1} max {:.1} | draw ms p50 {:.2} p95 {:.2} max {:.2}",
                adj.value(),
                elapsed,
                gaps.len(),
                pct(&mut gaps, 0.5),
                pct(&mut gaps, 0.95),
                pct(&mut gaps, 1.0),
                pct(&mut draws, 0.5),
                pct(&mut draws, 0.95),
                pct(&mut draws, 1.0),
            );
            if let Some(d) = done.take() {
                d(report);
            }
            glib::ControlFlow::Break
        });
    }
}
