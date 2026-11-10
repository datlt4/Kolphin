//! Image/video thumbnails: freedesktop cache, visible-first queue, at most 2 jobs per spinning disk.
use gtk::prelude::FileExt;
use image::RgbaImage;
use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// Largest thumbnail Kolphin asks for (freedesktop "large").
pub const MAX_PX: u32 = 256;
const HDD_JOBS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
}

pub fn kind(name: &str) -> Option<Kind> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif" | "tif" | "tiff" => Some(Kind::Image),
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" | "ts" => Some(Kind::Video),
        _ => None,
    }
}

pub struct Job {
    /// Opaque id returned with the result.
    pub key: u64,
    pub path: PathBuf,
    pub kind: Kind,
    pub px: u32,
    pub mtime: i64,
}

pub struct Done {
    pub key: u64,
    pub px: u32,
    pub image: Option<RgbaImage>,
}

pub type Sender = async_channel::Sender<Done>;

struct Pending {
    owner: u64,
    job: Job,
    dev: u64,
    tx: Sender,
}

#[derive(Default)]
struct State {
    queue: Vec<Pending>,
    busy: HashMap<u64, usize>,
    in_flight: HashSet<(u64, u64, u32)>,
    rotational: HashMap<u64, bool>,
}

struct Pool {
    state: Mutex<State>,
    cv: Condvar,
    workers: usize,
}

fn pool() -> &'static Arc<Pool> {
    static POOL: OnceLock<Arc<Pool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
        let pool = Arc::new(Pool { state: Default::default(), cv: Condvar::new(), workers });
        for _ in 0..workers {
            let p = pool.clone();
            std::thread::spawn(move || worker(&p));
        }
        pool
    })
}

/// Replace everything `owner` has queued with `jobs` (in priority order); jobs already running are not repeated.
pub fn request(owner: u64, jobs: Vec<Job>, tx: &Sender) {
    let pool = pool();
    let mut st = pool.state.lock().unwrap();
    st.queue.retain(|p| p.owner != owner);
    for job in jobs {
        if st.in_flight.contains(&(owner, job.key, job.px)) {
            continue;
        }
        let dev = std::fs::metadata(job.path.parent().unwrap_or(&job.path)).map_or(0, |m| m.dev());
        st.rotational.entry(dev).or_insert_with(|| rotational(dev));
        st.queue.push(Pending { owner, job, dev, tx: tx.clone() });
    }
    drop(st);
    pool.cv.notify_all();
}

fn worker(pool: &Pool) {
    loop {
        let p = {
            let mut st = pool.state.lock().unwrap();
            loop {
                let pick = st.queue.iter().position(|p| {
                    let limit = if st.rotational.get(&p.dev).copied().unwrap_or(false) { HDD_JOBS } else { pool.workers };
                    st.busy.get(&p.dev).copied().unwrap_or(0) < limit
                });
                if let Some(i) = pick {
                    let p = st.queue.remove(i);
                    *st.busy.entry(p.dev).or_default() += 1;
                    st.in_flight.insert((p.owner, p.job.key, p.job.px));
                    break p;
                }
                st = pool.cv.wait(st).unwrap();
            }
        };
        let image = thumbnail(&p.job);
        {
            let mut st = pool.state.lock().unwrap();
            *st.busy.entry(p.dev).or_default() -= 1;
            st.in_flight.remove(&(p.owner, p.job.key, p.job.px));
        }
        pool.cv.notify_all();
        let _ = p.tx.send_blocking(Done { key: p.job.key, px: p.job.px, image });
    }
}

/// Is the block device behind `dev` (st_dev) a spinning disk?
pub fn rotational(dev: u64) -> bool {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    let base = format!("/sys/dev/block/{major}:{minor}");
    // A partition has no queue/ of its own; the whole disk is its parent.
    [format!("{base}/queue/rotational"), format!("{base}/../queue/rotational")]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|s| s.trim() == "1")
}

fn thumbnail(job: &Job) -> Option<RgbaImage> {
    let size = if job.px <= 128 { 128 } else { 256 };
    let uri = gtk::gio::File::for_path(&job.path).uri().to_string();
    let dir = gtk::glib::user_cache_dir().join("thumbnails").join(if size == 128 { "normal" } else { "large" });
    let hash: String = <md5::Md5 as md5::Digest>::digest(uri.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let file = dir.join(format!("{hash}.png"));
    let img = match load_cached(&file, job.mtime) {
        Some(img) => img,
        None => {
            let img = match job.kind {
                Kind::Image => decode_image(&job.path, size)?,
                Kind::Video => decode_video(&job.path, size)?,
            };
            save_cached(&dir, &file, &img, &uri, job.mtime);
            img
        }
    };
    Some(fit(img, job.px))
}

fn fit(img: RgbaImage, px: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w <= px && h <= px {
        return img;
    }
    let s = px as f64 / w.max(h) as f64;
    image::imageops::thumbnail(&img, ((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
}

fn load_cached(file: &Path, mtime: i64) -> Option<RgbaImage> {
    let mut reader = png::Decoder::new(BufReader::new(std::fs::File::open(file).ok()?)).read_info().ok()?;
    let fresh = reader.info().uncompressed_latin1_text.iter().any(|t| t.keyword == "Thumb::MTime" && t.text == mtime.to_string());
    if !fresh {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let rgba: Vec<u8> = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf[..info.buffer_size()].to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => buf[..info.buffer_size()].chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => return None,
    };
    RgbaImage::from_raw(info.width, info.height, rgba)
}

fn save_cached(dir: &Path, file: &Path, img: &RgbaImage, uri: &str, mtime: i64) {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
    let tmp = file.with_extension(format!("kolphin-{}.tmp", std::process::id()));
    let write = || -> Option<()> {
        let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&tmp).ok()?), img.width(), img.height());
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.add_text_chunk("Thumb::URI".into(), uri.into()).ok()?;
        enc.add_text_chunk("Thumb::MTime".into(), mtime.to_string()).ok()?;
        enc.add_text_chunk("Software".into(), "Kolphin".into()).ok()?;
        enc.write_header().ok()?.write_image_data(img.as_raw()).ok()
    };
    if write().is_some() {
        let _ = std::fs::rename(&tmp, file);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn decode_image(path: &Path, size: u32) -> Option<RgbaImage> {
    if std::fs::metadata(path).ok()?.len() > 200 << 20 {
        return None;
    }
    let lower = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let img = if lower == "jpg" || lower == "jpeg" { decode_jpeg(path, size) } else { None };
    let img = match img {
        Some(i) => i,
        None => image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.to_rgba8(),
    };
    Some(orient(fit(img, size), exif_orientation(path)))
}

/// JPEG decoded at 1/2, 1/4 or 1/8 scale straight from the DCT: far less work than a full decode.
fn decode_jpeg(path: &Path, size: u32) -> Option<RgbaImage> {
    let mut dec = jpeg_decoder::Decoder::new(BufReader::new(std::fs::File::open(path).ok()?));
    dec.read_info().ok()?;
    let (w, h) = dec.scale(size as u16, size as u16).ok()?;
    let px = dec.decode().ok()?;
    let rgba: Vec<u8> = match dec.info()?.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => px.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&l| [l, l, l, 255]).collect(),
        jpeg_decoder::PixelFormat::CMYK32 => px
            .chunks(4)
            .flat_map(|p| {
                let k = 255 - p[3] as u16;
                let c = |v: u8| ((255 - v as u16) * k / 255) as u8;
                [c(p[0]), c(p[1]), c(p[2]), 255]
            })
            .collect(),
        _ => return None,
    };
    RgbaImage::from_raw(w as u32, h as u32, rgba)
}

pub fn exif_orientation(path: &Path) -> u32 {
    let Ok(f) = std::fs::File::open(path) else { return 1 };
    exif::Reader::new()
        .read_from_container(&mut BufReader::new(f))
        .ok()
        .and_then(|e| e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?.value.get_uint(0))
        .unwrap_or(1)
}

pub fn orient(img: RgbaImage, o: u32) -> RgbaImage {
    use image::imageops::{flip_horizontal, flip_vertical, rotate180, rotate270, rotate90};
    match o {
        2 => flip_horizontal(&img),
        3 => rotate180(&img),
        4 => flip_vertical(&img),
        5 => flip_horizontal(&rotate90(&img)),
        6 => rotate90(&img),
        7 => flip_horizontal(&rotate270(&img)),
        8 => rotate270(&img),
        _ => img,
    }
}

fn decode_video(path: &Path, size: u32) -> Option<RgbaImage> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // Unique per call so concurrent thumbnails never share a /tmp name.
    let out = std::env::temp_dir().join(format!(
        "kolphin-thumb-{}-{}-{}.png",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
    ));
    let ok = std::process::Command::new("ffmpegthumbnailer")
        .arg("-i")
        .arg(path)
        .arg("-o")
        .arg(&out)
        .args(["-s", &size.to_string(), "-c", "png"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let img = ok.then(|| image::open(&out).ok().map(|i| i.to_rgba8())).flatten();
    let _ = std::fs::remove_file(&out);
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert!(kind("a.JPG") == Some(Kind::Image));
        assert!(kind("clip.mkv") == Some(Kind::Video));
        assert!(kind("notes.txt").is_none());
        assert!(kind("noext").is_none());
    }

    #[test]
    fn jpeg_scaled_and_cached() {
        let dir = std::env::temp_dir().join(format!("kolphin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("big.jpg");
        image::RgbImage::from_pixel(1600, 1200, image::Rgb([200, 30, 30])).save(&src).unwrap();
        let img = decode_image(&src, 256).unwrap();
        assert_eq!(img.dimensions(), (256, 192));
        let file = dir.join("t.png");
        save_cached(&dir, &file, &img, "file:///x", 42);
        assert!(load_cached(&file, 42).is_some());
        assert!(load_cached(&file, 43).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
