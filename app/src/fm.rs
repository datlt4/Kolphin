//! Default file manager: folders open in Kolphin (xdg-mime) and Kolphin answers org.freedesktop.FileManager1,
//! the D-Bus call behind "Show in folder" in browsers and editors.
use gtk::{gio, glib, prelude::*};
use std::path::PathBuf;
use std::process::Command;

const NAME: &str = "org.freedesktop.FileManager1";
const XML: &str = r#"<node><interface name="org.freedesktop.FileManager1">
  <method name="ShowFolders"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
  <method name="ShowItems"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
  <method name="ShowItemProperties"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
</interface></node>"#;

fn service_file() -> PathBuf {
    glib::user_data_dir().join("dbus-1/services/org.freedesktop.FileManager1.service")
}

pub fn is_default() -> bool {
    service_file().exists()
}

/// Make Kolphin (or Files again) the file manager for folders and "Show in folder".
pub fn set_default(on: bool) -> Result<(), String> {
    let desktop = if on { "kolphin.desktop" } else { "org.gnome.Nautilus.desktop" };
    let ok = Command::new("xdg-mime").args(["default", desktop, "inode/directory"]).status().is_ok_and(|s| s.success());
    if !ok {
        return Err("xdg-mime failed".into());
    }
    if on {
        // A user service file wins over /usr/share/dbus-1/services (Nautilus).
        let f = service_file();
        std::fs::create_dir_all(f.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&f, format!("[D-BUS Service]\nName={NAME}\nExec=/usr/bin/kolphin --gapplication-service\n")).map_err(|e| e.to_string())?;
        // Nautilus' background service holds the name until it exits.
        let _ = Command::new("nautilus").arg("-q").status();
    } else {
        let _ = std::fs::remove_file(service_file());
    }
    Ok(())
}

/// Serve FileManager1 while Kolphin is the default (queued behind another owner until it goes).
pub fn serve(app: &adw::Application) {
    if !is_default() {
        return;
    }
    let app = app.clone();
    // The name stays owned for the life of the app (OwnerId has no Drop).
    let _ = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        // A dev build never takes the name from the installed app.
        if cfg!(debug_assertions) { gio::BusNameOwnerFlags::ALLOW_REPLACEMENT } else { gio::BusNameOwnerFlags::ALLOW_REPLACEMENT | gio::BusNameOwnerFlags::REPLACE },
        move |conn, _| {
            let Ok(node) = gio::DBusNodeInfo::for_xml(XML) else { return };
            let Some(iface) = node.lookup_interface(NAME) else { return };
            let app = app.clone();
            let _ = conn
                .register_object("/org/freedesktop/FileManager1", &iface)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    let paths: Vec<PathBuf> = params.get::<(Vec<String>, String)>().map(|(uris, _)| uris.iter().filter_map(|u| gio::File::for_uri(u).path()).collect()).unwrap_or_default();
                    match method {
                        "ShowFolders" => crate::window::open(&app, &paths),
                        "ShowItems" => crate::window::reveal(&app, &paths, false),
                        "ShowItemProperties" => crate::window::reveal(&app, &paths, true),
                        _ => {}
                    }
                    invocation.return_value(None);
                })
                .build();
        },
        |_, _| {},
        |_, _| {},
    );
}
