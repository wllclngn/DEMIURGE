// seccomp-bpf syscall allowlist for gordian_knot's unprivileged child.
//
// Raw BPF + prctl(PR_SET_SECCOMP). No libseccomp. Pattern lifted from
// ABRAXAS's seccomp.rs; allowlist trimmed to what a locker needs.
//
// Installed AFTER: privsep drop (setresuid/setresgid), X/VT fd acquisition,
// signalfd setup. Installed BEFORE: the main event loop.
//
// The allowlist is a superset of what a minimal lock UI would need because
// PAM internally fork/execs unix_chkpwd (setuid helper that reads /etc/shadow)
// and we can't restrict argv via plain BPF. Tightening further would require
// seccomp_notify + an unprivileged supervisor, which is out of scope here.

// BPF instruction encoding
const BPF_LD: u16 = 0x00;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;

// seccomp constants
const SECCOMP_RET_KILL_PROCESS: u32 = 0x80000000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff0000;
const SECCOMP_MODE_FILTER: libc::c_int = 2;

const AUDIT_ARCH_X86_64: u32 = 0xc000003e;

const OFFSET_ARCH: u32 = 4;
const OFFSET_NR: u32 = 0;

#[repr(C)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

const fn bpf_stmt(code: u16, k: u32) -> SockFilter {
    SockFilter { code, jt: 0, jf: 0, k }
}

const fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter { code, jt, jf, k }
}

// x86_64 syscall numbers (from asm/unistd_64.h). A subset of ABRAXAS's list,
// plus PAM-specific entries.
mod nr {
    pub const READ: u32 = 0;
    pub const WRITE: u32 = 1;
    pub const CLOSE: u32 = 3;
    pub const FSTAT: u32 = 5;
    pub const POLL: u32 = 7;
    pub const LSEEK: u32 = 8;
    pub const MMAP: u32 = 9;
    pub const MPROTECT: u32 = 10;
    pub const MUNMAP: u32 = 11;
    pub const BRK: u32 = 12;
    pub const RT_SIGACTION: u32 = 13;
    pub const RT_SIGPROCMASK: u32 = 14;
    pub const RT_SIGRETURN: u32 = 15;
    pub const IOCTL: u32 = 16;
    pub const PREAD64: u32 = 17;
    pub const WRITEV: u32 = 20;
    pub const ACCESS: u32 = 21;
    pub const PIPE: u32 = 22;
    pub const SELECT: u32 = 23;
    pub const SCHED_YIELD: u32 = 24;
    pub const MREMAP: u32 = 25;
    pub const MADVISE: u32 = 28;
    pub const DUP: u32 = 32;
    pub const DUP2: u32 = 33;
    pub const NANOSLEEP: u32 = 35;
    pub const GETPID: u32 = 39;
    pub const SOCKET: u32 = 41;
    pub const CONNECT: u32 = 42;
    pub const SENDTO: u32 = 44;
    pub const RECVFROM: u32 = 45;
    pub const SENDMSG: u32 = 46;
    pub const RECVMSG: u32 = 47;
    pub const SHUTDOWN: u32 = 48;
    pub const SETSOCKOPT: u32 = 54;
    pub const GETSOCKOPT: u32 = 55;
    pub const CLONE: u32 = 56;
    pub const FORK: u32 = 57;
    pub const VFORK: u32 = 58;
    pub const EXECVE: u32 = 59;
    pub const EXIT: u32 = 60;
    pub const WAIT4: u32 = 61;
    pub const KILL: u32 = 62;
    pub const UNAME: u32 = 63;
    pub const FCNTL: u32 = 72;
    pub const GETCWD: u32 = 79;
    pub const READLINK: u32 = 89;
    pub const GETTIMEOFDAY: u32 = 96;
    pub const GETUID: u32 = 102;
    pub const GETGID: u32 = 104;
    pub const GETEUID: u32 = 107;
    pub const GETEGID: u32 = 108;
    pub const GETPPID: u32 = 110;
    pub const GETPGID: u32 = 121;
    pub const SIGALTSTACK: u32 = 131;
    pub const ARCH_PRCTL: u32 = 158;
    pub const GETTID: u32 = 186;
    pub const FUTEX: u32 = 202;
    pub const SCHED_GETAFFINITY: u32 = 204;
    pub const GETDENTS64: u32 = 217;
    pub const SET_TID_ADDRESS: u32 = 218;
    pub const CLOCK_GETTIME: u32 = 228;
    pub const CLOCK_NANOSLEEP: u32 = 230;
    pub const EXIT_GROUP: u32 = 231;
    pub const EPOLL_WAIT: u32 = 232;
    pub const EPOLL_CTL: u32 = 233;
    pub const TGKILL: u32 = 234;
    pub const OPENAT: u32 = 257;
    pub const NEWFSTATAT: u32 = 262;
    pub const READLINKAT: u32 = 267;
    pub const PPOLL: u32 = 271;
    pub const SET_ROBUST_LIST: u32 = 273;
    pub const EPOLL_PWAIT: u32 = 281;
    pub const SIGNALFD4: u32 = 289;
    pub const EVENTFD2: u32 = 290;
    pub const EPOLL_CREATE1: u32 = 291;
    pub const DUP3: u32 = 292;
    pub const PIPE2: u32 = 293;
    pub const PRLIMIT64: u32 = 302;
    pub const GETRANDOM: u32 = 318;
    pub const STATX: u32 = 332;
    pub const RSEQ: u32 = 334;
    pub const CLONE3: u32 = 435;
    pub const FACCESSAT2: u32 = 439;
}

fn allow(nr: u32) -> [SockFilter; 2] {
    [
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    ]
}

pub fn install_filter() -> bool {
    let mut filter: Vec<SockFilter> = vec![
        // Arch check: kill if not x86_64.
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, OFFSET_ARCH),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        // Load syscall number.
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, OFFSET_NR),
    ];

    // Core I/O.
    for n in [nr::READ, nr::WRITE, nr::OPENAT, nr::CLOSE, nr::FSTAT, nr::NEWFSTATAT,
              nr::LSEEK, nr::PREAD64, nr::WRITEV, nr::DUP, nr::DUP2, nr::DUP3,
              nr::FCNTL, nr::IOCTL, nr::POLL, nr::PPOLL, nr::SELECT] {
        filter.extend(allow(n));
    }

    // Memory.
    for n in [nr::MMAP, nr::MUNMAP, nr::MPROTECT, nr::BRK, nr::MREMAP, nr::MADVISE] {
        filter.extend(allow(n));
    }

    // Time.
    for n in [nr::CLOCK_GETTIME, nr::CLOCK_NANOSLEEP, nr::NANOSLEEP, nr::GETTIMEOFDAY] {
        filter.extend(allow(n));
    }

    // Process/PAM (pam_unix forks unix_chkpwd).
    for n in [nr::CLONE, nr::CLONE3, nr::FORK, nr::VFORK, nr::EXECVE, nr::WAIT4,
              nr::EXIT, nr::EXIT_GROUP, nr::KILL, nr::TGKILL, nr::ARCH_PRCTL,
              nr::SET_TID_ADDRESS, nr::SET_ROBUST_LIST, nr::RSEQ, nr::PRLIMIT64] {
        filter.extend(allow(n));
    }

    // Process info.
    for n in [nr::GETPID, nr::GETPPID, nr::GETTID, nr::GETPGID, nr::UNAME,
              nr::GETUID, nr::GETGID, nr::GETEUID, nr::GETEGID] {
        filter.extend(allow(n));
    }

    // Signals + signalfd.
    for n in [nr::RT_SIGACTION, nr::RT_SIGPROCMASK, nr::RT_SIGRETURN, nr::SIGALTSTACK,
              nr::SIGNALFD4] {
        filter.extend(allow(n));
    }

    // File ops.
    for n in [nr::ACCESS, nr::FACCESSAT2, nr::READLINK, nr::READLINKAT, nr::GETCWD,
              nr::STATX, nr::GETDENTS64, nr::GETRANDOM, nr::PIPE, nr::PIPE2] {
        filter.extend(allow(n));
    }

    // Sockets (X11 connection, zbus D-Bus in daemon mode).
    for n in [nr::SOCKET, nr::CONNECT, nr::SETSOCKOPT, nr::GETSOCKOPT,
              nr::SENDTO, nr::SENDMSG, nr::RECVFROM, nr::RECVMSG, nr::SHUTDOWN] {
        filter.extend(allow(n));
    }

    // epoll (rust runtime, zbus).
    for n in [nr::EPOLL_CREATE1, nr::EPOLL_CTL, nr::EPOLL_WAIT, nr::EPOLL_PWAIT,
              nr::EVENTFD2] {
        filter.extend(allow(n));
    }

    // Futex + scheduler (Rust allocator / threading runtime).
    for n in [nr::FUTEX, nr::SCHED_YIELD, nr::SCHED_GETAFFINITY] {
        filter.extend(allow(n));
    }

    // Default action: kill.
    filter.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));

    let prog = SockFprog {
        len: filter.len() as u16,
        filter: filter.as_ptr(),
    };

    unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER,
            &prog as *const SockFprog,
        ) == 0
    }
}
