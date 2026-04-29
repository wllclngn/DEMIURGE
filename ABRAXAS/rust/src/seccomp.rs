//! seccomp-bpf syscall whitelist for ABRAXAS daemon.
//!
//! Restricts the process to only the syscalls needed for the event loop.
//! Uses raw BPF instructions + prctl(PR_SET_SECCOMP). No libseccomp.
//!
//! SECCOMP_RET_KILL_PROCESS on any syscall not in the whitelist.

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

// Architecture
const AUDIT_ARCH_X86_64: u32 = 0xc000003e;

// seccomp_data offsets
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

/// Syscall numbers (x86_64) -- from asm/unistd_64.h
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
    pub const SCHED_YIELD: u32 = 24;
    pub const MREMAP: u32 = 25;
    pub const MADVISE: u32 = 28;
    pub const DUP2: u32 = 33;
    pub const NANOSLEEP: u32 = 35;
    pub const GETPID: u32 = 39;
    pub const SOCKET: u32 = 41;
    pub const CONNECT: u32 = 42;
    pub const SENDTO: u32 = 44;
    pub const RECVFROM: u32 = 45;
    pub const SENDMSG: u32 = 46;
    pub const RECVMSG: u32 = 47;
    pub const RECVMMSG: u32 = 299;
    pub const SENDMMSG: u32 = 307;
    pub const SHUTDOWN: u32 = 48;
    pub const BIND: u32 = 49;
    pub const GETSOCKNAME: u32 = 51;
    pub const GETPEERNAME: u32 = 52;
    pub const SETSOCKOPT: u32 = 54;
    pub const GETSOCKOPT: u32 = 55;
    pub const CLONE: u32 = 56;
    pub const EXECVE: u32 = 59;
    pub const EXIT: u32 = 60;
    pub const WAIT4: u32 = 61;
    pub const KILL: u32 = 62;
    pub const UNAME: u32 = 63;
    pub const FCNTL: u32 = 72;
    pub const GETCWD: u32 = 79;
    pub const MKDIR: u32 = 83;
    pub const UNLINK: u32 = 87;
    pub const READLINK: u32 = 89;
    pub const GETTIMEOFDAY: u32 = 96;
    pub const GETUID: u32 = 102;
    pub const GETGID: u32 = 104;
    pub const GETEUID: u32 = 107;
    pub const GETEGID: u32 = 108;
    pub const SIGALTSTACK: u32 = 131;
    pub const PRCTL: u32 = 157;
    pub const ARCH_PRCTL: u32 = 158;
    pub const FUTEX: u32 = 202;
    pub const SCHED_GETAFFINITY: u32 = 204;
    pub const GETDENTS64: u32 = 217;
    pub const SET_TID_ADDRESS: u32 = 218;
    pub const CLOCK_GETTIME: u32 = 228;
    pub const CLOCK_NANOSLEEP: u32 = 230;
    pub const EXIT_GROUP: u32 = 231;
    pub const INOTIFY_ADD_WATCH: u32 = 254;
    pub const OPENAT: u32 = 257;
    pub const MKDIRAT: u32 = 258;
    pub const NEWFSTATAT: u32 = 262;
    pub const UNLINKAT: u32 = 263;
    pub const READLINKAT: u32 = 267;
    pub const PPOLL: u32 = 271;
    pub const SET_ROBUST_LIST: u32 = 273;
    pub const EPOLL_WAIT: u32 = 232;
    pub const EPOLL_CTL: u32 = 233;
    pub const SIGNALFD4: u32 = 289;
    pub const EVENTFD2: u32 = 290;
    pub const EPOLL_CREATE1: u32 = 291;
    pub const EPOLL_PWAIT: u32 = 281;
    pub const DUP3: u32 = 292;
    pub const PIPE2: u32 = 293;
    pub const INOTIFY_INIT1: u32 = 294;
    pub const PRLIMIT64: u32 = 302;
    pub const GETRANDOM: u32 = 318;
    pub const STATX: u32 = 332;
    pub const RSEQ: u32 = 334;
    pub const IO_URING_SETUP: u32 = 425;
    pub const IO_URING_ENTER: u32 = 426;
    pub const IO_URING_REGISTER: u32 = 427;
    pub const CLONE3: u32 = 435;
    pub const FACCESSAT2: u32 = 439;
}

pub fn install_filter() -> bool {
    // Each ALLOW_SYSCALL expands to 2 instructions: JEQ + RET_ALLOW
    let filter: &[SockFilter] = &[
        // Load architecture
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, OFFSET_ARCH),
        // Verify x86_64 -- kill if wrong arch
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        // Load syscall number
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, OFFSET_NR),

        // --- Core I/O ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::READ, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::WRITE, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::OPENAT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::CLOSE, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::FSTAT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::NEWFSTATAT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::LSEEK, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::PREAD64, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- Memory ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::MMAP, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::MUNMAP, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::MPROTECT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::BRK, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::MREMAP, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::MADVISE, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- io_uring ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::IO_URING_SETUP, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::IO_URING_ENTER, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::IO_URING_REGISTER, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- Time ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::CLOCK_GETTIME, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::CLOCK_NANOSLEEP, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::NANOSLEEP, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETTIMEOFDAY, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- ioctl (DRM gamma + inotify) ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::IOCTL, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- Process spawn (weather via curl) ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::CLONE3, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::CLONE, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::EXECVE, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::PIPE2, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::DUP2, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::DUP3, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::WAIT4, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SET_ROBUST_LIST, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::RSEQ, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::PRLIMIT64, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::ARCH_PRCTL, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SET_TID_ADDRESS, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- Signals ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::RT_SIGPROCMASK, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::RT_SIGACTION, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::RT_SIGRETURN, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SIGALTSTACK, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- File ops ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::UNLINK, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::UNLINKAT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::MKDIR, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::MKDIRAT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::ACCESS, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::FACCESSAT2, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::FCNTL, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETCWD, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::READLINK, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::READLINKAT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::STATX, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETRANDOM, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- Process info ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETPID, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETUID, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETEUID, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETGID, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETEGID, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::KILL, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::PRCTL, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::FUTEX, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- Exit ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::EXIT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::EXIT_GROUP, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- Event fds (inotify + signalfd) ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SIGNALFD4, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::INOTIFY_INIT1, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::INOTIFY_ADD_WATCH, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- Socket I/O (X11/Wayland backend, curl child) ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SOCKET, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::CONNECT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::BIND, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SETSOCKOPT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETSOCKOPT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SHUTDOWN, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SENDTO, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SENDMSG, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SENDMMSG, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::RECVFROM, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::RECVMSG, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::RECVMMSG, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETPEERNAME, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETSOCKNAME, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::POLL, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::PPOLL, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::WRITEV, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::UNAME, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- epoll + eventfd (curl child process) ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::EPOLL_CREATE1, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::EPOLL_CTL, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::EPOLL_WAIT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::EPOLL_PWAIT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::EVENTFD2, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- dlopen (backend loading) ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::GETDENTS64, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // --- Rust-specific (allocator, runtime) ---
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SCHED_YIELD, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr::SCHED_GETAFFINITY, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),

        // Default: KILL
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
    ];

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
