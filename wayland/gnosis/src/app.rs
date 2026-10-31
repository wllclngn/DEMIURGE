// GNOSIS app under Wayland: core's browser in an xdg toplevel, drawn by
// Cairo into wl_shm, its keys read through xkbcommon.
//
// It asks for the compositor's title bar and draws its own, with rounded
// corners, where the compositor leaves the bar to it, as the X11 binary
// does outside a window manager's frame. Its lights minimize and maximize
// through xdg-shell.

use std::path::PathBuf;
use std::time::Instant;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{
    DecorationMode, Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_output, delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1};

use demiurge_core::config::Config;
use gnosis::browser::{self, Browser, Outcome};
use wl_client::Next;
use wl_client::keyboard::Keyboard;

pub fn run(config: &Config, start: PathBuf) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {}", e))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn).map_err(|e| format!("registry: {}", e))?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {}", e))?;
    let xdg = XdgShell::bind(&globals, &qh).map_err(|e| format!("xdg_wm_base: {}", e))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {}", e))?;
    let pool = SlotPool::new(4096, &shm).map_err(|e| format!("shm pool: {}", e))?;

    // Title + app id, so DEMIURGE (and cherrypie matching) can target it.
    let window = xdg.create_window(compositor.create_surface(&qh), WindowDecorations::RequestServer, &qh);
    window.set_title(browser::TITLE);
    window.set_app_id(browser::CLASS);
    window.commit();

    let mut app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seat: SeatState::new(&globals, &qh),
        shm,
        cursor_shape: CursorShapeManager::bind(&globals, &qh).ok(),
        pool,
        window,
        buffer: None,
        browser: Browser::new(config, start),
        keyboard: Keyboard::new(),
        wl_keyboard: None,
        pointer: None,
        shape: None,
        configured: false,
        waiting: false,
        dirty: true,
        maximized: false,
        quit: false,
    };
    let fds = app.browser.wake_fds();
    // Block until the next event, key repeat or thumbnail; idle = zero work.
    wl_client::run(&mut queue, &mut app, &fds, |app, qh, woke| {
        if app.browser.woke(woke[0], woke[1]) {
            app.dirty = true;
        }
        while let Some(key) = app.keyboard.repeat(Instant::now()) {
            let outcome = app.browser.key(key.sym);
            app.outcome(outcome);
        }
        if app.quit {
            return Next::Done;
        }
        app.redraw(qh);
        Next::Wait(app.keyboard.next_repeat())
    })
}

struct App {
    registry: RegistryState,
    outputs: OutputState,
    seat: SeatState,
    shm: Shm,
    cursor_shape: Option<CursorShapeManager>,
    pool: SlotPool,
    window: Window,
    // The buffer last attached, kept until the next replaces it.
    buffer: Option<Buffer>,
    browser: Browser,
    keyboard: Keyboard,
    wl_keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    shape: Option<WpCursorShapeDeviceV1>,
    configured: bool,
    // A frame is on its way to the screen; the next waits for its callback.
    waiting: bool,
    dirty: bool,
    maximized: bool,
    quit: bool,
}

impl App {
    fn outcome(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Quit => self.quit = true,
            Outcome::Dirty => self.dirty = true,
            Outcome::Minimize => self.window.set_minimized(),
            Outcome::Zoom if self.maximized => self.window.unset_maximized(),
            Outcome::Zoom => self.window.set_maximized(),
            Outcome::Idle => {}
        }
    }

    // The window drawn again if it changed, unless its last frame is still
    // on its way. Its corners are cut while it draws its own title bar.
    fn redraw(&mut self, qh: &QueueHandle<Self>) {
        if !self.configured || !self.dirty || self.waiting {
            return;
        }
        let surface = self.window.wl_surface().clone();
        let (w, h) = self.browser.size();
        let rounded = !self.browser.framed();
        surface.frame(qh, surface.clone());
        let browser = &mut self.browser;
        let drawn = wl_client::shm::present(&mut self.pool, &surface, w as i32, h as i32, rounded, |cr| {
            if rounded {
                browser::rounded_rect(cr, 0.0, 0.0, w as f64, h as f64, browser::CORNER_R as f64);
                cr.clip();
            }
            browser.draw(cr)
        });
        match drawn {
            Ok(buffer) => {
                self.buffer = Some(buffer);
                self.waiting = true;
                self.dirty = false;
            }
            Err(e) => {
                eprintln!("[gnosis] draw: {}", e);
                self.quit = true;
            }
        }
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.quit = true;
    }

    // The size the compositor gives, else GNOSIS's own; the title bar the
    // compositor draws or leaves to GNOSIS; focus and maximize.
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        let (w, h) = self.browser.size();
        let w = configure.new_size.0.map_or(w, |v| v.get().min(u16::MAX as u32) as u16);
        let h = configure.new_size.1.map_or(h, |v| v.get().min(u16::MAX as u32) as u16);
        self.browser.resize(w, h);
        self.browser.set_framed(configure.decoration_mode == DecorationMode::Server);
        self.browser.set_focused(configure.is_activated());
        self.maximized = configure.is_maximized();
        self.configured = true;
        self.dirty = true;
    }
}

impl CompositorHandler for App {
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {
        self.waiting = false;
    }
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: wl_output::Transform) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlPointer, events: &[PointerEvent]) {
        for event in events {
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(shape) = &self.shape {
                        shape.set_shape(serial, Shape::Default);
                    }
                }
                PointerEventKind::Press { .. } => {
                    let outcome = self.browser.click(event.position.0, event.position.1);
                    self.outcome(outcome);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(app: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let Some(key) = app.keyboard.event(event) {
            let outcome = app.browser.key(key.sym);
            app.outcome(outcome);
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
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_registry!(App);
