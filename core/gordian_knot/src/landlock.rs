// Landlock filesystem sandbox for GORDIAN KNOT's UI child, installed with
// the seccomp filter after its first accepted frame.
//
// Raw landlock_create_ruleset / landlock_add_rule / landlock_restrict_self
// via libc::syscall. Every ABI-1 filesystem right is handled, and the only
// grants are reads: Fonts (system, the fontconfig caches, the user's own
// font dirs), /etc (localtime, fontconfig), /usr and /proc (uptime). No
// writes or execution anywhere, and the rest of $HOME is unreadable.
// Already-open descriptors (the X socket, the parent's socket, stderr) are
// not affected. Gracefully a no-op on kernels without landlock.

const NR_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const NR_LANDLOCK_ADD_RULE: libc::c_long = 445;
const NR_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;
// EXECUTE through MAKE_SYM: Every right landlock ABI 1 defines.
const ACCESS_FS_ALL_V1: u64 = (1 << 13) - 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

fn add_read(ruleset_fd: i32, path: &str) {
    let Ok(cpath) = std::ffi::CString::new(path) else {
        return;
    };
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        return;
    }
    let rule = PathBeneathAttr {
        allowed_access: ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR,
        parent_fd: fd,
    };
    unsafe {
        libc::syscall(
            NR_LANDLOCK_ADD_RULE,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH,
            &rule as *const PathBeneathAttr,
            0u32,
        );
        libc::close(fd);
    }
}

pub fn install_sandbox(home: &str) -> bool {
    let abi = unsafe {
        libc::syscall(
            NR_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi < 1 {
        return false;
    }
    let attr = RulesetAttr { handled_access_fs: ACCESS_FS_ALL_V1, handled_access_net: 0 };
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

    for p in ["/usr", "/etc", "/proc", "/var/cache/fontconfig"] {
        add_read(ruleset_fd, p);
    }
    for sub in [".cache/fontconfig", ".local/share/fonts", ".fonts"] {
        add_read(ruleset_fd, &format!("{}/{}", home, sub));
    }

    let rc = unsafe { libc::syscall(NR_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) };
    unsafe { libc::close(ruleset_fd) };
    rc == 0
}
