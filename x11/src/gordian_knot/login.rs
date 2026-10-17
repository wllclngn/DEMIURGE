// gordian_knot --login: greetd greeter.
//
// State machine:
//   1. Render UI, prompt for USERNAME (visible echo).
//   2. CreateSession{username} -> greetd.
//   3. Loop on greetd responses:
//        AuthMessage{secret}    -> read masked input, PostAuthMessageResponse{Some(text)}
//        AuthMessage{visible}   -> read echoed input, PostAuthMessageResponse{Some(text)}
//        AuthMessage{info|error}-> show as STATUS, PostAuthMessageResponse{None}
//        Success                -> break out of auth loop
//        Error{auth_error}      -> show msg, restart at step 1 (back to USERNAME)
//        Error{error}           -> fatal, return non-zero
//   4. StartSession{cmd, env} -> greetd; greetd starts the session and
//      we exit. On error here: fatal.
//
// Why no PAM in this mode: greetd owns the PAM stack. Our job is to
// shuttle prompts between the user (TTY I/O) and greetd (JSON over
// $GREETD_SOCK). The lock mode keeps direct PAM because there's no
// privilege separation needed in-session; login mode requires the
// privileged auth flow that greetd provides.
//
// The session command (what to start on success) comes from the
// caller. Typical Arch wiring: greetd config points at
// `gordian_knot --login` with a default command of `demiurge-wl`
// during the X11->Wayland transition, or
// `startx /usr/local/bin/demiurge` for the X11 path.

use std::io::{self, Write};
use std::os::fd::{AsRawFd, RawFd};

use crate::gordian_knot::greetd::{
    AuthMessageType, Client, ErrorType, Request, Response,
};
use crate::gordian_knot::vt;

pub struct LoginConfig {
    /// Command to launch on successful auth (e.g., ["startx",
    /// "/usr/local/bin/demiurge"]). From [gordian_knot].session_cmd.
    pub session_cmd: Vec<String>,
    /// Environment overrides to set in the session (e.g.,
    /// ["XDG_SESSION_TYPE=wayland"]).
    pub session_env: Vec<String>,
}

/// Run the --login mode. Returns 0 on a successful start_session,
/// non-zero on fatal error. AuthError is treated as recoverable:
/// the loop restarts at the USERNAME prompt.
pub fn run(cfg: &LoginConfig) -> i32 {
    let mut client = match Client::connect_default() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[gordian_knot --login] greetd connect: {}", e);
            return 1;
        }
    };

    // Stdin is whatever greetd attached to us (a TTY in agreety-style
    // mode, or a Wayland-cage pty in the cage path). Either way, we
    // put it into raw-no-echo so the prompt loop has byte-level
    // control over rendering.
    let tty_fd: RawFd = io::stdin().as_raw_fd();
    vt::set_prompt_termios_pub(tty_fd);

    let mut status = String::new();

    loop {
        // USERNAME prompt.
        let screen = vt::render_login_ui("", "USERNAME", &status);
        let _ = io::stdout().write_all(screen.as_bytes());
        let _ = io::stdout().flush();
        let username = match vt::read_visible_line(tty_fd) {
            Some(s) if !s.is_empty() => s,
            Some(_) => {
                status = "Username required.".to_string();
                continue;
            }
            None => return 1,
        };

        // Re-render so the prompt label updates and the typed
        // username is part of the static UI rather than mid-line.
        status.clear();

        let auth_outcome = drive_auth(&mut client, &username, tty_fd);
        match auth_outcome {
            AuthOutcome::Success => break,
            AuthOutcome::AuthError(msg) => {
                // Recoverable: cancel the session greetd has open,
                // bounce back to the USERNAME prompt with the error
                // shown.
                let _ = client.send(&Request::CancelSession);
                status = msg;
                continue;
            }
            AuthOutcome::Fatal(msg) => {
                eprintln!("[gordian_knot --login] {}", msg);
                let _ = client.send(&Request::CancelSession);
                return 1;
            }
        }
    }

    // Auth succeeded. Tell greetd what to start.
    let req = Request::StartSession {
        cmd: cfg.session_cmd.clone(),
        env: cfg.session_env.clone(),
    };
    match client.round_trip(&req) {
        Ok(Response::Success) => 0,
        Ok(Response::Error { description, .. }) => {
            eprintln!(
                "[gordian_knot --login] start_session error: {}",
                description
            );
            1
        }
        Ok(other) => {
            eprintln!(
                "[gordian_knot --login] unexpected response to start_session: {:?}",
                other
            );
            1
        }
        Err(e) => {
            eprintln!("[gordian_knot --login] start_session io: {}", e);
            1
        }
    }
}

enum AuthOutcome {
    Success,
    AuthError(String),
    Fatal(String),
}

fn drive_auth(client: &mut Client, username: &str, tty_fd: RawFd) -> AuthOutcome {
    let mut resp = match client.round_trip(&Request::CreateSession {
        username: username.to_string(),
    }) {
        Ok(r) => r,
        Err(e) => return AuthOutcome::Fatal(format!("create_session io: {}", e)),
    };

    let mut status_buf = String::new();
    loop {
        match resp {
            Response::Success => return AuthOutcome::Success,
            Response::Error {
                error_type: ErrorType::AuthError,
                description,
            } => return AuthOutcome::AuthError(description),
            Response::Error {
                error_type: ErrorType::Error,
                description,
            } => return AuthOutcome::Fatal(description),
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => {
                let answer = match auth_message_type {
                    AuthMessageType::Secret => {
                        let screen =
                            vt::render_login_ui(username, &auth_message, &status_buf);
                        let _ = io::stdout().write_all(screen.as_bytes());
                        let _ = io::stdout().flush();
                        match vt::read_password_line_pub(tty_fd) {
                            Some(s) => Some(s),
                            None => {
                                return AuthOutcome::Fatal("eof reading secret".into())
                            }
                        }
                    }
                    AuthMessageType::Visible => {
                        let screen =
                            vt::render_login_ui(username, &auth_message, &status_buf);
                        let _ = io::stdout().write_all(screen.as_bytes());
                        let _ = io::stdout().flush();
                        match vt::read_visible_line(tty_fd) {
                            Some(s) => Some(s),
                            None => {
                                return AuthOutcome::Fatal(
                                    "eof reading visible input".into(),
                                )
                            }
                        }
                    }
                    AuthMessageType::Info | AuthMessageType::Error => {
                        // Hold these in status for the next prompt
                        // render; ack with a null response.
                        status_buf = auth_message;
                        None
                    }
                };
                resp = match client
                    .round_trip(&Request::PostAuthMessageResponse { response: answer })
                {
                    Ok(r) => r,
                    Err(e) => {
                        return AuthOutcome::Fatal(format!(
                            "post_auth_message_response io: {}",
                            e
                        ))
                    }
                };
            }
        }
    }
}
