//! Landlock filesystem sandbox for ABRAXAS daemon.
//!
//! After init, restricts filesystem access to only what the daemon needs.
//! Uses raw landlock syscalls via libc::syscall(). No library dependency.
//! Gracefully fails on kernels without landlock support (pre-5.13).

use std::ffi::CString;

// Syscall numbers (x86_64)
const NR_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const NR_LANDLOCK_ADD_RULE: libc::c_long = 445;
const NR_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

// landlock constants
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

// Filesystem access flags
const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;
const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_FS_MAKE_REG: u64 = 1 << 8;

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

fn add_path_rule(ruleset_fd: i32, path: &str, access: u64) -> bool {
    let c_path = match CString::new(path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        return false;
    }

    let rule = PathBeneathAttr {
        allowed_access: access,
        parent_fd: fd,
    };

    let ret = unsafe {
        libc::syscall(
            NR_LANDLOCK_ADD_RULE,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH,
            &rule as *const PathBeneathAttr,
            0u32,
        )
    };

    unsafe { libc::close(fd) };
    ret == 0
}

pub fn install_sandbox(config_dir: &str) -> bool {
    // Check kernel support
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

    // Define handled access types
    let attr = RulesetAttr {
        handled_access_fs: ACCESS_FS_READ_FILE
            | ACCESS_FS_READ_DIR
            | ACCESS_FS_WRITE_FILE
            | ACCESS_FS_REMOVE_FILE
            | ACCESS_FS_MAKE_REG
            | ACCESS_FS_MAKE_DIR
            | ACCESS_FS_EXECUTE,
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

    // ~/.config/abraxas/ -- full read/write
    let config_access =
        ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR | ACCESS_FS_WRITE_FILE
        | ACCESS_FS_REMOVE_FILE | ACCESS_FS_MAKE_REG | ACCESS_FS_MAKE_DIR;
    add_path_rule(ruleset_fd, config_dir, config_access);

    // /dev -- read for DRM ioctls
    let read_only = ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;
    add_path_rule(ruleset_fd, "/dev", read_only);

    // /proc -- read for process info
    add_path_rule(ruleset_fd, "/proc", read_only);

    // /usr -- execute for curl, read for shared libs
    add_path_rule(ruleset_fd, "/usr", read_only | ACCESS_FS_EXECUTE);

    // /etc -- read for timezone, resolver
    add_path_rule(ruleset_fd, "/etc", read_only);

    // /lib, /lib64 -- shared libraries
    add_path_rule(ruleset_fd, "/lib", read_only);
    add_path_rule(ruleset_fd, "/lib64", read_only);

    // /tmp -- curl temp files
    add_path_rule(ruleset_fd, "/tmp",
        ACCESS_FS_READ_FILE | ACCESS_FS_WRITE_FILE | ACCESS_FS_MAKE_REG);

    // Enforce
    let ret = unsafe {
        libc::syscall(NR_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32)
    } as i32;
    unsafe { libc::close(ruleset_fd) };

    ret == 0
}
