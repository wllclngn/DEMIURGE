// greetd JSON-IPC client. Drives gordian_knot's --login mode.
//
// Protocol: each message is a 4-byte little-endian u32 length prefix
// followed by that many bytes of JSON. Both directions use the same
// envelope. We connect to $GREETD_SOCK (a UNIX stream).
//
// Spec: https://man.sr.ht/~kennylevinsen/greetd/
//
// Conversation:
//   greeter -> CreateSession{username}
//   greetd  -> Success | AuthMessage{type, message} | Error
//   ...repeated for each prompt...
//   greeter -> PostAuthMessageResponse{response: Some(text) | None}
//   greetd  -> Success | AuthMessage | Error
//   greeter -> StartSession{cmd: Vec<String>, env: Vec<String>}
//   greetd  -> Success | Error
//
// PAM-as-state-machine: greetd hides the libpam dance behind these
// messages. We reuse the locker's input reader (read_password_line)
// for any AuthMessage with type "secret" and a plain stdin/Cairo
// prompt for "visible". gordian_knot doesn't open PAM directly in
// --login mode -- greetd does, on our behalf.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

const SOCK_ENV: &str = "GREETD_SOCK";
// Read timeout protects the greeter against a stuck greetd. 10s is
// generous; auth messages typically arrive within milliseconds.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

// Outgoing messages. Tagged with #[serde(rename_all = "snake_case",
// tag = "type")] to match greetd's wire format:
//   {"type": "create_session", "username": "..."}
#[derive(Serialize, Debug)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Request {
    CreateSession {
        username: String,
    },
    PostAuthMessageResponse {
        response: Option<String>,
    },
    StartSession {
        cmd: Vec<String>,
        env: Vec<String>,
    },
    CancelSession,
}

// Incoming messages. greetd's responses use the same tag-on-type
// shape. AuthMessageType is one of "visible" | "secret" | "info" |
// "error" -- visible/secret are interactive prompts the greeter must
// echo to the user; info/error are status messages the greeter can
// display before re-prompting.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Response {
    Success,
    Error {
        error_type: ErrorType,
        description: String,
    },
    AuthMessage {
        auth_message_type: AuthMessageType,
        auth_message: String,
    },
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    AuthError,
    Error,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMessageType {
    Visible,
    Secret,
    Info,
    Error,
}

pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// Connect to `$GREETD_SOCK`. Returns Err if the env var is unset
    /// (i.e., gordian_knot was invoked outside a greetd-managed
    /// session) or the socket can't be reached.
    pub fn connect_default() -> io::Result<Self> {
        let path = std::env::var(SOCK_ENV).map_err(|_| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} not set; not running under greetd?", SOCK_ENV),
            )
        })?;
        Self::connect(&path)
    }

    /// Connect to an explicit socket path. Used by tests against a
    /// mock greetd; real callers use `connect_default`.
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(READ_TIMEOUT))?;
        Ok(Self { stream })
    }

    pub fn send(&mut self, req: &Request) -> io::Result<()> {
        let body = serde_json::to_vec(req)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let len = u32::try_from(body.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "request body > 4GiB")
        })?;
        self.stream.write_all(&len.to_le_bytes())?;
        self.stream.write_all(&body)?;
        self.stream.flush()
    }

    pub fn recv(&mut self) -> io::Result<Response> {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut body = vec![0u8; len];
        self.stream.read_exact(&mut body)?;
        serde_json::from_slice(&body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// One-shot helper: send a request and read the next response.
    pub fn round_trip(&mut self, req: &Request) -> io::Result<Response> {
        self.send(req)?;
        self.recv()
    }
}
