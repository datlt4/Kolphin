//! Embed the Material Icon Theme SVGs (plus our own in assets/custom-icons) and mapping into the binary.
//! Custom icons sit in <Group>/<Sub>/ folders, which become the icon picker's categories.
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// "Cars_and_4_wheels" -> "Cars & 4 wheels".
fn label(dir: &str) -> String {
    dir.replace("_and_", " & ").replace('_', " ")
}

fn svgs(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| Some((e.file_name().to_str()?.strip_suffix(".svg")?.to_string(), e.path())))
        .collect();
    v.sort();
    v
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort();
    v
}

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../assets/material-icons").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", dir.display());
    let custom = dir.join("../custom-icons").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", custom.display());

    let mut categories = vec!["Files".to_string(), "Folders".to_string(), "Brands › Software".to_string()];
    // name -> (file, category index); a custom icon wins over a Material one with the same name.
    let mut icons = BTreeMap::new();
    for (n, p) in svgs(&dir.join("icons")) {
        let cat = if n.starts_with("folder") { 1 } else { 0 };
        icons.insert(n, (p, cat));
    }
    for (n, p) in svgs(&custom) {
        icons.insert(n, (p, 2));
    }
    for group in subdirs(&custom) {
        for sub in subdirs(&group) {
            let name = format!("{} › {}", label(group.file_name().unwrap().to_str().unwrap()), label(sub.file_name().unwrap().to_str().unwrap()));
            let cat = categories.iter().position(|c| *c == name).unwrap_or_else(|| {
                categories.push(name);
                categories.len() - 1
            });
            for (n, p) in svgs(&sub) {
                icons.insert(n, (p, cat));
            }
        }
    }

    let mut out = String::from("pub static SVGS: &[(&str, &[u8], usize)] = &[\n");
    for (n, (path, cat)) in &icons {
        writeln!(out, "    ({n:?}, include_bytes!({path:?}), {cat}),").unwrap();
    }
    out.push_str("];\n");
    writeln!(out, "pub static CATEGORIES: &[&str] = &{categories:?};").unwrap();
    writeln!(out, "pub static MAPPING: &str = include_str!({:?});", dir.join("material-icons.json")).unwrap();
    std::fs::write(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("material.rs"), out).unwrap();
}
