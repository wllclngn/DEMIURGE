// The pointer's image when no client draws it: The named cursor from the
// [cursor] theme, the size nearest [cursor] size, loaded once per name.
// A name the theme lacks (or no theme at all, as on a bare install) gets
// a plain arrow drawn here, so the pointer is never invisible.

use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::pointer::CursorIcon;
use smithay::utils::{Physical, Point, Transform};
use xcursor::CursorTheme;
use xcursor::parser::{Image, parse_xcursor};

pub struct Cursor {
    theme: CursorTheme,
    size: u32,
    // Each name's image and its hotspot, once loaded.
    loaded: HashMap<CursorIcon, (MemoryRenderBuffer, Point<i32, Physical>)>,
}

impl Cursor {
    pub fn new(theme: &str, size: u32) -> Self {
        Self { theme: CursorTheme::load(theme), size: size.max(8), loaded: HashMap::new() }
    }

    // The named cursor as an element, its hotspot at `at`.
    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        icon: CursorIcon,
        at: Point<f64, Physical>,
    ) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
        let (theme, size) = (&self.theme, self.size);
        let (buffer, hotspot) = self.loaded.entry(icon).or_insert_with(|| load(theme, icon, size));
        MemoryRenderBufferRenderElement::from_buffer(renderer, at - hotspot.to_f64(), buffer, None, None, None, Kind::Cursor)
            .map_err(|e| tracing::warn!("cursor upload: {}", e))
            .ok()
    }
}

fn load(theme: &CursorTheme, icon: CursorIcon, size: u32) -> (MemoryRenderBuffer, Point<i32, Physical>) {
    let image = std::iter::once(icon.name())
        .chain(icon.alt_names().iter().copied())
        .filter_map(|name| theme.load_icon(name))
        .find_map(|path| nearest(parse_xcursor(&std::fs::read(path).ok()?)?, size));
    let image = image.unwrap_or_else(|| arrow(size));
    let buffer = MemoryRenderBuffer::from_slice(
        &image.pixels_rgba,
        Fourcc::Abgr8888,
        (image.width as i32, image.height as i32),
        1,
        Transform::Normal,
        None,
    );
    (buffer, Point::from((image.xhot as i32, image.yhot as i32)))
}

// The first frame of the size nearest `size`.
fn nearest(images: Vec<Image>, size: u32) -> Option<Image> {
    let best = images.iter().map(|i| i.size.abs_diff(size)).min()?;
    images.into_iter().find(|i| i.size.abs_diff(size) == best)
}

// A plain arrow, `size` tall: White with a black edge, premultiplied RGBA,
// its hotspot at the tip.
fn arrow(size: u32) -> Image {
    let (w, h) = (size * 2 / 3, size);
    let inside = |x: i64, y: i64| x >= 0 && y >= 0 && x * 3 <= y * 2 && x + y * 2 / 5 < h as i64 * 3 / 4 && y < h as i64;
    let mut pixels_rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let px: [u8; 4] = if !inside(x, y) {
                [0, 0, 0, 0]
            } else if [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().all(|(dx, dy)| inside(x + dx, y + dy)) {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 255]
            };
            pixels_rgba.extend_from_slice(&px);
        }
    }
    Image {
        size,
        width: w,
        height: h,
        xhot: 0,
        yhot: 0,
        delay: 0,
        pixels_argb: Vec::new(),
        pixels_rgba,
    }
}
