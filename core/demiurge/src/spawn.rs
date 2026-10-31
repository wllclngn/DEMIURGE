// Starting the session's programs, for both stacks: posix_spawnp, with
// `sh -c` only for a command that needs a shell, the child's signal mask
// cleared and its stdin closed. Every child is tracked until it is reaped,
// so quitting sends SIGTERM to what is still running instead of leaving
// terminals and daemons behind.

use std::collections::HashSet;
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::ptr;
use std::sync::{Mutex, OnceLock};

// POSIX `environ` -- the parent process's environment block, what a child
// gets when nothing overrides it.
unsafe extern "C" {
    static environ: *const *const libc::c_char;
}

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

fn tracked_pids() -> Vec<libc::pid_t> {
    match tracked().lock() {
        Ok(s) => s.iter().copied().collect(),
        Err(p) => p.into_inner().iter().copied().collect(),
    }
}

/// SIGTERM to every child started here that has not been reaped.
pub fn quit_children() {
    for pid in tracked_pids() {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

/// Reap every child that has exited, started here or not.
pub fn reap_any() {
    loop {
        let pid = unsafe { libc::waitpid(-1, ptr::null_mut(), libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
        forget(pid);
    }
}

/// Reap the children started here that have exited, leaving any other
/// child to whoever started it.
pub fn reap_tracked() {
    for pid in tracked_pids() {
        if unsafe { libc::waitpid(pid, ptr::null_mut(), libc::WNOHANG) } != 0 {
            forget(pid);
        }
    }
}

// Build a posix_spawnattr that resets the child's signal mask to empty.
// Both stacks block their quit signals for a signalfd; without this reset,
// children would inherit the block and ignore the SIGTERM they are sent at
// quit.
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
pub fn needs_shell(cmd: &str) -> bool {
    cmd.contains(|c: char| matches!(c,
        '$' | '`' | '"' | '\'' | '\\' | '~' | '*' | '?'
        | '[' | ']' | '{' | '}' | '(' | ')' | '|' | '&'
        | ';' | '<' | '>' | '!' | '#'
    ))
}

/// Start `cmd` with the parent's environment. Its pid, once started.
/// PATH with this binary's own directory first: Each stack's binaries live in
/// a directory of their own, and the session's children (the lock, MEDUSA,
/// the rest) must find that stack's before any other.
pub fn path_with_own_dir() -> Option<std::ffi::OsString> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(dir).chain(std::env::split_paths(&path))).ok()
}

pub fn spawn(cmd: &str) -> Option<libc::pid_t> {
    spawn_with(cmd, &[])
}

/// Start `cmd` with the parent's environment, `env` set over it. Its pid,
/// once started.
pub fn spawn_with(cmd: &str, env: &[(&str, String)]) -> Option<libc::pid_t> {
    let args: Vec<CString> = if needs_shell(cmd) {
        ["sh", "-c", cmd].iter().filter_map(|a| CString::new(*a).ok()).collect()
    } else {
        cmd.split_whitespace().filter_map(|a| CString::new(a).ok()).collect()
    };
    if args.is_empty() || (needs_shell(cmd) && args.len() != 3) {
        return None;
    }
    let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(ptr::null());
    let overridden = environment(env);
    let mut envp: Vec<*const libc::c_char> = overridden.iter().map(|v| v.as_ptr()).collect();
    envp.push(ptr::null());

    unsafe {
        let mut pid: libc::pid_t = 0;
        let mut file_actions: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut file_actions);
        libc::posix_spawn_file_actions_addclose(&mut file_actions, 0);
        let mut attr = make_attr();
        let ret = libc::posix_spawnp(
            &mut pid,
            args[0].as_ptr(),
            &file_actions,
            &attr,
            argv.as_ptr() as *const *mut libc::c_char,
            if env.is_empty() { environ } else { envp.as_ptr() } as *const *mut libc::c_char,
        );
        libc::posix_spawn_file_actions_destroy(&mut file_actions);
        libc::posix_spawnattr_destroy(&mut attr);
        if ret != 0 {
            eprintln!("[demiurge] spawn '{}' failed: {}", cmd, std::io::Error::from_raw_os_error(ret));
            return None;
        }
        track(pid);
        Some(pid)
    }
}

// The parent's environment with `env` set over it, as `KEY=value` strings;
// none when nothing is set over it, the parent's block then passed as is.
fn environment(env: &[(&str, String)]) -> Vec<CString> {
    if env.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<CString> = std::env::vars_os()
        .filter(|(key, _)| !env.iter().any(|(k, _)| OsStr::new(k) == key))
        .filter_map(|(key, value)| {
            let mut entry = key.as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            CString::new(entry).ok()
        })
        .collect();
    out.extend(env.iter().filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok()));
    out
}
