//! Embed the Material Icon Theme SVGs and mapping into the binary.
use std::fmt::Write;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../assets/material-icons").canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<String> = std::fs::read_dir(dir.join("icons"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok()?.strip_suffix(".svg").map(String::from))
        .collect();
    names.sort();
    let mut out = String::from("pub static SVGS: &[(&str, &[u8])] = &[\n");
    for n in &names {
        writeln!(out, "    ({n:?}, include_bytes!({:?})),", dir.join("icons").join(format!("{n}.svg"))).unwrap();
    }
    out.push_str("];\n");
    writeln!(out, "pub static MAPPING: &str = include_str!({:?});", dir.join("material-icons.json")).unwrap();
    std::fs::write(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("material.rs"), out).unwrap();
}
