// The API's transport: A non-blocking listening socket and its connections,
// read and answered from the daemon's poll loop. Requests are parsed here;
// what they mean is the daemon's business.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use crate::api::{Request, Response};

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    conns: Vec<Conn>,
}

struct Conn {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Server {
    // A socket file left by a cherrypie that died is replaced; one another
    // cherrypie still answers on is not, though the instance lock already
    // keeps a second daemon from getting this far.
    pub fn bind(path: &Path) -> std::io::Result<Self> {
        if path.exists() && UnixStream::connect(path).is_err() {
            let _ = std::fs::remove_file(path);
        }
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, path: path.to_path_buf(), conns: Vec::new() })
    }

    pub fn listener_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }

    pub fn conn_fds(&self) -> Vec<RawFd> {
        self.conns.iter().map(|c| c.stream.as_raw_fd()).collect()
    }

    pub fn connected(&self) -> bool {
        !self.conns.is_empty()
    }

    pub fn accept(&mut self) {
        while let Ok((stream, _)) = self.listener.accept() {
            if stream.set_nonblocking(true).is_ok() {
                eprintln!("[cherrypie] api: client connected; X11 actions stand down");
                self.conns.push(Conn { stream, buf: Vec::new() });
            }
        }
    }

    // Read whatever the ready connections hold and answer each complete
    // request with `answer`. A connection that closed or errored is dropped.
    pub fn service(&mut self, ready: &[RawFd], mut answer: impl FnMut(&Request) -> Response) {
        let mut closed = Vec::new();
        for (i, conn) in self.conns.iter_mut().enumerate() {
            if !ready.contains(&conn.stream.as_raw_fd()) {
                continue;
            }
            let mut chunk = [0u8; 4096];
            loop {
                match conn.stream.read(&mut chunk) {
                    Ok(0) => {
                        closed.push(i);
                        break;
                    }
                    Ok(n) => conn.buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => {
                        closed.push(i);
                        break;
                    }
                }
            }
            while let Some(end) = conn.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = conn.buf.drain(..=end).collect();
                let response = match serde_json::from_slice::<Request>(&line[..line.len() - 1]) {
                    Ok(request) => answer(&request),
                    Err(e) => Response::new(0, 400, format!("unparsable request: {e}")),
                };
                if conn.stream.write_all(response.line().as_bytes()).is_err() && !closed.contains(&i) {
                    closed.push(i);
                }
            }
        }
        let any_closed = !closed.is_empty();
        for i in closed.into_iter().rev() {
            self.conns.remove(i);
            eprintln!("[cherrypie] api: client disconnected");
        }
        if any_closed && !self.connected() {
            eprintln!("[cherrypie] api: no client; X11 actions resume");
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
