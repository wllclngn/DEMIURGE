// X11 in-session lock.
//
// Stays inside the graphical session. Grabs keyboard + pointer, maps a
// fullscreen override-redirect window over the desktop, renders the SYSTEM
// and USER panels via torrentius, reads keystrokes, authenticates via PAM.
// On success: unmap + ungrab + return 0. On grab failure: return Err so the
// dispatcher can fall back to the VT path.
//
// Limitations flagged upfront:
//   - No monitor-aware centering; panels are centered on the root-window
//     bounding box. On multi-monitor setups they land between the monitors.
//   - Clock ticks once per second; the event loop wakes every 1000ms
//     regardless of activity.
//   - Pointer hiding is best-effort; the grab with no-op cursor usually
//     suffices but some drivers still flash the cursor for a frame.

use std::os::fd::AsRawFd;
use std::time::Instant;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use crate::config::GordianKnot as Cfg;
use crate::gordian_knot::pam::{self, AuthError};
use crate::gordian_knot::sysinfo;
use crate::keys::{self, KeyMap};
use crate::torrentius::{self, PanelColors, PanelRow};

pub enum LockError {
    Connect(String),
    GrabFailed,
    Runtime(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Connect(s) => write!(f, "X11 connect: {}", s),
            LockError::GrabFailed => write!(f, "keyboard/pointer grab failed"),
            LockError::Runtime(s) => write!(f, "x11 runtime: {}", s),
        }
    }
}

// Entry point. Returns Ok(()) when the user authenticates. Returns
// LockError::GrabFailed if the grab never takes -- dispatcher should fall
// back to the VT path.
pub fn lock(cfg: &Cfg, user: &str) -> Result<(), LockError> {
    let (conn, screen_num) =
        RustConnection::connect(None).map_err(|e| LockError::Connect(e.to_string()))?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let depth = screen.root_depth;
    let w = screen.width_in_pixels as u32;
    let h = screen.height_in_pixels as u32;

    let win = conn
        .generate_id()
        .map_err(|e| LockError::Runtime(format!("generate_id: {}", e)))?;
    let gc = conn
        .generate_id()
        .map_err(|e| LockError::Runtime(format!("generate_id: {}", e)))?;

    let bg_pixel = torrentius::hex_to_pixel(&cfg.bg);

    conn.create_window(
        depth,
        win,
        root,
        0,
        0,
        w as u16,
        h as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(bg_pixel)
            .event_mask(
                EventMask::KEY_PRESS
                    | EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY,
            ),
    )
    .map_err(|e| LockError::Runtime(format!("create_window: {}", e)))?;

    conn.create_gc(gc, win, &CreateGCAux::new())
        .map_err(|e| LockError::Runtime(format!("create_gc: {}", e)))?;

    let _ = conn.map_window(win);
    let _ = conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    let _ = conn.flush();

    // Grab keyboard + pointer, retry a few times if another client is
    // holding a grab (happens when another lock is just releasing).
    if !grab_input(&conn, win) {
        let _ = conn.destroy_window(win);
        let _ = conn.flush();
        return Err(LockError::GrabFailed);
    }

    let keymap = keys::load_keymap(&conn);

    // PAM conversation runs synchronously on Enter; draw loop otherwise.
    let result = event_loop(&conn, win, gc, w, h, depth, cfg, user, &keymap);

    // Cleanup: ungrab, unmap, destroy, disconnect.
    let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
    let _ = conn.unmap_window(win);
    let _ = conn.destroy_window(win);
    let _ = conn.flush();

    result
}

fn grab_input(conn: &RustConnection, win: Window) -> bool {
    for attempt in 0..50 {
        let kb = conn.grab_keyboard(
            false,
            win,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        );
        let kb_ok = kb
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.status == GrabStatus::SUCCESS)
            .unwrap_or(false);

        let ptr = conn.grab_pointer(
            false,
            win,
            EventMask::NO_EVENT,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            x11rb::CURRENT_TIME,
        );
        let ptr_ok = ptr
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.status == GrabStatus::SUCCESS)
            .unwrap_or(false);

        if kb_ok && ptr_ok {
            return true;
        }

        // Back off and retry.
        unsafe {
            let ts = libc::timespec { tv_sec: 0, tv_nsec: 100_000_000 };
            libc::nanosleep(&ts, std::ptr::null_mut());
        }
        let _ = attempt;
    }
    false
}

fn event_loop(
    conn: &RustConnection,
    win: Window,
    gc: Gcontext,
    w: u32,
    h: u32,
    depth: u8,
    cfg: &Cfg,
    user: &str,
    keymap: &KeyMap,
) -> Result<(), LockError> {
    let mut state = UiState {
        password: String::new(),
        message: String::new(),
        last_tick: Instant::now(),
    };

    redraw(conn, win, gc, w, h, depth, cfg, user, &state)
        .map_err(|e| LockError::Runtime(e))?;

    let fd = conn.stream().as_raw_fd();
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };

    loop {
        // Drain queued events.
        loop {
            match conn.poll_for_event() {
                Ok(Some(ev)) => {
                    if let Some(outcome) = handle_event(ev, cfg, user, keymap, &mut state) {
                        match outcome {
                            LoopOutcome::Authenticated => return Ok(()),
                            LoopOutcome::Redraw => {
                                redraw(conn, win, gc, w, h, depth, cfg, user, &state)
                                    .map_err(|e| LockError::Runtime(e))?;
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(LockError::Runtime(e.to_string())),
            }
        }
        let _ = conn.flush();

        // 1s timeout so the clock ticks.
        pfd.revents = 0;
        let ret = unsafe { libc::poll(&mut pfd, 1, 1000) };
        if ret < 0 {
            continue;
        }
        if state.last_tick.elapsed().as_secs() >= 1 {
            state.last_tick = Instant::now();
            redraw(conn, win, gc, w, h, depth, cfg, user, &state)
                .map_err(|e| LockError::Runtime(e))?;
        }
    }
}

struct UiState {
    password: String,
    message: String,
    last_tick: Instant,
}

enum LoopOutcome {
    Authenticated,
    Redraw,
}

fn handle_event(
    ev: Event,
    cfg: &Cfg,
    user: &str,
    keymap: &KeyMap,
    state: &mut UiState,
) -> Option<LoopOutcome> {
    match ev {
        Event::KeyPress(kp) => on_key_press(kp, cfg, user, keymap, state),
        Event::Expose(ex) if ex.count == 0 => Some(LoopOutcome::Redraw),
        _ => None,
    }
}

fn on_key_press(
    ev: KeyPressEvent,
    _cfg: &Cfg,
    user: &str,
    keymap: &KeyMap,
    state: &mut UiState,
) -> Option<LoopOutcome> {
    let shift = u16::from(ev.state) & u16::from(ModMask::SHIFT) != 0;
    let keysym = keycode_to_keysym(ev.detail, shift, keymap);
    match keysym {
        0xff0d | 0xff8d => {
            // Return / KP_Enter -- submit.
            state.message.clear();
            match pam::authenticate(user, &state.password) {
                Ok(()) => Some(LoopOutcome::Authenticated),
                Err(AuthError::WrongCredentials) => {
                    state.message = "Authentication failed".to_string();
                    state.password.clear();
                    Some(LoopOutcome::Redraw)
                }
                Err(e) => {
                    state.message = format!("{}", e);
                    state.password.clear();
                    Some(LoopOutcome::Redraw)
                }
            }
        }
        0xff08 => {
            // BackSpace
            state.password.pop();
            Some(LoopOutcome::Redraw)
        }
        0xff1b => {
            // Escape: clear entered password, do not exit the lock.
            state.password.clear();
            state.message.clear();
            Some(LoopOutcome::Redraw)
        }
        0x0015 => {
            // Ctrl+U (in some layouts). Clear line.
            state.password.clear();
            Some(LoopOutcome::Redraw)
        }
        sym if (0x20..=0x7e).contains(&sym) => {
            state.password.push(sym as u8 as char);
            Some(LoopOutcome::Redraw)
        }
        _ => None,
    }
}

fn keycode_to_keysym(keycode: u8, shift: bool, keymap: &KeyMap) -> u32 {
    if keycode < keymap.min_keycode {
        return 0;
    }
    let base = (keycode - keymap.min_keycode) as usize * keymap.syms_per_code;
    let idx = base + if shift && keymap.syms_per_code > 1 { 1 } else { 0 };
    keymap.keysyms.get(idx).copied().unwrap_or(0)
}

fn redraw(
    conn: &RustConnection,
    win: Window,
    gc: Gcontext,
    w: u32,
    h: u32,
    depth: u8,
    cfg: &Cfg,
    user: &str,
    state: &UiState,
) -> Result<(), String> {
    let (surface, cr) = torrentius::new_surface(w as i32, h as i32)?;
    let font_opts = torrentius::make_font_options()?;
    cr.set_font_options(&font_opts);

    let (br, bg, bb) = torrentius::parse_hex(&cfg.bg);
    cr.set_source_rgb(br, bg, bb);
    let _ = cr.paint();

    let font_desc = pango::FontDescription::from_string(&cfg.font);
    let layout = pangocairo::functions::create_layout(&cr);
    layout.set_font_description(Some(&font_desc));
    let pango_ctx = layout.context();
    pangocairo::functions::context_set_font_options(&pango_ctx, Some(&font_opts));

    let colors = PanelColors {
        bg: torrentius::parse_hex(&cfg.bg),
        fg: torrentius::parse_hex(&cfg.fg),
        accent: torrentius::parse_hex(&cfg.accent),
        border: torrentius::parse_hex(&cfg.fg),
    };

    let sys_hostname = sysinfo::hostname();
    let sys_kernel = sysinfo::kernel_release();
    let sys_date = sysinfo::current_date();
    let sys_time = sysinfo::current_time();
    let sys_uptime = sysinfo::uptime_formatted();

    let sys_rows = [
        PanelRow { label: "HOSTNAME", value: &sys_hostname },
        PanelRow { label: "KERNEL", value: &sys_kernel },
        PanelRow { label: "DATE", value: &sys_date },
        PanelRow { label: "TIME", value: &sys_time },
        PanelRow { label: "UPTIME", value: &sys_uptime },
    ];

    let dots: String = std::iter::repeat('\u{25cf}')
        .take(state.password.chars().count())
        .collect();
    let user_rows = [
        PanelRow { label: "USER", value: user },
        PanelRow { label: &cfg.prompt, value: &dots },
    ];

    let panel_w: f64 = 520.0;
    let sys_h = torrentius::panel_height_for(&layout, sys_rows.len());
    let user_h = torrentius::panel_height_for(&layout, user_rows.len());
    let stack_h = sys_h + 24.0 + user_h;
    let x0 = (w as f64 - panel_w) / 2.0;
    let y0 = (h as f64 - stack_h) / 2.0;

    torrentius::draw_panel(
        &cr, &layout, x0, y0, panel_w, sys_h, "SYSTEM", &sys_rows, &colors,
    );
    torrentius::draw_panel(
        &cr,
        &layout,
        x0,
        y0 + sys_h + 24.0,
        panel_w,
        user_h,
        "USER",
        &user_rows,
        &colors,
    );

    if !state.message.is_empty() {
        let (fr, fg, fb) = torrentius::parse_hex(&cfg.accent);
        cr.set_source_rgb(fr, fg, fb);
        layout.set_text(&state.message);
        let (mw, _) = layout.pixel_size();
        let mx = (w as f64 - mw as f64) / 2.0;
        let my = y0 + stack_h + 16.0;
        cr.move_to(mx, my);
        pangocairo::functions::show_layout(&cr, &layout);
    }

    drop(cr);
    surface.flush();

    let _ = surface.with_data(|data| {
        let _ = conn.put_image(
            ImageFormat::Z_PIXMAP,
            win,
            gc,
            w as u16,
            h as u16,
            0,
            0,
            0,
            depth,
            data,
        );
    });
    let _ = conn.flush();
    Ok(())
}
