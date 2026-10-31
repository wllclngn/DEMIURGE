// wlr-gamma-control against demiurge-wl, as ABRAXAS's Wayland backend
// speaks it: The test window's output has no gamma LUT, so its control
// fails. On a real screen (DEMIURGE_WL_TEST_BIN, the QEMU guest's
// demiurge-wl-tty) an output whose CRTC has one gives its ramps' size,
// takes a ramp, and fails a second control while the first holds it.

mod common;

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use common::WlTest;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_wlr::gamma_control::v1::client::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1;
use wayland_protocols_wlr::gamma_control::v1::client::zwlr_gamma_control_v1::{self, ZwlrGammaControlV1};

const WAIT: Duration = Duration::from_secs(5);

// Each control's answer, by the index it was made with: Its ramps' size,
// or failed.
#[derive(Default)]
struct Controls {
    size: [Option<u32>; 2],
    failed: [bool; 2],
}

impl Dispatch<WlRegistry, GlobalListContents> for Controls {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as wayland_client::Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrGammaControlV1, usize> for Controls {
    fn event(state: &mut Self, _: &ZwlrGammaControlV1, event: zwlr_gamma_control_v1::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => state.size[*i] = Some(size),
            zwlr_gamma_control_v1::Event::Failed => state.failed[*i] = true,
            _ => {}
        }
    }
}

delegate_noop!(Controls: ignore WlOutput);
delegate_noop!(Controls: ignore ZwlrGammaControlManagerV1);

fn answered(queue: &mut EventQueue<Controls>, state: &mut Controls, i: usize) {
    let deadline = Instant::now() + WAIT;
    while state.size[i].is_none() && !state.failed[i] {
        assert!(Instant::now() < deadline, "control {i} got neither its size nor failed");
        queue.roundtrip(state).expect("roundtrip");
    }
}

// A linear ramp for each channel, `size` entries, in a sealed memfd.
fn linear_ramps(size: u32) -> OwnedFd {
    let fd = unsafe { libc::memfd_create(c"gamma-test".as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    assert!(fd >= 0, "memfd_create");
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let n = size as usize;
    let ramp: Vec<u8> = (0..n).flat_map(|i| ((i * 0xFFFF / (n - 1).max(1)) as u16).to_ne_bytes()).collect();
    let bytes: Vec<u8> = ramp.iter().chain(&ramp).chain(&ramp).copied().collect();
    let written = unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(written, bytes.len() as isize, "write the ramps");
    unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE) };
    fd
}

#[test]
fn a_gamma_control_answers_as_its_outputs_crtc_allows() {
    let wl = WlTest::new();
    let conn = wl.connect();
    let (globals, mut queue) = registry_queue_init::<Controls>(&conn).expect("registry_queue_init");
    let qh = queue.handle();
    let manager: ZwlrGammaControlManagerV1 = globals.bind(&qh, 1..=1, ()).expect("bind zwlr_gamma_control_manager_v1");
    let output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("bind wl_output");
    let mut state = Controls::default();

    let first = manager.get_gamma_control(&output, &qh, 0);
    answered(&mut queue, &mut state, 0);
    if std::env::var_os("DEMIURGE_WL_TEST_BIN").is_none() {
        assert!(state.failed[0], "the test window's output has no gamma LUT, so its control fails");
        return;
    }
    let Some(size) = state.size[0] else {
        eprintln!("this screen's CRTC has no gamma LUT; its control failed");
        return;
    };
    assert!(size > 1, "a gamma LUT of {size} entries");
    first.set_gamma(linear_ramps(size).as_fd());
    queue.roundtrip(&mut state).expect("roundtrip");
    assert!(!state.failed[0], "the CRTC refused a linear ramp:\n{}", wl.log());

    let _second = manager.get_gamma_control(&output, &qh, 1);
    answered(&mut queue, &mut state, 1);
    assert!(state.failed[1], "a second control for an output the first holds fails");
    first.destroy();
    queue.roundtrip(&mut state).expect("roundtrip");
}
