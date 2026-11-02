//! Virtualized file view: only the visible items are laid out and drawn (Icons / Compact / Details).
use crate::icons;
use crate::model::{Model, UNKNOWN_SIZE, UNKNOWN_TIME};
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
    cols: usize,
    cell_w: f64,
    cell_h: f64,
    icon: f64,
    top: f64,
}

struct Text {
    name: pango::Layout,
    /// Details columns: size, date, type.
    columns: Option<[pango::Layout; 3]>,
}

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
    }

    impl Default for Canvas {
        fn default() -> Self {
            Canvas {
                model: Default::default(),
                mode: Default::default(),
                zoom: Cell::new(64.0),
                vadj: gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 1.0, 0.0),
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
                Geometry { cols, cell_w: w / cols as f64, cell_h: icon + 2.0 * self.line_height() + 28.0, icon, top: 0.0 }
            }
            ViewMode::Compact => {
                let cols = ((w / 260.0) as usize).max(1);
                Geometry { cols, cell_w: w / cols as f64, cell_h: 32.0, icon: 22.0, top: 0.0 }
            }
            ViewMode::Details => Geometry { cols: 1, cell_w: w, cell_h: 30.0, icon: 22.0, top: HEADER_H },
        }
    }

    fn content_height(&self, g: &Geometry) -> f64 {
        let n = self.model().order.len();
        g.top + n.div_ceil(g.cols) as f64 * g.cell_h + 8.0
    }

    fn update_adjustment(&self) {
        let g = self.geometry();
        let (h, upper) = (self.height() as f64, self.content_height(&g));
        let adj = &self.imp().vadj;
        let value = adj.value().clamp(0.0, (upper - h).max(0.0));
        adj.configure(value, 0.0, upper, g.cell_h, h * 0.9, h);
    }

    /// Position (in `order`) under widget point (x, y).
    fn hit(&self, x: f64, y: f64) -> Option<usize> {
        let g = self.geometry();
        if y < g.top || x < 0.0 {
            return None;
        }
        let cy = y + self.imp().vadj.value() - g.top;
        let (row, col) = ((cy / g.cell_h) as usize, (x / g.cell_w) as usize);
        let p = row * g.cols + col;
        (col < g.cols && cy >= 0.0 && p < self.model().order.len()).then_some(p)
    }

    fn cursor_pos(&self) -> Option<usize> {
        let idx = self.imp().cursor.get()?;
        self.model().order.iter().position(|&i| i == idx)
    }

    fn ensure_visible(&self, p: usize) {
        let g = self.geometry();
        let adj = &self.imp().vadj;
        let (y, h) = ((p / g.cols) as f64 * g.cell_h, self.height() as f64 - g.top);
        let v = adj.value();
        self.stop_animation();
        if y < v {
            adj.set_value(y);
        } else if y + g.cell_h > v + h {
            adj.set_value(y + g.cell_h - h);
        }
    }

    // ---------- drawing ----------

    fn text_for(&self, idx: u32, g: &Geometry, details_name_w: f64) -> Ref<'_, Text> {
        let imp = self.imp();
        let width = match self.mode() {
            ViewMode::Icons => g.cell_w - 8.0,
            ViewMode::Compact => g.cell_w - g.icon - 24.0,
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
        let scroll = imp.vadj.value();
        let style = self.style_context();
        let fg = style.color();
        let accent = style.lookup_color("accent_bg_color").unwrap_or(gdk::RGBA::new(0.21, 0.52, 0.89, 1.0));
        let alpha = |c: &gdk::RGBA, a: f32| gdk::RGBA::new(c.red(), c.green(), c.blue(), c.alpha() * a);
        let rect = |x: f64, y: f64, w: f64, h: f64| graphene::Rect::new(x as f32, y as f32, w as f32, h as f32);
        let scale = self.scale_factor() as f64;

        // Details columns, right to left.
        let type_x = w - TYPE_W - 8.0;
        let date_x = type_x - DATE_W;
        let size_x = date_x - SIZE_W;
        let name_x = 6.0 + g.icon + 8.0;
        let name_w = (size_x - name_x - 8.0).max(40.0);

        snap.push_clip(&rect(0.0, 0.0, w, h));
        let len = self.model().order.len();
        let first = ((scroll / g.cell_h) as usize) * g.cols;
        let last = (((scroll + h) / g.cell_h).ceil() as usize + 1) * g.cols;
        let focus = self.has_focus();
        for p in first..last.min(len) {
            let (idx, selected, icon) = {
                let m = self.model();
                let idx = m.order[p];
                let e = &m.entries[idx as usize];
                (idx, m.selected[idx as usize], icons::icon_name(&e.name.to_string_lossy(), e.is_dir))
            };
            let x = (p % g.cols) as f64 * g.cell_w;
            let y = g.top + (p / g.cols) as f64 * g.cell_h - scroll;
            let (bx, by, bw, bh, radius) = match self.mode() {
                ViewMode::Icons => (x + 4.0, y + 4.0, g.cell_w - 8.0, g.cell_h - 8.0, 8.0),
                _ => (x + 2.0, y + 1.0, g.cell_w - 4.0, g.cell_h - 2.0, 6.0),
            };
            let shape = gsk::RoundedRect::from_rect(rect(bx, by, bw, bh), radius);
            if selected {
                snap.push_rounded_clip(&shape);
                snap.append_color(&alpha(&accent, 0.35), &rect(bx, by, bw, bh));
                snap.pop();
            } else if imp.hover.get() == Some(idx) {
                snap.push_rounded_clip(&shape);
                snap.append_color(&alpha(&fg, 0.07), &rect(bx, by, bw, bh));
                snap.pop();
            }
            if focus && imp.cursor.get() == Some(idx) {
                let c = alpha(&accent, 0.9);
                snap.append_border(&shape, &[1.0; 4], &[c, c, c, c]);
            }

            let (ix, iy) = match self.mode() {
                ViewMode::Icons => (x + (g.cell_w - g.icon) / 2.0, y + 10.0),
                _ => (x + 6.0, y + (g.cell_h - g.icon) / 2.0),
            };
            if let Some(tex) = icons::texture(icon, (g.icon * scale).round() as u32) {
                snap.append_texture(&tex, &rect(ix, iy, g.icon, g.icon));
            }

            let text = self.text_for(idx, &g, name_w);
            let th = text.name.pixel_size().1 as f64;
            let (tx, ty) = match self.mode() {
                ViewMode::Icons => (x + 4.0, y + 10.0 + g.icon + 6.0),
                ViewMode::Compact => (x + 6.0 + g.icon + 8.0, y + (g.cell_h - th) / 2.0),
                ViewMode::Details => (name_x, y + (g.cell_h - th) / 2.0),
            };
            snap.save();
            snap.translate(&graphene::Point::new(tx as f32, ty as f32));
            snap.append_layout(&text.name, &fg);
            snap.restore();
            if let Some(cols) = &text.columns {
                let dim = alpha(&fg, 0.65);
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
            snap.append_color(&alpha(&fg, 0.12), &rect(0.0, HEADER_H - 1.0, w, 1.0));
            let dim = alpha(&fg, 0.7);
            for (title, x) in [("Name", name_x), ("Size", size_x), ("Modified", date_x + 12.0), ("Type", type_x + 8.0)] {
                let l = self.create_pango_layout(Some(title));
                if title == "Size" {
                    l.set_width(((SIZE_W - 12.0) * pango::SCALE as f64) as i32);
                    l.set_alignment(pango::Alignment::Right);
                }
                snap.save();
                snap.translate(&graphene::Point::new(x as f32, ((HEADER_H - l.pixel_size().1 as f64) / 2.0) as f32));
                snap.append_layout(&l, &dim);
                snap.restore();
            }
        }

        if let Some(b) = imp.band.borrow().as_ref() {
            let (x0, y0) = (b.start.0.min(b.cur.0), b.start.1.min(b.cur.1) - scroll);
            let r = rect(x0, y0, (b.start.0 - b.cur.0).abs(), (b.start.1 - b.cur.1).abs());
            snap.append_color(&alpha(&accent, 0.18), &r);
            let c = alpha(&accent, 0.8);
            snap.append_border(&gsk::RoundedRect::from_rect(r, 0.0), &[1.0; 4], &[c, c, c, c]);
        }
        snap.pop();
    }

    // ---------- input ----------

    fn setup(&self) {
        self.set_focusable(true);
        self.set_overflow(gtk::Overflow::Hidden);
        self.set_hexpand(true);
        self.set_vexpand(true);
        let imp = self.imp();
        imp.vadj.connect_value_changed(glib::clone!(#[weak(rename_to = c)] self, move |_| c.queue_draw()));

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::KINETIC);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = c)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |ctl, _, dy| c.on_scroll(ctl, dy)
        ));
        scroll.connect_decelerate(glib::clone!(#[weak(rename_to = c)] self, move |_, _, vy| {
            let unit = c.imp().vadj.page_size().powf(2.0 / 3.0);
            c.imp().velocity.set(vy * unit);
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
                let p = (x, y + c.imp().vadj.value());
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
            let adj = c.adjustment();
            let h = c.height() as f64;
            if y < 0.0 || y > h {
                adj.set_value(adj.value() + if y < 0.0 { y } else { y - h } * 0.5);
            }
            c.update_band((x.clamp(0.0, c.width() as f64), y + adj.value()));
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

    fn on_scroll(&self, ctl: &gtk::EventControllerScroll, dy: f64) -> glib::Propagation {
        if ctl.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
            if self.mode() == ViewMode::Icons {
                self.zoom_by(if dy < 0.0 { 1.15 } else { 1.0 / 1.15 });
            }
            return glib::Propagation::Stop;
        }
        let adj = &self.imp().vadj;
        let delta = dy * adj.page_size().powf(2.0 / 3.0);
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
            let adj = &imp.vadj;
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
            let (c0, c1) = ((x0 / g.cell_w) as usize, ((x1 / g.cell_w) as usize).min(g.cols - 1));
            for row in (y0 / g.cell_h) as usize..=(y1 / g.cell_h) as usize {
                for col in c0..=c1 {
                    let p = row * g.cols + col;
                    if let Some(&i) = m.order.get(p) {
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
        let per_page = (((self.height() as f64 - g.top) / g.cell_h) as usize).max(1) * g.cols;
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
            gdk::Key::Up => target(-(g.cols as isize)),
            gdk::Key::Down => target(g.cols as isize),
            gdk::Key::Left if grid => target(-1),
            gdk::Key::Right if grid => target(1),
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
