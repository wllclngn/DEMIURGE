// seccomp-bpf filter for GORDIAN KNOT's UI child, installed after its first
// accepted frame: By then the X connection, the fonts and the config are
// already held, so what remains is drawing, sockets and time.
//
// Raw BPF + prctl(PR_SET_SECCOMP). No libseccomp.
//
// An unlisted syscall TRAPs instead of killing: The child's own SIGSYS
// handler logs "seccomp: denied syscall N" and returns -EPERM, so a gap in
// the list degrades one call and leaves a line the QEMU suite fails on,
// rather than a dead renderer. clone is allowed only with CLONE_THREAD (a
// thread, never a process); clone3 answers ENOSYS, because its flags sit
// behind a pointer a filter cannot read, and glibc then falls back to
// clone. No exec, no fork.
//
// x86_64 only; any other arch is killed outright.

const BPF_LD: u16 = 0x00;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JEQ: u16 = 0x10;
const BPF_JSET: u16 = 0x40;
const BPF_K: u16 = 0x00;

const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_TRAP: u32 = 0x0003_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_MODE_FILTER: libc::c_int = 2;

const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const OFFSET_ARG0_LO: u32 = 16;

const NR_CLONE: u32 = 56;
const NR_CLONE3: u32 = 435;
const CLONE_THREAD: u32 = 0x0001_0000;

// x86_64 numbers (asm/unistd_64.h): What Cairo, Pango, fontconfig, glib's
// threads and the x11rb socket need once the child is initialized.
const ALLOWED: &[u32] = &[
    0,   // read
    1,   // write
    3,   // close
    5,   // fstat
    7,   // poll
    8,   // lseek
    9,   // mmap
    10,  // mprotect
    11,  // munmap
    12,  // brk
    13,  // rt_sigaction
    14,  // rt_sigprocmask
    15,  // rt_sigreturn
    17,  // pread64
    19,  // readv
    20,  // writev
    21,  // access
    24,  // sched_yield
    25,  // mremap
    28,  // madvise
    35,  // nanosleep
    39,  // getpid
    44,  // sendto
    45,  // recvfrom
    46,  // sendmsg
    47,  // recvmsg
    60,  // exit
    63,  // uname
    72,  // fcntl
    79,  // getcwd
    89,  // readlink
    96,  // gettimeofday
    102, // getuid
    104, // getgid
    107, // geteuid
    108, // getegid
    110, // getppid
    131, // sigaltstack
    137, // statfs
    138, // fstatfs
    157, // prctl (thread names; NO_NEW_PRIVS is already set)
    186, // gettid
    202, // futex
    204, // sched_getaffinity
    217, // getdents64
    218, // set_tid_address
    219, // restart_syscall
    228, // clock_gettime
    229, // clock_getres
    230, // clock_nanosleep
    231, // exit_group
    232, // epoll_wait
    233, // epoll_ctl
    234, // tgkill
    257, // openat
    262, // newfstatat
    267, // readlinkat
    271, // ppoll
    273, // set_robust_list
    281, // epoll_pwait
    290, // eventfd2
    291, // epoll_create1
    293, // pipe2
    302, // prlimit64
    318, // getrandom
    324, // membarrier
    332, // statx
    334, // rseq
    439, // faccessat2
];

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

const fn stmt(code: u16, k: u32) -> SockFilter {
    SockFilter { code, jt: 0, jf: 0, k }
}

const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter { code, jt, jf, k }
}

fn program() -> Vec<SockFilter> {
    let mut f = vec![
        stmt(BPF_LD | BPF_W | BPF_ABS, OFFSET_ARCH),
        jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        stmt(BPF_LD | BPF_W | BPF_ABS, OFFSET_NR),
    ];
    for &nr in ALLOWED {
        f.push(jump(BPF_JMP | BPF_JEQ | BPF_K, nr, 0, 1));
        f.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    }
    f.push(jump(BPF_JMP | BPF_JEQ | BPF_K, NR_CLONE3, 0, 1));
    f.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | libc::ENOSYS as u32));
    f.push(jump(BPF_JMP | BPF_JEQ | BPF_K, NR_CLONE, 0, 3));
    f.push(stmt(BPF_LD | BPF_W | BPF_ABS, OFFSET_ARG0_LO));
    f.push(jump(BPF_JMP | BPF_JSET | BPF_K, CLONE_THREAD, 0, 1));
    f.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    f.push(stmt(BPF_RET | BPF_K, SECCOMP_RET_TRAP));
    f
}

// The denied syscall's number from siginfo's SIGSYS layout (si_call_addr,
// then si_syscall at offset 24 on x86_64), reported with write(2) only:
// Nothing here may allocate or lock.
extern "C" fn on_sigsys(_sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    unsafe {
        let nr = *((info as *const u8).add(24) as *const i32);
        let uc = ctx as *mut libc::ucontext_t;
        (*uc).uc_mcontext.gregs[libc::REG_RAX as usize] = -(libc::EPERM as i64);

        let prefix = b"[gordian_knot ui] seccomp: denied syscall ";
        let mut buf = [0u8; 64];
        buf[..prefix.len()].copy_from_slice(prefix);
        let mut n = prefix.len();
        let mut digits = [0u8; 10];
        let mut d = 0;
        let mut v = nr.unsigned_abs();
        loop {
            digits[d] = b'0' + (v % 10) as u8;
            d += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        while d > 0 {
            d -= 1;
            buf[n] = digits[d];
            n += 1;
        }
        buf[n] = b'\n';
        libc::write(2, buf.as_ptr() as *const libc::c_void, n + 1);
    }
}

pub fn install_filter() -> bool {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigsys as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(libc::SIGSYS, &sa, std::ptr::null_mut()) != 0 {
            return false;
        }
    }
    let filter = program();
    let prog = SockFprog { len: filter.len() as u16, filter: filter.as_ptr() };
    unsafe { libc::prctl(libc::PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog as *const SockFprog) == 0 }
}
