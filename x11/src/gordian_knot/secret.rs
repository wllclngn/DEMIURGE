// The password, held in one page mapped once: Locked out of swap, excluded
// from core dumps, wiped with explicit_bzero after every attempt and on
// drop. Typing never reallocates, so no freed buffer ever keeps a copy;
// a String's growth leaves every old buffer in the heap.
//
// The buffer always holds a NUL after the last byte, so PAM reads it in
// place without a CString copy.

use std::ffi::c_char;
use std::io;

const CAP: usize = 4096;

pub struct Secret {
    ptr: *mut u8,
    len: usize,
}

impl Secret {
    pub fn new() -> io::Result<Secret> {
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                CAP,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            // Best effort: mlock can fail on a tight RLIMIT_MEMLOCK, and
            // a lock that cannot pin its page still has to run.
            libc::mlock(ptr, CAP);
            libc::madvise(ptr, CAP, libc::MADV_DONTDUMP);
        }
        Ok(Secret { ptr: ptr as *mut u8, len: 0 })
    }

    // Append bytes; false (and nothing appended) when they do not fit.
    pub fn push(&mut self, bytes: &[u8]) -> bool {
        if self.len + bytes.len() >= CAP {
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.add(self.len), bytes.len());
            self.len += bytes.len();
            *self.ptr.add(self.len) = 0;
        }
        true
    }

    // Drop the last UTF-8 character.
    pub fn pop_char(&mut self) {
        while self.len > 0 {
            self.len -= 1;
            let b = unsafe { *self.ptr.add(self.len) };
            unsafe { *self.ptr.add(self.len) = 0 };
            if b & 0xC0 != 0x80 {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        unsafe { libc::explicit_bzero(self.ptr as *mut libc::c_void, CAP) };
        self.len = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn char_count(&self) -> usize {
        self.bytes().iter().filter(|&&b| b & 0xC0 != 0x80).count()
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_c_ptr(&self) -> *const c_char {
        self.ptr as *const c_char
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.clear();
        unsafe {
            libc::munlock(self.ptr as *const libc::c_void, CAP);
            libc::munmap(self.ptr as *mut libc::c_void, CAP);
        }
    }
}
