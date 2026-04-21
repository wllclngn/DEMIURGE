mod appearance;
mod atoms;
mod bar;
mod config;
mod event;
mod ewmh;
mod keys;
mod layout;
mod monitor;
mod mouse;
mod mru;
mod spawn;
mod torrentius;
mod wm;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;

use std::path::PathBuf;

fn main() {
    let Args { config_path, check_config, setup } = parse_args();

    let paths = match config_path {
        Some(p) => config::Paths::with_config(p),
        None => match config::Paths::init() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[demiurge] {}", e);
                std::process::exit(1);
            }
        },
    };

    let cfg = match config::load(&paths) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[demiurge] config error: {}", e);
            std::process::exit(1);
        }
    };

    // --check-config: parse succeeded, we're done. Used by install.py to
    // detect stale configs before they kill a session at login.
    if check_config {
        println!("demiurge: config ok ({})", paths.config_file.display());
        std::process::exit(0);
    }

    // --setup: write ~/.gtkrc-2.0, ~/.config/gtk-3.0/settings.ini, and
    // ~/.icons/default/index.theme from [cursor] and [font] config. Replaces
    // lxappearance. Does not start the WM.
    if setup {
        if let Err(e) = appearance::write_setup_files(&cfg) {
            eprintln!("[demiurge setup] {}", e);
            std::process::exit(1);
        }
        std::process::exit(0);
    }

    // Export XCURSOR_THEME and XCURSOR_SIZE so every child inherits the
    // configured cursor. Must happen before any spawn::spawn call.
    appearance::apply_runtime(&cfg);

    let mut wm = match wm::Wm::init(&cfg) {
        Ok(wm) => wm,
        Err(e) => {
            eprintln!("[demiurge] init failed: {}", e);
            std::process::exit(1);
        }
    };

    // Spawn startup commands
    for cmd in &cfg.startup.commands {
        eprintln!("[demiurge] startup: {}", cmd);
        spawn::spawn(cmd);
    }

    // Tag-targeted spawns. Each registers a pending WM_CLASS match before
    // firing the command; Wm::manage consumes the match when the window
    // appears and routes it to the requested tag.
    for sp in &cfg.startup.spawn {
        let target_tag = cfg
            .general
            .tags
            .iter()
            .position(|t| t == &sp.tag)
            .expect("startup.spawn tag validated by config::load");
        let class_match = sp.class_match();
        eprintln!(
            "[demiurge] startup spawn (class='{}', tag={}): {}",
            class_match, target_tag, sp.cmd
        );
        wm.pending_spawns.push(wm::PendingSpawn {
            class_match,
            target_tag,
        });
        spawn::spawn(&sp.cmd);
    }

    // Setup signalfd for clean shutdown
    let signal_fd = setup_signalfd();

    // Setup timerfd for 1-second bar clock tick
    let timer_fd = setup_timerfd();

    // Setup inotify for config hot-reload
    let inotify_fd = setup_inotify(&paths.config_file);

    // Event loop
    run(&mut wm, signal_fd, timer_fd, inotify_fd, &paths.config_file);

    // Cleanup
    cleanup(&wm, timer_fd, inotify_fd);
    eprintln!("[demiurge] exiting");
}

struct Args {
    config_path: Option<PathBuf>,
    check_config: bool,
    setup: bool,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut out = Args { config_path: None, check_config: false, setup: false };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-c" | "--config" => {
                if i + 1 < args.len() {
                    i += 1;
                    out.config_path = Some(PathBuf::from(&args[i]));
                } else {
                    eprintln!("[demiurge] --config requires a path");
                    std::process::exit(1);
                }
            }
            "--check-config" => {
                out.check_config = true;
            }
            "--setup" => {
                // lxappearance replacement: regenerate GTK config and
                // ~/.icons/default/index.theme from [cursor] and [font].
                out.setup = true;
            }
            "-v" | "--version" => {
                println!("demiurge {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("usage: demiurge [-c config.toml] [--check-config] [--setup] [-v] [-h]");
                std::process::exit(0);
            }
            _ => {
                eprintln!("[demiurge] unknown argument: {}", args[i]);
                std::process::exit(1);
            }
        }
        i += 1;
    }
    out
}

fn setup_signalfd() -> i32 {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGINT);
        libc::sigaddset(&mut mask, libc::SIGTERM);
        libc::sigaddset(&mut mask, libc::SIGCHLD);
        libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
        let fd = libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
        if fd < 0 {
            eprintln!("[demiurge] signalfd failed, signals won't be caught cleanly");
            return -1;
        }
        fd
    }
}

// Drain all finished children without blocking. Prevents spawned apps (kitty,
// chromium, electron helpers that exit on failure) from accumulating as
// zombies in the service cgroup. Also drops each reaped pid from the
// live-child registry so quit_children doesn't later signal a pid that
// has since been recycled by the kernel.
fn reap_children() {
    unsafe {
        loop {
            let pid = libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG);
            if pid <= 0 {
                break;
            }
            spawn::forget(pid);
        }
    }
}

fn setup_timerfd() -> i32 {
    unsafe {
        let fd = libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_NONBLOCK | libc::TFD_CLOEXEC,
        );
        if fd < 0 {
            eprintln!("[demiurge] timerfd_create failed, clock won't update");
            return -1;
        }
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            },
        };
        if libc::timerfd_settime(fd, 0, &spec, std::ptr::null_mut()) < 0 {
            eprintln!("[demiurge] timerfd_settime failed");
            libc::close(fd);
            return -1;
        }
        fd
    }
}

fn setup_inotify(config_path: &std::path::Path) -> i32 {
    unsafe {
        let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if fd < 0 {
            eprintln!("[demiurge] inotify_init failed, config reload disabled");
            return -1;
        }
        let path_str = match config_path.to_str() {
            Some(s) => s,
            None => {
                libc::close(fd);
                return -1;
            }
        };
        let c_path = match std::ffi::CString::new(path_str) {
            Ok(p) => p,
            Err(_) => {
                libc::close(fd);
                return -1;
            }
        };
        let wd = libc::inotify_add_watch(fd, c_path.as_ptr(), libc::IN_CLOSE_WRITE);
        if wd < 0 {
            eprintln!("[demiurge] inotify_add_watch failed for {}", path_str);
            libc::close(fd);
            return -1;
        }
        eprintln!("[demiurge] watching {} for changes", path_str);
        fd
    }
}

fn poll_events(fd: i32) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: if fd >= 0 { libc::POLLIN } else { 0 },
        revents: 0,
    }
}

fn run(
    wm: &mut wm::Wm,
    signal_fd: i32,
    timer_fd: i32,
    inotify_fd: i32,
    config_path: &std::path::Path,
) {
    let x11_fd = wm.connection_fd();

    // poll() ignores entries with fd < 0, so always pass all 4
    let mut fds = [
        libc::pollfd {
            fd: x11_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        poll_events(signal_fd),
        poll_events(timer_fd),
        poll_events(inotify_fd),
    ];
    let nfds = fds.len() as libc::nfds_t;

    while wm.running {
        // Drain all buffered X11 events before polling
        loop {
            match wm.conn.poll_for_event() {
                Ok(Some(ev)) => event::handle(wm, ev),
                Ok(None) => break,
                Err(e) => {
                    eprintln!("[demiurge] x11 error: {}", e);
                    wm.running = false;
                    return;
                }
            }
            if !wm.running {
                return;
            }
        }

        // Push any pending bar dirty regions to X. This is the single
        // commit point per event-loop iteration -- state mutations
        // earlier in this iteration just set dirty flags; the actual
        // render + put_image happens here. Fast path: if nothing is
        // dirty, commit_bar returns immediately.
        wm.commit_bar();

        let _ = wm.conn.flush();

        for fd in &mut fds {
            fd.revents = 0;
        }

        let ret = unsafe { libc::poll(fds.as_mut_ptr(), nfds, -1) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            eprintln!("[demiurge] poll error: {}", err);
            break;
        }

        // Signal received. Drain all queued siginfo entries; SIGCHLD reaps,
        // SIGINT/SIGTERM shut down.
        if fds[1].revents & libc::POLLIN != 0 {
            let mut shutdown = false;
            loop {
                let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
                let n = unsafe {
                    libc::read(
                        signal_fd,
                        &mut info as *mut _ as *mut libc::c_void,
                        std::mem::size_of::<libc::signalfd_siginfo>(),
                    )
                };
                if n as usize != std::mem::size_of::<libc::signalfd_siginfo>() {
                    break;
                }
                match info.ssi_signo as i32 {
                    libc::SIGCHLD => reap_children(),
                    libc::SIGINT | libc::SIGTERM => {
                        eprintln!("[demiurge] signal received, shutting down");
                        shutdown = true;
                    }
                    _ => {}
                }
            }
            if shutdown {
                break;
            }
        }

        // Timer tick: only the clock region needs to update. The
        // actual render + put_image happens at the top of the next
        // iteration in commit_bar(). mark_clock_dirty additionally
        // gates on text-changed so a same-second tick is a no-op.
        if fds[2].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 8];
            unsafe {
                libc::read(timer_fd, buf.as_mut_ptr() as *mut libc::c_void, 8);
            }
            if let Some(ref mut bar) = wm.bar {
                bar.mark_clock_dirty();
            }
        }

        // Config file changed: hot-reload
        if fds[3].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 1024];
            unsafe {
                libc::read(inotify_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
            }
            let paths = config::Paths::with_config(config_path.to_path_buf());
            match config::load(&paths) {
                Ok(cfg) => wm.reload_config(&cfg),
                Err(e) => eprintln!(
                    "[{}] [WARN]   config reload failed: {}",
                    wm::local_time(),
                    e,
                ),
            }
        }

        // X11 events will be drained at top of loop
    }
}

fn cleanup(wm: &wm::Wm, timer_fd: i32, inotify_fd: i32) {
    // Tear down the session's children before dropping the X connection,
    // so terminals, music players, and tag-pinned helpers (montauk) exit
    // alongside the WM instead of being reparented to init.
    spawn::quit_children();

    if let Some(ref bar) = wm.bar {
        bar.destroy_all(&wm.conn);
    }

    let _ = wm.conn.destroy_window(wm.check_window);

    let _ = wm.conn.delete_property(wm.root, wm.atoms._NET_SUPPORTED);
    let _ = wm.conn.delete_property(wm.root, wm.atoms._NET_SUPPORTING_WM_CHECK);
    let _ = wm.conn.flush();

    unsafe {
        if timer_fd >= 0 {
            libc::close(timer_fd);
        }
        if inotify_fd >= 0 {
            libc::close(inotify_fd);
        }
    }
}
