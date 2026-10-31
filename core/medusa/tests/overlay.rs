use medusa::overlay::{Key, Overlay};

const ENTER: u32 = 0xff0d;
const ESCAPE: u32 = 0xff1b;

// A desktop 200 by 100, red on its left half and blue on its right.
fn desktop() -> cairo::ImageSurface {
    let shot = cairo::ImageSurface::create(cairo::Format::Rgb24, 200, 100).unwrap();
    let cr = cairo::Context::new(&shot).unwrap();
    cr.set_source_rgb(1.0, 0.0, 0.0);
    cr.rectangle(0.0, 0.0, 100.0, 100.0);
    cr.fill().unwrap();
    cr.set_source_rgb(0.0, 0.0, 1.0);
    cr.rectangle(100.0, 0.0, 100.0, 100.0);
    cr.fill().unwrap();
    drop(cr);
    shot
}

// (r, g, b) at (x, y) of an Rgb24 surface.
fn pixel(surface: &mut cairo::ImageSurface, x: usize, y: usize) -> (u8, u8, u8) {
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().unwrap();
    let p = &data[y * stride + x * 4..y * stride + x * 4 + 4];
    (p[2], p[1], p[0])
}

fn kind(key: Key) -> &'static str {
    match key {
        Key::Continue => "continue",
        Key::Cancel => "cancel",
        Key::Save { copy: true } => "copy",
        Key::Save { copy: false } => "save",
        Key::Pin => "pin",
    }
}

#[test]
fn a_dragged_region_crops_the_frozen_desktop_under_it() {
    let mut overlay = Overlay::new(desktop(), (1.0, 0.6, 0.0));
    overlay.press((90, 10));
    overlay.motion((100, 20));
    overlay.release((110, 30));
    assert_eq!(kind(overlay.key(0x73, true, false)), "save");
    let (mut crop, rect) = overlay.crop().unwrap().expect("a 20 x 20 region is kept");
    assert_eq!(rect, (90, 10, 20, 20));
    assert_eq!((crop.width(), crop.height()), (20, 20));
    assert_eq!(pixel(&mut crop, 5, 5), (255, 0, 0));
    assert_eq!(pixel(&mut crop, 15, 5), (0, 0, 255));
}

#[test]
fn with_no_region_the_whole_desktop_is_kept() {
    let mut overlay = Overlay::new(desktop(), (1.0, 0.6, 0.0));
    // A click too small to be a selection leaves none.
    overlay.press((50, 50));
    overlay.release((52, 51));
    assert_eq!(kind(overlay.key(ENTER, false, false)), "copy");
    let (_, rect) = overlay.crop().unwrap().unwrap();
    assert_eq!(rect, (0, 0, 200, 100));
}

#[test]
fn each_key_asks_for_its_own_end() {
    let mut overlay = Overlay::new(desktop(), (1.0, 0.6, 0.0));
    assert_eq!(kind(overlay.key(0x70, true, false)), "pin");
    assert_eq!(kind(overlay.key(ENTER, false, false)), "copy");
    assert_eq!(kind(overlay.key(0x78, false, false)), "continue");
    assert_eq!(kind(overlay.key(ESCAPE, false, false)), "cancel");
}

#[test]
fn escape_drops_a_shape_in_progress_before_it_closes_the_overlay() {
    let mut overlay = Overlay::new(desktop(), (1.0, 0.6, 0.0));
    assert_eq!(kind(overlay.key(0x72, false, false)), "continue");
    assert_eq!(kind(overlay.key(0x20, false, false)), "continue");
    assert_eq!(kind(overlay.key(ESCAPE, false, false)), "continue");
    assert_eq!(kind(overlay.key(ESCAPE, false, false)), "cancel");
}

#[test]
fn typed_text_goes_into_the_annotation_not_to_the_tool_keys() {
    let mut overlay = Overlay::new(desktop(), (1.0, 0.6, 0.0));
    overlay.key(0x74, false, false);
    overlay.press((20, 50));
    overlay.release((20, 50));
    // 'a' would arm the arrow and Ctrl-less 's' leave the tool, were the
    // keys not going into the text; Escape cancels the text alone.
    for sym in [0x61, 0x73, ESCAPE] {
        assert_eq!(kind(overlay.key(sym, false, false)), "continue");
    }
    assert_eq!(kind(overlay.key(ESCAPE, false, false)), "cancel");
}

#[test]
fn a_view_draws_its_own_part_of_the_desktop_dimmed() {
    let overlay = Overlay::new(desktop(), (1.0, 0.6, 0.0));
    // The right half, as a second monitor at x = 100 shows it.
    let mut out = cairo::ImageSurface::create(cairo::Format::Rgb24, 100, 100).unwrap();
    let cr = cairo::Context::new(&out).unwrap();
    overlay.draw(&cr, (100, 0, 100, 100)).unwrap();
    drop(cr);
    let (r, g, b) = pixel(&mut out, 5, 95);
    assert_eq!((r, g), (0, 0));
    assert!((120..=135).contains(&b), "the backdrop shows dimmed: blue {}", b);
}
