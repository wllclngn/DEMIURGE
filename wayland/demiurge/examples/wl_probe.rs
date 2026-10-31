// wl_probe: The Wayland suite's instrument (tests/qemu/, and by hand
// against the test window). Built as an example so it is never installed
// with the product.
//
//   wl-probe color RRGGBB    map a toplevel filled with the color and keep
//                            it mapped, redrawn at each configured size,
//                            until killed or closed; pointer and keyboard
//                            events print one per line ("pointer enter X
//                            Y", "pointer button BUTTON STATE", "keyboard
//                            enter", "keyboard leave")
//   wl-probe menu RRGGBB     the same, and a press opens a 160x90 magenta
//                            menu at the pointer with a grab ("popup
//                            configured X Y W H"); "popup done" when the
//                            compositor dismisses it
//   wl-probe dmabuf RRGGBB [NODE]
//                            the color, each buffer a GBM allocation on
//                            NODE (the first render node by default),
//                            handed over through linux-dmabuf; exits 1 if
//                            the compositor refuses it. A software GBM
//                            allocates only on a card node, as root.
//   wl-probe scanout RRGGBB [NODE]
//                            the same, fullscreen with no cursor over it,
//                            allocating as its surface's own linux-dmabuf
//                            feedback asks
//                            ("feedback scanout" once it carries a scanout
//                            tranche, "buffer modifier M" per allocation),
//                            and a frame each time the last is presented:
//                            "presented zero-copy" the first time the
//                            compositor shows the buffer itself,
//                            "presented composited" the first time it
//                            shows a copy
//   wl-probe hdr RRGGBB [NODE]
//                            the scanout window as a game in HDR shows
//                            it: Its surface described as PQ over BT.2020
//                            with HDR10 metadata through color management
//                            ("hdr description ready" once the compositor
//                            makes the description)
//   wl-probe look RRGGBB     the color window with the pointer locked on
//                            it, as a game in mouse-look locks it ("pointer
//                            locked", "pointer unlocked"), printing each
//                            relative motion unaccelerated ("relative DX
//                            DY") and each motion of the pointer itself
//                            ("pointer motion X Y", "pointer leave")
//   wl-probe confine RRGGBB  the same with the pointer confined to the
//                            window ("pointer confined", "pointer
//                            unconfined")
//   wl-probe viewport RRGGBB the color window drawn from a 64x48 buffer
//                            scaled to each configured size through
//                            wp_viewporter, as XWayland shows a game's
//                            emulated mode ("viewport 64x48 to W H")
//   wl-probe sync RRGGBB [NODE]
//                            explicit sync through linux-drm-syncobj: A
//                            GPU buffer committed with an acquire point not
//                            yet signalled must not be shown until the
//                            probe signals it ("held until its acquire
//                            point"); a second buffer replaces it, and the
//                            first one's release point is signalled
//                            ("release point signalled")
//   wl-probe lock RRGGBB     take the session lock ("locked"), fill a lock
//                            surface on the first output with the color
//                            ("lock configured W H"), and unlock on Enter
//                            ("unlocked")
//
// Every mode prints the keys it gets ("key CODE STATE"), and the window
// modes the outputs ("output mode W H" for each one's current mode,
// "output removed" for one gone). Connects to $WAYLAND_DISPLAY.

use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::time::{Duration, Instant};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use smithay::reexports::drm::control::Device as _;
use smithay::reexports::gbm::{self, BufferObjectFlags};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop, event_created_child};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
use wayland_protocols::wp::color_management::v1::client::wp_color_management_surface_v1::WpColorManagementSurfaceV1;
use wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::{
    Primaries, RenderIntent, TransferFunction, WpColorManagerV1,
};
use wayland_protocols::wp::color_management::v1::client::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1;
use wayland_protocols::wp::color_management::v1::client::wp_image_description_v1::{self, WpImageDescriptionV1};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1};
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_v1::{self, ExtSessionLockV1};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1};
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_confined_pointer_v1::{self, ZwpConfinedPointerV1};
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::{self, ZwpLockedPointerV1};
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1};
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1;
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::{self, ZwpRelativePointerV1};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::{
    self, TrancheFlags, ZwpLinuxDmabufFeedbackV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::wp::linux_drm_syncobj::v1::client::wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1;
use wayland_protocols::wp::linux_drm_syncobj::v1::client::wp_linux_drm_syncobj_surface_v1::WpLinuxDrmSyncobjSurfaceV1;
use wayland_protocols::wp::linux_drm_syncobj::v1::client::wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1;
use wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::toplevel_icon::v1::client::xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1;
use wayland_protocols::xdg::toplevel_icon::v1::client::xdg_toplevel_icon_v1::XdgToplevelIconV1;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{self, ZxdgToplevelDecorationV1};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{self, WpPresentationFeedback};
use wayland_protocols::xdg::shell::client::xdg_popup::{self, XdgPopup};
use wayland_protocols::xdg::shell::client::xdg_positioner::{self, XdgPositioner};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

const MENU: (i32, i32) = (160, 90);
const MAGENTA: u32 = 0xff00ff;

// User data marking the menu's xdg_surface apart from the toplevel's.
struct Menu;

// How a mouse-look window constrains the pointer.
#[derive(Clone, Copy, PartialEq)]
enum Look {
    Lock,
    Confine,
}

#[derive(Default)]
struct Probe {
    size: (i32, i32),
    configured: bool,
    closed: bool,
    // The answer to the last linux-dmabuf create: The buffer, or a refusal.
    created: Option<Option<WlBuffer>>,
    // Where the pointer is on the toplevel, and the serial of a press not
    // yet answered with a menu.
    pointer: (f64, f64),
    press: Option<u32>,
    // The menu's configure not yet drawn, and its dismissal.
    menu_configured: Option<(i32, i32)>,
    menu_done: bool,
    // The session lock: Held, its surface's configure not yet drawn, and
    // Enter pressed to give it back.
    locked: Option<bool>,
    lock_configured: Option<(u32, i32, i32)>,
    enter: bool,
    // The outputs bound, by global name.
    outputs: Vec<u32>,
    // The surface's own linux-dmabuf feedback: Its format table, the
    // tranches read so far as (scanout, format indices), and the Xrgb8888
    // modifiers its scanout tranche lists while it has one.
    table: Vec<(u32, u64)>,
    tranche: (bool, Vec<u16>),
    tranches: Vec<(bool, Vec<u16>)>,
    scanout: Option<Vec<u64>>,
    feedback_changed: bool,
    // Presentation: A frame answered, and the two ways seen so far.
    presented: bool,
    zero_copy: bool,
    composited: bool,
    // No cursor over the window, as a game in mouse-look hides it.
    hide_cursor: bool,
    // The HDR description made, or why it was not.
    described: Option<Result<(), String>>,
    // Mouse-look: The seat's pointer once bound, and whether to print the
    // pointer's own motion.
    wl_pointer: Option<WlPointer>,
    look: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for Probe {
    fn event(probe: &mut Self, registry: &WlRegistry, event: <WlRegistry as wayland_client::Proxy>::Event, _: &GlobalListContents, _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wayland_client::protocol::wl_registry::Event::Global { name, interface, version } if interface == "wl_output" => {
                registry.bind::<WlOutput, _, _>(name, version.min(4), qh, name);
                probe.outputs.push(name);
            }
            wayland_client::protocol::wl_registry::Event::GlobalRemove { name } if probe.outputs.contains(&name) => {
                probe.outputs.retain(|&n| n != name);
                println!("output removed");
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for Probe {
    fn event(_: &mut Self, _: &WlOutput, event: wl_output::Event, _: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_output::Event::Mode { flags: WEnum::Value(flags), width, height, .. } = event {
            if flags.contains(wl_output::Mode::Current) {
                println!("output mode {} {}", width, height);
            }
        }
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for Probe {
    fn event(probe: &mut Self, _: &ZwpLinuxDmabufFeedbackV1, event: zwp_linux_dmabuf_feedback_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use zwp_linux_dmabuf_feedback_v1::Event;
        match event {
            // Entries of 16 bytes: The format, padding, the modifier.
            Event::FormatTable { fd, size } => {
                let mut table = vec![0u8; size as usize];
                std::fs::File::from(fd)
                    .read_exact_at(&mut table, 0)
                    .unwrap_or_else(|e| fail(&format!("format table: {}", e)));
                probe.table = table
                    .chunks_exact(16)
                    .map(|e| (u32::from_ne_bytes([e[0], e[1], e[2], e[3]]), u64::from_ne_bytes(e[8..16].try_into().unwrap())))
                    .collect();
            }
            Event::TrancheFormats { indices } => {
                probe.tranche.1.extend(indices.chunks_exact(2).map(|i| u16::from_ne_bytes([i[0], i[1]])));
            }
            Event::TrancheFlags { flags: WEnum::Value(flags) } => probe.tranche.0 = flags.contains(TrancheFlags::Scanout),
            Event::TrancheDone => {
                let tranche = std::mem::take(&mut probe.tranche);
                probe.tranches.push(tranche);
            }
            Event::Done => {
                let tranches = std::mem::take(&mut probe.tranches);
                let scanout = tranches.iter().any(|(scanout, _)| *scanout).then(|| {
                    tranches
                        .iter()
                        .filter(|(scanout, _)| *scanout)
                        .flat_map(|(_, indices)| indices.iter().filter_map(|&i| probe.table.get(i as usize)))
                        .filter(|(format, _)| *format == gbm::Format::Xrgb8888 as u32)
                        .map(|&(_, modifier)| modifier)
                        .collect::<Vec<_>>()
                });
                if scanout != probe.scanout {
                    println!("{}", if scanout.is_some() { "feedback scanout" } else { "feedback default" });
                    probe.scanout = scanout;
                    probe.feedback_changed = true;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WpPresentationFeedback, ()> for Probe {
    fn event(probe: &mut Self, _: &WpPresentationFeedback, event: wp_presentation_feedback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wp_presentation_feedback::Event::Presented { flags, .. } => {
                let zero_copy = matches!(flags, WEnum::Value(f) if f.contains(wp_presentation_feedback::Kind::ZeroCopy));
                if zero_copy && !probe.zero_copy {
                    println!("presented zero-copy");
                }
                if !zero_copy && !probe.composited {
                    println!("presented composited");
                }
                probe.zero_copy |= zero_copy;
                probe.composited |= !zero_copy;
                probe.presented = true;
            }
            wp_presentation_feedback::Event::Discarded => probe.presented = true,
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Probe {
    fn event(_: &mut Self, base: &XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Probe {
    fn event(probe: &mut Self, surface: &XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            probe.configured = true;
        }
    }
}

impl Dispatch<XdgSurface, Menu> for Probe {
    fn event(_: &mut Self, surface: &XdgSurface, event: xdg_surface::Event, _: &Menu, _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Probe {
    fn event(probe: &mut Self, _: &XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                probe.size = (width, height);
            }
            xdg_toplevel::Event::Close => probe.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<XdgPopup, Menu> for Probe {
    fn event(probe: &mut Self, _: &XdgPopup, event: xdg_popup::Event, _: &Menu, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            xdg_popup::Event::Configure { x, y, width, height } => {
                println!("popup configured {} {} {} {}", x, y, width, height);
                probe.menu_configured = Some((width, height));
            }
            xdg_popup::Event::PopupDone => {
                println!("popup done");
                probe.menu_done = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, ()> for Probe {
    fn event(probe: &mut Self, _: &ZwpLinuxBufferParamsV1, event: zwp_linux_buffer_params_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_linux_buffer_params_v1::Event::Created { buffer } => probe.created = Some(Some(buffer)),
            zwp_linux_buffer_params_v1::Event::Failed => probe.created = Some(None),
            _ => {}
        }
    }

    event_created_child!(Probe, ZwpLinuxBufferParamsV1, [
        zwp_linux_buffer_params_v1::EVT_CREATED_OPCODE => (WlBuffer, ()),
    ]);
}

impl Dispatch<WlSeat, ()> for Probe {
    fn event(probe: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) {
                probe.wl_pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for Probe {
    fn event(probe: &mut Self, pointer: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                println!("pointer enter {:.0} {:.0}", surface_x, surface_y);
                probe.pointer = (surface_x, surface_y);
                if probe.hide_cursor {
                    pointer.set_cursor(serial, None, 0, 0);
                }
            }
            wl_pointer::Event::Leave { .. } if probe.look => println!("pointer leave"),
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                probe.pointer = (surface_x, surface_y);
                if probe.look {
                    println!("pointer motion {:.0} {:.0}", surface_x, surface_y);
                }
            }
            wl_pointer::Event::Button { serial, button, state: WEnum::Value(state), .. } => {
                let pressed = state == wl_pointer::ButtonState::Pressed;
                println!("pointer button {} {}", button, if pressed { "pressed" } else { "released" });
                if pressed {
                    probe.press = Some(serial);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Probe {
    fn event(probe: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Enter { .. } => println!("keyboard enter"),
            wl_keyboard::Event::Leave { .. } => println!("keyboard leave"),
            wl_keyboard::Event::Key { key, state: WEnum::Value(state), .. } => {
                let pressed = state == wl_keyboard::KeyState::Pressed;
                println!("key {} {}", key, if pressed { "pressed" } else { "released" });
                // KEY_ENTER
                if pressed && key == 28 {
                    probe.enter = true;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for Probe {
    fn event(probe: &mut Self, _: &ExtSessionLockV1, event: ext_session_lock_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            ext_session_lock_v1::Event::Locked => {
                println!("locked");
                probe.locked = Some(true);
            }
            ext_session_lock_v1::Event::Finished => probe.locked = Some(false),
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for Probe {
    fn event(probe: &mut Self, _: &ExtSessionLockSurfaceV1, event: ext_session_lock_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let ext_session_lock_surface_v1::Event::Configure { serial, width, height } = event {
            probe.lock_configured = Some((serial, width as i32, height as i32));
        }
    }
}

delegate_noop!(Probe: ignore WlCompositor);
delegate_noop!(Probe: ignore WlShm);
delegate_noop!(Probe: ignore WlShmPool);
delegate_noop!(Probe: ignore WlBuffer);
delegate_noop!(Probe: ignore WlSurface);
delegate_noop!(Probe: ignore ZwpLinuxDmabufV1);
delegate_noop!(Probe: ignore XdgPositioner);
delegate_noop!(Probe: ignore WlOutput);
impl Dispatch<WpImageDescriptionV1, ()> for Probe {
    fn event(probe: &mut Self, _: &WpImageDescriptionV1, event: wp_image_description_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wp_image_description_v1::Event::Ready { .. } | wp_image_description_v1::Event::Ready2 { .. } => {
                probe.described = Some(Ok(()));
            }
            wp_image_description_v1::Event::Failed { msg, .. } => probe.described = Some(Err(msg)),
            _ => {}
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for Probe {
    fn event(_: &mut Self, _: &ZwpLockedPointerV1, event: zwp_locked_pointer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => println!("pointer locked"),
            zwp_locked_pointer_v1::Event::Unlocked => println!("pointer unlocked"),
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for Probe {
    fn event(_: &mut Self, _: &ZwpRelativePointerV1, event: zwp_relative_pointer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx_unaccel, dy_unaccel, .. } = event {
            println!("relative {:.0} {:.0}", dx_unaccel, dy_unaccel);
        }
    }
}

impl Dispatch<ZwpConfinedPointerV1, ()> for Probe {
    fn event(_: &mut Self, _: &ZwpConfinedPointerV1, event: zwp_confined_pointer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_confined_pointer_v1::Event::Confined => println!("pointer confined"),
            zwp_confined_pointer_v1::Event::Unconfined => println!("pointer unconfined"),
            _ => {}
        }
    }
}

delegate_noop!(Probe: ignore WpLinuxDrmSyncobjManagerV1);
delegate_noop!(Probe: ignore WpLinuxDrmSyncobjSurfaceV1);
delegate_noop!(Probe: ignore WpLinuxDrmSyncobjTimelineV1);
delegate_noop!(Probe: ignore WpViewporter);
delegate_noop!(Probe: ignore XdgToplevelIconManagerV1);
delegate_noop!(Probe: ignore XdgToplevelIconV1);
delegate_noop!(Probe: ignore ZxdgDecorationManagerV1);
delegate_noop!(Probe: ignore ZxdgToplevelDecorationV1);
delegate_noop!(Probe: ignore WpViewport);
delegate_noop!(Probe: ignore ZwpPointerConstraintsV1);
delegate_noop!(Probe: ignore ZwpRelativePointerManagerV1);
delegate_noop!(Probe: ignore WpColorManagerV1);
delegate_noop!(Probe: ignore WpImageDescriptionCreatorParamsV1);
delegate_noop!(Probe: ignore WpColorManagementSurfaceV1);
delegate_noop!(Probe: ignore ExtSessionLockManagerV1);
delegate_noop!(Probe: ignore WpPresentation);

fn usage() -> ! {
    eprintln!("usage: wl-probe color|menu|look|confine|viewport|icon|titled|lock RRGGBB | wl-probe dmabuf|scanout|hdr|sync RRGGBB [NODE]");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rgb = |hex: &String| u32::from_str_radix(hex, 16).unwrap_or_else(|_| usage());
    let node = || args.get(2).map(PathBuf::from).unwrap_or_else(first_render_node);
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("color"), Some(hex)) => show(rgb(hex), None, false, false, false, None, false, false, false),
        (Some("menu"), Some(hex)) => show(rgb(hex), None, true, false, false, None, false, false, false),
        (Some("look"), Some(hex)) => show(rgb(hex), None, false, false, false, Some(Look::Lock), false, false, false),
        (Some("confine"), Some(hex)) => show(rgb(hex), None, false, false, false, Some(Look::Confine), false, false, false),
        (Some("viewport"), Some(hex)) => show(rgb(hex), None, false, false, false, None, true, false, false),
        (Some("icon"), Some(hex)) => show(rgb(hex), None, false, false, false, None, false, true, false),
        (Some("titled"), Some(hex)) => show(rgb(hex), None, false, false, false, None, false, false, true),
        (Some("lock"), Some(hex)) => lock(rgb(hex)),
        (Some("dmabuf"), Some(hex)) => show(rgb(hex), Some(node()), false, false, false, None, false, false, false),
        (Some("scanout"), Some(hex)) => show(rgb(hex), Some(node()), false, true, false, None, false, false, false),
        (Some("hdr"), Some(hex)) => show(rgb(hex), Some(node()), false, true, true, None, false, false, false),
        (Some("sync"), Some(hex)) => sync(rgb(hex), node()),
        _ => usage(),
    }
}

// The open menu's objects, kept until it is dismissed.
struct OpenMenu {
    surface: WlSurface,
    xdg_surface: XdgSurface,
    popup: XdgPopup,
    drawn: Option<(WlBuffer, WlShmPool)>,
}

#[allow(clippy::too_many_arguments)]
fn show(rgb: u32, node: Option<PathBuf>, menu: bool, scanout: bool, hdr: bool, look: Option<Look>, viewport: bool, icon: bool, titled: bool) {
    let conn = Connection::connect_to_env().expect("connect to $WAYLAND_DISPLAY");
    let (globals, mut queue) = registry_queue_init::<Probe>(&conn).expect("registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).expect("wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("xdg_wm_base");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let seat: WlSeat = globals.bind(&qh, 5..=7, ()).expect("wl_seat");
    let mut probe = Probe { size: (400, 300), hide_cursor: scanout, look: look.is_some(), ..Default::default() };
    let look_globals: Option<(ZwpPointerConstraintsV1, ZwpRelativePointerManagerV1)> = look.map(|_| {
        (
            globals.bind(&qh, 1..=1, ()).unwrap_or_else(|_| fail("the compositor offers no zwp_pointer_constraints_v1")),
            globals.bind(&qh, 1..=1, ()).unwrap_or_else(|_| fail("the compositor offers no zwp_relative_pointer_manager_v1")),
        )
    });
    // The constraint and the relative pointer, once the seat's pointer is
    // bound.
    let mut looking: Option<(Option<ZwpLockedPointerV1>, Option<ZwpConfinedPointerV1>, ZwpRelativePointerV1)> = None;
    for output in globals.contents().clone_list().into_iter().filter(|g| g.interface == "wl_output") {
        globals.registry().bind::<WlOutput, _, _>(output.name, output.version.min(4), &qh, output.name);
        probe.outputs.push(output.name);
    }
    let gpu = node.map(|node| {
        let linux_dmabuf: ZwpLinuxDmabufV1 = globals
            .bind(&qh, if scanout { 4..=5 } else { 3..=5 }, ())
            .unwrap_or_else(|_| fail("the compositor offers no zwp_linux_dmabuf_v1"));
        (gbm_device(&node), linux_dmabuf)
    });
    let presentation: Option<WpPresentation> = scanout.then(|| {
        globals.bind(&qh, 1..=2, ()).unwrap_or_else(|_| fail("the compositor offers no wp_presentation"))
    });
    let surface = compositor.create_surface(&qh, ());
    let viewport = viewport.then(|| {
        let viewporter: WpViewporter =
            globals.bind(&qh, 1..=1, ()).unwrap_or_else(|_| fail("the compositor offers no wp_viewporter"));
        viewporter.get_viewport(&surface, &qh, ())
    });
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("wl-probe".to_string());
    toplevel.set_app_id("wl-probe".to_string());
    // A 32x32 magenta icon, set before the first commit as a client names
    // itself.
    let _icon = icon.then(|| {
        let manager: XdgToplevelIconManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .unwrap_or_else(|_| fail("the compositor offers no xdg_toplevel_icon_manager_v1"));
        let icon = manager.create_icon(&qh, ());
        let (buffer, pool) = shm_buffer(&shm, &qh, 32, 32, (0xff00_0000 | MAGENTA).to_le_bytes());
        icon.add_buffer(&buffer, 1);
        manager.set_icon(&toplevel, Some(&icon));
        (manager, icon, buffer, pool)
    });
    // Server-side decorations, asked of xdg-decoration before the first
    // commit.
    let _decoration = titled.then(|| {
        let manager: ZxdgDecorationManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .unwrap_or_else(|_| fail("the compositor offers no zxdg_decoration_manager_v1"));
        let decoration = manager.get_toplevel_decoration(&toplevel, &qh, ());
        decoration.set_mode(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        (manager, decoration)
    });
    if scanout {
        toplevel.set_fullscreen(None);
        if let Some((_, linux_dmabuf)) = &gpu {
            linux_dmabuf.get_surface_feedback(&surface, &qh, ());
        }
    }
    let _color = hdr.then(|| declare_hdr(&globals, &mut queue, &mut probe, &surface));
    surface.commit();

    let pixel = (0xff00_0000 | rgb).to_le_bytes();
    let mut drawn = (0, 0);
    // The buffer on screen, and the shm pool behind it. The pixels stay
    // alive through the fds the compositor holds.
    let mut shown: Option<(WlBuffer, Option<WlShmPool>)> = None;
    let mut open: Option<OpenMenu> = None;
    while !probe.closed {
        queue.blocking_dispatch(&mut probe).expect("dispatch");
        if let (Some((constraints, relative)), None, Some(pointer)) = (&look_globals, &looking, &probe.wl_pointer) {
            let confine = look == Some(Look::Confine);
            looking = Some((
                (!confine).then(|| constraints.lock_pointer(&surface, pointer, None, Lifetime::Persistent, &qh, ())),
                confine.then(|| constraints.confine_pointer(&surface, pointer, None, Lifetime::Persistent, &qh, ())),
                relative.get_relative_pointer(pointer, &qh, ()),
            ));
            surface.commit();
        }
        if menu {
            menu_turn(&mut probe, &mut open, &compositor, &wm_base, &xdg_surface, &seat, &shm, &qh);
        }
        if !probe.configured {
            continue;
        }
        // A new buffer for a new size or for feedback that changes how to
        // allocate one; with presentation, the same buffer again each time
        // the last frame is answered, so every frame says how it was shown.
        let realloc = probe.size != drawn || std::mem::take(&mut probe.feedback_changed);
        let again = std::mem::take(&mut probe.presented);
        if !realloc && !again {
            continue;
        }
        let (w, h) = probe.size;
        // Through a viewport, a small buffer scaled to the window.
        let (bw, bh) = if viewport.is_some() { (64, 48) } else { (w, h) };
        let old = if realloc {
            let next = match &gpu {
                Some((device, linux_dmabuf)) => {
                    let modifiers = probe.scanout.clone();
                    gpu_buffer(&mut queue, &mut probe, device, linux_dmabuf, w, h, pixel, modifiers.as_deref(), scanout)
                }
                None => shm_buffer(&shm, &qh, bw, bh, pixel),
            };
            drawn = (w, h);
            shown.replace(next)
        } else {
            None
        };
        let (buffer, _) = shown.as_ref().expect("a buffer drawn");
        surface.attach(Some(buffer), 0, 0);
        surface.damage_buffer(0, 0, bw, bh);
        if let Some(viewport) = &viewport {
            viewport.set_destination(w, h);
            println!("viewport {}x{} to {} {}", bw, bh, w, h);
        }
        if let Some(presentation) = &presentation {
            presentation.feedback(&surface, &qh, ());
        }
        surface.commit();
        if let Some((old, pool)) = old {
            old.destroy();
            if let Some(pool) = pool {
                pool.destroy();
            }
        }
    }
}

// PQ over BT.2020 with HDR10 metadata (a BT.2020 mastering display at
// 0.005 to 1000 cd/m², MaxCLL 1000, MaxFALL 400) on `surface`, as a game in
// HDR describes its swapchain, in effect from its next commit.
fn declare_hdr(globals: &GlobalList, queue: &mut EventQueue<Probe>, probe: &mut Probe, surface: &WlSurface) -> WpColorManagementSurfaceV1 {
    let qh = queue.handle();
    let manager: WpColorManagerV1 =
        globals.bind(&qh, 2..=2, ()).unwrap_or_else(|_| fail("the compositor offers no wp_color_manager_v1"));
    let creator = manager.create_parametric_creator(&qh, ());
    creator.set_tf_named(TransferFunction::St2084Pq);
    creator.set_primaries_named(Primaries::Bt2020);
    creator.set_mastering_display_primaries(708_000, 292_000, 170_000, 797_000, 131_000, 46_000, 312_700, 329_000);
    creator.set_mastering_luminance(50, 1000);
    creator.set_max_cll(1000);
    creator.set_max_fall(400);
    let description = creator.create(&qh, ());
    while probe.described.is_none() {
        queue.blocking_dispatch(probe).expect("dispatch");
    }
    if let Some(Err(why)) = &probe.described {
        fail(&format!("the HDR description failed: {}", why));
    }
    println!("hdr description ready");
    let color = manager.get_surface(surface, &qh, ());
    color.set_image_description(&description, RenderIntent::Perceptual);
    color
}

// A DRM device, for its timeline syncobjs.
struct Card(std::fs::File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl smithay::reexports::drm::Device for Card {}
impl smithay::reexports::drm::control::Device for Card {}

// Dispatch until `done` holds or `ms` pass, sleeping on the connection
// between; whether it came to hold.
fn pump(queue: &mut EventQueue<Probe>, probe: &mut Probe, ms: u64, done: impl Fn(&Probe) -> bool) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        queue.dispatch_pending(probe).expect("dispatch");
        if done(probe) {
            return true;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        queue.flush().expect("flush");
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let mut fds = [libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if unsafe { libc::poll(fds.as_mut_ptr(), 1, left.as_millis() as i32) } > 0 {
            let _ = guard.read();
        }
    }
}

fn sync(rgb: u32, node: PathBuf) {
    let conn = Connection::connect_to_env().expect("connect to $WAYLAND_DISPLAY");
    let (globals, mut queue) = registry_queue_init::<Probe>(&conn).expect("registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).expect("wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=5, ()).expect("xdg_wm_base");
    let linux_dmabuf: ZwpLinuxDmabufV1 =
        globals.bind(&qh, 3..=5, ()).unwrap_or_else(|_| fail("the compositor offers no zwp_linux_dmabuf_v1"));
    let manager: WpLinuxDrmSyncobjManagerV1 =
        globals.bind(&qh, 1..=1, ()).unwrap_or_else(|_| fail("the compositor offers no wp_linux_drm_syncobj_manager_v1"));
    let presentation: WpPresentation =
        globals.bind(&qh, 1..=2, ()).unwrap_or_else(|_| fail("the compositor offers no wp_presentation"));
    let device = gbm_device(&node);
    let card = Card(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node)
            .unwrap_or_else(|e| fail(&format!("{}: {}", node.display(), e))),
    );
    let mut probe = Probe { size: (400, 300), ..Default::default() };
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("wl-probe".to_string());
    surface.commit();
    if !pump(&mut queue, &mut probe, 5000, |p| p.configured) {
        fail("the window was never configured");
    }
    let (w, h) = probe.size;
    let pixel = (0xff00_0000 | rgb).to_le_bytes();
    let (first, _) = gpu_buffer(&mut queue, &mut probe, &device, &linux_dmabuf, w, h, pixel, None, false);
    let (second, _) = gpu_buffer(&mut queue, &mut probe, &device, &linux_dmabuf, w, h, pixel, None, false);
    let syncobj = card.create_syncobj(false).unwrap_or_else(|e| fail(&format!("create a syncobj: {}", e)));
    let fd = card.syncobj_to_fd(syncobj, false).unwrap_or_else(|e| fail(&format!("export the syncobj: {}", e)));
    let timeline = manager.import_timeline(fd.as_fd(), &qh, ());
    let points = manager.get_surface(&surface, &qh, ());
    let signal = |point: u64| {
        card.syncobj_timeline_signal(&[syncobj], &[point])
            .unwrap_or_else(|e| fail(&format!("signal point {}: {}", point, e)))
    };

    // The first buffer, ready only once point 1 is signalled.
    points.set_acquire_point(&timeline, 0, 1);
    points.set_release_point(&timeline, 0, 2);
    surface.attach(Some(&first), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    presentation.feedback(&surface, &qh, ());
    surface.commit();
    if pump(&mut queue, &mut probe, 300, |p| p.presented) {
        fail("the frame was shown before its acquire point was signalled");
    }
    signal(1);
    if !pump(&mut queue, &mut probe, 5000, |p| p.presented) {
        fail("the frame was never shown once its acquire point was signalled");
    }
    println!("held until its acquire point");

    // The second, ready at once; the first is the compositor's to let go.
    probe.presented = false;
    signal(3);
    points.set_acquire_point(&timeline, 0, 3);
    points.set_release_point(&timeline, 0, 4);
    surface.attach(Some(&second), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    presentation.feedback(&surface, &qh, ());
    surface.commit();
    if !pump(&mut queue, &mut probe, 5000, |p| p.presented) {
        fail("the second frame was never shown");
    }
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    let deadline = now.tv_sec * 1_000_000_000 + now.tv_nsec + 2_000_000_000;
    match card.syncobj_timeline_wait(&[syncobj], &[2], deadline, false, false, false) {
        Ok(_) => println!("release point signalled"),
        Err(e) => fail(&format!("the first buffer's release point was never signalled: {}", e)),
    }
    while !probe.closed {
        queue.blocking_dispatch(&mut probe).expect("dispatch");
    }
}

// The menu's part of a dispatch turn: Open one on a press, draw it once
// configured, drop it once dismissed.
#[allow(clippy::too_many_arguments)]
fn menu_turn(
    probe: &mut Probe,
    open: &mut Option<OpenMenu>,
    compositor: &WlCompositor,
    wm_base: &XdgWmBase,
    parent: &XdgSurface,
    seat: &WlSeat,
    shm: &WlShm,
    qh: &QueueHandle<Probe>,
) {
    if std::mem::take(&mut probe.menu_done) {
        if let Some(menu) = open.take() {
            menu.popup.destroy();
            menu.xdg_surface.destroy();
            menu.surface.destroy();
        }
    }
    if let Some(serial) = probe.press.take() {
        if open.is_none() {
            let positioner = wm_base.create_positioner(qh, ());
            positioner.set_size(MENU.0, MENU.1);
            positioner.set_anchor_rect(probe.pointer.0 as i32, probe.pointer.1 as i32, 1, 1);
            positioner.set_anchor(xdg_positioner::Anchor::TopLeft);
            positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
            positioner.set_constraint_adjustment(
                xdg_positioner::ConstraintAdjustment::SlideX | xdg_positioner::ConstraintAdjustment::SlideY,
            );
            let surface = compositor.create_surface(qh, ());
            let xdg_surface = wm_base.get_xdg_surface(&surface, qh, Menu);
            let popup = xdg_surface.get_popup(Some(parent), &positioner, qh, Menu);
            popup.grab(seat, serial);
            surface.commit();
            positioner.destroy();
            *open = Some(OpenMenu { surface, xdg_surface, popup, drawn: None });
        }
    }
    if let (Some(menu), Some((w, h))) = (open.as_mut(), probe.menu_configured.take()) {
        let (buffer, pool) = shm_buffer(shm, qh, w, h, (0xff00_0000 | MAGENTA).to_le_bytes());
        menu.surface.attach(Some(&buffer), 0, 0);
        menu.surface.damage_buffer(0, 0, w, h);
        menu.surface.commit();
        menu.drawn = pool.map(|pool| (buffer, pool));
    }
}

fn lock(rgb: u32) {
    let conn = Connection::connect_to_env().expect("connect to $WAYLAND_DISPLAY");
    let (globals, mut queue) = registry_queue_init::<Probe>(&conn).expect("registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let _seat: WlSeat = globals.bind(&qh, 5..=7, ()).expect("wl_seat");
    let output: WlOutput = globals.bind(&qh, 1..=4, ()).expect("wl_output");
    let manager: ExtSessionLockManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .unwrap_or_else(|_| fail("the compositor offers no ext_session_lock_manager_v1"));
    let mut probe = Probe::default();
    let session = manager.lock(&qh, ());
    while probe.locked.is_none() {
        queue.blocking_dispatch(&mut probe).expect("dispatch");
    }
    if probe.locked == Some(false) {
        fail("the compositor refused the lock");
    }
    let surface = compositor.create_surface(&qh, ());
    let lock_surface = session.get_lock_surface(&surface, &output, &qh, ());
    // The buffer on the lock surface, kept while it shows.
    let mut _shown = None;
    while !probe.enter {
        queue.blocking_dispatch(&mut probe).expect("dispatch");
        if let Some((serial, w, h)) = probe.lock_configured.take() {
            lock_surface.ack_configure(serial);
            let shown = shm_buffer(&shm, &qh, w, h, (0xff00_0000 | rgb).to_le_bytes());
            surface.attach(Some(&shown.0), 0, 0);
            surface.damage_buffer(0, 0, w, h);
            surface.commit();
            println!("lock configured {} {}", w, h);
            _shown = Some(shown);
        }
    }
    session.unlock_and_destroy();
    conn.roundtrip().expect("roundtrip");
    println!("unlocked");
}

fn shm_buffer(shm: &WlShm, qh: &QueueHandle<Probe>, w: i32, h: i32, pixel: [u8; 4]) -> (WlBuffer, Option<WlShmPool>) {
    let mut memory = tempfile::tempfile().expect("buffer memory");
    memory.write_all(&pixel.repeat((w * h) as usize)).expect("fill buffer");
    let pool = shm.create_pool(memory.as_fd(), w * h * 4, qh, ());
    let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, qh, ());
    (buffer, Some(pool))
}

// A GBM buffer of the color, handed over through linux-dmabuf. With the
// modifiers a scanout tranche lists, allocated for scanout with one of
// them; else linear, for rendering.
#[allow(clippy::too_many_arguments)]
fn gpu_buffer(
    queue: &mut EventQueue<Probe>,
    probe: &mut Probe,
    device: &gbm::Device<std::fs::File>,
    linux_dmabuf: &ZwpLinuxDmabufV1,
    w: i32,
    h: i32,
    pixel: [u8; 4],
    scanout_modifiers: Option<&[u64]>,
    report: bool,
) -> (WlBuffer, Option<WlShmPool>) {
    let explicit: Vec<gbm::Modifier> = scanout_modifiers
        .into_iter()
        .flatten()
        .map(|&m| gbm::Modifier::from(m))
        .filter(|&m| m != gbm::Modifier::Invalid)
        .collect();
    // NVIDIA's GBM refuses a linear buffer to render to, and maps none in
    // its own layout; a linear one the probe only writes it allocates.
    let allocated = if explicit.is_empty() {
        device
            .create_buffer_object::<()>(w as u32, h as u32, gbm::Format::Xrgb8888, BufferObjectFlags::LINEAR | BufferObjectFlags::RENDERING)
            .or_else(|_| device.create_buffer_object::<()>(w as u32, h as u32, gbm::Format::Xrgb8888, BufferObjectFlags::LINEAR))
    } else {
        device.create_buffer_object_with_modifiers2::<()>(
            w as u32,
            h as u32,
            gbm::Format::Xrgb8888,
            explicit.into_iter(),
            BufferObjectFlags::SCANOUT | BufferObjectFlags::RENDERING,
        )
    };
    let mut bo = allocated.unwrap_or_else(|e| fail(&format!("gbm allocation: {}", e)));
    if report {
        println!("buffer modifier {:#x}", u64::from(bo.modifier()));
    }
    bo.map_mut(0, 0, w as u32, h as u32, |map| {
        let stride = map.stride() as usize;
        let rows = map.buffer_mut();
        for y in 0..h as usize {
            for x in 0..w as usize {
                rows[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&pixel);
            }
        }
    })
    .unwrap_or_else(|e| fail(&format!("gbm map: {}", e)));
    let fd = bo.fd().unwrap_or_else(|e| fail(&format!("gbm export: {}", e)));
    let modifier: u64 = bo.modifier().into();
    let params = linux_dmabuf.create_params(&queue.handle(), ());
    params.add(fd.as_fd(), 0, 0, bo.stride(), (modifier >> 32) as u32, modifier as u32);
    params.create(w, h, gbm::Format::Xrgb8888 as u32, zwp_linux_buffer_params_v1::Flags::empty());
    probe.created = None;
    while probe.created.is_none() {
        queue.blocking_dispatch(probe).expect("dispatch");
    }
    match probe.created.take().flatten() {
        Some(buffer) => (buffer, None),
        None => fail("the compositor refused the dmabuf"),
    }
}

fn first_render_node() -> PathBuf {
    let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
        .unwrap_or_else(|e| fail(&format!("/dev/dri: {}", e)))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("renderD")))
        .collect();
    nodes.sort();
    nodes.into_iter().next().unwrap_or_else(|| fail("no render node"))
}

fn gbm_device(path: &Path) -> gbm::Device<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|e| fail(&format!("{}: {}", path.display(), e)));
    gbm::Device::new(file).unwrap_or_else(|e| fail(&format!("gbm on {}: {}", path.display(), e)))
}

fn fail(why: &str) -> ! {
    eprintln!("wl-probe: {}", why);
    std::process::exit(1);
}
