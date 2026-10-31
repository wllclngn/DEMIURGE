//! ZIP code database lookup.
//!
//! mmap'd binary search on us_zipcodes.bin.
//! Entry format: 5 bytes ASCII ZIP + 4 bytes f32 lat + 4 bytes f32 lon.
//! File header: 4 bytes u32 count (little-endian).

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::Path;

const ENTRY_SIZE: usize = 13; // 5 + 4 + 4
const HEADER_SIZE: usize = 4; // u32 count

pub fn lookup(db_path: &Path, zipcode: &str) -> Option<(f32, f32)> {
    // Normalize to exactly 5 digits
    let mut zip5 = [b'0'; 5];
    let bytes = zipcode.as_bytes();
    let len = bytes.len().min(5);
    zip5[5 - len..].copy_from_slice(&bytes[..len]);

    let file = File::open(db_path).ok()?;
    let file_size = file.metadata().ok()?.len() as usize;

    if file_size < HEADER_SIZE {
        return None;
    }

    // mmap the file
    let data = unsafe {
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            file_size,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        );
        if ptr == libc::MAP_FAILED {
            return None;
        }
        std::slice::from_raw_parts(ptr as *const u8, file_size)
    };

    // Read entry count (little-endian u32)
    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;

    // Binary search
    let mut low: usize = 0;
    let mut high = count.wrapping_sub(1);
    let mut result = None;

    while low <= high && high < count {
        let mid = low + (high - low) / 2;
        let offset = HEADER_SIZE + mid * ENTRY_SIZE;

        if offset + ENTRY_SIZE > file_size {
            break;
        }

        let entry_zip = &data[offset..offset + 5];
        match entry_zip.cmp(&zip5) {
            std::cmp::Ordering::Equal => {
                let lat = f32::from_le_bytes([
                    data[offset + 5],
                    data[offset + 6],
                    data[offset + 7],
                    data[offset + 8],
                ]);
                let lon = f32::from_le_bytes([
                    data[offset + 9],
                    data[offset + 10],
                    data[offset + 11],
                    data[offset + 12],
                ]);
                result = Some((lat, lon));
                break;
            }
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => {
                if mid == 0 {
                    break;
                }
                high = mid - 1;
            }
        }
    }

    // munmap
    unsafe {
        libc::munmap(data.as_ptr() as *mut libc::c_void, file_size);
    }

    result
}
