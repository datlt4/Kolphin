//! Archives: browse one as a folder (gvfs archive backend), extract here, compress (File Roller does the work).
use std::path::{Path, PathBuf};
use std::process::Command;

const EXTS: &[&str] = &["zip", "tar", "tgz", "tbz2", "txz", "tzst", "7z", "rar", "gz", "bz2", "xz", "zst", "iso", "jar", "apk", "deb", "rpm", "cab"];

pub fn is_archive(p: &Path) -> bool {
    !p.is_dir() && p.extension().is_some_and(|e| EXTS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()))
}

/// The gvfs address that shows an archive as a folder.
pub fn uri(p: &Path) -> String {
    let inner = gtk::gio::prelude::FileExt::uri(&gtk::gio::File::for_path(p)).to_string();
    // gvfs keys archive mounts by the host escaped twice; a once-escaped address mounts but then reads as unmounted.
    let once = gtk::glib::Uri::escape_string(&inner, None, false);
    format!("archive://{}/", gtk::glib::Uri::escape_string(&once, None, false))
}

/// Extract next to the archive (File Roller makes a folder when there are several top-level items).
pub fn extract_here(archives: &[PathBuf]) -> Result<(), String> {
    quiet(Command::new("file-roller").arg("--extract-here").args(archives)).spawn().map(|_| ()).map_err(|e| format!("file-roller: {e}"))
}

/// Formats Compress offers: extension, the programs File Roller needs for it (any one), the package to install.
pub const FORMATS: &[(&str, &[&str], &str)] = &[
    ("zip", &["zip"], "zip"),
    ("tar.gz", &["gzip"], "gzip"),
    ("tar.xz", &["xz"], "xz-utils"),
    ("tar.zst", &["zstd"], "zstd"),
    ("7z", &["7z", "7za", "7zz"], "p7zip-full"),
    ("rar", &["rar"], "rar"),
];

/// Is one of `programs` on PATH?
pub fn available(programs: &[&str]) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| programs.iter().any(|p| d.join(p).is_file())))
}

/// Pack `files` into `archive`; the format comes from its extension.
pub fn compress(archive: &Path, files: &[PathBuf]) -> Result<(), String> {
    quiet(Command::new("file-roller").arg(format!("--add-to={}", archive.display())).args(files)).spawn().map(|_| ()).map_err(|e| format!("file-roller: {e}"))
}

/// File Roller prints GTK warnings on every run; keep them out of Kolphin's output.
fn quiet(c: &mut Command) -> &mut Command {
    c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
}

#[cfg(test)]
mod tests {
    #[test]
    fn detects_and_escapes() {
        use std::path::Path;
        assert!(super::is_archive(Path::new("/x/a.ZIP")) && super::is_archive(Path::new("a.tar.gz")));
        assert!(!super::is_archive(Path::new("a.txt")));
        assert_eq!(super::uri(Path::new("/tmp/a b.zip")), "archive://file%253A%252F%252F%252Ftmp%252Fa%252520b.zip/");
    }
}
