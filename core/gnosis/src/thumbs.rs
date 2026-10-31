// Image thumbnails, the freedesktop way: A PNG of at most 128 px per image
// under $XDG_CACHE_HOME/thumbnails/normal/, named by the MD5 of the image's
// file URI and carrying that URI and the image's mtime (Thumb::URI,
// Thumb::MTime). A thumbnail any application made is reused, and the ones
// GNOSIS makes are shared. A failure is remembered under
// thumbnails/fail/gnosis/ so a broken file is not decoded again until it
// changes. The work runs on a worker thread; GNOSIS draws the grey card until
// the answer arrives.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::UNIX_EPOCH;

pub const SIZE: u32 = 128;
const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
// A larger file is not decoded for a thumbnail.
const MAX_BYTES: u64 = 64 << 20;

pub struct Thumb {
    pub width: u32,
    pub height: u32,
    // Straight (not premultiplied) RGBA, row after row.
    pub rgba: Vec<u8>,
}

// A file GNOSIS shows a thumbnail for, by its extension.
pub fn wanted(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

// $XDG_CACHE_HOME/thumbnails, or ~/.cache/thumbnails.
pub fn cache_root() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache.join("thumbnails"))
}

// The file URI the spec hashes, escaped as GLib's g_filename_to_uri escapes
// it, so the name matches the thumbnails GNOME and others write.
pub fn uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()/:@&=+$,".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// MD5 (RFC 1321), for naming thumbnail files; not for anything secret.
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
        14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15,
        21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());

    let mut h: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let rotated = a.wrapping_add(f).wrapping_add(k[i]).wrapping_add(m[g]).rotate_left(S[i]);
            (a, d, c, b) = (d, c, b, b.wrapping_add(rotated));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 16];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

pub fn md5_hex(data: &[u8]) -> String {
    md5(data).iter().map(|b| format!("{b:02x}")).collect()
}

// The thumbnail for `path` under the cache `root`: The cached one if it is
// current, else a new one written to the cache. None for a file that cannot
// be read as an image, remembered so it is not tried again until it changes.
pub fn thumbnail(path: &Path, root: &Path) -> Option<Thumb> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let uri = uri(path);
    let name = format!("{}.png", md5_hex(uri.as_bytes()));
    let normal = root.join("normal").join(&name);
    if let Some((thumb, true)) = read(&normal, &uri, mtime) {
        return Some(thumb);
    }
    let fail = root.join("fail").join("gnosis").join(&name);
    if matches!(read(&fail, &uri, mtime), Some((_, true))) {
        return None;
    }
    match make(path, meta.len()) {
        Some(thumb) => {
            let _ = write(&normal, &uri, mtime, &thumb);
            Some(thumb)
        }
        None => {
            let _ = write(&fail, &uri, mtime, &Thumb { width: 1, height: 1, rgba: vec![0; 4] });
            None
        }
    }
}

// A thumbnail file's pixels, and whether its Thumb::URI and Thumb::MTime
// match. The whole image is decoded: Text chunks may follow the pixels.
fn read(file: &Path, uri: &str, mtime: u64) -> Option<(Thumb, bool)> {
    let mut decoder = png::Decoder::new(BufReader::new(File::open(file).ok()?));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    reader.finish().ok()?;
    let info = reader.info();
    let text = |key: &str| {
        info.uncompressed_latin1_text
            .iter()
            .find(|t| t.keyword == key)
            .map(|t| t.text.clone())
            .or_else(|| info.utf8_text.iter().find(|t| t.keyword == key).and_then(|t| t.get_text().ok()))
    };
    let current = text("Thumb::URI").as_deref() == Some(uri) && text("Thumb::MTime") == Some(mtime.to_string());

    let pixels = &buf[..frame.buffer_size()];
    let rgba: Vec<u8> = match frame.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => pixels.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((Thumb { width: frame.width, height: frame.height, rgba }, current))
}

// Decode and shrink an image to fit SIZE x SIZE; a smaller one keeps its
// size.
fn make(path: &Path, len: u64) -> Option<Thumb> {
    if len > MAX_BYTES {
        return None;
    }
    let image = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let image = if image.width() > SIZE || image.height() > SIZE { image.thumbnail(SIZE, SIZE) } else { image };
    let rgba = image.to_rgba8();
    Some(Thumb { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

// Write a thumbnail with its text chunks, atomically and private to the
// user, as the spec asks.
fn write(file: &Path, uri: &str, mtime: u64, thumb: &Thumb) -> Option<()> {
    let dir = file.parent()?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).ok()?;
    let temp = dir.join(format!(".{}.gnosis-{}", file.file_name()?.to_str()?, std::process::id()));
    let out = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temp).ok()?;
    let mut encoder = png::Encoder::new(BufWriter::new(out), thumb.width, thumb.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.add_text_chunk("Thumb::URI".into(), uri.into()).ok()?;
    encoder.add_text_chunk("Thumb::MTime".into(), mtime.to_string()).ok()?;
    encoder.add_text_chunk("Software".into(), "GNOSIS".into()).ok()?;
    let written = encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(&thumb.rgba).and_then(|_| w.finish()))
        .is_ok();
    if !written {
        let _ = fs::remove_file(&temp);
        return None;
    }
    fs::rename(&temp, file).ok()
}

// The thumbnail thread: Paths in, thumbnails out, an eventfd to wake the
// event loop when one is ready.
pub struct Worker {
    requests: mpsc::Sender<PathBuf>,
    results: mpsc::Receiver<(PathBuf, Option<Thumb>)>,
    wake: OwnedFd,
}

impl Worker {
    pub fn start(root: PathBuf) -> Option<Self> {
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if raw < 0 {
            return None;
        }
        let wake = unsafe { OwnedFd::from_raw_fd(raw) };
        let signal = wake.try_clone().ok()?;
        let (requests, inbox) = mpsc::channel::<PathBuf>();
        let (outbox, results) = mpsc::channel();
        std::thread::Builder::new()
            .name("gnosis-thumbs".into())
            .spawn(move || {
                for path in inbox {
                    let thumb = thumbnail(&path, &root);
                    if outbox.send((path, thumb)).is_err() {
                        break;
                    }
                    let one: u64 = 1;
                    unsafe { libc::write(signal.as_raw_fd(), (&one as *const u64).cast(), 8) };
                }
            })
            .ok()?;
        Some(Self { requests, results, wake })
    }

    pub fn ask(&self, path: PathBuf) {
        let _ = self.requests.send(path);
    }

    pub fn fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    // The thumbnails ready since the last call.
    pub fn take(&self) -> Vec<(PathBuf, Option<Thumb>)> {
        let mut count = 0u64;
        unsafe { libc::read(self.wake.as_raw_fd(), (&mut count as *mut u64).cast(), 8) };
        self.results.try_iter().collect()
    }
}
