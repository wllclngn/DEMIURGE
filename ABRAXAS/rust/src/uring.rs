//! Raw io_uring implementation via syscall().
//!
//! No liburing dependency. Talks directly to the kernel through:
//!   - syscall(__NR_io_uring_setup, entries, &params)
//!   - syscall(__NR_io_uring_enter, ring_fd, to_submit, min_complete, flags, ...)
//!   - mmap for SQ/CQ ring buffers and SQE array
//!
//! Memory ordering: acquire/release fences around shared ring indices.
//! The kernel writes cq_tail and reads sq_tail; userspace does the inverse.

use std::sync::atomic::{fence, Ordering};

// Syscall numbers (x86_64)
const NR_IO_URING_SETUP: libc::c_long = 425;
const NR_IO_URING_ENTER: libc::c_long = 426;

// mmap offsets
const IORING_OFF_SQ_RING: i64 = 0;
const IORING_OFF_CQ_RING: i64 = 0x8000000;
const IORING_OFF_SQES: i64 = 0x10000000;

// io_uring_enter flags
const IORING_ENTER_GETEVENTS: u32 = 1;

// Opcodes (from enum in linux/io_uring.h)
const IORING_OP_POLL_ADD: u8 = 6;
const IORING_OP_TIMEOUT: u8 = 11;
const IORING_OP_ASYNC_CANCEL: u8 = 14;

// Multi-shot poll (Linux 5.13+) -- sqe.len flag
const IORING_POLL_ADD_MULTI: u32 = 1 << 0;

// CQE flags
pub const IORING_CQE_F_MORE: u32 = 1 << 1;

// Event tags
pub const EV_INOTIFY: u64 = 1;
pub const EV_SIGNAL: u64 = 2;
pub const EV_TIMEOUT: u64 = 3;
pub const EV_CANCEL: u64 = 4;
pub const EV_WEATHER: u64 = 5;

/// Kernel struct io_sqring_offsets (40 bytes)
#[repr(C)]
#[derive(Default)]
struct SqringOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    flags: u32,
    dropped: u32,
    array: u32,
    resv1: u32,
    user_addr: u64,
}

/// Kernel struct io_cqring_offsets (40 bytes)
#[repr(C)]
#[derive(Default)]
struct CqringOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    overflow: u32,
    cqes: u32,
    flags: u32,
    resv1: u32,
    user_addr: u64,
}

/// Kernel struct io_uring_params (120 bytes)
#[repr(C)]
#[derive(Default)]
struct IoUringParams {
    sq_entries: u32,
    cq_entries: u32,
    flags: u32,
    sq_thread_cpu: u32,
    sq_thread_idle: u32,
    features: u32,
    wq_fd: u32,
    resv: [u32; 3],
    sq_off: SqringOffsets,
    cq_off: CqringOffsets,
}

/// Kernel struct io_uring_sqe (64 bytes) -- flat layout, access fields directly
#[repr(C)]
pub struct IoUringSqe {
    pub opcode: u8,
    pub flags: u8,
    pub ioprio: u16,
    pub fd: i32,
    pub off: u64,       // union: off / addr2
    pub addr: u64,      // union: addr / splice_off_in
    pub len: u32,
    pub rw_flags: u32,  // union: poll32_events / timeout_flags / etc
    pub user_data: u64,
    pub buf_index: u16,
    pub personality: u16,
    pub splice_fd_in: i32,
    pub addr3: u64,
    pub _pad2: [u64; 1],
}

/// Kernel struct io_uring_cqe (16 bytes)
#[repr(C)]
pub struct IoUringCqe {
    pub user_data: u64,
    pub res: i32,
    pub flags: u32,
}

/// Kernel struct __kernel_timespec
#[repr(C)]
pub struct KernelTimespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

pub struct AbraxasRing {
    ring_fd: i32,

    // Submission ring
    sq_ring_ptr: *mut u8,
    sq_ring_size: usize,
    sq_head: *mut u32,
    sq_tail: *mut u32,
    sq_mask: *mut u32,
    sq_array: *mut u32,
    sq_entries: u32,
    sqes: *mut IoUringSqe,
    sqes_size: usize,

    // Completion ring
    cq_ring_ptr: *mut u8,
    cq_ring_size: usize,
    cq_head: *mut u32,
    cq_tail: *mut u32,
    cq_mask: *mut u32,
    cqes: *mut IoUringCqe,
}

impl AbraxasRing {
    pub fn init(entries: u32) -> Option<Self> {
        let mut params = IoUringParams::default();

        let fd = unsafe {
            libc::syscall(NR_IO_URING_SETUP, entries, &mut params as *mut IoUringParams)
        } as i32;
        if fd < 0 {
            return None;
        }

        // Map SQ ring
        let sq_ring_size =
            params.sq_off.array as usize + params.sq_entries as usize * std::mem::size_of::<u32>();
        let sq_ring_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                sq_ring_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_POPULATE,
                fd,
                IORING_OFF_SQ_RING,
            )
        };
        if sq_ring_ptr == libc::MAP_FAILED {
            unsafe { libc::close(fd) };
            return None;
        }
        let sq = sq_ring_ptr as *mut u8;

        // Map SQE array
        let sqes_size = params.sq_entries as usize * std::mem::size_of::<IoUringSqe>();
        let sqes_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                sqes_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_POPULATE,
                fd,
                IORING_OFF_SQES,
            )
        };
        if sqes_ptr == libc::MAP_FAILED {
            unsafe {
                libc::munmap(sq_ring_ptr, sq_ring_size);
                libc::close(fd);
            }
            return None;
        }

        // Map CQ ring
        let cq_ring_size = params.cq_off.cqes as usize
            + params.cq_entries as usize * std::mem::size_of::<IoUringCqe>();
        let cq_ring_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                cq_ring_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_POPULATE,
                fd,
                IORING_OFF_CQ_RING,
            )
        };
        if cq_ring_ptr == libc::MAP_FAILED {
            unsafe {
                libc::munmap(sqes_ptr, sqes_size);
                libc::munmap(sq_ring_ptr, sq_ring_size);
                libc::close(fd);
            }
            return None;
        }
        let cq = cq_ring_ptr as *mut u8;

        Some(AbraxasRing {
            ring_fd: fd,
            sq_ring_ptr: sq,
            sq_ring_size,
            sq_head: unsafe { sq.add(params.sq_off.head as usize) as *mut u32 },
            sq_tail: unsafe { sq.add(params.sq_off.tail as usize) as *mut u32 },
            sq_mask: unsafe { sq.add(params.sq_off.ring_mask as usize) as *mut u32 },
            sq_array: unsafe { sq.add(params.sq_off.array as usize) as *mut u32 },
            sq_entries: params.sq_entries,
            sqes: sqes_ptr as *mut IoUringSqe,
            sqes_size,
            cq_ring_ptr: cq,
            cq_ring_size,
            cq_head: unsafe { cq.add(params.cq_off.head as usize) as *mut u32 },
            cq_tail: unsafe { cq.add(params.cq_off.tail as usize) as *mut u32 },
            cq_mask: unsafe { cq.add(params.cq_off.ring_mask as usize) as *mut u32 },
            cqes: unsafe { cq.add(params.cq_off.cqes as usize) as *mut IoUringCqe },
        })
    }

    /// Get next SQE slot, zeroed.
    fn get_sqe(&mut self) -> Option<*mut IoUringSqe> {
        unsafe {
            let tail = *self.sq_tail;
            let head = *self.sq_head;

            if tail - head >= self.sq_entries {
                return None; // Ring full
            }

            let idx = tail & *self.sq_mask;
            *self.sq_array.add(idx as usize) = idx;

            let sqe = self.sqes.add(idx as usize);
            std::ptr::write_bytes(sqe as *mut u8, 0, std::mem::size_of::<IoUringSqe>());
            Some(sqe)
        }
    }

    /// Commit one SQE by advancing sq_tail.
    fn commit_sqe(&mut self) {
        fence(Ordering::Release);
        unsafe { *self.sq_tail += 1 };
    }

    /// Multi-shot POLL_ADD: fd stays monitored until closed or cancelled.
    pub fn prep_poll(&mut self, fd: i32, user_data: u64) {
        if let Some(sqe) = self.get_sqe() {
            unsafe {
                (*sqe).opcode = IORING_OP_POLL_ADD;
                (*sqe).fd = fd;
                (*sqe).len = IORING_POLL_ADD_MULTI;
                (*sqe).rw_flags = libc::POLLIN as u32;
                (*sqe).user_data = user_data;
            }
            self.commit_sqe();
        }
    }

    pub fn prep_timeout(&mut self, ts: &KernelTimespec, user_data: u64) {
        if let Some(sqe) = self.get_sqe() {
            unsafe {
                (*sqe).opcode = IORING_OP_TIMEOUT;
                (*sqe).fd = -1;
                (*sqe).addr = ts as *const KernelTimespec as u64;
                (*sqe).len = 1; // 1 timespec entry; event count is sqe.off (0 = pure timeout)
                (*sqe).user_data = user_data;
            }
            self.commit_sqe();
        }
    }

    pub fn prep_cancel(&mut self, target_user_data: u64, user_data: u64) {
        if let Some(sqe) = self.get_sqe() {
            unsafe {
                (*sqe).opcode = IORING_OP_ASYNC_CANCEL;
                (*sqe).fd = -1;
                (*sqe).addr = target_user_data;
                (*sqe).user_data = user_data;
            }
            self.commit_sqe();
        }
    }

    pub fn submit_and_wait(&mut self) -> i32 {
        unsafe {
            let tail = *self.sq_tail;
            fence(Ordering::Acquire);
            let head = *self.sq_head;

            let to_submit = tail - head;
            if to_submit == 0 {
                return 0;
            }

            let ret = libc::syscall(
                NR_IO_URING_ENTER,
                self.ring_fd,
                to_submit,
                1u32, // min_complete
                IORING_ENTER_GETEVENTS,
                std::ptr::null::<libc::c_void>(),
                0usize,
            ) as i32;

            if ret < 0 && *libc::__errno_location() == libc::EINTR {
                return 0;
            }
            ret
        }
    }

    pub fn peek_cqe(&self) -> Option<&IoUringCqe> {
        unsafe {
            let head = *self.cq_head;
            fence(Ordering::Acquire);
            let tail = *self.cq_tail;

            if head == tail {
                return None;
            }

            let idx = head & *self.cq_mask;
            Some(&*self.cqes.add(idx as usize))
        }
    }

    pub fn cqe_seen(&mut self) {
        fence(Ordering::Release);
        unsafe { *self.cq_head += 1 };
    }
}

impl Drop for AbraxasRing {
    fn drop(&mut self) {
        unsafe {
            if !self.sqes.is_null() {
                libc::munmap(self.sqes as *mut libc::c_void, self.sqes_size);
            }
            if !self.sq_ring_ptr.is_null() {
                libc::munmap(self.sq_ring_ptr as *mut libc::c_void, self.sq_ring_size);
            }
            if !self.cq_ring_ptr.is_null() {
                libc::munmap(self.cq_ring_ptr as *mut libc::c_void, self.cq_ring_size);
            }
            if self.ring_fd >= 0 {
                libc::close(self.ring_fd);
            }
        }
    }
}
