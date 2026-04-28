use std::collections::HashSet;
use std::ffi::CString;
use std::ptr;
use std::sync::{Mutex, OnceLock};

// POSIX `environ` -- the parent process's environment block. Must be passed
// explicitly to posix_spawnp; passing NULL gives the child an empty environment
// (no DISPLAY/PATH/HOME/XAUTHORITY), which silently kills any X11 client.
unsafe extern "C" {
    static environ: *const *const libc::c_char;
}

// Live-child registry. Each successful spawn inserts its pid; reap_children
// in main.rs removes pids as they exit; quit_children sends SIGTERM to
// everything still in the set so Alt+Shift+F4 actually tears the session
// down instead of just dropping the WM while terminals and daemons survive.
static TRACKED_PIDS: OnceLock<Mutex<HashSet<libc::pid_t>>> = OnceLock::new();

fn tracked() -> &'static Mutex<HashSet<libc::pid_t>> {
    TRACKED_PIDS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn track(pid: libc::pid_t) {
    if pid > 0 {
        if let Ok(mut s) = tracked().lock() {
            s.insert(pid);
        }
    }
}

pub fn forget(pid: libc::pid_t) {
    if let Ok(mut s) = tracked().lock() {
        s.remove(&pid);
    }
}

// Send SIGTERM to every child we spawned that hasn't already exited.
// Called from cleanup() on quit so music players, terminals, and helpers
// don't leak across "logout".
pub fn quit_children() {
    let pids: Vec<libc::pid_t> = match tracked().lock() {
        Ok(s) => s.iter().copied().collect(),
        Err(p) => p.into_inner().iter().copied().collect(),
    };
    for pid in pids {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

// Build a posix_spawnattr that resets the child's signal mask to empty.
// demiurge blocks SIGTERM/SIGINT/SIGCHLD via signalfd; without this reset,
// children spawned after signalfd setup inherit the block and ignore the
// SIGTERM we send them at quit.
unsafe fn make_attr() -> libc::posix_spawnattr_t {
    unsafe {
        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        libc::posix_spawnattr_init(&mut attr);
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        libc::posix_spawnattr_setsigmask(&mut attr, &empty);
        libc::posix_spawnattr_setflags(&mut attr, libc::POSIX_SPAWN_SETSIGMASK as _);
        attr
    }
}

// Returns true if `cmd` contains shell metacharacters that require sh -c
// for correct interpretation (env expansion, quoting, redirects, pipes, globs).
// Whitespace alone is not a metacharacter -- simple multi-arg commands like
// "brightnessctl set 5%+" still take the fast direct-exec path.
fn needs_shell(cmd: &str) -> bool {
    cmd.contains(|c: char| matches!(c,
        '$' | '`' | '"' | '\'' | '\\' | '~' | '*' | '?'
        | '[' | ']' | '{' | '}' | '(' | ')' | '|' | '&'
        | ';' | '<' | '>' | '!' | '#'
    ))
}

pub fn spawn(cmd: &str) {
    if needs_shell(cmd) {
        spawn_via_shell(cmd);
    } else {
        spawn_direct(cmd);
    }
}

fn spawn_via_shell(cmd: &str) {
    let prog = CString::new("/bin/sh").unwrap();
    let arg0 = CString::new("sh").unwrap();
    let arg1 = CString::new("-c").unwrap();
    let arg2 = match CString::new(cmd) {
        Ok(s) => s,
        Err(_) => return,
    };

    let argv: [*const libc::c_char; 4] = [
        arg0.as_ptr(),
        arg1.as_ptr(),
        arg2.as_ptr(),
        ptr::null(),
    ];

    unsafe {
        let mut pid: libc::pid_t = 0;
        let mut file_actions: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut file_actions);
        libc::posix_spawn_file_actions_addclose(&mut file_actions, 0);
        let mut attr = make_attr();

        let ret = libc::posix_spawnp(
            &mut pid,
            prog.as_ptr(),
            &file_actions,
            &attr,
            argv.as_ptr() as *const *mut libc::c_char,
            environ as *const *mut libc::c_char,
        );

        libc::posix_spawn_file_actions_destroy(&mut file_actions);
        libc::posix_spawnattr_destroy(&mut attr);

        if ret != 0 {
            eprintln!("[demiurge] spawn (sh -c) '{}' failed: {}", cmd, ret);
        } else {
            track(pid);
        }
    }
}

fn spawn_direct(cmd: &str) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }

    let prog = match CString::new(parts[0]) {
        Ok(s) => s,
        Err(_) => return,
    };

    let c_args: Vec<CString> = parts
        .iter()
        .filter_map(|s| CString::new(*s).ok())
        .collect();
    let mut argv: Vec<*const libc::c_char> = c_args.iter().map(|s| s.as_ptr()).collect();
    argv.push(ptr::null());

    unsafe {
        let mut pid: libc::pid_t = 0;
        let mut file_actions: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut file_actions);
        libc::posix_spawn_file_actions_addclose(&mut file_actions, 0);
        let mut attr = make_attr();

        let ret = libc::posix_spawnp(
            &mut pid,
            prog.as_ptr(),
            &file_actions,
            &attr,
            argv.as_ptr() as *const *mut libc::c_char,
            environ as *const *mut libc::c_char,
        );

        libc::posix_spawn_file_actions_destroy(&mut file_actions);
        libc::posix_spawnattr_destroy(&mut attr);

        if ret != 0 {
            eprintln!("[demiurge] spawn '{}' failed: {}", parts[0], ret);
        } else {
            track(pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::needs_shell;

    #[test]
    fn simple_command_direct() {
        assert!(!needs_shell("kitty"));
        assert!(!needs_shell("physlock"));
    }

    #[test]
    fn multi_arg_no_metachars_direct() {
        assert!(!needs_shell("brightnessctl set 5%+"));
        assert!(!needs_shell("brightnessctl set 5%-"));
        assert!(!needs_shell("wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+"));
        assert!(!needs_shell("wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle"));
    }

    #[test]
    fn dollar_var_needs_shell() {
        assert!(needs_shell("flameshot gui -p $HOME"));
        assert!(needs_shell("flameshot full -p $HOME"));
    }

    #[test]
    fn pipe_needs_shell() {
        assert!(needs_shell("echo hello | grep o"));
    }

    #[test]
    fn redirect_needs_shell() {
        assert!(needs_shell("echo hi > /tmp/out"));
    }

    #[test]
    fn tilde_needs_shell() {
        assert!(needs_shell("kitty -d ~"));
    }

    #[test]
    fn glob_needs_shell() {
        assert!(needs_shell("rm /tmp/foo*"));
    }

    #[test]
    fn semicolon_needs_shell() {
        assert!(needs_shell("a ; b"));
    }
}
