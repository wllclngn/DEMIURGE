// gordian_knot --login, either stack's: The greetd greeter drawn inside
// cage, greetd's kiosk compositor, whatever session it then starts. The
// lock's SYSTEM and USER panels fill a fullscreen window, its keys come
// through xkbcommon and map as the locks' do, and Login carries the
// conversation with greetd, as the TTY greeter's does. Without a Wayland
// display it is the TTY greeter.

use std::time::Instant;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_output, delegate_registry, delegate_seat, delegate_shm, delegate_xdg_shell,
    delegate_xdg_window, registry_handlers,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle};

use demiurge_core::torrentius::PanelRow;
use wl_client::Next;
use wl_client::keyboard::Keyboard;

use crate::greetd::Client;
use crate::keyinput::{self, KeyAction};
use crate::login::{self, Ask, Login, LoginConfig};
use crate::screen::{self, Theme};
use crate::secret::Secret;
use crate::sysinfo;

/// Greet through greetd: 0 once the session started, non-zero on a fatal
/// error.
pub fn run(cfg: LoginConfig, theme: Theme) -> i32 {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return login::run(cfg);
    }
    let shown = Client::connect_default()
        .map_err(|e| format!("greetd connect: {}", e))
        .and_then(|client| show(Login::new(client, cfg), theme));
    match shown {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("[gordian_knot --login] {}", e);
            1
        }
    }
}

struct App {
    registry: RegistryState,
    outputs: OutputState,
    seat: SeatState,
    shm: Shm,
    pool: SlotPool,
    window: Window,
    // The buffer last attached, kept until the next replaces it.
    buffer: Option<Buffer>,
    size: Option<(u32, u32)>,
    keyboard: Keyboard,
    wl_keyboard: Option<WlKeyboard>,
    theme: Theme,
    login: Login,
    // What is typed for what is asked: The username, or greetd's prompt.
    field: Secret,
    // The SYSTEM panel's time as last drawn.
    clock: String,
    // A frame is on its way to the screen; the next waits for its callback.
    waiting: bool,
    dirty: bool,
    // Set once the session started, or with greetd's fatal error.
    done: Option<Result<(), String>>,
}

fn show(login: Login, theme: Theme) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {}", e))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn).map_err(|e| format!("registry: {}", e))?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {}", e))?;
    let xdg = XdgShell::bind(&globals, &qh).map_err(|e| format!("xdg_wm_base: {}", e))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {}", e))?;
    let pool = SlotPool::new(4096, &shm).map_err(|e| format!("shm pool: {}", e))?;

    let window = xdg.create_window(compositor.create_surface(&qh), WindowDecorations::None, &qh);
    window.set_title("GORDIAN KNOT");
    window.set_app_id("gordian_knot");
    window.set_fullscreen(None);
    window.commit();

    let mut app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seat: SeatState::new(&globals, &qh),
        shm,
        pool,
        window,
        buffer: None,
        size: None,
        keyboard: Keyboard::new(),
        wl_keyboard: None,
        theme,
        login,
        field: Secret::new().map_err(|e| format!("input buffer: {}", e))?,
        clock: String::new(),
        waiting: false,
        dirty: true,
        done: None,
    };
    wl_client::run(&mut queue, &mut app, &[], |app, qh, _| {
        while let Some(key) = app.keyboard.repeat(Instant::now()) {
            app.key(key);
        }
        if app.done.is_some() {
            return Next::Done;
        }
        let time = sysinfo::current_time();
        if time != app.clock {
            app.clock = time;
            app.dirty = true;
        }
        app.redraw(qh);
        // The SYSTEM panel's clock turns over on the second.
        let tick = Instant::now() + demiurge_core::clock::until_next_second();
        Next::Wait(Some(app.keyboard.next_repeat().map_or(tick, |at| at.min(tick))))
    })?;
    app.done.unwrap_or(Ok(()))
}

impl App {
    fn key(&mut self, key: wl_client::keyboard::Key) {
        match keyinput::action(key.sym, &key.utf8, key.ctrl) {
            KeyAction::Submit => {
                let typed = String::from_utf8_lossy(self.field.bytes()).into_owned();
                self.field.clear();
                if let Err(e) = self.login.answer(typed) {
                    self.done = Some(Err(e));
                    return;
                }
                if self.login.ask == Ask::Started {
                    self.done = Some(Ok(()));
                    return;
                }
                if self.login.ask == Ask::Username && !self.login.status.is_empty() {
                    eprintln!("[gordian_knot --login] {}", self.login.status);
                }
            }
            KeyAction::Backspace => self.field.pop_char(),
            KeyAction::Clear => self.field.clear(),
            KeyAction::Append(s) => {
                if !self.field.push(s.as_bytes()) {
                    self.login.status = "Too long.".into();
                }
            }
            KeyAction::Ignore => return,
        }
        self.dirty = true;
    }

    // The screen drawn again for a key or the clock, unless its last frame
    // is still on its way.
    fn redraw(&mut self, qh: &QueueHandle<Self>) {
        let Some((w, h)) = self.size else {
            return;
        };
        if self.waiting || !self.dirty {
            return;
        }
        // The rows: The username, then what greetd asks with what is typed,
        // masked for a secret, then the status.
        let typed = String::from_utf8_lossy(self.field.bytes()).into_owned();
        let dots: String = std::iter::repeat_n('\u{25cf}', self.field.char_count()).collect();
        let (user, label, value) = match &self.login.ask {
            Ask::Username => (format!("{}_", typed), self.theme.prompt.clone(), String::new()),
            Ask::Prompt { text, secret } => {
                let label = text.trim().trim_end_matches(':').to_uppercase();
                (self.login.username.clone(), label, if *secret { dots } else { typed })
            }
            Ask::Started => return,
        };
        let rows = [
            PanelRow { label: "USER", value: &user },
            PanelRow { label: &label, value: &value },
            PanelRow { label: "STATUS", value: &self.login.status },
        ];
        let surface = self.window.wl_surface().clone();
        surface.frame(qh, surface.clone());
        let theme = &self.theme;
        match wl_client::shm::present(&mut self.pool, &surface, w as i32, h as i32, false, |cr| {
            screen::draw(cr, w, h, theme, &rows)
        }) {
            Ok(buffer) => {
                self.buffer = Some(buffer);
                self.waiting = true;
                self.dirty = false;
            }
            Err(e) => self.done = Some(Err(format!("draw: {}", e))),
        }
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {}

    // cage gives the whole screen; anything else, a sensible window.
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        let w = configure.new_size.0.map_or(1024, |v| v.get());
        let h = configure.new_size.1.map_or(768, |v| v.get());
        self.size = Some((w, h));
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
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_registry!(App);
