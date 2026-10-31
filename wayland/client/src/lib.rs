// What DEMIURGE's Wayland binaries share as clients of the compositor: The
// keyboard read through xkbcommon with key repeat, a Cairo frame drawn
// straight into a wl_shm buffer, the event loop, and the monitors' frames
// copied through ext-image-copy-capture. smithay-client-toolkit binds the
// globals and keeps the outputs, the seat and the shm pool.

use std::os::fd::{AsRawFd, RawFd};
use std::time::Instant;

use wayland_client::backend::WaylandError;
use wayland_client::{EventQueue, QueueHandle};

pub mod capture;
pub mod keyboard;
pub mod shm;

/// What `run` waits for after a round of events.
pub enum Next {
    /// The compositor's next event, or the deadline, whichever comes first.
    Wait(Option<Instant>),
    Done,
}

/// Dispatch `queue` into `state`, calling `step` after each round of events,
/// until `step` is done. Between rounds the loop sleeps until the
/// compositor writes, one of the binary's own `fds` (-1 for none) is
/// readable, or the deadline `step` names (a held key's repeat) passes;
/// `step` is told which of `fds` woke it.
pub fn run<S: 'static>(
    queue: &mut EventQueue<S>,
    state: &mut S,
    fds: &[RawFd],
    mut step: impl FnMut(&mut S, &QueueHandle<S>, &[bool]) -> Next,
) -> Result<(), String> {
    let qh = queue.handle();
    let mut woke = vec![false; fds.len()];
    loop {
        queue.dispatch_pending(state).map_err(|e| format!("dispatch: {}", e))?;
        // What arrived while a handler ran (a greeter waiting on PAM) is read
        // before `step` acts on the clock: A key let go meanwhile must not
        // repeat.
        if let Some(guard) = queue.prepare_read() {
            let mut fd = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut fd, 1, 0) } > 0 {
                match guard.read() {
                    Ok(_) => {}
                    Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(format!("read: {}", e)),
                }
            }
            queue.dispatch_pending(state).map_err(|e| format!("dispatch: {}", e))?;
        }
        let next = step(state, &qh, &woke);
        woke.fill(false);
        let deadline = match next {
            Next::Done => return Ok(()),
            Next::Wait(deadline) => deadline,
        };
        queue.flush().map_err(|e| format!("flush: {}", e))?;
        // None: Events were queued meanwhile, to be dispatched first.
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        // Rounded up, so a deadline less than a millisecond off is slept
        // to rather than spun on.
        let timeout = deadline.map_or(-1, |at| {
            let left = at.saturating_duration_since(Instant::now());
            left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
        });
        let mut polled: Vec<libc::pollfd> = std::iter::once(guard.connection_fd().as_raw_fd())
            .chain(fds.iter().copied())
            .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
            .collect();
        match unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, timeout) } {
            n if n > 0 => {
                for (woke, fd) in woke.iter_mut().zip(&polled[1..]) {
                    *woke = fd.revents & libc::POLLIN != 0;
                }
                // A wakeup with nothing to read yet is no error.
                if polled[0].revents != 0 {
                    match guard.read() {
                        Ok(_) => {}
                        Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => return Err(format!("read: {}", e)),
                    }
                }
            }
            0 => {}
            _ => {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    return Err(format!("poll: {}", e));
                }
            }
        }
    }
}
