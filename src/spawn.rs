use std::ffi::CString;
use std::ptr;

// POSIX `environ` -- the parent process's environment block. Must be passed
// explicitly to posix_spawnp; passing NULL gives the child an empty environment
// (no DISPLAY/PATH/HOME/XAUTHORITY), which silently kills any X11 client.
unsafe extern "C" {
    static environ: *const *const libc::c_char;
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

        let ret = libc::posix_spawnp(
            &mut pid,
            prog.as_ptr(),
            &file_actions,
            ptr::null(),
            argv.as_ptr() as *const *mut libc::c_char,
            environ as *const *mut libc::c_char,
        );

        libc::posix_spawn_file_actions_destroy(&mut file_actions);

        if ret != 0 {
            eprintln!("[demiurge] spawn (sh -c) '{}' failed: {}", cmd, ret);
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

        let ret = libc::posix_spawnp(
            &mut pid,
            prog.as_ptr(),
            &file_actions,
            ptr::null(),
            argv.as_ptr() as *const *mut libc::c_char,
            environ as *const *mut libc::c_char,
        );

        libc::posix_spawn_file_actions_destroy(&mut file_actions);

        if ret != 0 {
            eprintln!("[demiurge] spawn '{}' failed: {}", parts[0], ret);
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
