//! Embed the Material Icon Theme SVGs (plus our own in assets/custom-icons) and mapping into the binary.
use std::fmt::Write;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../assets/material-icons").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", dir.display());
    let custom = dir.join("../custom-icons").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", custom.display());
    // (name, file); a custom icon wins over a Material one with the same name.
    let mut icons = std::collections::BTreeMap::new();
    for d in [dir.join("icons"), custom] {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if let Some(n) = e.file_name().to_str().and_then(|f| f.strip_suffix(".svg")) {
                icons.insert(n.to_string(), e.path());
            }
        }
    }
    let mut out = String::from("pub static SVGS: &[(&str, &[u8])] = &[\n");
    for (n, path) in &icons {
        writeln!(out, "    ({n:?}, include_bytes!({path:?})),").unwrap();
    }
    out.push_str("];\n");
    writeln!(out, "pub static MAPPING: &str = include_str!({:?});", dir.join("material-icons.json")).unwrap();
    std::fs::write(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("material.rs"), out).unwrap();
}
