mod update;

use adw::prelude::*;
use gtk::{gio, glib};

const APP_ID: &str = "io.github.datlt4.Kolphin";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_window);
    app.run()
}

fn build_window(app: &adw::Application) {
    let menu = gio::Menu::new();
    menu.append(Some("Check for Updates"), Some("win.check-update"));
    let header = adw::HeaderBar::new();
    header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).build());

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&header);
    content.append(&gtk::Label::builder().label("Kolphin").vexpand(true).build());
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&content));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Kolphin")
        .icon_name("kolphin")
        .default_width(1100)
        .default_height(700)
        .content(&toasts)
        .build();

    let check = gio::SimpleAction::new("check-update", None);
    check.connect_activate(glib::clone!(#[weak] toasts, move |_, _| check_update(&toasts, true)));
    window.add_action(&check);
    let install_action = gio::SimpleAction::new("install-update", Some(glib::VariantTy::STRING));
    install_action.connect_activate(glib::clone!(#[weak] toasts, move |_, url| {
        if let Some(url) = url.and_then(|u| u.get::<String>()) {
            install(&toasts, url);
        }
    }));
    window.add_action(&install_action);
    window.present();
    check_update(&toasts, false);
}

/// Look for a release; `manual` also reports "up to date" and errors.
fn check_update(toasts: &adw::ToastOverlay, manual: bool) {
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
fn install(toasts: &adw::ToastOverlay, url: String) {
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
