// The window manager's half of the window-policy API (api.rs), shared by
// DEMIURGE's X11 and Wayland stacks: Before a window maps, ask cherrypie
// where it goes. The socket and a reply-deadline timer sit in the caller's
// event loop; nothing here blocks. No cherrypie, or no answer within
// REPLY_WITHIN, and the window maps with the caller's own default.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::api::{self, NewWindow, Placement, Request, Response};

pub const REPLY_WITHIN: Duration = Duration::from_millis(100);

struct Waiting {
    id: u64,
    window: u32,
    deadline: Instant,
}

pub struct Policy {
    path: PathBuf,
    stream: Option<UnixStream>,
    buf: Vec<u8>,
    next_id: u64,
    waiting: Vec<Waiting>,
    timer_fd: RawFd,
}

pub enum Link {
    Down,
    // Just connected: The caller owes cherrypie the monitor list.
    Fresh,
    Up,
}

impl Policy {
    pub fn new(display: &str) -> Self {
        let timer_fd = unsafe {
            libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_NONBLOCK | libc::TFD_CLOEXEC)
        };
        Self {
            path: api::socket_path(display),
            stream: None,
            buf: Vec::new(),
            next_id: 1,
            waiting: Vec::new(),
            timer_fd,
        }
    }

    pub fn fd(&self) -> RawFd {
        self.stream.as_ref().map_or(-1, |s| s.as_raw_fd())
    }

    pub fn timer_fd(&self) -> RawFd {
        self.timer_fd
    }

    pub fn is_up(&self) -> bool {
        self.stream.is_some()
    }

    // Connect if not connected. A cherrypie that is not running costs one
    // failed connect(2) per new window.
    pub fn link(&mut self) -> Link {
        if self.stream.is_some() {
            return Link::Up;
        }
        match UnixStream::connect(&self.path) {
            Ok(s) if s.set_nonblocking(true).is_ok() => {
                eprintln!("[demiurge] policy: connected to {}", self.path.display());
                self.stream = Some(s);
                Link::Fresh
            }
            _ => Link::Down,
        }
    }

    // Fire and forget: The reply is read and dropped with the rest.
    pub fn send(&mut self, op: &str, path: &str, body: impl Serialize) -> Option<u64> {
        let stream = self.stream.as_mut()?;
        let id = self.next_id;
        self.next_id += 1;
        if stream.write_all(Request::new(id, op, path, body).line().as_bytes()).is_err() {
            self.stream = None;
            return None;
        }
        Some(id)
    }

    // Ask where `window` goes. False when it could not be asked; the caller
    // maps it at once.
    pub fn ask(&mut self, window: &NewWindow) -> bool {
        let Some(id) = self.send("POST", "/windows", window) else {
            return false;
        };
        self.waiting.push(Waiting { id, window: window.window, deadline: Instant::now() + REPLY_WITHIN });
        self.arm();
        true
    }

    // A window destroyed before its answer arrived.
    pub fn forget(&mut self, window: u32) {
        self.waiting.retain(|w| w.window != window);
        self.arm();
    }

    pub fn is_waiting(&self, window: u32) -> bool {
        self.waiting.iter().any(|w| w.window == window)
    }

    // Read what the socket holds. Returns the windows whose answers arrived;
    // on a lost connection, every waiting window with no placement.
    pub fn read(&mut self) -> Vec<(u32, Option<Placement>)> {
        let mut lost = false;
        if let Some(stream) = self.stream.as_mut() {
            let mut chunk = [0u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => {
                        lost = true;
                        break;
                    }
                    Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => {
                        lost = true;
                        break;
                    }
                }
            }
        }
        let mut answered = Vec::new();
        while let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=end).collect();
            let Ok(response) = serde_json::from_slice::<Response>(&line[..line.len() - 1]) else {
                continue;
            };
            if let Some(i) = self.waiting.iter().position(|w| w.id == response.id) {
                let w = self.waiting.remove(i);
                let placement = (response.status == 200)
                    .then(|| serde_json::from_value(response.body).ok())
                    .flatten();
                answered.push((w.window, placement));
            }
        }
        if lost {
            eprintln!("[demiurge] policy: cherrypie went away; placing windows myself");
            self.stream = None;
            self.buf.clear();
            answered.extend(self.waiting.drain(..).map(|w| (w.window, None)));
        }
        self.arm();
        answered
    }

    // The windows whose answers are overdue; they map with the default.
    pub fn expired(&mut self) -> Vec<u32> {
        let mut buf = [0u8; 8];
        unsafe { libc::read(self.timer_fd, buf.as_mut_ptr() as *mut libc::c_void, 8) };
        let now = Instant::now();
        let (late, waiting): (Vec<Waiting>, Vec<Waiting>) =
            std::mem::take(&mut self.waiting).into_iter().partition(|w| w.deadline <= now);
        self.waiting = waiting;
        self.arm();
        late.into_iter().map(|w| w.window).collect()
    }

    // Point the timer at the earliest deadline, or disarm it.
    fn arm(&self) {
        let next = self.waiting.iter().map(|w| w.deadline).min();
        let value = next.map_or(libc::timespec { tv_sec: 0, tv_nsec: 0 }, |d| {
            let left = d.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            libc::timespec { tv_sec: left.as_secs() as libc::time_t, tv_nsec: left.subsec_nanos() as libc::c_long }
        });
        let spec = libc::itimerspec { it_interval: libc::timespec { tv_sec: 0, tv_nsec: 0 }, it_value: value };
        unsafe { libc::timerfd_settime(self.timer_fd, 0, &spec, std::ptr::null_mut()) };
    }
}

impl Drop for Policy {
    fn drop(&mut self) {
        if self.timer_fd >= 0 {
            unsafe { libc::close(self.timer_fd) };
        }
    }
}
