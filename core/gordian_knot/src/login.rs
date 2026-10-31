// gordian_knot --login: greetd greeter.
//
// `Login` is the conversation with greetd, which both greeters drive: The
// TTY greeter here, and the one drawn inside cage (greeter.rs).
//
// State machine:
//   1. Ask for the USERNAME (visible echo).
//   2. CreateSession{username} -> greetd.
//   3. Loop on greetd responses:
//        AuthMessage{secret}    -> ask masked, PostAuthMessageResponse{Some(text)}
//        AuthMessage{visible}   -> ask echoed, PostAuthMessageResponse{Some(text)}
//        AuthMessage{info|error}-> show as STATUS, PostAuthMessageResponse{None}
//        Success                -> break out of auth loop
//        Error{auth_error}      -> CancelSession, show msg, back to step 1
//        Error{error}           -> fatal
//   4. StartSession{cmd, env} -> greetd; greetd starts the session and
//      the greeter exits. On error here: fatal.
//
// Why no PAM in this mode: greetd owns the PAM stack. Our job is to
// shuttle prompts between the user and greetd (JSON over $GREETD_SOCK).
// The lock mode keeps direct PAM because there's no privilege separation
// needed in-session; login mode requires the privileged auth flow that
// greetd provides.
//
// The session command (what to start on success) comes from
// [gordian_knot].session_cmd, else it is the stack's own: Each binary's
// greeter starts its stack's session.

use std::io::{self, Write};
use std::os::fd::{AsRawFd, RawFd};

use crate::greetd::{AuthMessageType, Client, ErrorType, Request, Response};
use crate::vt;

pub struct LoginConfig {
    /// Command to launch on successful auth (e.g., ["startx",
    /// "/usr/local/bin/demiurge"]). From [gordian_knot].session_cmd.
    pub session_cmd: Vec<String>,
    /// Environment overrides to set in the session (e.g.,
    /// ["XDG_SESSION_TYPE=wayland"]).
    pub session_env: Vec<String>,
}

/// The session the X11 binary's greeter starts unless [gordian_knot] names
/// another. OZONE_PLATFORM rides along because the greetd path never
/// touches the xsessions entry or the demiurge.service user unit that
/// otherwise carry it -- without it, Chromium / Electron children launched
/// from a greetd login try Wayland and exit.
pub const X11_SESSION: (&[&str], &[&str]) =
    (&["startx", "/usr/local/bin/demiurge"], &["XDG_SESSION_TYPE=x11", "OZONE_PLATFORM=x11"]);
/// The Wayland binary's.
pub const WAYLAND_SESSION: (&[&str], &[&str]) = (&["/usr/local/bin/demiurge-wl-tty"], &["XDG_SESSION_TYPE=wayland"]);

impl LoginConfig {
    /// [gordian_knot]'s session command and environment, each falling back
    /// to the greeter's stack's `session`.
    pub fn new(gk: &demiurge_core::config::GordianKnot, session: (&[&str], &[&str])) -> LoginConfig {
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        LoginConfig {
            session_cmd: gk.session_cmd.clone().unwrap_or_else(|| owned(session.0)),
            session_env: gk.session_env.clone().unwrap_or_else(|| owned(session.1)),
        }
    }
}

/// What the greeter asks the user for next.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// The username, typed in the open.
    Username,
    /// greetd's prompt: Its text, and whether what is typed is masked.
    Prompt { text: String, secret: bool },
    /// The session started: The greeter's work is done.
    Started,
}

/// A login through greetd.
pub struct Login {
    client: Client,
    cfg: LoginConfig,
    /// The username given, once it is.
    pub username: String,
    pub ask: Ask,
    /// What greetd last told the user: An info or error message, or why
    /// the last login failed.
    pub status: String,
}

impl Login {
    /// A login over `client`'s greetd, asking for the username.
    pub fn new(client: Client, cfg: LoginConfig) -> Login {
        Login { client, cfg, username: String::new(), ask: Ask::Username, status: String::new() }
    }

    /// What the user typed for what was asked. A failed login asks for the
    /// username again, the reason in `status`; an Err is fatal, greetd
    /// itself failing.
    pub fn answer(&mut self, text: String) -> Result<(), String> {
        let resp = match &self.ask {
            Ask::Username if text.is_empty() => {
                self.status = "Username required.".into();
                return Ok(());
            }
            Ask::Username => {
                self.status.clear();
                self.username = text.clone();
                self.client
                    .round_trip(&Request::CreateSession { username: text })
                    .map_err(|e| format!("create_session io: {}", e))?
            }
            Ask::Prompt { .. } => self
                .client
                .round_trip(&Request::PostAuthMessageResponse { response: Some(text) })
                .map_err(|e| format!("post_auth_message_response io: {}", e))?,
            Ask::Started => return Ok(()),
        };
        self.follow(resp)
    }

    // greetd's answers, until it asks the user something or the login ends.
    fn follow(&mut self, mut resp: Response) -> Result<(), String> {
        loop {
            match resp {
                Response::Success => return self.start(),
                Response::Error { error_type: ErrorType::AuthError, description } => {
                    // Recoverable: Cancel the session greetd has open, its
                    // answer read so the next request's is its own, and
                    // ask for the username again with the error shown.
                    self.client.round_trip(&Request::CancelSession).map_err(|e| format!("cancel_session io: {}", e))?;
                    self.status = description;
                    self.username.clear();
                    self.ask = Ask::Username;
                    return Ok(());
                }
                Response::Error { error_type: ErrorType::Error, description } => {
                    let _ = self.client.round_trip(&Request::CancelSession);
                    return Err(description);
                }
                Response::AuthMessage { auth_message_type, auth_message } => match auth_message_type {
                    AuthMessageType::Secret | AuthMessageType::Visible => {
                        let secret = auth_message_type == AuthMessageType::Secret;
                        self.ask = Ask::Prompt { text: auth_message, secret };
                        return Ok(());
                    }
                    AuthMessageType::Info | AuthMessageType::Error => {
                        // Held in status for the next prompt; acked with a
                        // null response.
                        self.status = auth_message;
                        resp = self
                            .client
                            .round_trip(&Request::PostAuthMessageResponse { response: None })
                            .map_err(|e| format!("post_auth_message_response io: {}", e))?;
                    }
                },
            }
        }
    }

    // Auth succeeded. Tell greetd what to start.
    fn start(&mut self) -> Result<(), String> {
        let req = Request::StartSession { cmd: self.cfg.session_cmd.clone(), env: self.cfg.session_env.clone() };
        match self.client.round_trip(&req) {
            Ok(Response::Success) => {
                self.ask = Ask::Started;
                Ok(())
            }
            Ok(Response::Error { description, .. }) => Err(format!("start_session error: {}", description)),
            Ok(other) => Err(format!("unexpected response to start_session: {:?}", other)),
            Err(e) => Err(format!("start_session io: {}", e)),
        }
    }
}

/// Run the --login mode on the TTY greetd attached. Returns 0 on a
/// successful start_session, non-zero on fatal error.
pub fn run(cfg: LoginConfig) -> i32 {
    let mut login = match Client::connect_default() {
        Ok(client) => Login::new(client, cfg),
        Err(e) => {
            eprintln!("[gordian_knot --login] greetd connect: {}", e);
            return 1;
        }
    };

    // Stdin is whatever greetd attached to us, a TTY. It goes into
    // raw-no-echo so the prompt loop has byte-level control over
    // rendering.
    let tty_fd: RawFd = io::stdin().as_raw_fd();
    vt::set_prompt_termios_pub(tty_fd);

    loop {
        let (label, secret) = match &login.ask {
            Ask::Username => ("USERNAME".to_string(), false),
            Ask::Prompt { text, secret } => (text.clone(), *secret),
            Ask::Started => return 0,
        };
        let screen = vt::render_login_ui(&login.username, &label, &login.status);
        let _ = io::stdout().write_all(screen.as_bytes());
        let _ = io::stdout().flush();
        let line = if secret { vt::read_password_line_pub(tty_fd) } else { vt::read_visible_line(tty_fd) };
        let Some(text) = line else {
            return 1;
        };
        if let Err(e) = login.answer(text) {
            eprintln!("[gordian_knot --login] {}", e);
            return 1;
        }
    }
}
