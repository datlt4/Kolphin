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

pub fn mount_volume(parent: &gtk::Window, vol: gio::Volume, done: impl FnOnce(Result<PathBuf, String>) + 'static) {
    let op = gtk::MountOperation::new(Some(parent));
    let v = vol.clone();
    vol.mount(gio::MountMountFlags::NONE, Some(&op), None::<&gio::Cancellable>, move |res| {
        done(res.map_err(|e| e.to_string()).and_then(|_| v.get_mount().and_then(|m| m.root().path()).ok_or_else(|| "mounted, but has no local path".into())));
    });
}

/// Eject (powers a USB stick off) when possible, otherwise unmount.
pub fn eject(parent: &gtk::Window, m: gio::Mount, done: impl FnOnce(Result<(), String>) + 'static) {
    let op = gtk::MountOperation::new(Some(parent));
    let cb = move |res: Result<(), glib::Error>| done(res.map_err(|e| e.to_string()));
    if m.can_eject() {
        m.eject_with_operation(gio::MountUnmountFlags::NONE, Some(&op), None::<&gio::Cancellable>, cb);
    } else {
        m.unmount_with_operation(gio::MountUnmountFlags::NONE, Some(&op), None::<&gio::Cancellable>, cb);
    }
}

pub enum Connected {
    Path(PathBuf),
    /// A server address without a share (smb://host): pick one of these (name, URI).
    Shares(Vec<(String, String)>),
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
    let file = gio::File::for_uri(&normalize(uri));
    let op = gtk::MountOperation::new(Some(parent));
    let f = file.clone();
    file.mount_enclosing_volume(gio::MountMountFlags::NONE, Some(&op), None::<&gio::Cancellable>, move |res| {
        match res {
            Ok(()) => {}
            Err(e) if e.matches(gio::IOErrorEnum::AlreadyMounted) => {}
            Err(e) => return done(Err(e.to_string())),
        }
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
