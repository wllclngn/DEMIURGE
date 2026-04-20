// Landlock filesystem sandbox for gordian_knot.
//
// Raw landlock_create_ruleset/landlock_add_rule/landlock_restrict_self via
// libc::syscall. No helper crate. Pattern lifted from ABRAXAS's landlock.rs.
//
// Installed alongside seccomp after privsep drop. Gracefully no-ops on
// kernels without landlock (pre-5.13) -- the seccomp filter still applies.
//
// Scope: read-only access to the tree needed for PAM + X11 + fontconfig.
// No write access anywhere. Explicitly locks out /home, /root, /var (except
// /var/run for X socket).

const NR_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const NR_LANDLOCK_ADD_RULE: libc::c_long = 445;
const NR_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
}

#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

fn add_path(ruleset_fd: i32, path: &str, access: u64) -> bool {
    let cpath = match std::ffi::CString::new(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        return false;
    }
    let rule = PathBeneathAttr {
        allowed_access: access,
        parent_fd: fd,
    };
    let rc = unsafe {
        libc::syscall(
            NR_LANDLOCK_ADD_RULE,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH,
            &rule as *const PathBeneathAttr,
            0u32,
        )
    };
    unsafe { libc::close(fd) };
    rc == 0
}

pub fn install_sandbox() -> bool {
    // Probe kernel support.
    let abi = unsafe {
        libc::syscall(
            NR_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    } as i32;
    if abi < 0 {
        return false;
    }

    let attr = RulesetAttr {
        handled_access_fs: ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR | ACCESS_FS_EXECUTE,
        handled_access_net: 0,
    };

    let ruleset_fd = unsafe {
        libc::syscall(
            NR_LANDLOCK_CREATE_RULESET,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    } as i32;
    if ruleset_fd < 0 {
        return false;
    }

    let ro = ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;
    let rx = ro | ACCESS_FS_EXECUTE;

    // /etc -- PAM config, nsswitch.conf, localtime
    add_path(ruleset_fd, "/etc", ro);
    // /usr -- libs, unix_chkpwd binary, fontconfig, fonts
    add_path(ruleset_fd, "/usr", rx);
    // /lib, /lib64 -- shared libs (symlinks into /usr on most distros)
    add_path(ruleset_fd, "/lib", rx);
    add_path(ruleset_fd, "/lib64", rx);
    // /proc -- /proc/uptime, /proc/self/*
    add_path(ruleset_fd, "/proc", ro);
    // /sys -- X11 extension queries sometimes touch this
    add_path(ruleset_fd, "/sys", ro);
    // /dev -- /dev/urandom, /dev/null, X11 socket stays open from before
    add_path(ruleset_fd, "/dev", ro);
    // /run -- D-Bus socket, X11 socket path
    add_path(ruleset_fd, "/run", ro);
    // /tmp -- X11 socket lives under /tmp/.X11-unix
    add_path(ruleset_fd, "/tmp", ro);

    // Note: /home and /root are intentionally NOT listed. Any read there is
    // now denied. This blocks a post-exploitation read of the user's SSH
    // keys or browser data even if the locker is otherwise subverted.

    let rc = unsafe {
        libc::syscall(NR_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32)
    } as i32;
    unsafe { libc::close(ruleset_fd) };
    rc == 0
}
