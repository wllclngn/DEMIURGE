// Shared XWayland spawn + X11Wm wiring.
//
// Both backends need the same setup: spawn XWayland via smithay's
// helper, hook its calloop event source so XWaylandEvent::Ready
// triggers X11Wm::start_wm, then stash the resulting X11Wm on the
// per-backend state so XwmHandler callbacks can resolve it.
//
// The state type differs:
//   - winit / dev backend (main.rs):   State = Wm (calloop carries
//     the Wm directly)
//   - TTY / DRM backend (tty.rs):      State = TtyState (calloop
//     carries the bundle, Wm is a field)
//
// To handle both, the helper is generic over State + takes a closure
// that knows how to stash the X11Wm into the caller's State. Both
// callers also need to satisfy XwmHandler (Wm already does; TtyState
// gets a delegating impl in tty.rs).

use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::xwayland_shell::XWaylandShellHandler;
use smithay::xwayland::{X11Wm, XWayland, XWaylandEvent, XwmHandler};

/// Spawn XWayland and wire it into `loop_handle`. On
/// XWaylandEvent::Ready, sets `$DISPLAY` for child processes and
/// invokes `set_xwm(state, wm)` so the caller can attach the
/// X11Wm to its state.
///
/// Returns Ok(()) even when XWayland is disabled or the binary is
/// missing -- the compositor must still come up. `DEMIURGE_WL_NO_XWAYLAND=1`
/// short-circuits before any spawn happens (used by the smoke harness
/// so tests don't depend on /usr/bin/Xwayland).
pub fn spawn_xwayland<S>(
    dh: &DisplayHandle,
    loop_handle: calloop::LoopHandle<'static, S>,
    set_xwm: impl Fn(&mut S, X11Wm) + 'static,
) -> Result<(), String>
where
    S: XwmHandler + XWaylandShellHandler + 'static,
{
    if std::env::var_os("DEMIURGE_WL_NO_XWAYLAND").is_some() {
        tracing::info!("DEMIURGE_WL_NO_XWAYLAND set; skipping XWayland spawn");
        return Ok(());
    }

    let (xwayland, xwayland_client) = match XWayland::spawn(
        dh,
        None,
        std::iter::empty::<(String, String)>(),
        true,
        std::process::Stdio::null(),
        std::process::Stdio::null(),
        |_| {},
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "XWayland::spawn failed; continuing without X11 support",
            );
            return Ok(());
        }
    };

    let inserter = loop_handle.clone();
    loop_handle
        .insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => {
                tracing::info!(display_number, "XWayland ready");
                // SAFETY: child processes that read $DISPLAY haven't
                // been forked yet at this point in startup; setting
                // env here is race-free in practice. Future startup
                // commands inherit it.
                unsafe {
                    std::env::set_var(
                        "DISPLAY",
                        format!(":{}", display_number),
                    );
                }
                match X11Wm::start_wm(
                    inserter.clone(),
                    x11_socket,
                    xwayland_client.clone(),
                ) {
                    Ok(wm) => set_xwm(state, wm),
                    Err(e) => tracing::error!(
                        error = %e,
                        "X11Wm::start_wm failed",
                    ),
                }
            }
            XWaylandEvent::Error => {
                tracing::warn!("XWayland reported an error");
            }
        })
        .map_err(|e| format!("insert XWayland source: {}", e))?;

    Ok(())
}
