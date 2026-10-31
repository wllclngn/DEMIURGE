// Input from either backend, through one handler. A press that matches
// a keybind runs as an action instead of reaching the client;
// Ctrl+Alt+F1 to F12 (the XF86Switch_VT keysyms) switch VTs. The pointer
// goes to the surface under it, and a press focuses the window it lands
// on (click to focus, as on X11). For mouse-look, a mouse's motion also
// reaches the surface under the pointer as relative motion, and a surface
// may lock the pointer in place or confine it to a region of itself while
// it has the pointer.

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState,
    KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};
use smithay::desktop::WindowSurfaceType;
use smithay::desktop::utils::under_from_surface_tree;
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent, PointerHandle, RelativeMotionEvent};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};
use smithay::wayland::seat::WaylandFocus;

use demiurge_core::Action;

use crate::focus::Focus;
use crate::wm::{Toplevel, Wm};

const SWITCH_VT_1: u32 = 0x1008_FE01;
const SWITCH_VT_12: u32 = 0x1008_FE0C;

enum Intercept {
    Action(Action),
    Vt(i32),
    Prompt(u32),
    Menu(u32),
}

// What lies under a point: The focus there (a surface, or an X11 window
// as itself) and its origin, and the managed client it belongs to.
pub struct Under {
    pub focus: Focus,
    pub origin: Point<f64, Logical>,
    pub client: Option<u64>,
}

impl Wm {
    pub fn process_input<B: InputBackend>(&mut self, event: InputEvent<B>) {
        self.input_seen();
        match event {
            InputEvent::Keyboard { event } => self.key(event.key_code(), event.state(), event.time_msec()),
            InputEvent::PointerMotion { event } => {
                let relative =
                    RelativeMotionEvent { delta: event.delta(), delta_unaccel: event.delta_unaccel(), utime: event.time() };
                self.pointer_moved(self.pointer_location + event.delta(), Some(relative), event.time_msec());
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let mon = self.monitors.first();
                let to = event.position_transformed((mon.width as i32, mon.height as i32).into())
                    + Point::from((mon.x as f64, mon.y as f64));
                self.pointer_moved(to, None, event.time_msec());
            }
            InputEvent::PointerButton { event } => self.pointer_button(event.button_code(), event.state(), event.time_msec()),
            InputEvent::PointerAxis { event } => self.pointer_axis::<B>(&event),
            _ => {}
        }
    }

    pub fn key(&mut self, keycode: Keycode, state: KeyState, time: u32) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        // The filter only decides; the action runs after input() returns,
        // since acting inside it would reenter the keyboard state input()
        // holds (handle_action can move focus).
        let hit = keyboard.input::<Intercept, _>(
            self,
            keycode,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |wm, mods, handle| {
                if state != KeyState::Pressed {
                    return FilterResult::Forward;
                }
                let sym = handle.modified_sym().raw();
                if (SWITCH_VT_1..=SWITCH_VT_12).contains(&sym) {
                    return FilterResult::Intercept(Intercept::Vt((sym - SWITCH_VT_1 + 1) as i32));
                }
                // Locked, or with a layer surface holding the keyboard, no
                // keybind runs: Every key goes to the locker or the layer,
                // as to an X11 client's keyboard grab. A VT switch only
                // reaches a console that asks for a login.
                if wm.locked() || wm.exclusive_layer.is_some() {
                    return FilterResult::Forward;
                }
                // The tag menu, then the run prompt, take every key while
                // open, as the X11 stack's keyboard grabs do.
                if wm.tag_menu.is_some() {
                    return FilterResult::Intercept(Intercept::Menu(sym));
                }
                if wm.prompt.is_some() {
                    return FilterResult::Intercept(Intercept::Prompt(sym));
                }
                match crate::keys::match_binding(&wm.bindings, mods, &handle) {
                    Some(action) => FilterResult::Intercept(Intercept::Action(action)),
                    None => FilterResult::Forward,
                }
            },
        );
        // An Alt+Tab cycle ends as its modifier is let go; the release still
        // reaches the window, which saw the press.
        if state == KeyState::Released && self.cycle.is_some() {
            let mods = keyboard.modifier_state();
            if !mods.alt && !mods.logo {
                self.finish_cycle();
            }
        }
        match hit {
            Some(Intercept::Action(action)) => self.handle_action(&action),
            Some(Intercept::Prompt(sym)) => self.prompt_key(sym),
            Some(Intercept::Menu(sym)) => self.tag_menu_key(sym),
            Some(Intercept::Vt(vt)) => {
                if let Some(rt) = self.runtime.as_mut() {
                    rt.backend.change_vt(vt);
                }
            }
            None => {}
        }
    }

    // Motion toward `to`: Its relative part, from a mouse, to the surface
    // under the pointer, then the pointer itself there unless that
    // surface's lock holds it or its confinement keeps it out.
    pub fn pointer_moved(&mut self, to: Point<f64, Logical>, relative: Option<RelativeMotionEvent>, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if let Some(event) = relative.filter(|_| self.drag.is_none()) {
            let focus = self.under(self.pointer_location).map(|u| (u.focus, u.origin));
            pointer.relative_motion(self, focus, &event);
        }
        if self.drag.is_none() && self.constrained(&pointer, to) {
            pointer.frame(self);
            return;
        }
        self.pointer_to(to, time);
    }

    // Whether the active constraint on the surface with the pointer keeps
    // it from `to`: A lock always, a confinement once `to` leaves the
    // surface or the region it was given.
    fn constrained(&self, pointer: &PointerHandle<Wm>, to: Point<f64, Logical>) -> bool {
        let Some(under) = self.under(self.pointer_location) else {
            return false;
        };
        let Some(surface) = under.focus.wl_surface().map(|s| s.into_owned()) else {
            return false;
        };
        // Read out under the surface's state, which smithay holds while the
        // constraint is looked at: Finding what is under `to` takes it
        // again.
        let active = with_pointer_constraint(&surface, pointer, |c| {
            c.filter(|c| c.is_active()).map(|c| match &*c {
                PointerConstraint::Locked(_) => None,
                PointerConstraint::Confined(_) => Some(c.region().cloned()),
            })
        });
        match active {
            None => false,
            Some(None) => true,
            Some(Some(region)) => {
                let same = self.under(to).and_then(|u| u.focus.wl_surface().map(|s| s.into_owned())) == Some(surface);
                !same || region.is_some_and(|r| !r.contains((to - under.origin).to_i32_round()))
            }
        }
    }

    // The origin of `surface` if it is the one under the pointer.
    pub(crate) fn focused_origin(&self, surface: &WlSurface) -> Option<Point<f64, Logical>> {
        let under = self.under(self.pointer_location)?;
        (under.focus.wl_surface().as_deref() == Some(surface)).then_some(under.origin)
    }

    // Turn on the constraint on `surface` if the pointer is on it and
    // inside the region the constraint was given.
    pub(crate) fn activate_constraint(&self, surface: &WlSurface, pointer: &PointerHandle<Wm>) {
        let Some(origin) = self.focused_origin(surface) else {
            return;
        };
        with_pointer_constraint(surface, pointer, |c| {
            let Some(c) = c.filter(|c| !c.is_active()) else {
                return;
            };
            if c.region().is_none_or(|r| r.contains((self.pointer_location - origin).to_i32_round())) {
                c.activate();
            }
        });
    }

    // The pointer's focus recomputed where it stands, once windows have
    // mapped, gone or moved under it: A game mapping under a still pointer
    // is entered, and can take its pointer lock, without the mouse moving.
    pub(crate) fn refresh_pointer_focus(&mut self, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let focus = self.under(self.pointer_location).map(|u| (u.focus, u.origin));
        if focus.as_ref().map(|(f, _)| f) == pointer.current_focus().as_ref() {
            return;
        }
        let entered = focus.as_ref().and_then(|(f, _)| f.wl_surface().map(|s| s.into_owned()));
        let motion = MotionEvent { location: self.pointer_location, serial: SERIAL_COUNTER.next_serial(), time };
        pointer.motion(self, focus, &motion);
        pointer.frame(self);
        if let Some(surface) = entered {
            self.activate_constraint(&surface, &pointer);
        }
    }

    fn pointer_to(&mut self, to: Point<f64, Logical>, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        // Kept on the monitors, on the one nearest where it was headed,
        // which becomes the focused monitor.
        let gap = |m: &demiurge_core::Monitor| {
            let dx = (m.x as f64 - to.x).max(to.x - (m.x + m.width as i32 - 1) as f64).max(0.0);
            let dy = (m.y as f64 - to.y).max(to.y - (m.y + m.height as i32 - 1) as f64).max(0.0);
            dx * dx + dy * dy
        };
        let (index, mon) =
            self.monitors.iter().enumerate().min_by(|a, b| gap(a.1).total_cmp(&gap(b.1))).expect("a monitor");
        let x = to.x.clamp(mon.x as f64, (mon.x + mon.width as i32 - 1) as f64);
        let y = to.y.clamp(mon.y as f64, (mon.y + mon.height as i32 - 1) as f64);
        self.focused_monitor = index;
        self.pointer_location = (x, y).into();
        self.pointer_stirred();
        // A drag or the tag menu holds the pointer, as X11's grabs do.
        let held = self.drag.is_some() || self.tag_menu.is_some();
        let focus = if held { None } else { self.under(self.pointer_location).map(|u| (u.focus, u.origin)) };
        let entered = focus.as_ref().and_then(|(f, _)| f.wl_surface().map(|s| s.into_owned()));
        let motion = MotionEvent { location: self.pointer_location, serial: SERIAL_COUNTER.next_serial(), time };
        pointer.motion(self, focus, &motion);
        pointer.frame(self);
        if let Some(surface) = entered {
            self.activate_constraint(&surface, &pointer);
        }
        if self.drag.is_some() {
            self.drag_to(self.pointer_location);
        }
        if self.tag_menu.is_some() {
            self.tag_menu_hover();
        }
        self.queue_redraw();
    }

    pub fn pointer_button(&mut self, button: u32, state: ButtonState, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        // The tag menu holds the pointer: A press picks or closes, and no
        // button reaches a window while it is open.
        if self.tag_menu.is_some() {
            if state == ButtonState::Pressed {
                self.tag_menu_press();
            }
            return;
        }
        // A release ends a drag; a Super drag's press never reached the
        // window, so neither does its release, nor any press while it lasts.
        if self.drag.is_some() {
            if state == ButtonState::Pressed {
                return;
            }
            let forwarded = self.drag.as_ref().is_some_and(|drag| drag.forwarded);
            self.end_drag();
            self.queue_redraw();
            if !forwarded {
                return;
            }
        } else if state == ButtonState::Pressed
            && !pointer.is_grabbed()
            && self.front_layer_under(self.pointer_location).is_none()
            && (self.bar_press(button) || self.super_drag(button) || self.title_press(button))
        {
            self.queue_redraw();
            return;
        }
        if state == ButtonState::Pressed && !pointer.is_grabbed() {
            if let Some(id) = self.under(self.pointer_location).and_then(|u| u.client) {
                if self.focused != Some(id) {
                    self.focus_client(id);
                    self.queue_redraw();
                }
            }
        }
        let event = ButtonEvent { button, state, serial: SERIAL_COUNTER.next_serial(), time };
        pointer.button(self, &event);
        pointer.frame(self);
    }

    fn pointer_axis<B: InputBackend>(&mut self, event: &B::PointerAxisEvent) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let source = event.source();
        let mut frame = AxisFrame::new(event.time_msec()).source(source);
        for axis in [Axis::Horizontal, Axis::Vertical] {
            // A wheel reports in v120 steps alone; 15 is libinput's degrees
            // per notch.
            let amount = event
                .amount(axis)
                .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
            if amount != 0.0 {
                frame = frame.relative_direction(axis, event.relative_direction(axis)).value(axis, amount);
                if let Some(v120) = event.amount_v120(axis) {
                    frame = frame.v120(axis, v120 as i32);
                }
            } else if source == AxisSource::Finger {
                frame = frame.stop(axis);
            }
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    // What is under `at`, searched front to back as frames are drawn.
    pub fn under(&self, at: Point<f64, Logical>) -> Option<Under> {
        if self.locked() {
            return self.lock_surfaces.iter().find_map(|(s, _)| {
                under_from_surface_tree(s.wl_surface(), at, (0, 0), WindowSurfaceType::ALL)
                    .map(|(surface, origin)| Under { focus: Focus::Surface(surface), origin: origin.to_f64(), client: None })
            });
        }
        self.front_layer_under(at).or_else(|| self.windows_under(at)).or_else(|| self.back_layer_under(at))
    }

    // The window, popup or X11 menu under `at`, front first.
    fn windows_under(&self, at: Point<f64, Logical>) -> Option<Under> {
        let hit = |surface: &WlSurface, origin: Point<i32, Logical>, client: Option<u64>| {
            under_from_surface_tree(surface, at, origin, WindowSurfaceType::ALL).map(|(surface, origin)| Under {
                focus: Focus::Surface(surface),
                origin: origin.to_f64(),
                client,
            })
        };
        // An X11 window takes the pointer as itself, not as its surface.
        let as_x11 = |under: Under, window: &smithay::xwayland::X11Surface| Under { focus: Focus::X11(window.clone()), ..under };
        self.x11_surfaces
            .iter()
            .filter(|x| x.is_mapped() && x.is_override_redirect())
            .find_map(|x| hit(&x.wl_surface()?, x.geometry().loc, None).map(|u| as_x11(u, x)))
            .or_else(|| {
                self.stacking().into_iter().find_map(|c| {
                    c.popups().iter().find_map(|(popup, origin)| hit(popup, *origin, Some(c.id))).or_else(|| {
                        let under = hit(&c.toplevel.wl_surface()?, c.surface_origin(), Some(c.id))?;
                        Some(match &c.toplevel {
                            Toplevel::X11(window) => as_x11(under, window),
                            Toplevel::Xdg(_) => under,
                        })
                    })
                })
            })
    }
}
