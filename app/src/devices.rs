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

/// Mount `smb://`, `sftp://`, `ftp://`, `dav://` … through gvfs; passwords go through GTK's dialog and the keyring.
pub fn connect(parent: &gtk::Window, uri: &str, done: impl FnOnce(Result<PathBuf, String>) + 'static) {
    let file = gio::File::for_uri(uri.trim());
    let op = gtk::MountOperation::new(Some(parent));
    let f = file.clone();
    file.mount_enclosing_volume(gio::MountMountFlags::NONE, Some(&op), None::<&gio::Cancellable>, move |res| {
        let ok = match res {
            Ok(()) => Ok(()),
            Err(e) if e.matches(gio::IOErrorEnum::AlreadyMounted) => Ok(()),
            Err(e) => Err(e.to_string()),
        };
        done(ok.and_then(|_| f.path().ok_or_else(|| "this share has no local path (is gvfs-fuse running?)".into())));
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn usage_of_root() {
        let (free, size) = super::usage(std::path::Path::new("/")).unwrap();
        assert!(size > 0 && free <= size);
        assert!(super::is_remote(std::path::Path::new("/run/user/1000/gvfs/smb-share:server=x,share=y")));
    }
}
