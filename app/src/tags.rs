//! macOS-style tags: 7 preset colors plus custom tags, assignments in `user.kolphin.tags`
//! xattrs (they survive rename/move), definitions in tags.json. A symlink shows the
//! union of its own tags and its target's.
use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub const XATTR: &str = "user.kolphin.tags";
pub const MAX_DOTS: usize = 4;

pub struct Tag {
    pub id: String,
    pub name: String,
    pub hex: String,
}

const PRESETS: &[(&str, &str, &str)] = &[
    ("red", "Red", "#FF453A"),
    ("orange", "Orange", "#FF9F0A"),
    ("yellow", "Yellow", "#FFD60A"),
    ("green", "Green", "#30D158"),
    ("blue", "Blue", "#0A84FF"),
    ("purple", "Purple", "#BF5AF2"),
    ("gray", "Gray", "#8E8E93"),
];

/// Extra colors offered for new tags.
pub const PALETTE: &[(&str, &str)] = &[
    ("Red", "#FF453A"),
    ("Orange", "#FF9F0A"),
    ("Yellow", "#FFD60A"),
    ("Green", "#30D158"),
    ("Blue", "#0A84FF"),
    ("Purple", "#BF5AF2"),
    ("Pink", "#FF375F"),
    ("Teal", "#64D2FF"),
    ("Brown", "#AC8E68"),
    ("Gray", "#8E8E93"),
];

fn defs_file() -> PathBuf {
    gtk::glib::user_data_dir().join("kolphin").join("tags.json")
}

fn read_custom() -> Vec<Tag> {
    let v: serde_json::Value = std::fs::read(defs_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    v.get("custom").and_then(|c| c.as_array()).map_or(Vec::new(), |a| {
        a.iter()
            .filter_map(|t| {
                let (id, name, hex) = (t.get("id")?.as_str()?, t.get("name")?.as_str()?, t.get("color")?.as_str()?);
                (valid_id(id) && !name.is_empty() && parse_hex(hex).is_some()).then(|| Tag { id: id.into(), name: name.into(), hex: hex.into() })
            })
            .collect()
    })
}

fn write_custom(tags: &[Tag]) {
    use std::os::unix::fs::PermissionsExt;
    let path = defs_file();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let v = serde_json::json!({"custom": tags.iter().map(|t| serde_json::json!({"id": t.id, "name": t.name, "color": t.hex})).collect::<Vec<_>>()});
    if std::fs::write(&path, v.to_string()).is_ok() {
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// All tags: the 7 presets, then customs.
pub fn defs() -> Vec<Tag> {
    let mut out: Vec<Tag> = PRESETS.iter().map(|(id, name, hex)| Tag { id: id.to_string(), name: name.to_string(), hex: hex.to_string() }).collect();
    out.extend(read_custom());
    out
}

pub fn is_custom(id: &str) -> bool {
    !PRESETS.iter().any(|(p, _, _)| *p == id)
}

/// id -> (r, g, b) for the canvas dots.
pub fn color_map() -> HashMap<String, (f64, f64, f64)> {
    defs().into_iter().filter_map(|t| parse_hex(&t.hex).map(|c| (t.id, c))).collect()
}

pub fn parse_hex(hex: &str) -> Option<(f64, f64, f64)> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|b| b as f64 / 255.0);
    Some((v(0)?, v(2)?, v(4)?))
}

pub fn add_custom(name: &str, hex: &str) -> Result<Tag, String> {
    let name = name.trim();
    if name.is_empty() || name.contains(',') {
        return Err("Invalid name".into());
    }
    if parse_hex(hex).is_none() {
        return Err("Invalid color".into());
    }
    let mut customs = read_custom();
    if customs.iter().any(|t| t.name == name) {
        return Err(format!("{name} already exists"));
    }
    let id = format!("c{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    let tag = Tag { id, name: name.into(), hex: hex.into() };
    customs.push(Tag { id: tag.id.clone(), name: tag.name.clone(), hex: tag.hex.clone() });
    write_custom(&customs);
    Ok(tag)
}

pub fn remove_custom(id: &str) -> bool {
    if !is_custom(id) {
        return false;
    }
    let customs: Vec<Tag> = read_custom().into_iter().filter(|t| t.id != id).collect();
    write_custom(&customs);
    true
}

fn parse(raw: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(raw).split(',').filter_map(|s| valid_id(s.trim()).then(|| s.trim().to_string())).collect()
}

fn xname() -> CString {
    CString::new(XATTR).unwrap()
}

fn xpath(p: &Path) -> Option<CString> {
    CString::new(p.as_os_str().as_bytes()).ok()
}

/// This file's own tags (never follows symlinks; links themselves hold none on Linux).
pub fn own(path: &Path) -> Vec<String> {
    let (Some(p), name) = (xpath(path), xname()) else { return Vec::new() };
    let n = unsafe { libc::lgetxattr(p.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; n as usize];
    let m = unsafe { libc::lgetxattr(p.as_ptr(), name.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if m <= 0 {
        return Vec::new();
    }
    parse(&buf[..m as usize])
}

/// Tags of `name` in `dir`: its own plus, for symlinks, its target's.
pub fn tags_for(dir: &Path, name: &OsStr) -> Vec<String> {
    let p = dir.join(name);
    let mut out = own(&p);
    if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
        if let Ok(t) = std::fs::read_link(&p) {
            let t = if t.is_absolute() { t } else { dir.join(t) };
            // getxattr follows the link: the target's tags.
            let (Some(p), name) = (xpath(&t), xname()) else { return out };
            let n = unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
            if n > 0 {
                let mut buf = vec![0u8; n as usize];
                let m = unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if m > 0 {
                    for t in parse(&buf[..m as usize]) {
                        if !out.contains(&t) {
                            out.push(t);
                        }
                    }
                }
            }
        }
    }
    out
}

fn os_error() -> String {
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(n) if n == libc::ENOTSUP || n == libc::EOPNOTSUPP => "Tags need extended attributes (not FAT/exFAT/NTFS)".into(),
        _ => e.to_string(),
    }
}

/// Replace this file's tags. The kernel forbids xattrs on the link itself
/// (`lsetxattr` gives EPERM), so tagging a symlink tags its target.
pub fn set(path: &Path, ids: &[String]) -> Result<(), String> {
    if ids.iter().any(|i| !valid_id(i)) {
        return Err("Invalid tag".into());
    }
    let (Some(p), name) = (xpath(path), xname()) else { return Err("Bad path".into()) };
    if ids.is_empty() {
        let r = unsafe { libc::removexattr(p.as_ptr(), name.as_ptr()) };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ENODATA) {
                return Err(os_error());
            }
        }
        return Ok(());
    }
    let val = CString::new(ids.join(",")).map_err(|_| "Invalid tag".to_string())?;
    let bytes = val.as_bytes();
    if unsafe { libc::setxattr(p.as_ptr(), name.as_ptr(), bytes.as_ptr() as *const libc::c_void, bytes.len(), 0) } != 0 {
        return Err(os_error());
    }
    Ok(())
}

/// Toggle `id` on this file; returns whether it now has it.
/// On a symlink this flips the target's tag (links hold no xattr themselves).
pub fn toggle(path: &Path, id: &str) -> Result<bool, String> {
    if !valid_id(id) {
        return Err("Invalid tag".into());
    }
    let mut ids = match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => tags_for(dir, name),
        _ => own(path),
    };
    let has = !ids.contains(&id.to_string());
    if has {
        ids.push(id.to_string());
    } else {
        ids.retain(|t| t != id);
    }
    set(path, &ids)?;
    Ok(has)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_hex() {
        assert_eq!(defs().len(), 7);
        assert_eq!(parse_hex("#FF453A"), Some((1.0, 0x45 as f64 / 255.0, 0x3A as f64 / 255.0)));
        assert!(parse_hex("red").is_none() && parse_hex("#12345").is_none());
        assert!(valid_id("red") && !valid_id("a,b") && !valid_id(""));
    }

    #[test]
    fn xattr_round_trip() {
        let d = std::env::temp_dir().join(format!("kolphin-tags-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("f.txt");
        std::fs::write(&f, "x").unwrap();
        let r = set(&f, &["red".into(), "blue".into()]);
        if r.is_err() {
            // Filesystems without xattr (or missing permission): nothing to check.
            std::fs::remove_dir_all(&d).unwrap();
            return;
        }
        assert_eq!(own(&f), ["red", "blue"]);
        assert!(!toggle(&f, "red").unwrap());
        assert_eq!(own(&f), ["blue"]);
        assert!(toggle(&f, "green").unwrap());
        assert_eq!(own(&f), ["blue", "green"]);
        // A symlink shows its target's tags; tagging it tags the target (lsetxattr is EPERM).
        std::os::unix::fs::symlink(&f, d.join("link")).unwrap();
        assert_eq!(tags_for(&d, std::ffi::OsStr::new("link")), ["blue", "green"]);
        set(&d.join("link"), &["yellow".into()]).unwrap();
        assert_eq!(tags_for(&d, std::ffi::OsStr::new("link")), ["yellow"]);
        assert_eq!(own(&f), ["yellow"]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
