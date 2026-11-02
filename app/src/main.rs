mod actions;
mod canvas;
mod clipboard;
mod devices;
mod dialogs;
mod git;
mod icons;
mod kermius;
mod model;
mod ops;
mod palette;
mod pane;
mod pathbar;
mod props;
mod quicklook;
mod stats;
mod thumbs;
mod update;
mod viewprops;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};

const APP_ID: &str = "io.github.datlt4.Kolphin";

// Dark navy + blue accent, shared with Kermius / Kellyfin.
const CSS: &str = "
@define-color accent_color #5b8cff;
@define-color accent_bg_color #5b8cff;
@define-color accent_fg_color #ffffff;
@define-color window_bg_color #14161d;
@define-color window_fg_color #e6e8ee;
@define-color view_bg_color #14161d;
@define-color view_fg_color #e6e8ee;
@define-color headerbar_bg_color #191c24;
@define-color headerbar_fg_color #e6e8ee;
@define-color headerbar_border_color #2a2f3b;
@define-color headerbar_backdrop_color #191c24;
@define-color headerbar_shade_color #2a2f3b;
@define-color card_bg_color #20242f;
@define-color popover_bg_color #1e222c;
@define-color popover_fg_color #e6e8ee;
@define-color dialog_bg_color #1e222c;
@define-color dialog_fg_color #e6e8ee;
@define-color kolphin_side #191c24;
@define-color kolphin_elev #1e222c;
@define-color kolphin_border #2a2f3b;
@define-color kolphin_muted #8b91a1;
@define-color kolphin_faint #5f6677;
@define-color kolphin_hover #222632;
@define-color kolphin_hover_strong #2c3243;

window, .background { background: @window_bg_color; }
headerbar { box-shadow: inset 0 -1px @kolphin_border; min-height: 46px; }
headerbar button { border-radius: 8px; }
headerbar button:hover { background: @kolphin_hover_strong; color: #ffffff; box-shadow: inset 0 0 0 1px #3a4153; }
headerbar button:active { background: #353c50; }
headerbar button:checked { background: alpha(@accent_color, 0.16); color: #cfdcff; }

.sidebar-pane { background: @kolphin_side; }
headerbar { background: @kolphin_side; }
.brand { margin: 0 10px 0 4px; }
.header-sep { margin: 10px 8px; min-width: 1px; background: @kolphin_border; }
.places { padding-top: 6px; }
.brand label { font-weight: 700; font-size: 16px; }
.places { background: transparent; padding: 0 10px 12px; }
.places row { border-radius: 9px; padding: 7px 11px; margin: 1px 0; color: @kolphin_muted; }
.places row:hover { background: @kolphin_hover_strong; color: #ffffff; }
.places row:selected { background: alpha(@accent_color, 0.14); color: #ffffff; }
.places row:selected image { color: @accent_color; }
.places row.side-title, .places row.side-title:hover { background: none; padding: 16px 11px 6px; font-size: 11px; font-weight: 600; letter-spacing: 1px; color: @kolphin_faint; }
.places levelbar.usage { margin: 0 2px 2px 29px; }
.places levelbar.usage trough { min-height: 3px; border-radius: 99px; background: @kolphin_elev; }
.places levelbar.usage block.filled { min-height: 3px; border-radius: 99px; background: @accent_color; }
.places levelbar.usage block.empty { background: none; }
.places button.eject { min-height: 22px; min-width: 22px; padding: 0; margin: -4px -6px -4px 0; }
.side-foot { color: @kolphin_faint; font-size: 11.5px; padding: 10px 21px; border-top: 1px solid @kolphin_border; }
paned > separator { min-width: 1px; min-height: 1px; margin: 0; padding: 0; background: @kolphin_border; background-image: none; box-shadow: none; }
headerbar .linked > button:not(:checked):not(:hover) { background: none; box-shadow: none; }

tabbar .box { background: none; border: none; box-shadow: none; padding: 0; }
tabbar tab { border-radius: 8px; margin: 4px 2px; min-height: 30px; color: @kolphin_muted; }
tabbar tab:hover:not(:selected):not(:checked) { background: @kolphin_hover_strong; color: #ffffff; }
tabbar tab, tabbar tab:backdrop { background: none; box-shadow: none; }
tabbar tab:selected, tabbar tab:checked { background: #262b38; color: #ffffff; box-shadow: inset 0 0 0 1px #363c4b; }

.pathbar { background: @kolphin_elev; border: 1px solid @kolphin_border; border-radius: 9px; margin: 10px 12px 6px; min-height: 34px; }
.pathbar entry { background: none; border: none; box-shadow: none; outline: none; min-height: 32px; }
.pathbar button { padding: 2px 8px; min-height: 26px; margin: 3px 0; border-radius: 7px; color: @kolphin_muted; font-weight: 600; }
.pathbar button:hover { background: @kolphin_hover_strong; color: #ffffff; }
.pathbar button.current { color: @window_fg_color; }
.pathbar label.dim-label { color: @kolphin_faint; opacity: 1; }
.dialog-body { padding: 18px; }
.side-title-label { font-size: 11px; font-weight: 600; letter-spacing: 1px; color: @kolphin_faint; margin-top: 6px; }
progressbar trough { min-height: 6px; border-radius: 99px; background: @kolphin_elev; }
progressbar progress { min-height: 6px; border-radius: 99px; background: @accent_color; }
.palette row { padding: 8px 10px; border-radius: 8px; }
.palette row:selected { background: alpha(@accent_color, 0.18); }
.statusbar { color: @kolphin_muted; padding: 7px 16px; font-size: 12px; border-top: 1px solid @kolphin_border; }
.pane-inactive .pathbar, .pane-inactive .statusbar { opacity: 0.5; }
.pane-close { margin: 10px 10px 6px 0; border-radius: 8px; }
.pane-close:hover { background: @kolphin_hover_strong; color: #ffffff; }
scrollbar { background: transparent; border: none; }
scrollbar slider { background: #363c4b; border-radius: 99px; }
scrollbar.vertical slider { min-width: 6px; min-height: 48px; }
scrollbar.horizontal slider { min-height: 6px; min-width: 48px; }
scrollbar slider:hover { background: @kolphin_muted; }
popover contents { background: @popover_bg_color; border: 1px solid @kolphin_border; border-radius: 12px; }
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
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
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
