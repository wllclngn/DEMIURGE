// Wayland-side locker. Sister to x11_lock.rs; same UI, different
// transport.
//
// Flow:
//   1. Connect to $WAYLAND_DISPLAY, bind required globals.
//   2. Issue ext_session_lock_manager_v1.lock(). Wait for `locked`.
//   3. For each wl_output, create a lock surface, configure-ack
//      cycle, allocate a wl_shm buffer sized to the output, render
//      the SYSTEM + USER panels into it via torrentius.
//   4. Listen for keyboard events. xkbcommon decodes keycodes to
//      keysyms; we drive the same keypress state machine the X11
//      locker uses (printable -> append, BS -> pop, Ctrl+U -> clear,
//      Enter -> PAM).
//   5. On PAM success: lock.unlock_and_destroy(), exit 0.
//   6. On PAM fail: clear buffer, redraw with error, continue.
//
// The compositor side (demiurge-wl, see wayland/src/wm.rs) hides all
// non-locker surfaces while the lock is active. This module is the
// matching client.

use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::io::OwnedFd;
use std::time::{Duration, Instant};

use memmap2::MmapMut;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, KeyState, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_surface_v1::{
    self, ExtSessionLockSurfaceV1,
};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_v1::{
    self, ExtSessionLockV1,
};
use xkbcommon::xkb;

use crate::config::GordianKnot as Cfg;
use crate::gordian_knot::keyinput::{self, KeyAction};
use crate::gordian_knot::pam::{self, AuthError};
use crate::gordian_knot::secret::Secret;
use crate::gordian_knot::sysinfo;
use crate::torrentius::{self, PanelColors, PanelRow};

const FRAME_BUDGET: Duration = Duration::from_millis(50);

pub fn run(cfg: &Cfg, user: &str) -> Result<(), String> {
    let conn = Connection::connect_to_env()
        .map_err(|e| format!("connect WAYLAND_DISPLAY: {}", e))?;
    let (globals, mut queue): (_, EventQueue<State>) =
        registry_queue_init(&conn).map_err(|e| format!("registry_queue_init: {}", e))?;
    let qh = queue.handle();

    // Bind required globals. Versions match what demiurge-wl
    // advertises (wl_compositor v6, wl_shm v1, wl_seat v7,
    // wl_output v4, ext_session_lock_manager_v1 v1).
    let compositor: WlCompositor =
        globals.bind(&qh, 1..=6, ()).map_err(|e| format!("bind wl_compositor: {}", e))?;
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| format!("bind wl_shm: {}", e))?;
    let lock_mgr: ExtSessionLockManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("bind ext_session_lock_manager_v1: {}", e))?;

    // GordianKnot config doesn't implement Clone (the toml-loaded
    // structs in demiurge-core skip it). Pre-extract the few fields
    // the locker UI uses into owned strings so State doesn't need
    // to borrow from `cfg` for its lifetime.
    let theme = Theme {
        bg: cfg.bg.clone(),
        fg: cfg.fg.clone(),
        accent: cfg.accent.clone(),
        font: cfg.font.clone(),
        prompt: cfg.prompt.clone(),
    };

    let mut state = State {
        compositor: compositor.clone(),
        shm: shm.clone(),
        theme,
        user: user.to_string(),
        seats: Vec::new(),
        outputs: Vec::new(),
        lock: None,
        lock_surfaces: Vec::new(),
        locked_received: false,
        finished: false,
        authenticated: false,
        keyboard: None,
        xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
        xkb_keymap: None,
        xkb_state: None,
        password: Secret::new().map_err(|e| format!("password buffer: {}", e))?,
        message: String::new(),
        last_clock_at: Instant::now(),
        dirty: true,
    };

    for g in globals.contents().clone_list() {
        match g.interface.as_str() {
            "wl_seat" => {
                let seat: WlSeat = globals
                    .registry()
                    .bind::<WlSeat, _, _>(g.name, g.version.min(7), &qh, ());
                state.seats.push(seat);
            }
            "wl_output" => {
                let output: WlOutput = globals
                    .registry()
                    .bind::<WlOutput, _, _>(g.name, g.version.min(4), &qh, ());
                state.outputs.push(OutputBinding { output });
            }
            _ => {}
        }
    }

    // Issue the lock request and roundtrip so the compositor
    // dispatches it before we proceed.
    let lock = lock_mgr.lock(&qh, ());
    state.lock = Some(lock);
    queue.roundtrip(&mut state).map_err(|e| format!("roundtrip after lock: {}", e))?;

    // Drive the event loop until `locked` arrives or `finished`
    // tells us the compositor refused.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !state.locked_received && !state.finished {
        if Instant::now() >= deadline {
            return Err("timed out waiting for `locked` event".into());
        }
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| format!("dispatch waiting for locked: {}", e))?;
    }
    if state.finished {
        return Err("compositor refused lock acquisition".into());
    }

    // Create one lock surface per output.
    for ob in &state.outputs {
        let wl_surface = state.compositor.create_surface(&qh, ());
        let lock_surface = state
            .lock
            .as_ref()
            .expect("lock acquired")
            .get_lock_surface(&wl_surface, &ob.output, &qh, ());
        state.lock_surfaces.push(LockSurface {
            wl_surface,
            lock_surface,
            output: ob.output.clone(),
            configured_size: None,
            last_serial: None,
            buffer: None,
            mmap: None,
        });
    }

    // Main event loop. Dispatches Wayland events with a short
    // timeout so the clock can tick and PAM responses can be sent
    // promptly.
    while !state.authenticated && !state.finished {
        // Only on change (a key, a configure, the 1 Hz tick): Each redraw
        // allocates a fresh memfd + shm buffer per output.
        if state.dirty {
            for ls_idx in 0..state.lock_surfaces.len() {
                redraw_surface(&mut state, ls_idx, &qh)?;
            }
            state.dirty = false;
        }

        if state.last_clock_at.elapsed() >= Duration::from_secs(1) {
            state.last_clock_at = Instant::now();
            state.dirty = true;
        }

        let _ = queue.flush();
        match queue.dispatch_pending(&mut state) {
            Ok(_) => {}
            Err(e) => return Err(format!("dispatch_pending: {}", e)),
        }
        let timeout = FRAME_BUDGET.min(
            Duration::from_secs(1)
                .saturating_sub(state.last_clock_at.elapsed()),
        );
        if let Some(read_guard) = conn.prepare_read() {
            let fd = read_guard.connection_fd();
            let raw_fd = fd.as_raw_fd();
            let mut pfd = libc::pollfd {
                fd: raw_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let to_ms = timeout.as_millis() as i32;
            let n = unsafe { libc::poll(&mut pfd, 1, to_ms) };
            if n > 0 && pfd.revents & libc::POLLIN != 0 {
                let _ = read_guard.read();
            }
            // n == 0 (timeout): fall through to the next iter.
        }
    }

    if state.authenticated {
        if let Some(lock) = state.lock.take() {
            lock.unlock_and_destroy();
        }
        let _ = queue.roundtrip(&mut state);
        Ok(())
    } else {
        Err("locker exited without authenticating".into())
    }
}

struct State {
    compositor: WlCompositor,
    shm: WlShm,
    theme: Theme,
    user: String,
    seats: Vec<WlSeat>,
    outputs: Vec<OutputBinding>,
    lock: Option<ExtSessionLockV1>,
    lock_surfaces: Vec<LockSurface>,
    locked_received: bool,
    finished: bool,
    authenticated: bool,
    keyboard: Option<WlKeyboard>,
    xkb_context: xkb::Context,
    xkb_keymap: Option<xkb::Keymap>,
    xkb_state: Option<xkb::State>,
    password: Secret,
    message: String,
    last_clock_at: Instant,
    dirty: bool,
}

struct Theme {
    bg: String,
    fg: String,
    accent: String,
    font: String,
    prompt: String,
}

struct OutputBinding {
    // Held to keep the wl_output proxy alive across the lock
    // session; we never query it directly post-creation, but
    // dropping the proxy would release the server-side binding.
    #[allow(dead_code)]
    output: WlOutput,
}

struct LockSurface {
    wl_surface: WlSurface,
    lock_surface: ExtSessionLockSurfaceV1,
    // Held for proxy lifetime, like OutputBinding::output above.
    #[allow(dead_code)]
    output: WlOutput,
    configured_size: Option<(u32, u32)>,
    last_serial: Option<u32>,
    buffer: Option<WlBuffer>,
    // Held for the lifetime of the wl_buffer that's pointing into
    // it. The compositor reads the pixels asynchronously after our
    // commit; freeing the mapping early would crash it.
    #[allow(dead_code)]
    mmap: Option<MmapMut>,
}

// ---------- Dispatch impls ----------

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Hot-add of seats / outputs after lock acquisition is rare
        // and complex; we bind the existing set at startup and
        // accept that new ones arriving mid-lock won't be covered.
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: <WlSeat as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if let WEnum::Value(c) = capabilities {
                if c.contains(Capability::Keyboard) && state.keyboard.is_none() {
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                }
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _kb: &WlKeyboard,
        event: <WlKeyboard as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if let WEnum::Value(KeymapFormat::XkbV1) = format {
                    let mmap = unsafe {
                        memmap2::MmapOptions::new()
                            .len(size as usize)
                            .map_copy_read_only(&fd)
                    };
                    if let Ok(map) = mmap {
                        let bytes = &map[..];
                        // Keymap blob may contain a trailing NUL.
                        let s = std::str::from_utf8(bytes)
                            .unwrap_or("")
                            .trim_end_matches('\0');
                        if let Some(km) = xkb::Keymap::new_from_string(
                            &state.xkb_context,
                            s.to_string(),
                            xkb::KEYMAP_FORMAT_TEXT_V1,
                            xkb::KEYMAP_COMPILE_NO_FLAGS,
                        ) {
                            let xstate = xkb::State::new(&km);
                            state.xkb_keymap = Some(km);
                            state.xkb_state = Some(xstate);
                        }
                    }
                }
                let _ = fd;
            }
            wl_keyboard::Event::Key { state: keystate, key, .. } => {
                if !matches!(keystate, WEnum::Value(KeyState::Pressed)) {
                    return;
                }
                // Decode keysym + utf8 + ctrl-mod from xkb state,
                // drop the borrow, then mutate state. Splits the
                // immutable read from the mutable write so the
                // borrow checker stays happy without cloning xkb's
                // (non-Clone) State.
                let decoded = state.xkb_state.as_ref().map(|xs| {
                    let keycode: xkb::Keycode = (key + 8).into();
                    let keysym = xs.key_get_one_sym(keycode);
                    let utf8 = xs.key_get_utf8(keycode);
                    let ctrl = xs.mod_name_is_active(
                        xkb::MOD_NAME_CTRL,
                        xkb::STATE_MODS_EFFECTIVE,
                    );
                    (keysym, utf8, ctrl)
                });
                if let Some((keysym, utf8, ctrl)) = decoded {
                    handle_keysym(state, keysym, &utf8, ctrl);
                }
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(xs) = state.xkb_state.as_mut() {
                    xs.update_mask(
                        mods_depressed,
                        mods_latched,
                        mods_locked,
                        0,
                        0,
                        group,
                    );
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockManagerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _mgr: &ExtSessionLockManagerV1,
        _event: <ExtSessionLockManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtSessionLockV1, ()> for State {
    fn event(
        state: &mut Self,
        _lock: &ExtSessionLockV1,
        event: <ExtSessionLockV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.locked_received = true,
            ext_session_lock_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        ls: &ExtSessionLockSurfaceV1,
        event: <ExtSessionLockSurfaceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            ls.ack_configure(serial);
            if let Some(slot) = state
                .lock_surfaces
                .iter_mut()
                .find(|s| &s.lock_surface == ls)
            {
                slot.configured_size = Some((width, height));
                slot.last_serial = Some(serial);
            }
            state.dirty = true;
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlOutput);

// ---------- Keysym dispatch ----------

fn handle_keysym(state: &mut State, keysym: xkb::Keysym, utf8: &str, ctrl: bool) {
    match keyinput::action(keysym.into(), utf8, ctrl) {
        KeyAction::Submit if state.password.is_empty() => {
            state.message = "Type your password and press Enter.".into();
        }
        KeyAction::Submit => {
            let attempt = pam::authenticate_forked(&state.user, &mut state.password);
            state.password.clear();
            match attempt.result {
                Ok(()) => state.authenticated = true,
                Err(e) => {
                    state.message = attempt.messages.last().cloned().unwrap_or_else(|| match e {
                        AuthError::WrongCredentials | AuthError::UserUnknown => "Authentication failed.".into(),
                        other => format!("PAM: {}", other),
                    });
                }
            }
        }
        KeyAction::Backspace => state.password.pop_char(),
        KeyAction::Clear => state.password.clear(),
        KeyAction::Append(s) => {
            if !state.password.push(s.as_bytes()) {
                state.message = "Password too long.".into();
            }
        }
        KeyAction::Ignore => return,
    }
    state.dirty = true;
}

// ---------- Rendering ----------

fn redraw_surface(state: &mut State, idx: usize, qh: &QueueHandle<State>) -> Result<(), String> {
    let Some((w, h)) = state.lock_surfaces[idx].configured_size else {
        return Ok(());
    };
    if w == 0 || h == 0 {
        return Ok(());
    }
    let stride = (w as i32) * 4;
    let size = (h as i32) * stride;

    // Allocate a new shm buffer for this frame. Re-using the
    // previous buffer would be an optimization; the simpler
    // alloc-per-frame avoids Wayland's release-event tracking
    // for v0.
    let fd = create_anonymous_fd(size as usize)?;
    let mut mmap = unsafe {
        MmapMut::map_mut(&fd).map_err(|e| format!("mmap: {}", e))?
    };
    render_into(state, w, h, stride as usize, &mut mmap)?;

    let pool: WlShmPool = state.shm.create_pool(fd.as_fd(), size, qh, ());
    let buffer = pool.create_buffer(
        0,
        w as i32,
        h as i32,
        stride,
        wl_shm::Format::Xrgb8888,
        qh,
        (),
    );
    pool.destroy();

    let ls = &mut state.lock_surfaces[idx];
    ls.wl_surface.attach(Some(&buffer), 0, 0);
    ls.wl_surface.damage_buffer(0, 0, w as i32, h as i32);
    ls.wl_surface.commit();
    if let Some(old) = ls.buffer.replace(buffer) {
        old.destroy();
    }
    ls.mmap = Some(mmap);
    Ok(())
}

fn render_into(
    state: &State,
    w: u32,
    h: u32,
    stride: usize,
    mmap: &mut MmapMut,
) -> Result<(), String> {
    // Cairo Rgb24 is BGRX in memory which matches DRM XRGB8888 byte
    // order. Render into a Cairo surface, then memcpy into the mmap.
    let surface =
        cairo::ImageSurface::create(cairo::Format::Rgb24, w as i32, h as i32)
            .map_err(|e| format!("cairo surface: {}", e))?;
    let cr = cairo::Context::new(&surface).map_err(|e| format!("cairo ctx: {}", e))?;
    let opts = torrentius::make_font_options()?;
    cr.set_font_options(&opts);

    let bg = torrentius::parse_hex(&state.theme.bg);
    let fg = torrentius::parse_hex(&state.theme.fg);
    let accent = torrentius::parse_hex(&state.theme.accent);

    cr.set_source_rgb(bg.0, bg.1, bg.2);
    cr.rectangle(0.0, 0.0, w as f64, h as f64);
    let _ = cr.fill();

    let layout = pangocairo::functions::create_layout(&cr);
    let font_desc = pango::FontDescription::from_string(&state.theme.font);
    layout.set_font_description(Some(&font_desc));
    let pango_ctx = layout.context();
    pangocairo::functions::context_set_font_options(&pango_ctx, Some(&opts));

    let colors = PanelColors {
        bg,
        fg,
        accent,
        border: fg,
    };

    // SYSTEM panel
    let panel_w = 480.0_f64.min((w as f64) * 0.45);
    let sys_rows = [
        PanelRow {
            label: "HOSTNAME",
            value: &sysinfo::hostname(),
        },
        PanelRow {
            label: "KERNEL",
            value: &sysinfo::kernel_release(),
        },
        PanelRow {
            label: "DATE",
            value: &sysinfo::current_date(),
        },
        PanelRow {
            label: "TIME",
            value: &sysinfo::current_time(),
        },
        PanelRow {
            label: "UPTIME",
            value: &sysinfo::uptime_formatted(),
        },
    ];
    let sys_h = torrentius::panel_height_for(&layout, sys_rows.len());
    let center_x = (w as f64) / 2.0 - panel_w / 2.0;
    let sys_y = (h as f64) / 2.0 - sys_h - 40.0;
    torrentius::draw_panel(
        &cr,
        &layout,
        center_x,
        sys_y,
        panel_w,
        sys_h,
        "SYSTEM",
        &sys_rows,
        &colors,
    );

    // USER panel
    let masked: String = (0..state.password.char_count())
        .map(|_| '\u{25cf}')
        .collect();
    let user_rows: &[PanelRow] = &[
        PanelRow {
            label: "USER",
            value: &state.user,
        },
        PanelRow {
            label: &state.theme.prompt,
            value: &masked,
        },
        PanelRow {
            label: "STATUS",
            value: &state.message,
        },
    ];
    let user_h = torrentius::panel_height_for(&layout, user_rows.len());
    let user_y = sys_y + sys_h + 30.0;
    torrentius::draw_panel(
        &cr,
        &layout,
        center_x,
        user_y,
        panel_w,
        user_h,
        "USER",
        user_rows,
        &colors,
    );

    surface.flush();
    drop(cr);

    // Copy Cairo's bytes into the wl_shm mmap. Cairo stride may
    // exceed w*4 for alignment; copy row-by-row.
    let mut surface = surface;
    let cairo_stride = surface.stride() as usize;
    let row_bytes = (w as usize) * 4;
    let data =
        surface.data().map_err(|e| format!("cairo data: {}", e))?;
    for row in 0..h as usize {
        let src_off = row * cairo_stride;
        let dst_off = row * stride;
        mmap[dst_off..dst_off + row_bytes]
            .copy_from_slice(&data[src_off..src_off + row_bytes]);
    }
    Ok(())
}

// memfd-backed anonymous fd for the wl_shm pool. The fd is sized
// upfront via ftruncate so the mmap covers the whole pool.
fn create_anonymous_fd(size: usize) -> Result<OwnedFd, String> {
    let name =
        std::ffi::CString::new("gordian_knot-shm").expect("cstring with no nul");
    let raw = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr(),
            libc::MFD_CLOEXEC,
        )
    } as i32;
    if raw < 0 {
        return Err(format!("memfd_create: errno {}", unsafe {
            *libc::__errno_location()
        }));
    }
    if unsafe { libc::ftruncate(raw, size as libc::off_t) } < 0 {
        unsafe { libc::close(raw) };
        return Err(format!("ftruncate: errno {}", unsafe {
            *libc::__errno_location()
        }));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}
