// MEDUSA's clipboard holder under Wayland. To put an image on the clipboard
// its owner must persist and answer each paste for as long as it holds the
// selection, so a detached child owns it and serves the PNG bytes (as
// image/png) until another client takes the selection. It owns it through
// ext-data-control, which needs no surface focused: `medusa full` has none,
// and the overlay's are gone by then. The parent (MEDUSA) returns at once
// and exits.

use std::io::Write;
use std::path::Path;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop, event_created_child};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{self, ExtDataControlDeviceV1};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::ExtDataControlOfferV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{self, ExtDataControlSourceV1};

const PNG: &str = "image/png";

pub fn copy_to_clipboard(path: &Path) {
    // DEMIURGE's own clipboard holder: forks a process that owns the
    // clipboard and serves the image until another client takes it. Falls
    // back to wl-copy only if the compositor offers no ext-data-control or
    // the fork fails. The file is saved regardless.
    if copy_png(path) {
        eprintln!("[medusa] copied to clipboard");
        return;
    }
    let copied = std::fs::File::open(path).and_then(|png| {
        std::process::Command::new("wl-copy").args(["--type", PNG]).stdin(png).spawn()
    });
    match copied {
        Ok(_) => eprintln!("[medusa] copied to clipboard (wl-copy fallback)"),
        Err(e) => eprintln!("[medusa] clipboard copy skipped (holder + wl-copy both failed: {}); file saved", e),
    }
}

// Read `path` and serve it on the clipboard from a forked holder process.
// Returns true if the holder was launched.
fn copy_png(path: &Path) -> bool {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[medusa] clipboard read {}: {}", path.display(), e);
            return false;
        }
    };
    if !offers_data_control() {
        return false;
    }

    // Fork: parent returns, child becomes the clipboard holder.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return false;
    }
    if pid > 0 {
        return true; // parent: holder detached
    }

    // Child: detach from the session and serve until another client takes
    // the selection.
    unsafe {
        libc::setsid();
    }
    if let Err(e) = serve(data) {
        eprintln!("[medusa] clipboard: {}", e);
    }
    unsafe {
        libc::_exit(0);
    }
}

// Whether the compositor offers ext-data-control, asked on a connection of
// its own so that none is open across the fork.
fn offers_data_control() -> bool {
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    registry_queue_init::<Holder>(&conn).is_ok_and(|(globals, _)| {
        globals.contents().with_list(|list| list.iter().any(|g| g.interface == ExtDataControlManagerV1::interface().name))
    })
}

struct Holder {
    data: Vec<u8>,
    done: bool,
}

fn serve(data: Vec<u8>) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {}", e))?;
    let (globals, mut queue) = registry_queue_init::<Holder>(&conn).map_err(|e| format!("registry: {}", e))?;
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=1, ()).map_err(|_| "the compositor offers no seat")?;
    let manager: ExtDataControlManagerV1 =
        globals.bind(&qh, 1..=1, ()).map_err(|_| "the compositor offers no ext-data-control")?;
    let source = manager.create_data_source(&qh, ());
    source.offer(PNG.into());
    let device = manager.get_data_device(&seat, &qh, ());
    device.set_selection(Some(&source));

    let mut holder = Holder { data, done: false };
    while !holder.done {
        queue.blocking_dispatch(&mut holder).map_err(|e| format!("dispatch: {}", e))?;
    }
    source.destroy();
    device.destroy();
    let _ = queue.roundtrip(&mut holder);
    Ok(())
}

impl Dispatch<ExtDataControlSourceV1, ()> for Holder {
    fn event(
        holder: &mut Self,
        _: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // A paste: The bytes down the pipe it handed over, which closes
            // as the file drops.
            ext_data_control_source_v1::Event::Send { mime_type, fd } if mime_type == PNG => {
                let _ = std::fs::File::from(fd).write_all(&holder.data);
            }
            // Another client took the clipboard -- our job is done.
            ext_data_control_source_v1::Event::Cancelled => holder.done = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for Holder {
    fn event(
        holder: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_data_control_device_v1::Event::Finished = event {
            holder.done = true;
        }
    }

    event_created_child!(Holder, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<WlRegistry, GlobalListContents> for Holder {
    fn event(_: &mut Self, _: &WlRegistry, _: wayland_client::protocol::wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(Holder: ignore WlSeat);
delegate_noop!(Holder: ignore ExtDataControlManagerV1);
delegate_noop!(Holder: ignore ExtDataControlOfferV1);
