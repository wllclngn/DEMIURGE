// Idleness under demiurge-wl, from `[input.idle]`: Once no key, button or
// motion has come for `lock_seconds` the compositor's own timer runs the
// locker, where the X11 stack runs gordian_knot's daemon on X's idle time;
// after `screensaver_seconds` the monitors show black, and after the first
// DPMS timeout they are powered off, as the X11 stack sets X's screensaver
// and DPMS (DRM's CRTCs are on or off, with no standby or suspend between).
// Any input brings them back. idle-inhibit holds all three off while the
// window asking is shown, as a game or a video does.

use std::time::{Duration, Instant};

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::get_parent;
use smithay::wayland::idle_inhibit::IdleInhibitHandler;

use crate::wm::Wm;

impl Wm {
    /// Input arrived: The idle clock starts over, and blanked or powered-off
    /// monitors come back.
    pub(crate) fn input_seen(&mut self) {
        self.last_input = Instant::now();
        self.idle_locked = false;
        if self.blanked || self.outputs_off {
            self.blanked = false;
            self.outputs_off = false;
            self.queue_redraw();
        }
        self.arm_idle();
    }

    /// The idle timer, one at a time, waking at the next threshold.
    pub(crate) fn arm_idle(&mut self) {
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        if self.idle_pending {
            return;
        }
        let Some(first) = self.idle_wait() else {
            return;
        };
        self.idle_pending = true;
        let _ = rt.handle.insert_source(Timer::from_duration(first), |_, _, wm| match wm.idle_step() {
            Some(wait) => TimeoutAction::ToDuration(wait),
            None => {
                wm.idle_pending = false;
                TimeoutAction::Drop
            }
        });
    }

    // The thresholds not yet passed: The lock, the blank and the power-off.
    fn idle_ahead(&self) -> [Option<Duration>; 3] {
        [
            self.idle_lock_after.filter(|_| !self.idle_locked),
            self.idle_blank_after.filter(|_| !self.blanked),
            self.idle_off_after.filter(|_| !self.outputs_off),
        ]
    }

    // How long until the next threshold passes; None when none is ahead.
    fn idle_wait(&self) -> Option<Duration> {
        let idle = self.last_input.elapsed();
        self.idle_ahead().into_iter().flatten().map(|after| after.saturating_sub(idle)).min()
    }

    // The thresholds passed, each acted on once per idle spell, then the
    // wait until the next. An inhibitor shown holds them all off.
    fn idle_step(&mut self) -> Option<Duration> {
        if self.idle_inhibited() {
            return self.idle_ahead().into_iter().flatten().min();
        }
        let idle = self.last_input.elapsed();
        let [lock, blank, off] = self.idle_ahead();
        if lock.is_some_and(|after| idle >= after) {
            self.idle_locked = true;
            if !self.locked() {
                tracing::info!(seconds = idle.as_secs(), "idle; locking");
                self.spawn_locker();
            }
        }
        if blank.is_some_and(|after| idle >= after) {
            tracing::info!(seconds = idle.as_secs(), "idle; blanking");
            self.blanked = true;
            self.queue_redraw();
        }
        if off.is_some_and(|after| idle >= after) {
            tracing::info!(seconds = idle.as_secs(), "idle; outputs off");
            self.outputs_off = true;
            if let Some(rt) = self.runtime.as_mut() {
                rt.backend.power_off();
            }
        }
        self.idle_wait()
    }

    // A window holding off the lock: An inhibitor's surface alive, on a
    // window shown.
    fn idle_inhibited(&mut self) -> bool {
        self.idle_inhibitors.retain(|surface| surface.is_alive());
        self.idle_inhibitors.iter().any(|surface| {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            self.clients.iter().any(|c| c.toplevel.wl_surface().as_ref() == Some(&root) && self.is_visible(c))
        })
    }
}

impl IdleInhibitHandler for Wm {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.push(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.retain(|s| *s != surface);
    }
}

smithay::delegate_idle_inhibit!(Wm);
