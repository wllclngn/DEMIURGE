use std::path::Path;
use std::time::{Duration, Instant};

use crate::backend::{UnmatchedWindow, WindowManager};
use crate::config;
use crate::learn;
use crate::rules::{self, CompiledRule};

// Exit status for a config the daemon cannot start from (sysexits
// EX_CONFIG). cherrypie.service names it in RestartPreventExitStatus, so a
// bad config stops once instead of restarting forever.
pub const EXIT_CONFIG: i32 = 78;

// How long a window of an unmatched class must stay open before its learned
// rule is written to the config. Installers, consoles and launch helpers
// (Wine's conhost and wineboot, Proton's VC_redist runs) close well inside
// it and leave nothing behind; an application the user keeps open does not.
pub const LEARN_AFTER: Duration = Duration::from_secs(30);

// A learned rule held in memory until one of its class's windows has stayed
// open for the learn-after interval.
struct Pending {
    class: String,
    text: String,
    rule: CompiledRule,
    windows: Vec<u32>,
    since: Instant,
}

// Take an exclusive flock on $XDG_RUNTIME_DIR/cherrypie-<display>.lock.
// False when another cherrypie holds it. The fd is deliberately kept open
// for the life of the process; the kernel drops the lock at exit.
pub fn claim_instance() -> bool {
    let display: String = std::env::var("DISPLAY")
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let Ok(path) = std::ffi::CString::new(format!("{}/cherrypie-{}.lock", dir, display)) else {
        return true;
    };
    let fd = unsafe {
        libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC, 0o600)
    };
    if fd < 0 {
        // No lock file to take means no way to tell; run rather than not.
        return true;
    }
    unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

pub fn setup_signalfd() -> i32 {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGTERM);
        libc::sigaddset(&mut mask, libc::SIGINT);
        libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
        libc::signalfd(-1, &mask, libc::SFD_CLOEXEC)
    }
}

pub fn run(
    wm: WindowManager,
    config_path: &Path,
    dry_run: bool,
    signal_fd: i32,
    learn_after: Duration,
) {
    let compiled = match load_rules(config_path) {
        Some(r) => r,
        None => std::process::exit(EXIT_CONFIG),
    };

    let inotify_fd = setup_inotify(config_path);
    let x11_fd = wm.connection_fd();

    eprintln!(
        "[cherrypie] daemon started (backend: {}, rules: {}, dry_run: {})",
        wm.backend_name(),
        compiled.len(),
        dry_run,
    );

    event_loop(wm, compiled, x11_fd, signal_fd, inotify_fd, config_path, dry_run, learn_after);

    // Cleanup
    if signal_fd >= 0 {
        unsafe { libc::close(signal_fd); }
    }
    if inotify_fd >= 0 {
        unsafe { libc::close(inotify_fd); }
    }

    eprintln!("[cherrypie] shutdown");
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    wm: WindowManager,
    mut rules: Vec<CompiledRule>,
    x11_fd: i32,
    signal_fd: i32,
    inotify_fd: i32,
    config_path: &Path,
    dry_run: bool,
    learn_after: Duration,
) {
    let mut pending: Vec<Pending> = Vec::new();
    let mut fds = Vec::with_capacity(3);

    // X11 connection fd
    fds.push(libc::pollfd {
        fd: x11_fd,
        events: libc::POLLIN,
        revents: 0,
    });

    // Signal fd
    if signal_fd >= 0 {
        fds.push(libc::pollfd {
            fd: signal_fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }

    // Inotify fd for config reload
    if inotify_fd >= 0 {
        fds.push(libc::pollfd {
            fd: inotify_fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }

    // Apply rules to windows that already existed at startup
    let unmatched = wm.process_events(&rules, dry_run);
    learn_rules(&wm, &mut rules, &mut pending, unmatched, dry_run);

    loop {
        let timeout = pending
            .iter()
            .map(|p| (p.since + learn_after).saturating_duration_since(Instant::now()))
            .min()
            .map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32 + 1);
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ret < 0 {
            let errno = unsafe { *libc::__errno_location() };
            if errno == libc::EINTR {
                continue;
            }
            eprintln!("[cherrypie] poll error: {}", errno);
            break;
        }

        // Check signal fd (clean shutdown)
        if signal_fd >= 0 {
            let sig_idx = 1;
            if fds[sig_idx].revents & libc::POLLIN != 0 {
                drain_signalfd(signal_fd);
                break;
            }
        }

        // Check inotify fd (config reload)
        if inotify_fd >= 0 {
            let ino_idx = if signal_fd >= 0 { 2 } else { 1 };
            if ino_idx < fds.len() && fds[ino_idx].revents & libc::POLLIN != 0 {
                drain_inotify(inotify_fd);
                if let Some(new_rules) = load_rules(config_path) {
                    eprintln!(
                        "[cherrypie] config reloaded ({} rules)",
                        new_rules.len()
                    );
                    rules = new_rules;
                }
            }
        }

        // Check X11 fd (window events)
        if fds[0].revents & libc::POLLIN != 0 {
            let unmatched = wm.process_events(&rules, dry_run);
            learn_rules(&wm, &mut rules, &mut pending, unmatched, dry_run);
        }

        persist_lasting(&wm, &mut rules, &mut pending, config_path, learn_after);
    }
}

// Give each unmatched window class a rule: Apply it to the window now, and
// hold it in memory (pending) so further windows of the class match it
// instead of learning again. persist_lasting writes it to the config once a
// window of the class has stayed open; the inotify reload that append
// triggers recompiles the same rule.
fn learn_rules(
    wm: &WindowManager,
    rules: &mut Vec<CompiledRule>,
    pending: &mut Vec<Pending>,
    unmatched: Vec<UnmatchedWindow>,
    dry_run: bool,
) {
    if unmatched.is_empty() {
        return;
    }
    let date = learn::today();
    for w in unmatched {
        if let Some(rule) = rules
            .iter()
            .find(|r| r.matches(&w.class, &w.title, &w.role, &w.process, &w.window_type))
        {
            if !dry_run {
                wm.apply(w.window, rule);
            }
            continue;
        }
        if let Some(p) = pending.iter_mut().find(|p| p.class == w.class) {
            wm.apply(w.window, &p.rule);
            p.windows.push(w.window);
            continue;
        }
        let text = learn::rule_text(&w.class, &date);
        let rule = match learn::compile(&text) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[cherrypie] learned rule for '{}' does not compile: {}", w.class, e);
                continue;
            }
        };
        if dry_run {
            eprintln!("[cherrypie] [DRY] would learn a rule for class '{}'", w.class);
            rules.push(rule);
            continue;
        }
        wm.apply(w.window, &rule);
        pending.push(Pending {
            class: w.class,
            text,
            rule,
            windows: vec![w.window],
            since: Instant::now(),
        });
    }
}

// Write each pending rule whose interval has run out, if any window of its
// class is still open; a class whose windows all closed first is dropped.
fn persist_lasting(
    wm: &WindowManager,
    rules: &mut Vec<CompiledRule>,
    pending: &mut Vec<Pending>,
    config_path: &Path,
    learn_after: Duration,
) {
    let now = Instant::now();
    let (due, waiting): (Vec<Pending>, Vec<Pending>) = std::mem::take(pending)
        .into_iter()
        .partition(|p| now.duration_since(p.since) >= learn_after);
    *pending = waiting;
    for p in due {
        if !p.windows.iter().any(|&w| wm.client_alive(w)) {
            eprintln!("[cherrypie] not learning '{}': no window of it stayed open", p.class);
            continue;
        }
        match learn::append(config_path, &p.text) {
            Ok(()) => eprintln!(
                "[cherrypie] learned a rule for class '{}' -> {}",
                p.class,
                config_path.display()
            ),
            Err(e) => eprintln!(
                "[cherrypie] could not write the learned rule for '{}': {}",
                p.class, e
            ),
        }
        rules.push(p.rule);
    }
}

fn load_rules(config_path: &Path) -> Option<Vec<CompiledRule>> {
    let paths = config::Paths::with_config(config_path.to_path_buf());
    match config::load(&paths) {
        Ok(cfg) => match rules::compile(&cfg) {
            Ok(compiled) => Some(compiled),
            Err(e) => {
                eprintln!("[cherrypie] rule compile error: {}", e);
                None
            }
        },
        Err(e) => {
            eprintln!("[cherrypie] config error: {}", e);
            None
        }
    }
}

fn setup_inotify(config_path: &Path) -> i32 {
    let parent = match config_path.parent() {
        Some(p) => p,
        None => return -1,
    };

    let dir_str = match std::ffi::CString::new(parent.to_string_lossy().as_bytes()) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    unsafe {
        let fd = libc::inotify_init1(libc::IN_CLOEXEC);
        if fd < 0 {
            return -1;
        }

        let wd = libc::inotify_add_watch(fd, dir_str.as_ptr(), libc::IN_CLOSE_WRITE);
        if wd < 0 {
            libc::close(fd);
            return -1;
        }

        fd
    }
}

fn drain_signalfd(fd: i32) {
    unsafe {
        let mut buf = [0u8; 128];
        libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
    }
}

fn drain_inotify(fd: i32) {
    unsafe {
        let mut buf = [0u8; 4096];
        libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
    }
}
