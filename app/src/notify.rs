//! Notifications that stack and show at once (AdwToastOverlay shows one at a time, queueing the rest).
use gtk::{glib, prelude::*};

const MAX_SHOWN: usize = 5;

/// The window content wrapped with a toast stack at the bottom.
pub fn overlay() -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    let stack = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).valign(gtk::Align::End).halign(gtk::Align::Center).margin_bottom(28).css_classes(["toast-stack"]).build();
    overlay.add_overlay(&stack);
    overlay
}

pub trait ToastExt {
    fn add_toast(&self, toast: Toast);
}

impl ToastExt for gtk::Overlay {
    fn add_toast(&self, toast: Toast) {
        let Some(stack) = self.last_child().and_downcast::<gtk::Box>() else { return };
        stack.append(&toast.root);
        // Keep the newest few; the oldest go first.
        let mut n = 0;
        let mut c = stack.last_child();
        while let Some(w) = c {
            c = w.prev_sibling();
            n += 1;
            if n > MAX_SHOWN {
                stack.remove(&w);
            }
        }
        if toast.timeout > 0 {
            let root = toast.root.clone();
            glib::timeout_add_seconds_local_once(toast.timeout, move || remove(&root));
        }
    }
}

fn remove(w: &gtk::Box) {
    if let Some(parent) = w.parent().and_downcast::<gtk::Box>() {
        parent.remove(w);
    }
}

#[derive(Clone)]
pub struct Toast {
    root: gtk::Box,
    label: gtk::Label,
    timeout: u32,
}

impl Toast {
    /// A message that goes away after 3 s.
    pub fn new(title: &str) -> Toast {
        Toast::builder().title(title).build()
    }

    pub fn builder() -> ToastBuilder {
        ToastBuilder { title: String::new(), timeout: 3, button: None, on_button: Vec::new() }
    }

    pub fn set_title(&self, title: &str) {
        self.label.set_text(title);
    }

    pub fn dismiss(&self) {
        remove(&self.root);
    }
}

type ButtonFn = Box<dyn Fn(&gtk::Button)>;

pub struct ToastBuilder {
    title: String,
    timeout: u32,
    button: Option<(String, Option<String>, Option<glib::Variant>)>,
    on_button: Vec<(String, ButtonFn)>,
}

impl ToastBuilder {
    pub fn title(mut self, t: impl Into<String>) -> Self {
        self.title = t.into();
        self
    }

    /// Seconds before it closes itself; 0 keeps it until dismissed.
    pub fn timeout(mut self, s: u32) -> Self {
        self.timeout = s;
        self
    }

    pub fn button_label(mut self, l: &str) -> Self {
        self.button.get_or_insert((String::new(), None, None)).0 = l.to_string();
        self
    }

    pub fn action_name(mut self, a: &str) -> Self {
        self.button.get_or_insert((String::new(), None, None)).1 = Some(a.to_string());
        self
    }

    pub fn action_target(mut self, v: &glib::Variant) -> Self {
        self.button.get_or_insert((String::new(), None, None)).2 = Some(v.clone());
        self
    }

    /// A button running `f` (for example Pause or Cancel on a progress toast); `f` may relabel it.
    pub fn on_button(mut self, label: &str, f: impl Fn(&gtk::Button) + 'static) -> Self {
        self.on_button.push((label.to_string(), Box::new(f)));
        self
    }

    pub fn build(self) -> Toast {
        let root = gtk::Box::builder().spacing(10).css_classes(["toast"]).build();
        let label = gtk::Label::builder().label(&self.title).wrap(true).width_chars(36).max_width_chars(70).xalign(0.0).build();
        root.append(&label);
        if let Some((text, action, target)) = self.button {
            let b = gtk::Button::builder().label(&text).css_classes(["suggested-action"]).build();
            b.set_action_name(action.as_deref());
            b.set_action_target_value(target.as_ref());
            // Done once clicked: the toast goes away so it can't be clicked twice.
            let r = root.clone();
            b.connect_clicked(move |_| remove(&r));
            root.append(&b);
        }
        for (text, f) in self.on_button {
            let b = gtk::Button::builder().label(&text).build();
            b.connect_clicked(move |b| f(b));
            root.append(&b);
        }
        let close = gtk::Button::builder().icon_name("window-close-symbolic").css_classes(["flat", "circular"]).build();
        let r = root.clone();
        close.connect_clicked(move |_| remove(&r));
        root.append(&close);
        Toast { root, label, timeout: self.timeout }
    }
}
