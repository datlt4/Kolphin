//! Convert to PDF / merge into one PDF. Images are written here (one A4 page each, fitted, upright per EXIF);
//! documents go through ONLYOFFICE's converter (or LibreOffice); pages are joined with pdfunite (poppler-utils).
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const DOCS: &[&str] = &["doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods", "csv", "ppt", "pptx", "odp"];
/// A4 in points.
const A4: (f64, f64) = (595.0, 842.0);

fn ext(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

fn is_image(p: &Path) -> bool {
    crate::thumbs::kind(&p.file_name().unwrap_or_default().to_string_lossy()) == Some(crate::thumbs::Kind::Image)
}

pub fn is_pdf(p: &Path) -> bool {
    ext(p) == "pdf"
}

/// Can this file become PDF pages (an image, an office/text document, or a PDF already)?
pub fn convertible(p: &Path) -> bool {
    p.is_file() && (is_pdf(p) || is_image(p) || DOCS.contains(&ext(p).as_str()))
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(program).is_file()))
}

/// One image ready to place on a page.
struct Picture {
    w: u32,
    h: u32,
    /// "DeviceRGB" or "DeviceGray".
    color: &'static str,
    filter: &'static str,
    data: Vec<u8>,
}

fn picture(path: &Path) -> Result<Picture, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    // An upright RGB or grey JPEG goes in as is (no quality loss).
    if matches!(ext(path).as_str(), "jpg" | "jpeg") && crate::thumbs::exif_orientation(path) == 1 {
        let data = std::fs::read(path).map_err(|e| err(&e))?;
        let mut dec = jpeg_decoder::Decoder::new(&data[..]);
        dec.read_info().map_err(|e| err(&e))?;
        let info = dec.info().ok_or("no JPEG header")?;
        let color = match info.pixel_format {
            jpeg_decoder::PixelFormat::RGB24 => Some("DeviceRGB"),
            jpeg_decoder::PixelFormat::L8 => Some("DeviceGray"),
            _ => None,
        };
        if let Some(color) = color {
            return Ok(Picture { w: info.width as u32, h: info.height as u32, color, filter: "DCTDecode", data });
        }
    }
    let img = image::ImageReader::open(path).map_err(|e| err(&e))?.with_guessed_format().map_err(|e| err(&e))?.decode().map_err(|e| err(&e))?;
    let img = crate::thumbs::orient(img.to_rgba8(), crate::thumbs::exif_orientation(path));
    // Transparent parts become white paper.
    let rgb: Vec<u8> = img
        .pixels()
        .flat_map(|p| {
            let [r, g, b, a] = p.0;
            let over = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
            [over(r), over(g), over(b)]
        })
        .collect();
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&rgb).map_err(|e| err(&e))?;
    Ok(Picture { w: img.width(), h: img.height(), color: "DeviceRGB", filter: "FlateDecode", data: z.finish().map_err(|e| err(&e))? })
}

/// A PDF with one A4 page per picture (landscape for wide pictures), each fitted and centred.
fn write_pictures(pictures: &[Picture], out: &Path) -> Result<(), String> {
    let mut pdf: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    let mut obj = |pdf: &mut Vec<u8>, body: &[u8]| {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    };
    // 1: catalog, 2: page tree, then page / content / image for each picture.
    let kids: Vec<String> = (0..pictures.len()).map(|i| format!("{} 0 R", 3 + i * 3)).collect();
    obj(&mut pdf, b"<< /Type /Catalog /Pages 2 0 R >>");
    obj(&mut pdf, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pictures.len()).as_bytes());
    for (i, p) in pictures.iter().enumerate() {
        let (content, image) = (4 + i * 3, 5 + i * 3);
        let (pw, ph) = if p.w > p.h { (A4.1, A4.0) } else { A4 };
        let s = (pw / p.w as f64).min(ph / p.h as f64);
        let (w, h) = (p.w as f64 * s, p.h as f64 * s);
        let draw = format!("q {w:.2} 0 0 {h:.2} {:.2} {:.2} cm /Im Do Q", (pw - w) / 2.0, (ph - h) / 2.0);
        obj(&mut pdf, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Resources << /XObject << /Im {image} 0 R >> >> /Contents {content} 0 R >>").as_bytes());
        obj(&mut pdf, format!("<< /Length {} >>\nstream\n{draw}\nendstream", draw.len()).as_bytes());
        let mut body = format!("<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /{} /BitsPerComponent 8 /Filter /{} /Length {} >>\nstream\n", p.w, p.h, p.color, p.filter, p.data.len()).into_bytes();
        body.extend_from_slice(&p.data);
        body.extend_from_slice(b"\nendstream");
        obj(&mut pdf, &body);
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1).as_bytes());
    std::fs::write(out, pdf).map_err(|e| e.to_string())
}

/// A document turned into `out`: ONLYOFFICE's converter when it is installed (deb or Flatpak), else LibreOffice.
fn office_to_pdf(doc: &Path, out: &Path, scratch: &Path) -> Result<(), String> {
    if let Some(x2t) = OnlyOffice::find() {
        if x2t.convert(doc, out, scratch).is_ok() {
            return Ok(());
        }
    }
    libreoffice_to_pdf(doc, out, scratch)
}

/// ONLYOFFICE Desktop Editors' converter (x2t) and the font list it needs (made when the editors first ran).
struct OnlyOffice {
    /// How to run x2t: the program, then arguments before the params file.
    run: Vec<String>,
    fonts: PathBuf,
    /// Root of the editors as x2t sees it (inside the Flatpak sandbox for the Flatpak).
    root: &'static str,
}

impl OnlyOffice {
    fn find() -> Option<OnlyOffice> {
        let home = gtk::glib::home_dir();
        let deb = Path::new("/opt/onlyoffice/desktopeditors/converter/x2t");
        let fonts = home.join(".local/share/onlyoffice/desktopeditors/data/fonts");
        if deb.is_file() && fonts.join("AllFonts.js").is_file() {
            return Some(OnlyOffice { run: vec![deb.display().to_string()], fonts, root: "/opt/onlyoffice/desktopeditors" });
        }
        let fonts = home.join(".var/app/org.onlyoffice.desktopeditors/data/onlyoffice/desktopeditors/data/fonts");
        let installed = ["/var/lib/flatpak/app/org.onlyoffice.desktopeditors", ".local/share/flatpak/app/org.onlyoffice.desktopeditors"]
            .iter()
            .any(|d| home.join(d).is_dir());
        (installed && on_path("flatpak") && fonts.join("AllFonts.js").is_file()).then(|| OnlyOffice {
            run: ["flatpak", "run", "--command=/app/bin/opt/onlyoffice/desktopeditors/converter/x2t", "org.onlyoffice.desktopeditors"].map(String::from).to_vec(),
            fonts,
            root: "/app/bin/opt/onlyoffice/desktopeditors",
        })
    }

    fn convert(&self, doc: &Path, out: &Path, scratch: &Path) -> Result<(), String> {
        let esc = |p: &Path| p.display().to_string().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        // 513: PDF.
        let params = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<TaskQueueDataConvert><m_sFileFrom>{}</m_sFileFrom><m_sFileTo>{}</m_sFileTo><m_nFormatTo>513</m_nFormatTo><m_sAllFontsPath>{}</m_sAllFontsPath><m_sFontDir>{}</m_sFontDir><m_sThemeDir>{}/editors/sdkjs/slide/themes</m_sThemeDir><m_bDontSaveAdditional>true</m_bDontSaveAdditional></TaskQueueDataConvert>",
            esc(doc),
            esc(out),
            esc(&self.fonts.join("AllFonts.js")),
            esc(&self.fonts),
            self.root
        );
        let file = scratch.join(format!("x2t-{}.xml", std::fs::read_dir(scratch).map_or(0, |d| d.count())));
        std::fs::write(&file, params).map_err(|e| e.to_string())?;
        let status = Command::new(&self.run[0])
            .args(&self.run[1..])
            .arg(&file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() && out.is_file() {
            Ok(())
        } else {
            let _ = std::fs::remove_file(out);
            Err(format!("ONLYOFFICE could not convert {}", doc.display()))
        }
    }
}

/// LibreOffice, headless, into a scratch folder so nothing is overwritten.
fn libreoffice_to_pdf(doc: &Path, out: &Path, scratch: &Path) -> Result<(), String> {
    let office = ["soffice", "libreoffice"].into_iter().find(|p| on_path(p)).ok_or("Converting documents needs ONLYOFFICE Desktop Editors or LibreOffice")?;
    let dir = scratch.join(format!("doc{}", std::fs::read_dir(scratch).map_or(0, |d| d.count())));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let status = Command::new(office)
        .args(["--headless", "--convert-to", "pdf", "--outdir"])
        .arg(&dir)
        .arg(doc)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    let made = dir.join(Path::new(doc.file_name().unwrap_or_default()).with_extension("pdf"));
    if !status.success() || !made.is_file() {
        return Err(format!("LibreOffice could not convert {}", doc.display()));
    }
    std::fs::rename(&made, out).or_else(|_| std::fs::copy(&made, out).map(|_| ())).map_err(|e| e.to_string())
}

fn scratch_dir() -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // Unpredictable + 0700: converted documents can be sensitive.
    let d = std::env::temp_dir().join(format!(
        "kolphin-pdf-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d).map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    Ok(d)
}

/// `name.pdf` next to each file; returns the PDFs made.
pub fn convert_each(files: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let scratch = scratch_dir()?;
    let mut made = Vec::new();
    let result = files.iter().filter(|f| !is_pdf(f)).try_for_each(|f| {
        let out = crate::ops::unique(&f.with_extension("pdf"));
        if is_image(f) {
            write_pictures(&[picture(f)?], &out)?;
        } else {
            office_to_pdf(f, &out, &scratch)?;
        }
        made.push(out);
        Ok(())
    });
    let _ = std::fs::remove_dir_all(&scratch);
    result.map(|_| made)
}

/// All `files`, in this order, as the pages of `out`.
pub fn merge(files: &[PathBuf], out: &Path) -> Result<(), String> {
    if files.iter().all(|f| is_image(f)) {
        let pictures = files.iter().map(|f| picture(f)).collect::<Result<Vec<_>, _>>()?;
        return write_pictures(&pictures, out);
    }
    if !on_path("pdfunite") {
        return Err("Merging PDFs needs pdfunite (sudo apt install poppler-utils)".into());
    }
    let scratch = scratch_dir()?;
    let result = (|| {
        let mut parts = Vec::new();
        for (i, f) in files.iter().enumerate() {
            if is_pdf(f) {
                parts.push(f.clone());
                continue;
            }
            let part = scratch.join(format!("{i}.pdf"));
            if is_image(f) {
                write_pictures(&[picture(f)?], &part)?;
            } else {
                office_to_pdf(f, &part, &scratch)?;
            }
            parts.push(part);
        }
        let out_run = Command::new("pdfunite").args(&parts).arg(out).output().map_err(|e| e.to_string())?;
        if !out_run.status.success() {
            return Err(format!("pdfunite: {}", String::from_utf8_lossy(&out_run.stderr).trim()));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    #[test]
    fn images_become_pages_in_order() {
        let d = std::env::temp_dir().join(format!("kolphin-pdftest-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let wide = d.join("wide.png");
        image::RgbaImage::from_pixel(40, 20, image::Rgba([255, 0, 0, 128])).save(&wide).unwrap();
        let tall = d.join("tall.jpg");
        image::RgbImage::from_pixel(20, 40, image::Rgb([0, 0, 255])).save(&tall).unwrap();
        let out = d.join("out.pdf");
        super::merge(&[tall.clone(), wide.clone()], &out).unwrap();
        let pdf = std::fs::read(&out).unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4") && text.ends_with("%%EOF\n"));
        assert_eq!(text.matches("/Type /Page ").count(), 2);
        // First page is the tall JPEG kept as is (portrait), second the wide PNG (landscape).
        let first = text.find("/DCTDecode").unwrap();
        assert!(first < text.find("/FlateDecode").unwrap());
        assert!(text.contains("/MediaBox [0 0 595 842]") && text.contains("/MediaBox [0 0 842 595]"));
        // xref offsets point at the objects.
        let xref: usize = text.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert!(pdf[xref..].starts_with(b"xref"));
        assert_eq!(super::convert_each(std::slice::from_ref(&wide)).unwrap(), vec![d.join("wide.pdf")]);
        assert!(super::convertible(&wide) && !super::convertible(&PathBuf::from("/nope.zip")));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
