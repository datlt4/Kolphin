//! Small modal windows: a one-line prompt and the Material icon picker.
use crate::icons;
use gtk::{glib, prelude::*};

/// The input method sending `text` at `pos` repeats the word it inserted `elapsed` ago, ending at `end`.
fn ime_repeat(prev: &str, end: i32, elapsed: std::time::Duration, text: &str, pos: i32) -> bool {
    prev == text && end == pos && elapsed < std::time::Duration::from_millis(100) && (!text.is_ascii() || text.chars().count() > 1)
}

/// Work around GTK 4.6 + input method (Unikey via ibus or XIM) bugs: the first letter typed over a selection lands
/// at the end ("abc" → "bc a"), and Enter commits the pending word twice.
pub fn ime_safe(entry: &(impl IsA<gtk::Editable> + IsA<gtk::Widget>)) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let e = entry.clone().upcast::<gtk::Editable>();
    let w = entry.clone().upcast::<gtk::Widget>();
    // A word is being typed with the input method (its preedit is showing).
    let composing = std::rc::Rc::new(std::cell::Cell::new(false));
    if let Some(t) = entry.delegate().and_downcast::<gtk::Text>() {
        t.connect_preedit_changed(glib::clone!(#[strong] composing, move |_, p| composing.set(!p.is_empty())));
    }
    // Committed text lands in the field; some input methods never report the emptied preedit.
    entry.connect_changed(glib::clone!(#[strong] composing, move |_| composing.set(false)));
    // Whatever path makes the input method send a word twice (as Kermius found with xterm.js), drop a repeat of
    // the same word right after it within 100 ms. Plain ASCII letters are left alone: "ll" can be typed that fast.
    if let Some(t) = entry.delegate().and_downcast::<gtk::Text>() {
        let last: std::rc::Rc<std::cell::RefCell<Option<(String, i32, std::time::Instant)>>> = Default::default();
        t.connect_insert_text(move |t, text, pos| {
            if last.borrow().as_ref().is_some_and(|(prev, end, at)| ime_repeat(prev, *end, at.elapsed(), text, *pos)) {
                t.stop_signal_emission_by_name("insert-text");
                return;
            }
            *last.borrow_mut() = Some((text.to_string(), *pos + text.chars().count() as i32, std::time::Instant::now()));
        });
    }
    keys.connect_key_pressed(move |_, key, _, state| {
        // Enter: the input method commits its pending word once on focus-out; the key itself would commit it a
        // second time ("abc" → "abcabc"). Keep Enter from it, let focus-out commit, then activate.
        if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter) {
            if let Some(root) = w.root() {
                root.set_focus(None::<&gtk::Widget>);
            }
            let w = w.clone();
            glib::idle_add_local_once(move || {
                w.grab_focus();
                if let Some(en) = w.downcast_ref::<gtk::Entry>() {
                    en.emit_activate();
                    if en.activates_default() {
                        if let Some(d) = en.root().and_downcast::<gtk::Window>().and_then(|win| win.default_widget()) {
                            d.activate();
                        }
                    }
                } else if let Some(se) = w.downcast_ref::<gtk::SearchEntry>() {
                    se.emit_activate();
                }
            });
            return glib::Propagation::Stop;
        }
        // Esc and Up/Down make the input method commit its pending word twice as well: commit it by focus-out
        // instead and come back. Like other IMEs, the first Esc ends the word; the next one closes.
        if composing.get() && matches!(key, gtk::gdk::Key::Escape | gtk::gdk::Key::Up | gtk::gdk::Key::Down) {
            composing.set(false);
            if let Some(root) = w.root() {
                root.set_focus(None::<&gtk::Widget>);
                let (root, e) = (root.downgrade(), e.clone());
                glib::idle_add_local_once(move || {
                    if root.upgrade().is_some_and(|r| r.focus().is_none()) {
                        if let Some(t) = e.delegate().and_downcast::<gtk::Text>() {
                            t.grab_focus_without_selecting();
                        }
                    }
                });
            }
            return glib::Propagation::Proceed;
        }
        let modifier = state.intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK | gtk::gdk::ModifierType::SUPER_MASK);
        if !modifier && key.to_unicode().is_some_and(|c| !c.is_control()) && e.selection_bounds().is_some() {
            e.delete_selection();
        }
        glib::Propagation::Proceed
    });
    entry.add_controller(keys);
}

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

/// Compress: an archive name and a format (formats whose tools are missing are greyed out); `f` gets the file name.
pub fn compress(parent: &gtk::Window, stem: &str, f: impl Fn(String) + 'static) {
    let (win, body) = dialog(parent, "Compress", 440, -1);
    body.append(&gtk::Label::builder().label("Archive name").xalign(0.0).css_classes(["dim-label"]).build());
    let entry = gtk::Entry::builder().text(stem).activates_default(true).build();
    ime_safe(&entry);
    body.append(&entry);
    let formats = gtk::Grid::builder().column_spacing(18).row_spacing(8).column_homogeneous(true).build();
    let mut first: Option<gtk::CheckButton> = None;
    let chosen = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    for (i, (ext, programs, package)) in crate::archive::FORMATS.iter().enumerate() {
        let radio = gtk::CheckButton::builder().label(format!(".{ext}")).build();
        radio.set_group(first.as_ref());
        if crate::archive::available(programs) {
            if first.is_none() {
                radio.set_active(true);
                *chosen.borrow_mut() = ext.to_string();
            }
        } else {
            radio.set_sensitive(false);
            radio.set_tooltip_text(Some(&format!("Install the \"{package}\" package to create .{ext} archives")));
        }
        radio.connect_toggled(glib::clone!(#[strong] chosen, move |r| {
            if r.is_active() {
                *chosen.borrow_mut() = ext.to_string();
            }
        }));
        first.get_or_insert(radio.clone());
        formats.attach(&radio, i as i32 % 3, i as i32 / 3, 1, 1);
    }
    body.append(&formats);
    let ok = gtk::Button::builder().label("Compress").css_classes(["suggested-action"]).halign(gtk::Align::End).build();
    body.append(&ok);
    win.set_default_widget(Some(&ok));
    ok.connect_clicked(glib::clone!(#[weak] win, #[weak] entry, move |_| {
        let name = entry.text().trim().to_string();
        if !name.is_empty() && !chosen.borrow().is_empty() {
            f(format!("{name}.{}", chosen.borrow()));
            win.close();
        }
    }));
    win.present();
    entry.grab_focus();
}

/// Ask for one line of text; `f` gets it when the user presses Enter or OK.
pub fn prompt(parent: &gtk::Window, title: &str, hint: &str, initial: &str, f: impl Fn(String) + 'static) {
    let (win, body) = dialog(parent, title, 420, -1);
    body.append(&gtk::Label::builder().label(hint).xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let entry = gtk::Entry::builder().text(initial).activates_default(true).build();
    ime_safe(&entry);
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
    ime_safe(&search);
    // The search field keeps Esc to itself.
    search.connect_stop_search(glib::clone!(#[weak] win, move |_| win.close()));
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

/// Pick one item from a list; `f` gets its index.
pub fn choose(parent: &gtk::Window, title: &str, hint: &str, items: Vec<String>, f: impl Fn(usize) + 'static) {
    let (win, body) = dialog(parent, title, 420, 420);
    body.append(&gtk::Label::builder().label(hint).xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let list = gtk::ListBox::builder().css_classes(["palette"]).build();
    for item in &items {
        list.append(&gtk::Label::builder().label(item).xalign(0.0).build());
    }
    body.append(&gtk::ScrolledWindow::builder().child(&list).vexpand(true).build());
    list.connect_row_activated(glib::clone!(#[weak] win, move |_, row| {
        if (row.index() as usize) < items.len() {
            f(row.index() as usize);
        }
        win.close();
    }));
    win.present();
    if let Some(r) = list.row_at_index(0) {
        r.grab_focus();
    }
}

/// Name clashes in a copy/move: Replace (old ones to the trash), Skip, or Keep Both; closing cancels.
pub fn conflict(parent: &gtk::Window, n: usize, dest: &str, f: impl Fn(crate::ops::Conflict) + 'static) {
    use crate::ops::Conflict;
    let (win, body) = dialog(parent, "Items Already Exist", 460, -1);
    body.append(&gtk::Label::builder().label(format!("{n} item(s) with the same name already exist in “{dest}”.")).xalign(0.0).wrap(true).build());
    body.append(&gtk::Label::builder().label("Replace moves the existing items to the trash.").xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
    let f = std::rc::Rc::new(f);
    for (label, choice, class) in [("Skip", Conflict::Skip, ""), ("Replace", Conflict::Replace, "destructive-action"), ("Keep Both", Conflict::KeepBoth, "suggested-action")] {
        let b = gtk::Button::with_label(label);
        if !class.is_empty() {
            b.add_css_class(class);
        }
        let f = f.clone();
        b.connect_clicked(glib::clone!(#[weak] win, move |_| {
            f(choice);
            win.close();
        }));
        buttons.append(&b);
        if choice == Conflict::KeepBoth {
            win.set_default_widget(Some(&b));
        }
    }
    body.append(&buttons);
    win.present();
    // Enter = Keep Both, the choice that loses nothing.
    if let Some(keep) = buttons.last_child() {
        keep.grab_focus();
    }
}

/// Yes/no question; `f` runs only on confirmation.
pub fn confirm(parent: &gtk::Window, title: &str, text: &str, action: &str, f: impl Fn() + 'static) {
    let (win, body) = dialog(parent, title, 420, -1);
    body.append(&gtk::Label::builder().label(text).xalign(0.0).wrap(true).build());
    let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).build();
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::builder().label(action).css_classes(["destructive-action"]).build();
    buttons.append(&cancel);
    buttons.append(&ok);
    body.append(&buttons);
    cancel.connect_clicked(glib::clone!(#[weak] win, move |_| win.close()));
    ok.connect_clicked(glib::clone!(#[weak] win, move |_| {
        f();
        win.close();
    }));
    win.present();
    cancel.grab_focus();
}

/// New name for each path: regex find/replace over the name; `{n}` in the replacement is a zero-padded counter.
pub fn batch_names(paths: &[std::path::PathBuf], find: &str, replace: &str) -> Result<Vec<String>, String> {
    let re = regex::Regex::new(if find.is_empty() { "^.*$" } else { find }).map_err(|e| e.to_string())?;
    let width = paths.len().to_string().len();
    Ok(paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            let rep = replace.replace("{n}", &format!("{:0width$}", i + 1));
            re.replace(&name, rep.as_str()).into_owned()
        })
        .collect())
}

/// Rename many files with a live preview; `f` gets (path, new name) pairs.
pub fn batch_rename(parent: &gtk::Window, paths: Vec<std::path::PathBuf>, f: impl Fn(Vec<(std::path::PathBuf, String)>) + 'static) {
    let (win, body) = dialog(parent, &format!("Rename {} Items", paths.len()), 560, 520);
    body.append(&gtk::Label::builder().label("Find is a regular expression ($1… are its groups). {n} in Replace numbers the files.").xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let find = gtk::Entry::builder().placeholder_text("Find, e.g. ^(.*)\\.jpeg$").text("^(.*)$").build();
    let replace = gtk::Entry::builder().placeholder_text("Replace, e.g. img_{n}.jpg").text("$1").build();
    ime_safe(&find);
    ime_safe(&replace);
    body.append(&find);
    body.append(&replace);
    let preview = gtk::Label::builder().xalign(0.0).yalign(0.0).selectable(true).css_classes(["monospace"]).build();
    body.append(&gtk::ScrolledWindow::builder().child(&preview).vexpand(true).build());
    let ok = gtk::Button::builder().label("Rename").css_classes(["suggested-action"]).halign(gtk::Align::End).build();
    body.append(&ok);
    let paths = std::rc::Rc::new(paths);
    let refresh = glib::clone!(#[weak] find, #[weak] replace, #[weak] preview, #[weak] ok, #[strong] paths, move || {
        match batch_names(&paths, &find.text(), &replace.text()) {
            Ok(names) => {
                let lines: Vec<String> = paths
                    .iter()
                    .zip(&names)
                    .take(300)
                    .map(|(p, n)| format!("{}  →  {n}", p.file_name().unwrap_or_default().to_string_lossy()))
                    .collect();
                preview.set_text(&lines.join("\n"));
                ok.set_sensitive(true);
            }
            Err(e) => {
                preview.set_text(&e);
                ok.set_sensitive(false);
            }
        }
    });
    refresh();
    let r = refresh.clone();
    find.connect_changed(move |_| r());
    replace.connect_changed(move |_| refresh());
    ok.connect_clicked(glib::clone!(#[weak] win, #[weak] find, #[weak] replace, move |_| {
        if let Ok(names) = batch_names(&paths, &find.text(), &replace.text()) {
            f(paths.iter().cloned().zip(names).collect());
        }
        win.close();
    }));
    win.present();
}

/// Choose an application for these files, optionally as the default for their type.
pub fn open_with(parent: &gtk::Window, files: Vec<std::path::PathBuf>) {
    let Some(first) = files.first() else { return };
    let name = first.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let (ct, _) = gtk::gio::content_type_guess(Some(first), None::<&[u8]>);
    let ct = if first.is_dir() { "inode/directory".into() } else { ct };
    let (win, body) = dialog(parent, &format!("Open {name} with"), 460, 520);
    let chooser = gtk::AppChooserWidget::builder().content_type(ct.as_str()).show_default(true).show_recommended(true).show_fallback(true).show_other(true).vexpand(true).build();
    let always = gtk::CheckButton::with_label(&format!("Always use for {}", gtk::gio::content_type_get_description(&ct)));
    let ok = gtk::Button::builder().label("Open").css_classes(["suggested-action"]).halign(gtk::Align::End).build();
    body.append(&chooser);
    body.append(&always);
    body.append(&ok);
    let open = glib::clone!(#[weak] win, #[weak] always, move |app: gtk::gio::AppInfo| {
        if always.is_active() {
            let _ = app.set_as_default_for_type(&ct);
        }
        let list: Vec<gtk::gio::File> = files.iter().map(gtk::gio::File::for_path).collect();
        let _ = app.launch(&list, Some(&WidgetExt::display(&win).app_launch_context()));
        win.close();
    });
    let o = open.clone();
    chooser.connect_application_activated(move |_, app| o(app.clone()));
    ok.connect_clicked(glib::clone!(#[weak] chooser, move |_| {
        if let Some(app) = chooser.app_info() {
            open(app);
        }
    }));
    win.present();
}

#[cfg(test)]
mod tests {
    #[test]
    fn batch_names() {
        let paths: Vec<std::path::PathBuf> = ["a.jpeg", "b.jpeg", "c.png"].iter().map(Into::into).collect();
        assert_eq!(super::batch_names(&paths, r"^(.*)\.jpeg$", "$1.jpg").unwrap(), ["a.jpg", "b.jpg", "c.png"]);
        assert_eq!(super::batch_names(&paths, "", "img_{n}").unwrap(), ["img_1", "img_2", "img_3"]);
        assert!(super::batch_names(&paths, "(", "x").is_err());
    }

    #[test]
    fn drops_only_quick_word_repeats() {
        use super::ime_repeat;
        use std::time::Duration;
        let quick = Duration::from_millis(20);
        assert!(ime_repeat("fủ", 2, quick, "fủ", 2));
        assert!(ime_repeat("abc", 3, quick, "abc", 3));
        // Typing "ll" fast is real.
        assert!(!ime_repeat("l", 3, quick, "l", 3));
        assert!(!ime_repeat("fủ", 2, Duration::from_millis(300), "fủ", 2));
        // Elsewhere in the text, or another word.
        assert!(!ime_repeat("fủ", 2, quick, "fủ", 0));
        assert!(!ime_repeat("fủ", 2, quick, "là", 2));
    }
}
