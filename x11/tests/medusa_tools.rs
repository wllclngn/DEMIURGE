// MEDUSA tools: The palette, tool classes and the pixelating blur.

use demiurge_x11::medusa::tools::*;

#[test]
fn palette_first_is_accent() {
    let accent = (0.1, 0.2, 0.3);
    assert_eq!(palette(accent)[0], accent);
}

#[test]
fn tool_classification() {
    assert!(ToolKind::Arrow.is_two_point());
    assert!(ToolKind::Blur.is_two_point());
    assert!(ToolKind::Marker.is_path());
    assert!(!ToolKind::Text.is_two_point());
    assert!(!ToolKind::Text.is_path());
}

// apply_blur pixelates in 12 px cells: Each cell takes its average color.
// A cell half black and half white turns uniformly mid-gray; an all-white
// cell stays white.
#[test]
fn blur_averages_each_cell() {
    let mut s = cairo::ImageSurface::create(cairo::Format::Rgb24, 24, 12).unwrap();
    {
        let cr = cairo::Context::new(&s).unwrap();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().unwrap();
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.rectangle(0.0, 0.0, 6.0, 12.0);
        cr.fill().unwrap();
    }
    s.flush();
    apply_blur(&mut s, 0, 0, 24, 12);
    let stride = s.stride() as usize;
    let data = s.data().unwrap();
    let px = |x: usize, y: usize| {
        let o = y * stride + x * 4;
        (data[o + 2], data[o + 1], data[o])
    };
    assert_eq!(px(0, 0), (127, 127, 127));
    assert_eq!(px(11, 11), (127, 127, 127));
    assert_eq!(px(5, 6), px(6, 6), "the cell is not uniform");
    assert_eq!(px(12, 0), (255, 255, 255));
    assert_eq!(px(23, 11), (255, 255, 255));
}
