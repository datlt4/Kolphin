//! Command palette (Ctrl+K): type to filter every command and custom action, Enter runs it.
use gtk::{gdk, gio, glib, prelude::*};
use std::rc::Rc;

/// (label, shortcut, action, target)
const COMMANDS: &[(&str, &str, &str, Option<&str>)] = &[
    ("Open Terminal Here", "F4", "win.terminal", None),
    ("Share via Kermius", "", "win.share", None),
    ("New Tab", "Ctrl+T", "win.new-tab", None),
    ("New Folder", "Ctrl+Shift+N", "win.new-folder", None),
    ("Split View", "F3", "win.split", None),
    ("Show Hidden Files", "Ctrl+H", "win.toggle-hidden", None),
    ("Filter", "Ctrl+I", "win.filter", None),
    ("Search Subfolders", "Ctrl+F", "win.search", None),
    ("Select by Pattern", "", "win.select-pattern", None),
    ("Random Sample", "", "win.random-sample", None),
    ("Folder Statistics", "", "win.stats", None),
    ("Properties", "Alt+Enter", "win.properties", None),
    ("Rename", "F2", "win.rename", None),
    ("Move to Trash", "Del", "win.trash", None),
    ("Undo", "Ctrl+Z", "win.undo", None),
    ("Copy Path", "", "win.copy-path", None),
    ("Open With", "", "win.open-with", None),
    ("Quick Look", "Space", "win.quicklook", None),
    ("Create Symlink", "", "win.make-symlink", None),
    ("Create Hard Link", "", "win.make-hardlink", None),
    ("Connect to Server", "", "win.connect-server", None),
    ("View: Icons", "Ctrl+1", "win.view-mode", Some("icons")),
    ("View: Compact", "Ctrl+2", "win.view-mode", Some("compact")),
    ("View: Details", "Ctrl+3", "win.view-mode", Some("details")),
    ("Sort by Name", "", "win.sort-by", Some("name")),
    ("Sort by Size", "", "win.sort-by", Some("size")),
    ("Sort by Modified", "", "win.sort-by", Some("modified")),
    ("Group by Extension", "", "win.group-by", Some("extension")),
    ("Group by Modified", "", "win.group-by", Some("modified")),
    ("Group by Image Size", "", "win.group-by", Some("dimensions")),
    ("No Grouping", "", "win.group-by", Some("none")),
    ("Show Sidebar", "F9", "win.show-sidebar", None),
    ("Drop Shelf", "Ctrl+Shift+D", "win.shelf", None),
    ("Edit Custom Actions", "", "win.edit-actions", None),
    ("Check for Updates", "", "win.check-update", None),
];

/// Every character of `query` appears in `text`, in order (case-insensitive).
pub fn fuzzy(query: &str, text: &str) -> bool {
    let mut chars = text.chars().flat_map(char::to_lowercase);
    query.chars().flat_map(char::to_lowercase).filter(|c| !c.is_whitespace()).all(|q| chars.any(|c| c == q))
}

pub fn show(parent: &gtk::ApplicationWindow) {
    let search = gtk::SearchEntry::builder().placeholder_text("Type a command…").build();
    crate::dialogs::ime_safe(&search);
    let list = gtk::ListBox::builder().css_classes(["palette"]).build();
    let mut entries: Vec<(String, String, String, Option<glib::Variant>)> =
        COMMANDS.iter().map(|(l, k, a, t)| (l.to_string(), k.to_string(), a.to_string(), t.map(|t| t.to_variant()))).collect();
    for (i, a) in crate::actions::load().iter().enumerate() {
        entries.push((a.name.clone(), "action".into(), "win.custom-action".into(), Some((i as i32).to_variant())));
    }
    for (label, key, _, _) in &entries {
        let row = gtk::Box::builder().spacing(12).build();
        row.append(&gtk::Label::builder().label(label).xalign(0.0).hexpand(true).build());
        row.append(&gtk::Label::builder().label(key).css_classes(["dim-label"]).build());
        list.append(&row);
    }
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).css_classes(["dialog-body"]).build();
    body.append(&search);
    body.append(&gtk::ScrolledWindow::builder().child(&list).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build());
    let win = gtk::Window::builder().title("Command Palette").transient_for(parent).modal(true).decorated(false).default_width(560).default_height(420).child(&body).build();

    let entries = Rc::new(entries);
    list.set_filter_func(glib::clone!(#[weak] search, #[strong] entries, #[upgrade_or] true, move |row| {
        entries.get(row.index() as usize).is_some_and(|e| fuzzy(&search.text(), &e.0))
    }));
    let first_visible = glib::clone!(#[weak] list, #[upgrade_or] None, move || {
        let mut i = 0;
        while let Some(r) = list.row_at_index(i) {
            if r.is_child_visible() {
                return Some(r);
            }
            i += 1;
        }
        None
    });
    let fv = first_visible.clone();
    search.connect_search_changed(glib::clone!(#[weak] list, move |_| {
        list.invalidate_filter();
        list.select_row(fv().as_ref());
    }));
    let run = glib::clone!(#[weak] win, #[weak] parent, #[strong] entries, move |i: i32| {
        win.close();
        if let Some((_, _, action, target)) = entries.get(i as usize) {
            let name = action.trim_start_matches("win.");
            gio::prelude::ActionGroupExt::activate_action(&parent, name, target.as_ref());
        }
    });
    let r = run.clone();
    list.connect_row_activated(move |_, row| r(row.index()));
    search.connect_activate(glib::clone!(#[weak] list, move |_| {
        if let Some(row) = list.selected_row().or_else(&first_visible) {
            run(row.index());
        }
    }));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak] win,
        #[weak] list,
        #[upgrade_or] glib::Propagation::Proceed,
        move |_, key, _, _| {
            let step = match key {
                gdk::Key::Escape => {
                    win.close();
                    return glib::Propagation::Stop;
                }
                gdk::Key::Down => 1,
                gdk::Key::Up => -1,
                _ => return glib::Propagation::Proceed,
            };
            let mut i = list.selected_row().map_or(-1, |r| r.index());
            loop {
                i += step;
                match list.row_at_index(i) {
                    Some(r) if r.is_child_visible() => {
                        list.select_row(Some(&r));
                        r.grab_focus();
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
            glib::Propagation::Stop
        }
    ));
    win.add_controller(keys);
    list.select_row(list.row_at_index(0).as_ref());
    win.present();
    search.grab_focus();
}

#[cfg(test)]
mod tests {
    #[test]
    fn fuzzy() {
        assert!(!super::fuzzy("ots", "Open Terminal Here"));
        assert!(super::fuzzy("opter", "Open Terminal Here"));
        assert!(super::fuzzy("vsc", "Open in VS Code"));
        assert!(!super::fuzzy("xyz", "Open in VS Code"));
    }
}
