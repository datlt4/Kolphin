//! Drives, USB sticks and network shares through GIO (udisks, gvfs): mount, eject, connect to a server, free space.
use gtk::{gio, glib, prelude::*};
use std::path::{Path, PathBuf};

/// (free, size) in bytes of the filesystem holding `path`.
pub fn usage(path: &Path) -> Option<(u64, u64)> {
    let info = gio::File::for_path(path).query_filesystem_info("filesystem::free,filesystem::size", None::<&gio::Cancellable>).ok()?;
    let (free, size) = (info.attribute_uint64("filesystem::free"), info.attribute_uint64("filesystem::size"));
    (size > 0).then_some((free, size))
}

/// gvfs FUSE paths are network shares: no free-space query (it can block).
pub fn is_remote(path: &Path) -> bool {
    path.to_string_lossy().contains("/gvfs/")
}

/// Can this mount be ejected or unmounted from the sidebar (removable, network or user-mounted)?
pub fn ejectable(m: &gio::Mount) -> bool {
    m.can_eject() || m.volume().is_some_and(|v| v.can_eject()) || m.drive().is_some_and(|d| d.is_removable()) || (m.volume().is_none() && m.can_unmount())
}

/// A password prompt that defaults to "Remember forever" (the keyring), so shares don't ask again.
/// Delete the passwords gvfs saved in the keyring for this server (Secret Service over D-Bus), so the next
/// connection asks for it again: after the password changed, the stale one would otherwise be tried silently.
pub fn forget_password(uri: &str) {
    let Ok(u) = glib::Uri::parse(uri, glib::UriFlags::NONE) else { return };
    let Some(host) = u.host() else { return };
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else { return };
    let mut query = std::collections::HashMap::new();
    query.insert("protocol".to_string(), u.scheme().to_string());
    query.insert("server".to_string(), host.to_string());
    let call = |path: &str, iface: &str, method: &str, args: Option<&glib::Variant>| {
        bus.call_sync(Some("org.freedesktop.secrets"), path, iface, method, args, None, gio::DBusCallFlags::NONE, 3000, None::<&gio::Cancellable>).ok()
    };
    let Some(found) = call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "SearchItems", Some(&(query,).to_variant())) else { return };
    // (unlocked, locked) item paths; only unlocked ones can be deleted without a prompt.
    let Some((unlocked, _)) = found.get::<(Vec<glib::variant::ObjectPath>, Vec<glib::variant::ObjectPath>)>() else { return };
    for item in unlocked {
        call(item.as_str(), "org.freedesktop.Secret.Item", "Delete", None);
    }
}

/// Drop the share mounted at `path` and its saved password; `done` gets the share's address to connect to again
/// (which then asks for the password).
pub fn reconnect(path: PathBuf, done: impl FnOnce(Result<String, String>) + 'static) {
    let mount = gio::VolumeMonitor::get().mounts().into_iter().find(|m| m.root().path().is_some_and(|r| path.starts_with(r)));
    let Some(mount) = mount else { return done(Err("the share is not mounted".into())) };
    let uri = mount.root().uri().to_string();
    mount.unmount_with_operation(gio::MountUnmountFlags::FORCE, None::<&gio::MountOperation>, None::<&gio::Cancellable>, move |res| match res {
        Ok(()) => {
            forget_password(&uri);
            done(Ok(uri));
        }
        Err(e) => done(Err(e.to_string())),
    });
}

fn mount_op(parent: &gtk::Window) -> gtk::MountOperation {
    let op = gtk::MountOperation::new(Some(parent));
    op.set_password_save(gio::PasswordSave::Permanently);
    op
}

pub fn mount_volume(parent: &gtk::Window, vol: gio::Volume, done: impl FnOnce(Result<PathBuf, String>) + 'static) {
    let op = mount_op(parent);
    let v = vol.clone();
    vol.mount(gio::MountMountFlags::NONE, Some(&op), None::<&gio::Cancellable>, move |res| {
        done(res.map_err(|e| e.to_string()).and_then(|_| v.get_mount().and_then(|m| m.root().path()).ok_or_else(|| "mounted, but has no local path".into())));
    });
}

/// Eject (powers a USB stick off) when possible, otherwise unmount.
pub fn eject(parent: &gtk::Window, m: gio::Mount, done: impl FnOnce(Result<(), String>) + 'static) {
    let op = gtk::MountOperation::new(Some(parent));
    let cb = move |res: Result<(), glib::Error>| done(res.map_err(|e| e.to_string()));
    // Unmount only (data is flushed, safe to unplug): the drive stays listed so a click mounts it again.
    if m.can_unmount() {
        m.unmount_with_operation(gio::MountUnmountFlags::NONE, Some(&op), None::<&gio::Cancellable>, cb);
    } else {
        m.eject_with_operation(gio::MountUnmountFlags::NONE, Some(&op), None::<&gio::Cancellable>, cb);
    }
}

pub enum Connected {
    Path(PathBuf),
    /// A server address without a share (smb://host): pick one of these (name, URI).
    Shares(Vec<(String, String)>),
}

/// Hosts gvfs found on the LAN (DNS-SD/SMB), as (name, smb:// URI); the empty "Windows Network" root is left out.
pub fn discover(done: impl FnOnce(Vec<(String, String)>) + 'static) {
    let net = gio::File::for_uri("network:///");
    glib::spawn_future_local(async move {
        let mut hosts = Vec::new();
        if let Ok(en) = net.enumerate_children_future("standard::display-name,standard::target-uri", gio::FileQueryInfoFlags::NONE, glib::Priority::LOW).await {
            while let Ok(infos) = en.next_files_future(32, glib::Priority::LOW).await {
                if infos.is_empty() {
                    break;
                }
                for i in infos {
                    if let Some(uri) = i.attribute_string("standard::target-uri").filter(|u| u.as_str() != "smb:///") {
                        hosts.push((i.display_name().to_string(), uri.to_string()));
                    }
                }
            }
        }
        hosts.sort();
        done(hosts);
    });
}

/// "HDD-1TB on 192.168.0.186" for smb://192.168.0.186/HDD-1TB; other URIs as they are.
pub fn server_label(uri: &str) -> String {
    match uri.split_once("://").map(|(_, r)| r.trim_end_matches('/')) {
        Some(rest) => match rest.split_once('/') {
            Some((host, path)) if !path.is_empty() => format!("{} on {}", path.rsplit('/').next().unwrap_or(path), host.rsplit('@').next().unwrap_or(host)),
            _ => rest.to_string(),
        },
        None => uri.to_string(),
    }
}

/// `smb://host:445/x` → `smb://host/x` (445 is SMB's default port; gvfs treats an explicit one as another server).
pub fn normalize(uri: &str) -> String {
    let uri = uri.trim();
    match uri.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("smb") => {
            let (host, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
            format!("smb://{}/{path}", host.strip_suffix(":445").unwrap_or(host))
        }
        _ => uri.to_string(),
    }
}

/// Mount `smb://`, `sftp://`, `ftp://`, `dav://` … through gvfs; passwords go through GTK's dialog and the keyring.
pub fn connect(parent: &gtk::Window, uri: &str, done: impl FnOnce(Result<Connected, String>) + 'static) {
    let uri = normalize(uri);
    let file = gio::File::for_uri(&uri);
    let op = mount_op(parent);
    file.mount_enclosing_volume(gio::MountMountFlags::NONE, Some(&op), None::<&gio::Cancellable>, move |res| {
        match res {
            Ok(()) => {}
            Err(e) if e.matches(gio::IOErrorEnum::AlreadyMounted) => {}
            Err(e) => return done(Err(e.to_string())),
        }
        // A File made before mounting still believes it is unmounted (no path); ask again.
        let f = gio::File::for_uri(&uri);
        if let Some(path) = f.path() {
            return done(Ok(Connected::Path(path)));
        }
        // Browsing a server: list its shares instead.
        glib::spawn_future_local(async move {
            let attrs = "standard::name,standard::display-name,standard::target-uri";
            let list = f.enumerate_children_future(attrs, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT).await;
            let shares = match list {
                Ok(en) => {
                    let mut out = Vec::new();
                    while let Ok(infos) = en.next_files_future(64, glib::Priority::DEFAULT).await {
                        if infos.is_empty() {
                            break;
                        }
                        // Browse entries are mountables: their real address is target-uri.
                        out.extend(
                            infos
                                .iter()
                                .map(|i| {
                                    let uri = i.attribute_string("standard::target-uri").map_or_else(|| f.child(i.name()).uri().to_string(), |u| u.to_string());
                                    (i.display_name().to_string(), uri)
                                })
                                .filter(|(name, _)| !name.ends_with('$')),
                        );
                    }
                    out
                }
                Err(e) => return done(Err(e.to_string())),
            };
            done(if shares.is_empty() { Err("the server has no shares you can open".into()) } else { Ok(Connected::Shares(shares)) });
        });
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn labels() {
        assert_eq!(super::server_label("smb://192.168.0.186/HDD-1TB"), "HDD-1TB on 192.168.0.186");
        assert_eq!(super::server_label("sftp://koi@host/home/koi/"), "koi on host");
        assert_eq!(super::server_label("smb://nas/"), "nas");
    }

    #[test]
    fn normalizes_smb() {
        assert_eq!(super::normalize("smb://192.168.0.186:445"), "smb://192.168.0.186/");
        assert_eq!(super::normalize(" smb://nas:445/HOME "), "smb://nas/HOME");
        assert_eq!(super::normalize("sftp://u@h:22/x"), "sftp://u@h:22/x");
    }

    #[test]
    fn usage_of_root() {
        let (free, size) = super::usage(std::path::Path::new("/")).unwrap();
        assert!(size > 0 && free <= size);
        assert!(super::is_remote(std::path::Path::new("/run/user/1000/gvfs/smb-share:server=x,share=y")));
    }
}
