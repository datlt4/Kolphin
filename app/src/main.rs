mod canvas;
mod icons;
mod model;
mod pane;
mod pathbar;
mod update;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};

const APP_ID: &str = "io.github.datlt4.Kolphin";

const CSS: &str = "
.pathbar { margin: 4px 6px; }
.pathbar button.current { font-weight: bold; }
.statusbar { padding: 4px 10px; font-size: smaller; }
.pane-inactive .pathbar, .pane-inactive .statusbar { opacity: 0.5; }
";

fn main() -> glib::ExitCode {
    // GTK 4.6's GL renderers drop frames (and take ~1 s to start) on NVIDIA/X11; cairo holds full refresh rate.
    if std::env::var_os("GSK_RENDERER").is_none() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }
    let mut flags = gio::ApplicationFlags::HANDLES_OPEN;
    if cfg!(debug_assertions) || std::env::var_os("KOLPHIN_BENCH").is_some() {
        flags |= gio::ApplicationFlags::NON_UNIQUE; // don't hand off to an installed, running Kolphin
    }
    let app = adw::Application::builder().application_id(APP_ID).flags(flags).build();
    app.connect_startup(|app| {
        let css = gtk::CssProvider::new();
        css.load_from_data(CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        window::set_accels(app);
    });
    app.connect_activate(|app| window::open(app, &[]));
    app.connect_open(|app, files, _| {
        let paths: Vec<_> = files.iter().filter_map(|f| f.path()).filter(|p| p.is_dir()).collect();
        window::open(app, &paths);
    });
    app.run()
}

/// Look for a release; `manual` also reports "up to date" and errors.
pub fn check_update(toasts: &adw::ToastOverlay, manual: bool) {
    glib::spawn_future_local(glib::clone!(#[weak] toasts, async move {
        let res = gio::spawn_blocking(update::check).await.unwrap_or_else(|_| Err("update check crashed".into()));
        match res {
            Ok(Some(rel)) => offer(&toasts, rel),
            Ok(None) if manual => toasts.add_toast(adw::Toast::new("Kolphin is up to date")),
            Err(e) if manual => toasts.add_toast(adw::Toast::new(&format!("Update check failed: {e}"))),
            _ => {}
        }
    }));
}

fn offer(toasts: &adw::ToastOverlay, rel: update::Release) {
    let (title, button) = if rel.fresh_install {
        (format!("Install Kolphin {} system-wide?", rel.version), "Install")
    } else {
        (format!("Kolphin {} is available", rel.version), "Update")
    };
    let toast = adw::Toast::builder()
        .title(&title)
        .button_label(button)
        .action_name("win.install-update")
        .action_target(&rel.deb_url.to_variant())
        .timeout(0)
        .build();
    toasts.add_toast(toast);
}

/// Download with progress, install via pkexec, then restart into the installed build.
pub fn install(toasts: &adw::ToastOverlay, url: String) {
    let status = adw::Toast::builder().title("Downloading…").timeout(0).build();
    toasts.add_toast(status.clone());
    let (tx, rx) = async_channel::unbounded::<u32>();
    glib::spawn_future_local(glib::clone!(#[weak] toasts, #[strong] status, async move {
        let job = gio::spawn_blocking(move || {
            let deb = update::download(&url, |done, total| {
                let _ = tx.send_blocking((done * 100).checked_div(total).unwrap_or(0) as u32);
            })?;
            let _ = tx.send_blocking(101);
            update::install(&deb)
        });
        while let Ok(p) = rx.recv().await {
            status.set_title(&if p > 100 { "Installing…".to_string() } else { format!("Downloading… {p}%") });
        }
        status.dismiss();
        match job.await.unwrap_or_else(|_| Err("installer crashed".into())) {
            Ok(()) => {
                update::restart_later();
                if let Some(app) = gio::Application::default() {
                    app.quit();
                }
            }
            Err(e) => toasts.add_toast(adw::Toast::new(&format!("Install failed: {e}"))),
        }
    }));
}
