//! New File / New Document: empty files, blank office documents (open in LibreOffice, OnlyOffice, WPS or MS Office)
//! and the user's own templates from ~/Templates.
use std::path::{Path, PathBuf};

/// (id, menu label, default name, contents)
pub const KINDS: &[(&str, &str, &str, &[u8])] = &[
    ("empty", "Empty File", "New File", b""),
    ("txt", "Text File", "New Text File.txt", b""),
    ("md", "Markdown", "New Note.md", b""),
    ("docx", "Word Document (.docx)", "New Document.docx", include_bytes!("../../assets/templates/blank.docx")),
    ("xlsx", "Excel Spreadsheet (.xlsx)", "New Spreadsheet.xlsx", include_bytes!("../../assets/templates/blank.xlsx")),
    ("pptx", "PowerPoint Presentation (.pptx)", "New Presentation.pptx", include_bytes!("../../assets/templates/blank.pptx")),
    ("odt", "Text Document (.odt)", "New Document.odt", include_bytes!("../../assets/templates/blank.odt")),
    ("ods", "Spreadsheet (.ods)", "New Spreadsheet.ods", include_bytes!("../../assets/templates/blank.ods")),
    ("odp", "Presentation (.odp)", "New Presentation.odp", include_bytes!("../../assets/templates/blank.odp")),
];

/// Files in the XDG Templates folder (GNOME's "New Document" source), sorted by name.
pub fn user_templates() -> Vec<PathBuf> {
    let Some(dir) = gtk::glib::user_special_dir(gtk::glib::UserDirectory::Templates).filter(|d| *d != gtk::glib::home_dir()) else { return Vec::new() };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    files.sort();
    files
}

/// Default name for `id`: a built-in kind, or "template:<path>".
pub fn default_name(id: &str) -> Option<String> {
    match id.strip_prefix("template:") {
        Some(path) => Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()),
        None => KINDS.iter().find(|k| k.0 == id).map(|k| k.2.to_string()),
    }
}

/// Create `name` in `dir` from `id`; never overwrites.
pub fn create(dir: &Path, name: &str, id: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.contains('/') {
        return Err("Invalid name".into());
    }
    let contents = match id.strip_prefix("template:") {
        Some(path) => std::fs::read(path).map_err(|e| e.to_string())?,
        None => KINDS.iter().find(|k| k.0 == id).ok_or("Unknown document type")?.3.to_vec(),
    };
    let p = dir.join(name);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&p).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut f, &contents).map_err(|e| e.to_string())?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    #[test]
    fn creates_without_overwriting() {
        let d = std::env::temp_dir().join(format!("kolphin-newdoc-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = super::create(&d, "a.docx", "docx").unwrap();
        assert!(std::fs::read(&p).unwrap().starts_with(b"PK"));
        assert!(super::create(&d, "a.docx", "empty").is_err());
        assert_eq!(std::fs::metadata(super::create(&d, "b", "empty").unwrap()).unwrap().len(), 0);
        assert_eq!(super::default_name("xlsx").as_deref(), Some("New Spreadsheet.xlsx"));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
