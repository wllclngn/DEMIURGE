// GNOSIS image thumbnails against the freedesktop thumbnail spec: MD5 by RFC
// 1321's vectors, the URI escaped the way GLib escapes it and named by the
// spec's own example, a thumbnail written with its text chunks and reused
// while current, remade when the image changes, a broken file remembered,
// and the worker thread answering through its eventfd.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gnosis::thumbs::{self, md5_hex, thumbnail, uri, Worker};

#[test]
fn md5_matches_rfc_1321() {
    let vectors = [
        ("", "d41d8cd98f00b204e9800998ecf8427e"),
        ("a", "0cc175b9c0f1b6a831c399e269772661"),
        ("abc", "900150983cd24fb0d6963f7d28e17f72"),
        ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
        ("abcdefghijklmnopqrstuvwxyz", "c3fcd3d76192e4007dfb496cca67e13b"),
        ("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789", "d174ab98d277d9f5a5611c2c9f419d9f"),
        (
            "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            "57edf4a22be3c955ac49da2e2107b67a",
        ),
    ];
    for (input, digest) in vectors {
        assert_eq!(md5_hex(input.as_bytes()), digest, "md5({input:?})");
    }
}

#[test]
fn names_follow_the_spec_and_glib() {
    // The spec's own example.
    let u = uri(Path::new("/home/jens/photos/me.png"));
    assert_eq!(u, "file:///home/jens/photos/me.png");
    assert_eq!(md5_hex(u.as_bytes()), "c6ee772d9e49320e97ec29a7eb5b1697");
    assert_eq!(uri(Path::new("/home/u/My Pictures/\u{fc} (1).png")), "file:///home/u/My%20Pictures/%C3%BC%20(1).png");
    assert!(thumbs::wanted(Path::new("a.JPG")) && !thumbs::wanted(Path::new("a.txt")));
}

fn save(path: &Path, w: u32, h: u32) {
    image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 40, 255])).save(path).unwrap();
}

fn mtime(path: &Path) -> u64 {
    fs::metadata(path).unwrap().modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn cached(root: &Path, image: &Path) -> std::path::PathBuf {
    root.join("normal").join(format!("{}.png", md5_hex(uri(image).as_bytes())))
}

fn text_chunks(file: &Path) -> Vec<(String, String)> {
    let mut reader = png::Decoder::new(std::io::BufReader::new(fs::File::open(file).unwrap())).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut buf).unwrap();
    reader.finish().unwrap();
    reader.info().uncompressed_latin1_text.iter().map(|t| (t.keyword.clone(), t.text.clone())).collect()
}

#[test]
fn a_thumbnail_is_written_with_its_chunks_and_reused_while_current() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("thumbnails");
    let photo = dir.path().join("wide.png");
    save(&photo, 300, 150);

    let t = thumbnail(&photo, &root).expect("a thumbnail");
    assert_eq!((t.width, t.height), (128, 64), "shrunk to fit 128, aspect kept");
    let file = cached(&root, &photo);
    let chunks = text_chunks(&file);
    assert!(chunks.contains(&("Thumb::URI".into(), uri(&photo))));
    assert!(chunks.contains(&("Thumb::MTime".into(), mtime(&photo).to_string())));
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);

    // A current thumbnail is read, not remade: Replace it with a marked one.
    let mut encoder = png::Encoder::new(fs::File::create(&file).unwrap(), 2, 2);
    encoder.set_color(png::ColorType::Rgb);
    encoder.add_text_chunk("Thumb::URI".into(), uri(&photo)).unwrap();
    encoder.add_text_chunk("Thumb::MTime".into(), mtime(&photo).to_string()).unwrap();
    encoder.write_header().unwrap().write_image_data(&[0; 12]).unwrap();
    let t = thumbnail(&photo, &root).unwrap();
    assert_eq!((t.width, t.height, t.rgba.len()), (2, 2, 16), "the cached thumbnail was not used");

    // A changed image is remade.
    save(&photo, 100, 200);
    fs::File::options().write(true).open(&photo).unwrap().set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
    let t = thumbnail(&photo, &root).unwrap();
    assert_eq!((t.width, t.height), (64, 128));
    assert!(text_chunks(&file).contains(&("Thumb::MTime".into(), mtime(&photo).to_string())));
}

#[test]
fn a_small_image_keeps_its_size_and_jpeg_decodes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("thumbnails");
    let small = dir.path().join("small.png");
    save(&small, 40, 30);
    let t = thumbnail(&small, &root).unwrap();
    assert_eq!((t.width, t.height), (40, 30));

    let jpeg = dir.path().join("photo.jpg");
    image::RgbImage::from_pixel(400, 300, image::Rgb([10, 120, 200])).save(&jpeg).unwrap();
    let t = thumbnail(&jpeg, &root).unwrap();
    assert_eq!((t.width, t.height), (128, 96));
}

#[test]
fn a_broken_image_is_remembered_until_it_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("thumbnails");
    let broken = dir.path().join("broken.jpg");
    fs::write(&broken, b"not an image").unwrap();

    assert!(thumbnail(&broken, &root).is_none());
    let marker = root.join("fail").join("gnosis").join(format!("{}.png", md5_hex(uri(&broken).as_bytes())));
    assert!(marker.exists(), "the failure was not remembered");
    assert!(!cached(&root, &broken).exists());

    // Fixed and newer: The marker no longer applies.
    image::RgbImage::from_pixel(64, 64, image::Rgb([0, 0, 0])).save_with_format(&broken, image::ImageFormat::Jpeg).unwrap();
    fs::File::options().write(true).open(&broken).unwrap().set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
    assert!(thumbnail(&broken, &root).is_some());
}

#[test]
fn the_worker_answers_through_its_eventfd() {
    let dir = tempfile::tempdir().unwrap();
    let photo = dir.path().join("p.png");
    save(&photo, 256, 256);
    let worker = Worker::start(dir.path().join("thumbnails")).unwrap();
    worker.ask(photo.clone());

    let mut pfd = libc::pollfd { fd: worker.fd(), events: libc::POLLIN, revents: 0 };
    assert_eq!(unsafe { libc::poll(&mut pfd, 1, 5000) }, 1, "the eventfd never woke");
    let done = worker.take();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].0, photo);
    assert_eq!(done[0].1.as_ref().map(|t| (t.width, t.height)), Some((128, 128)));
}
