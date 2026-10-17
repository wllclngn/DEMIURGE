// The greetd wire format: Request serialization, response parsing and a
// scripted conversation over a real socket.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;

use demiurge_x11::gordian_knot::greetd::*;

// The wire format is "u32 length LE + JSON body". These tests
// verify our serialization matches greetd's expected format
// exactly -- no live socket required.

#[test]
fn create_session_serialization() {
    let req = Request::CreateSession {
        username: "alice".to_string(),
    };
    let body = serde_json::to_string(&req).unwrap();
    // greetd expects exactly this shape.
    assert_eq!(body, r#"{"type":"create_session","username":"alice"}"#);
}

#[test]
fn post_auth_message_response_with_password() {
    let req = Request::PostAuthMessageResponse {
        response: Some("hunter2".to_string()),
    };
    let body = serde_json::to_string(&req).unwrap();
    assert_eq!(
        body,
        r#"{"type":"post_auth_message_response","response":"hunter2"}"#
    );
}

#[test]
fn post_auth_message_response_acks_info() {
    // Info / error messages from greetd are acked with a null
    // response (no input needed, just continue).
    let req = Request::PostAuthMessageResponse { response: None };
    let body = serde_json::to_string(&req).unwrap();
    assert_eq!(
        body,
        r#"{"type":"post_auth_message_response","response":null}"#
    );
}

#[test]
fn start_session_serialization() {
    let req = Request::StartSession {
        cmd: vec!["demiurge-wl".to_string()],
        env: vec!["XDG_SESSION_TYPE=wayland".to_string()],
    };
    let body = serde_json::to_string(&req).unwrap();
    assert_eq!(
        body,
        r#"{"type":"start_session","cmd":["demiurge-wl"],"env":["XDG_SESSION_TYPE=wayland"]}"#
    );
}

#[test]
fn parse_success_response() {
    let body = br#"{"type":"success"}"#;
    let resp: Response = serde_json::from_slice(body).unwrap();
    assert!(matches!(resp, Response::Success));
}

#[test]
fn parse_auth_message_secret() {
    let body = br#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#;
    let resp: Response = serde_json::from_slice(body).unwrap();
    match resp {
        Response::AuthMessage {
            auth_message_type,
            auth_message,
        } => {
            assert_eq!(auth_message_type, AuthMessageType::Secret);
            assert_eq!(auth_message, "Password: ");
        }
        other => panic!("expected AuthMessage, got {:?}", other),
    }
}

#[test]
fn parse_auth_error_response() {
    let body =
        br#"{"type":"error","error_type":"auth_error","description":"bad password"}"#;
    let resp: Response = serde_json::from_slice(body).unwrap();
    match resp {
        Response::Error {
            error_type,
            description,
        } => {
            assert_eq!(error_type, ErrorType::AuthError);
            assert_eq!(description, "bad password");
        }
        other => panic!("expected Error, got {:?}", other),
    }
}

// End-to-end test against a mock greetd: A listening socket where the
// "greetd" side scripts a fixed conversation. Verifies the
// length-prefixed framing is correct in both directions.
#[test]
fn round_trip_against_mock_greetd() {
    use std::thread;

    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("greetd.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    // Mock greetd: read CreateSession, reply with AuthMessage,
    // read PostAuthMessageResponse, reply with Success.
    let mock = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        // Read length-prefixed frame.
        let mut len_buf = [0u8; 4];
        s.read_exact(&mut len_buf).unwrap();
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).unwrap();
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(req["type"], "create_session");
        assert_eq!(req["username"], "alice");

        // Reply: AuthMessage(secret).
        let reply = br#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#;
        s.write_all(&(reply.len() as u32).to_le_bytes()).unwrap();
        s.write_all(reply).unwrap();

        // Read PostAuthMessageResponse.
        s.read_exact(&mut len_buf).unwrap();
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).unwrap();
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(req["type"], "post_auth_message_response");
        assert_eq!(req["response"], "hunter2");

        // Reply: Success.
        let reply = br#"{"type":"success"}"#;
        s.write_all(&(reply.len() as u32).to_le_bytes()).unwrap();
        s.write_all(reply).unwrap();
    });

    let mut client = Client::connect(&sock).unwrap();
    let resp = client
        .round_trip(&Request::CreateSession {
            username: "alice".to_string(),
        })
        .unwrap();
    match resp {
        Response::AuthMessage { auth_message_type, .. } => {
            assert_eq!(auth_message_type, AuthMessageType::Secret);
        }
        other => panic!("unexpected response: {:?}", other),
    }
    let resp = client
        .round_trip(&Request::PostAuthMessageResponse {
            response: Some("hunter2".to_string()),
        })
        .unwrap();
    assert!(matches!(resp, Response::Success));

    mock.join().unwrap();
}
