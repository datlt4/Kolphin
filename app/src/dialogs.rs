//! Small modal windows: a one-line prompt and the Material icon picker.
use crate::icons;
use gtk::{glib, prelude::*};

fn dialog(parent: &gtk::Window, title: &str, width: i32, height: i32) -> (gtk::Window, gtk::Box) {
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).css_classes(["dialog-body"]).build();
    let header = gtk::HeaderBar::builder().title_widget(&gtk::Label::builder().label(title).css_classes(["heading"]).build()).build();
    let win = gtk::Window::builder().title(title).transient_for(parent).modal(true).default_width(width).default_height(height).titlebar(&header).child(&body).build();
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak] win,
        #[upgrade_or] glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                win.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    win.add_controller(keys);
    (win, body)
}

/// Ask for one line of text; `f` gets it when the user presses Enter or OK.
pub fn prompt(parent: &gtk::Window, title: &str, hint: &str, initial: &str, f: impl Fn(String) + 'static) {
    let (win, body) = dialog(parent, title, 420, -1);
    body.append(&gtk::Label::builder().label(hint).xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let entry = gtk::Entry::builder().text(initial).activates_default(true).build();
    body.append(&entry);
    let ok = gtk::Button::builder().label("OK").css_classes(["suggested-action"]).halign(gtk::Align::End).build();
    body.append(&ok);
    win.set_default_widget(Some(&ok));
    let f = std::rc::Rc::new(f);
    ok.connect_clicked(glib::clone!(#[weak] win, #[weak] entry, move |_| {
        f(entry.text().to_string());
        win.close();
    }));
    win.present();
    entry.grab_focus();
}

/// Pick a Material icon (searchable, virtualized grid); `f` gets its name.
pub fn pick_icon(parent: &gtk::Window, f: impl Fn(&'static str) + 'static) {
    let (win, body) = dialog(parent, "Choose Icon", 680, 560);
    let search = gtk::SearchEntry::builder().placeholder_text("Search icons").build();
    let names = gtk::StringList::new(&icons::all().collect::<Vec<_>>());
    let filter = gtk::CustomFilter::new(glib::clone!(#[weak] search, #[upgrade_or] true, move |o| {
        o.downcast_ref::<gtk::StringObject>().is_some_and(|s| s.string().contains(&*search.text().to_lowercase()))
    }));
    let model = gtk::FilterListModel::new(Some(names), Some(filter.clone()));
    search.connect_search_changed(move |_| filter.changed(gtk::FilterChange::Different));

    let scale = parent.scale_factor().max(1) as u32;
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let tile = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).width_request(92).build();
        tile.append(&gtk::Image::builder().pixel_size(32).build());
        tile.append(&gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(11).css_classes(["caption"]).build());
        item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&tile));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let (Some(tile), Some(name)) = (item.child(), item.item().and_downcast::<gtk::StringObject>()) else { return };
        let name = name.string();
        tile.set_tooltip_text(Some(&name));
        let img = tile.first_child().and_downcast::<gtk::Image>().unwrap();
        img.set_paintable(icons::find(&name).and_then(|n| icons::texture(n, 32 * scale)).as_ref());
        tile.last_child().and_downcast::<gtk::Label>().unwrap().set_label(&name);
    });
    let grid = gtk::GridView::builder().model(&gtk::NoSelection::new(Some(model))).factory(&factory).max_columns(12).single_click_activate(true).build();
    let f = std::rc::Rc::new(f);
    grid.connect_activate(glib::clone!(#[weak] win, move |g, pos| {
        let name = g.model().and_then(|m| m.item(pos)).and_downcast::<gtk::StringObject>().map(|s| s.string());
        if let Some(name) = name.as_deref().and_then(icons::find) {
            f(name);
        }
        win.close();
    }));
    body.append(&search);
    body.append(&gtk::ScrolledWindow::builder().child(&grid).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build());
    win.present();
    search.grab_focus();
}
