//! Material Icon Theme: pick an icon by folder name / file name / extension, render it to a texture.
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/material.rs"));

struct Mapping {
    folder_names: HashMap<String, String>,
    file_names: HashMap<String, String>,
    extensions: HashMap<String, String>,
    folder: String,
    file: String,
}

fn mapping() -> &'static Mapping {
    static M: OnceLock<Mapping> = OnceLock::new();
    M.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(MAPPING).expect("material-icons.json");
        // Icon ids resolve to an SVG file stem through iconDefinitions.
        let svg = |id: &str| {
            let path = v["iconDefinitions"][id]["iconPath"].as_str().unwrap_or("");
            path.rsplit('/').next().and_then(|f| f.strip_suffix(".svg")).unwrap_or(id).to_string()
        };
        let table = |key: &str| -> HashMap<String, String> {
            v[key].as_object().into_iter().flatten().filter_map(|(k, id)| Some((k.to_lowercase(), svg(id.as_str()?)))).collect()
        };
        Mapping {
            folder_names: table("folderNames"),
            file_names: table("fileNames"),
            extensions: table("fileExtensions"),
            folder: svg(v["folder"].as_str().unwrap_or("folder")),
            file: svg(v["file"].as_str().unwrap_or("file")),
        }
    })
}

/// SVG stem of the icon for this entry.
pub fn icon_name(name: &str, is_dir: bool) -> &'static str {
    let m = mapping();
    let lower = name.to_lowercase();
    if is_dir {
        return m.folder_names.get(&lower).unwrap_or(&m.folder);
    }
    if let Some(icon) = m.file_names.get(&lower) {
        return icon;
    }
    // Longest extension first: "a.d.ts" tries "d.ts" before "ts".
    let mut rest = lower.as_str();
    while let Some(p) = rest.find('.') {
        rest = &rest[p + 1..];
        if let Some(icon) = m.extensions.get(rest) {
            return icon;
        }
    }
    &m.file
}

/// The embedded icon called `name`, as a static string.
pub fn find(name: &str) -> Option<&'static str> {
    SVGS.binary_search_by(|(n, _)| (*n).cmp(name)).ok().map(|i| SVGS[i].0)
}

/// Every embedded icon name, sorted.
pub fn all() -> impl Iterator<Item = &'static str> {
    SVGS.iter().map(|(n, _)| *n)
}

thread_local! {
    static TEXTURES: RefCell<HashMap<(&'static str, u32), gdk::Texture>> = RefCell::new(HashMap::new());
}

/// The icon rendered at `px` device pixels (cached).
pub fn texture(icon: &'static str, px: u32) -> Option<gdk::Texture> {
    TEXTURES.with(|t| {
        if let Some(tex) = t.borrow().get(&(icon, px)) {
            return Some(tex.clone());
        }
        let tex = render(icon, px).or_else(|| render(&mapping().file, px))?;
        t.borrow_mut().insert((icon, px), tex.clone());
        Some(tex)
    })
}

fn render(icon: &str, px: u32) -> Option<gdk::Texture> {
    let data = SVGS.binary_search_by(|(n, _)| (*n).cmp(icon)).ok().map(|i| SVGS[i].1)?;
    let tree = resvg::usvg::Tree::from_data(data, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    let size = tree.size();
    let scale = px as f32 / size.width().max(size.height());
    let (dx, dy) = ((px as f32 - size.width() * scale) / 2.0, (px as f32 - size.height() * scale) / 2.0);
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(dx, dy), &mut pixmap.as_mut());
    let bytes = glib::Bytes::from_owned(pixmap.take());
    Some(gdk::MemoryTexture::new(px as i32, px as i32, gdk::MemoryFormat::R8g8b8a8Premultiplied, &bytes, px as usize * 4).into())
}

#[cfg(test)]
mod tests {
    use super::icon_name;

    #[test]
    fn lookup() {
        assert_eq!(icon_name("main.rs", false), "rust");
        assert_eq!(icon_name("PHOTO.JPG", false), "image");
        assert_eq!(icon_name("Cargo.toml", false), icon_name("cargo.toml", false));
        assert_eq!(icon_name("unknown.zzzz", false), "file");
        assert_eq!(icon_name("src", true), "folder-src");
        assert_eq!(icon_name("random-dir", true), "folder");
    }
}
