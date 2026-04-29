use std::path::Path;

use crate::backend::WindowManager;
use crate::config;
use crate::rules::{self, CompiledRule};

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

pub fn run(wm: WindowManager, config_path: &Path, dry_run: bool, signal_fd: i32) {
    let compiled = match load_rules(config_path) {
        Some(r) => r,
        None => return,
    };

    let inotify_fd = setup_inotify(config_path);
    let x11_fd = wm.connection_fd();

    eprintln!(
        "[cherrypie] daemon started (backend: {}, rules: {}, dry_run: {})",
        wm.backend_name(),
        compiled.len(),
        dry_run,
    );

    event_loop(wm, compiled, x11_fd, signal_fd, inotify_fd, config_path, dry_run);

    // Cleanup
    if signal_fd >= 0 {
        unsafe { libc::close(signal_fd); }
    }
    if inotify_fd >= 0 {
        unsafe { libc::close(inotify_fd); }
    }

    eprintln!("[cherrypie] shutdown");
}

fn event_loop(
    wm: WindowManager,
    mut rules: Vec<CompiledRule>,
    x11_fd: i32,
    signal_fd: i32,
    inotify_fd: i32,
    config_path: &Path,
    dry_run: bool,
) {
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
    wm.process_events(&rules, dry_run);

    loop {
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
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
            wm.process_events(&rules, dry_run);
        }
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
