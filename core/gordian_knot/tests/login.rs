// The greeters' conversation with greetd, against a scripted greetd: A
// wrong password shows greetd's error and asks for the username again, the
// cancelled session's answer read so the next request gets its own, and
// the right one starts the session.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use gordian_knot_core::greetd::Client;
use gordian_knot_core::login::{Ask, Login, LoginConfig};

fn read(s: &mut UnixStream) -> serde_json::Value {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).unwrap();
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    s.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn write(s: &mut UnixStream, reply: &str) {
    s.write_all(&(reply.len() as u32).to_le_bytes()).unwrap();
    s.write_all(reply.as_bytes()).unwrap();
}

const PASSWORD: &str = r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#;
const SUCCESS: &str = r#"{"type":"success"}"#;

#[test]
fn a_wrong_password_asks_again_and_the_right_one_starts_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("greetd.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let greetd = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut seen = Vec::new();
        for reply in [
            PASSWORD,
            r#"{"type":"error","error_type":"auth_error","description":"pam_authenticate: AUTH_ERR"}"#,
            SUCCESS,
            PASSWORD,
            SUCCESS,
            SUCCESS,
        ] {
            seen.push(read(&mut s));
            write(&mut s, reply);
        }
        seen
    });

    let cfg = LoginConfig { session_cmd: vec!["demiurge-wl-tty".into()], session_env: vec![] };
    let mut login = Login::new(Client::connect(&sock).unwrap(), cfg);
    assert_eq!(login.ask, Ask::Username);
    login.answer(String::new()).unwrap();
    assert_eq!((&login.ask, login.status.as_str()), (&Ask::Username, "Username required."));

    login.answer("alice".into()).unwrap();
    assert_eq!(login.ask, Ask::Prompt { text: "Password: ".into(), secret: true });
    login.answer("wrong".into()).unwrap();
    assert_eq!(login.ask, Ask::Username, "a wrong password did not ask for the username again");
    assert_eq!(login.status, "pam_authenticate: AUTH_ERR");

    login.answer("alice".into()).unwrap();
    assert_eq!(login.ask, Ask::Prompt { text: "Password: ".into(), secret: true }, "the retry got a stale answer");
    login.answer("right".into()).unwrap();
    assert_eq!(login.ask, Ask::Started);

    let seen = greetd.join().unwrap();
    let kinds: Vec<&str> = seen.iter().map(|r| r["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        ["create_session", "post_auth_message_response", "cancel_session", "create_session",
         "post_auth_message_response", "start_session"]
    );
    assert_eq!(seen[1]["response"], "wrong");
    assert_eq!(seen[4]["response"], "right");
    assert_eq!(seen[5]["cmd"][0], "demiurge-wl-tty");
}
