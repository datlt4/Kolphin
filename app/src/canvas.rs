//! Virtualized file view: only the visible items are laid out and drawn (Icons / Compact / Details).
use crate::icons;
use crate::model::{Model, UNKNOWN_SIZE, UNKNOWN_TIME};
use crate::thumbs;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk, pango, prelude::*};
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
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
    /// Selection or contents changed (refresh the status bar).
    Changed,
}

type ActionHandler = Box<dyn Fn(Action)>;

const HEADER_H: f64 = 28.0;
const SIZE_W: f64 = 100.0;
const DATE_W: f64 = 150.0;
const TYPE_W: f64 = 160.0;

#[derive(Clone, Copy)]
struct Geometry {
    /// Columns per row (vertical modes).
    cols: usize,
    /// Items per column (Compact, which scrolls horizontally like Dolphin).
    rows: usize,
    horizontal: bool,
    cell_w: f64,
    cell_h: f64,
    icon: f64,
    top: f64,
}

impl Geometry {
    /// Content-space top-left corner of the item at position `p`.
    fn origin(&self, p: usize) -> (f64, f64) {
        if self.horizontal {
            ((p / self.rows) as f64 * self.cell_w, (p % self.rows) as f64 * self.cell_h)
        } else {
            ((p % self.cols) as f64 * self.cell_w, self.top + (p / self.cols) as f64 * self.cell_h)
        }
    }

    fn at(&self, col: usize, row: usize) -> usize {
        if self.horizontal {
            col * self.rows + row
        } else {
            row * self.cols + col
        }
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
        *imp.model.borrow_mut() = Model::default();
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

    pub fn zoom_by(&self, factor: f64) {
        let imp = self.imp();
        imp.zoom.set((imp.zoom.get() * factor).clamp(32.0, 256.0));
        self.relayout();
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
        let w = self.width().max(1) as f64;
        match self.mode() {
            ViewMode::Icons => {
                let icon = self.imp().zoom.get();
                let cols = ((w / (icon + 48.0).max(104.0)) as usize).max(1);
                let cell_h = icon + 2.0 * self.line_height() + 28.0;
                Geometry { cols, rows: 0, horizontal: false, cell_w: w / cols as f64, cell_h, icon, top: 0.0 }
            }
            ViewMode::Compact => {
                let rows = (((self.height() as f64 - 8.0) / 30.0) as usize).max(1);
                Geometry { cols: 0, rows, horizontal: true, cell_w: 280.0, cell_h: 30.0, icon: 22.0, top: 0.0 }
            }
            ViewMode::Details => Geometry { cols: 1, rows: 0, horizontal: false, cell_w: w, cell_h: 30.0, icon: 22.0, top: HEADER_H },
        }
    }

    fn update_adjustment(&self) {
        let g = self.geometry();
        let n = self.model().order.len();
        let imp = self.imp();
        let (adj, other, page, upper, step) = if g.horizontal {
            (&imp.hadj, &imp.vadj, self.width() as f64, n.div_ceil(g.rows) as f64 * g.cell_w, g.cell_w)
        } else {
            (&imp.vadj, &imp.hadj, self.height() as f64, g.top + n.div_ceil(g.cols) as f64 * g.cell_h + 8.0, g.cell_h)
        };
        let value = adj.value().clamp(0.0, (upper - page).max(0.0));
        adj.configure(value, 0.0, upper, step, page * 0.9, page);
        other.configure(0.0, 0.0, 0.0, 1.0, 1.0, 0.0);
    }

    /// Position (in `order`) under widget point (x, y).
    fn hit(&self, x: f64, y: f64) -> Option<usize> {
        let g = self.geometry();
        if y < g.top || x < 0.0 {
            return None;
        }
        let (sx, sy) = self.offset();
        let (cx, cy) = (x + sx, y + sy - g.top);
        let (col, row) = ((cx / g.cell_w) as usize, (cy / g.cell_h) as usize);
        if (g.horizontal && row >= g.rows) || (!g.horizontal && col >= g.cols) {
            return None;
        }
        let p = g.at(col, row);
        (p < self.model().order.len()).then_some(p)
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
            (y - g.top, g.cell_h, self.height() as f64 - g.top)
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
            ViewMode::Icons => g.cell_w - 8.0,
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
            let name = self.create_pango_layout(Some(&e.name.to_string_lossy()));
            name.set_width((width.max(1.0) * pango::SCALE as f64) as i32);
            name.set_ellipsize(pango::EllipsizeMode::End);
            if self.mode() == ViewMode::Icons {
                name.set_wrap(pango::WrapMode::WordChar);
                name.set_height(-2);
                name.set_alignment(pango::Alignment::Center);
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
        let len = self.model().order.len();
        let (first, last) = if g.horizontal {
            (((sx / g.cell_w) as usize) * g.rows, (((sx + w) / g.cell_w).ceil() as usize + 1) * g.rows)
        } else {
            (((sy / g.cell_h) as usize) * g.cols, (((sy + h) / g.cell_h).ceil() as usize + 1) * g.cols)
        };
        let focus = self.has_focus();
        for p in first..last.min(len) {
            let (idx, selected, icon) = {
                let m = self.model();
                let idx = m.order[p];
                let e = &m.entries[idx as usize];
                (idx, m.selected[idx as usize], icons::icon_name(&e.name.to_string_lossy(), e.is_dir))
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
            if let Some(tex) = thumb {
                // Fit the preview in the icon box, keeping its aspect ratio.
                let (tw, th) = (tex.width() as f64, tex.height() as f64);
                let s = g.icon / tw.max(th);
                let r = rect(ix + (g.icon - tw * s) / 2.0, iy + (g.icon - th * s) / 2.0, tw * s, th * s);
                snap.push_rounded_clip(&gsk::RoundedRect::from_rect(r, if g.icon > 40.0 { 6.0 } else { 3.0 }));
                snap.append_texture(&tex, &r);
                snap.pop();
            } else if let Some(tex) = icons::texture(icon, (g.icon * scale).round() as u32) {
                snap.append_texture(&tex, &rect(ix, iy, g.icon, g.icon));
            }

            let text = self.text_for(idx, &g, name_w);
            let th = text.name.pixel_size().1 as f64;
            let (tx, ty) = match self.mode() {
                ViewMode::Icons => (x + 4.0, y + 10.0 + g.icon + 6.0),
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

        if self.mode() == ViewMode::Details {
            let bg = style.lookup_color("view_bg_color").unwrap_or(gdk::RGBA::new(1.0, 1.0, 1.0, 1.0));
            snap.append_color(&bg, &rect(0.0, 0.0, w, HEADER_H));
            let border = style.lookup_color("kolphin_border").unwrap_or(alpha(&fg, 0.12));
            snap.append_color(&border, &rect(0.0, HEADER_H - 1.0, w, 1.0));
            for (title, x) in [("NAME", name_x), ("SIZE", size_x), ("MODIFIED", date_x + 12.0), ("TYPE", type_x + 8.0)] {
                let l = self.create_pango_layout(None);
                l.set_markup(&format!("<span size='small' weight='600' letter_spacing='800'>{title}</span>"));
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

        self.request_thumbs(first.min(len), last.min(len), (g.icon * scale).round() as u32);

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

    fn on_scroll(&self, ctl: &gtk::EventControllerScroll, dx: f64, dy: f64) -> glib::Propagation {
        if ctl.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
            if self.mode() == ViewMode::Icons {
                self.zoom_by(if dy < 0.0 { 1.15 } else { 1.0 / 1.15 });
            }
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
            (3, Some((_, idx))) if !self.model().selected[idx as usize] => self.select_only(idx),
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
        let (x0, x1) = (b.start.0.min(cur.0), b.start.0.max(cur.0));
        let (y0, y1) = ((b.start.1.min(cur.1) - g.top).max(0.0), b.start.1.max(cur.1) - g.top);
        let mut m = imp.model.borrow_mut();
        m.selected.copy_from_slice(&b.base);
        if y1 >= 0.0 {
            let (c0, mut c1) = ((x0 / g.cell_w) as usize, (x1 / g.cell_w) as usize);
            let (r0, mut r1) = ((y0 / g.cell_h) as usize, (y1 / g.cell_h) as usize);
            if g.horizontal {
                r1 = r1.min(g.rows - 1);
            } else {
                c1 = c1.min(g.cols - 1);
            }
            for row in r0..=r1 {
                for col in c0..=c1 {
                    if let Some(&i) = m.order.get(g.at(col, row)) {
                        m.selected[i as usize] = true;
                    }
                }
            }
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
            gdk::Key::Return | gdk::Key::KP_Enter => {
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
