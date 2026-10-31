// Per-client state attached to each Wayland connection: The
// CompositorClientState smithay associates with the client, and the
// process at the other end of the socket, as its peer credentials name it
// when it connects.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::wayland::compositor::CompositorClientState;

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    pub pid: Option<i32>,
}

impl ClientState {
    pub fn of(stream: &UnixStream) -> Self {
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: SO_PEERCRED fills a ucred of the length given.
        let ok = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        } == 0;
        ClientState { pid: (ok && cred.pid > 0).then_some(cred.pid), ..Default::default() }
    }
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        tracing::info!(?client_id, pid = ?self.pid, "client connected");
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        tracing::info!(?client_id, ?reason, "client disconnected");
    }
}
