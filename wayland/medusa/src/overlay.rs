// MEDUSA's overlay under Wayland: core's overlay over the compositor's
// frozen frames, as a surface on the overlay layer of every monitor holding
// the keyboard, with a crosshair through cursor-shape. Each surface draws
// its monitor's part of the desktop. What a key asks for (save, copy, pin)
// is done once the surfaces are gone. The pin is a surface on the same
// layer at the region's place, holding the keyboard until any key or click
// dismisses it, as X11's override-redirect pin does.

use std::time::Instant;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_shm, registry_handlers,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1};

use medusa::overlay::{self, GuiOpts, Key, Overlay};
use wl_client::Next;
use wl_client::keyboard::Keyboard;

// The pointer's left button, as evdev numbers it.
const BTN_LEFT: u32 = 0x110;

pub fn run(opts: &GuiOpts) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {}", e))?;
    // Freeze first, before any surface of MEDUSA's shows.
    let (shot, origin) = wl_client::capture::desktop(&conn)?;
    let views = |outputs: &OutputState| {
        outputs
            .outputs()
            .filter_map(|output| {
                let (place, (w, h)) = monitor(outputs, &output)?;
                let rect = (place.0 - origin.0, place.1 - origin.1, w, h);
                Some(Spec { output, rect, anchor: Anchor::all(), margin: (0, 0), size: (0, 0) })
            })
            .collect()
    };
    let (asked, shown) = show(&conn, Shown::Overlay(Overlay::new(shot, opts.accent)), views)?;
    drop(conn);
    let Shown::Overlay(overlay) = shown else {
        unreachable!("the overlay comes back as it went");
    };

    match asked {
        Key::Save { copy } => {
            if let Some((crop, _)) = overlay.crop()? {
                let path = overlay::save(&crop, &opts.save_dir)?;
                if copy && opts.copy {
                    crate::clipboard::copy_to_clipboard(&path);
                }
            }
        }
        Key::Pin => {
            // A pin is never the only record: The crop is saved too.
            if let Some((crop, (x, y, _, _))) = overlay.crop()? {
                let _ = overlay::save(&crop, &opts.save_dir);
                pin(crop, (x + origin.0, y + origin.1));
            }
        }
        Key::Cancel | Key::Continue => {}
    }
    Ok(())
}

// Pin the annotated crop at `at`, in the compositor's space. Forks a
// detached holder so MEDUSA can exit; the pin stays put until any key or
// click dismisses it.
fn pin(crop: cairo::ImageSurface, at: (i32, i32)) {
    let pid = unsafe { libc::fork() };
    if pid != 0 {
        // Parent (or fork failure): MEDUSA exits; the pin runs in the
        // child if the fork succeeded.
        return;
    }
    unsafe {
        libc::setsid();
    }
    let _ = pin_window(crop, at);
    unsafe {
        libc::_exit(0);
    }
}

fn pin_window(crop: cairo::ImageSurface, at: (i32, i32)) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {}", e))?;
    // The crop with a 1 px white border around it, on the monitor showing
    // its top-left corner.
    let (w, h) = (crop.width() + 2, crop.height() + 2);
    let view = |outputs: &OutputState| {
        let found = outputs.outputs().filter_map(|output| Some((monitor(outputs, &output)?, output))).find(
            |(((x, y), (mw, mh)), _)| (*x..x + mw).contains(&at.0) && (*y..y + mh).contains(&at.1),
        );
        let Some(((place, _), output)) = found.or_else(|| {
            let output = outputs.outputs().next()?;
            Some((monitor(outputs, &output)?, output))
        }) else {
            return Vec::new();
        };
        let margin = (at.1 - 1 - place.1, at.0 - 1 - place.0);
        vec![Spec { output, rect: (0, 0, w, h), anchor: Anchor::TOP | Anchor::LEFT, margin, size: (w as u32, h as u32) }]
    };
    show(&conn, Shown::Pin(crop), view).map(|_| ())
}

// A monitor's place in the compositor's space and its size.
fn monitor(outputs: &OutputState, output: &WlOutput) -> Option<((i32, i32), (i32, i32))> {
    let info = outputs.info(output)?;
    let place = info.logical_position.unwrap_or(info.location);
    let size = info.logical_size.or_else(|| info.modes.iter().find(|m| m.current).map(|m| m.dimensions))?;
    Some((place, size))
}

// What the surfaces show.
enum Shown {
    Overlay(Overlay),
    Pin(cairo::ImageSurface),
}

// A surface to show on `output`: The desktop rectangle it draws, where it
// sits (its anchors, top and left margins and size, 0 for the compositor's).
struct Spec {
    output: WlOutput,
    rect: (i32, i32, i32, i32),
    anchor: Anchor,
    margin: (i32, i32),
    size: (u32, u32),
}

// One surface: Its rectangle, and its frame in flight.
struct View {
    layer: LayerSurface,
    rect: (i32, i32, i32, i32),
    configured: bool,
    // The buffer last attached, kept until the next replaces it.
    buffer: Option<Buffer>,
    // A frame is on its way to the screen; the next waits for its callback.
    waiting: bool,
    dirty: bool,
}

struct App {
    registry: RegistryState,
    outputs: OutputState,
    compositor: CompositorState,
    seat: SeatState,
    shm: Shm,
    layer_shell: LayerShell,
    cursor_shape: Option<CursorShapeManager>,
    pool: SlotPool,
    shown: Shown,
    views: Vec<View>,
    keyboard: Keyboard,
    wl_keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    shape: Option<WpCursorShapeDeviceV1>,
    asked: Option<Key>,
}

// Show `shown` on the surfaces `specs` lays out on the monitors, holding the
// keyboard, until a key or click asks to close them: What it asked, and
// what was shown.
fn show(conn: &Connection, shown: Shown, specs: impl FnOnce(&OutputState) -> Vec<Spec>) -> Result<(Key, Shown), String> {
    let (globals, mut queue) = registry_queue_init::<App>(conn).map_err(|e| format!("registry: {}", e))?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {}", e))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| "the compositor offers no wlr-layer-shell")?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {}", e))?;
    let pool = SlotPool::new(4096, &shm).map_err(|e| format!("shm pool: {}", e))?;
    let mut app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        compositor,
        seat: SeatState::new(&globals, &qh),
        shm,
        layer_shell,
        cursor_shape: CursorShapeManager::bind(&globals, &qh).ok(),
        pool,
        shown,
        views: Vec::new(),
        keyboard: Keyboard::new(),
        wl_keyboard: None,
        pointer: None,
        shape: None,
        asked: None,
    };
    // Two round trips: The outputs and the seat, then their details.
    for _ in 0..2 {
        queue.roundtrip(&mut app).map_err(|e| format!("roundtrip: {}", e))?;
    }

    for spec in specs(&app.outputs) {
        let surface = app.compositor.create_surface(&qh);
        let layer = app.layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("medusa"), Some(&spec.output));
        layer.set_anchor(spec.anchor);
        layer.set_margin(spec.margin.0, 0, 0, spec.margin.1);
        layer.set_size(spec.size.0, spec.size.1);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        app.views.push(View { layer, rect: spec.rect, configured: false, buffer: None, waiting: false, dirty: true });
    }
    if app.views.is_empty() {
        return Err("no monitor to show MEDUSA on".into());
    }

    wl_client::run(&mut queue, &mut app, &[], |app, qh, _| {
        while let Some(key) = app.keyboard.repeat(Instant::now()) {
            app.key(key);
        }
        if app.asked.is_some() {
            return Next::Done;
        }
        app.redraw(qh);
        Next::Wait(app.keyboard.next_repeat())
    })?;
    let App { asked, shown, .. } = app;
    Ok((asked.unwrap_or(Key::Cancel), shown))
}

impl App {
    fn key(&mut self, key: wl_client::keyboard::Key) {
        let asked = match &mut self.shown {
            Shown::Overlay(overlay) => overlay.key(key.sym, key.ctrl, key.shift),
            // Any key dismisses a pin.
            Shown::Pin(_) => Key::Cancel,
        };
        match asked {
            Key::Continue => self.views.iter_mut().for_each(|v| v.dirty = true),
            asked => self.asked = Some(asked),
        }
    }

    // A pointer event's place on the desktop, from the surface it came on.
    fn place(&self, event: &PointerEvent) -> Option<(i32, i32)> {
        let view = self.views.iter().find(|v| v.layer.wl_surface() == &event.surface)?;
        Some((view.rect.0 + event.position.0 as i32, view.rect.1 + event.position.1 as i32))
    }

    // Each surface whose picture changed, drawn again unless its last frame
    // is still on its way.
    fn redraw(&mut self, qh: &QueueHandle<Self>) {
        for i in 0..self.views.len() {
            let view = &self.views[i];
            if !view.configured || !view.dirty || view.waiting {
                continue;
            }
            let (rect, surface) = (view.rect, view.layer.wl_surface().clone());
            surface.frame(qh, surface.clone());
            let shown = &self.shown;
            let drawn = wl_client::shm::present(&mut self.pool, &surface, rect.2, rect.3, false, |cr| match shown {
                Shown::Overlay(overlay) => overlay.draw(cr, rect),
                Shown::Pin(crop) => {
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.paint().map_err(|e| e.to_string())?;
                    cr.set_source_surface(crop, 1.0, 1.0).map_err(|e| e.to_string())?;
                    cr.paint().map_err(|e| e.to_string())
                }
            });
            match drawn {
                Ok(buffer) => {
                    let view = &mut self.views[i];
                    view.buffer = Some(buffer);
                    view.waiting = true;
                    view.dirty = false;
                }
                Err(e) => {
                    eprintln!("[medusa] draw: {}", e);
                    self.asked = Some(Key::Cancel);
                }
            }
        }
    }
}

impl LayerShellHandler for App {
    // The compositor took a surface away, as with its monitor: With none
    // left, MEDUSA closes.
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.views.retain(|v| &v.layer != layer);
        if self.views.is_empty() {
            self.asked = Some(Key::Cancel);
        }
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        if let Some(view) = self.views.iter_mut().find(|v| &v.layer == layer) {
            let (w, h) = configure.new_size;
            if w > 0 && h > 0 {
                (view.rect.2, view.rect.3) = (w as i32, h as i32);
            }
            view.configured = true;
            view.dirty = true;
        }
    }
}

impl CompositorHandler for App {
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        if let Some(view) = self.views.iter_mut().find(|v| v.layer.wl_surface() == surface) {
            view.waiting = false;
        }
    }
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: wl_output::Transform) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlPointer, events: &[PointerEvent]) {
        for event in events {
            let Some(at) = self.place(event) else {
                continue;
            };
            let Shown::Overlay(overlay) = &mut self.shown else {
                // A click dismisses a pin.
                if matches!(event.kind, PointerEventKind::Press { .. }) {
                    self.asked = Some(Key::Cancel);
                }
                continue;
            };
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(shape) = &self.shape {
                        shape.set_shape(serial, Shape::Crosshair);
                    }
                    overlay.motion(at);
                }
                PointerEventKind::Motion { .. } => overlay.motion(at),
                PointerEventKind::Press { button: BTN_LEFT, .. } => overlay.press(at),
                PointerEventKind::Release { button: BTN_LEFT, .. } => overlay.release(at),
                _ => continue,
            }
            self.views.iter_mut().for_each(|v| v.dirty = true);
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(app: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let Some(key) = app.keyboard.event(event) {
            app.key(key);
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat, capability: Capability) {
        if capability == Capability::Keyboard && self.wl_keyboard.is_none() {
            self.wl_keyboard = Some(seat.get_keyboard(qh, ()));
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            if let Ok(pointer) = self.seat.get_pointer(qh, &seat) {
                self.shape = self.cursor_shape.as_ref().map(|m| m.get_shape_device(&pointer, qh));
                self.pointer = Some(pointer);
            }
        }
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_registry!(App);
