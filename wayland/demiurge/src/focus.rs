// What the keyboard, the pointer and touch can focus: A Wayland surface,
// or an X11 window. Focusing the X11 window itself, not just its surface,
// is what makes smithay's X11Wm give it X input focus (or send
// WM_TAKE_FOCUS), without which XWayland's clients get no keys.

use std::borrow::Cow;
use std::sync::Arc;

use smithay::backend::input::KeyState;
use smithay::desktop::PopupKind;
use smithay::input::Seat;
use smithay::input::dnd::{DndFocus, OfferData, Source};
use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, ModifiersState};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, MotionEvent,
    PointerTarget, RelativeMotionEvent,
};
use smithay::input::touch::{
    DownEvent, MotionEvent as TouchMotionEvent, OrientationEvent, ShapeEvent, TouchTarget, UpEvent,
};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Serial};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::data_device::WlOfferData;
use smithay::xwayland::X11Surface;
use smithay::xwayland::xwm::XwmOfferData;

use crate::wm::Wm;

#[derive(Debug, Clone, PartialEq)]
pub enum Focus {
    Surface(WlSurface),
    X11(X11Surface),
}

impl From<WlSurface> for Focus {
    fn from(surface: WlSurface) -> Self {
        Focus::Surface(surface)
    }
}

impl From<PopupKind> for Focus {
    fn from(popup: PopupKind) -> Self {
        Focus::Surface(popup.wl_surface().clone())
    }
}

impl IsAlive for Focus {
    fn alive(&self) -> bool {
        match self {
            Focus::Surface(surface) => surface.alive(),
            Focus::X11(window) => window.alive(),
        }
    }
}

impl WaylandFocus for Focus {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Focus::Surface(surface) => Some(Cow::Borrowed(surface)),
            Focus::X11(window) => window.wl_surface().map(Cow::Owned),
        }
    }
}

// Each target method, passed to whichever of the two this is.
macro_rules! each {
    ($focus:expr, $target:ident => $call:expr) => {
        match $focus {
            Focus::Surface($target) => $call,
            Focus::X11($target) => $call,
        }
    };
}

impl KeyboardTarget<Wm> for Focus {
    fn enter(&self, seat: &Seat<Wm>, data: &mut Wm, keys: Vec<KeysymHandle<'_>>, serial: Serial) {
        each!(self, t => KeyboardTarget::enter(t, seat, data, keys, serial))
    }

    fn leave(&self, seat: &Seat<Wm>, data: &mut Wm, serial: Serial) {
        each!(self, t => KeyboardTarget::leave(t, seat, data, serial))
    }

    fn key(&self, seat: &Seat<Wm>, data: &mut Wm, key: KeysymHandle<'_>, state: KeyState, serial: Serial, time: u32) {
        each!(self, t => KeyboardTarget::key(t, seat, data, key, state, serial, time))
    }

    fn modifiers(&self, seat: &Seat<Wm>, data: &mut Wm, modifiers: ModifiersState, serial: Serial) {
        each!(self, t => KeyboardTarget::modifiers(t, seat, data, modifiers, serial))
    }
}

impl PointerTarget<Wm> for Focus {
    fn enter(&self, seat: &Seat<Wm>, data: &mut Wm, event: &MotionEvent) {
        each!(self, t => PointerTarget::enter(t, seat, data, event))
    }

    fn motion(&self, seat: &Seat<Wm>, data: &mut Wm, event: &MotionEvent) {
        each!(self, t => PointerTarget::motion(t, seat, data, event))
    }

    fn relative_motion(&self, seat: &Seat<Wm>, data: &mut Wm, event: &RelativeMotionEvent) {
        each!(self, t => PointerTarget::relative_motion(t, seat, data, event))
    }

    fn button(&self, seat: &Seat<Wm>, data: &mut Wm, event: &ButtonEvent) {
        each!(self, t => PointerTarget::button(t, seat, data, event))
    }

    fn axis(&self, seat: &Seat<Wm>, data: &mut Wm, frame: AxisFrame) {
        each!(self, t => PointerTarget::axis(t, seat, data, frame))
    }

    fn frame(&self, seat: &Seat<Wm>, data: &mut Wm) {
        each!(self, t => PointerTarget::frame(t, seat, data))
    }

    fn gesture_swipe_begin(&self, seat: &Seat<Wm>, data: &mut Wm, event: &GestureSwipeBeginEvent) {
        each!(self, t => PointerTarget::gesture_swipe_begin(t, seat, data, event))
    }

    fn gesture_swipe_update(&self, seat: &Seat<Wm>, data: &mut Wm, event: &GestureSwipeUpdateEvent) {
        each!(self, t => PointerTarget::gesture_swipe_update(t, seat, data, event))
    }

    fn gesture_swipe_end(&self, seat: &Seat<Wm>, data: &mut Wm, event: &GestureSwipeEndEvent) {
        each!(self, t => PointerTarget::gesture_swipe_end(t, seat, data, event))
    }

    fn gesture_pinch_begin(&self, seat: &Seat<Wm>, data: &mut Wm, event: &GesturePinchBeginEvent) {
        each!(self, t => PointerTarget::gesture_pinch_begin(t, seat, data, event))
    }

    fn gesture_pinch_update(&self, seat: &Seat<Wm>, data: &mut Wm, event: &GesturePinchUpdateEvent) {
        each!(self, t => PointerTarget::gesture_pinch_update(t, seat, data, event))
    }

    fn gesture_pinch_end(&self, seat: &Seat<Wm>, data: &mut Wm, event: &GesturePinchEndEvent) {
        each!(self, t => PointerTarget::gesture_pinch_end(t, seat, data, event))
    }

    fn gesture_hold_begin(&self, seat: &Seat<Wm>, data: &mut Wm, event: &GestureHoldBeginEvent) {
        each!(self, t => PointerTarget::gesture_hold_begin(t, seat, data, event))
    }

    fn gesture_hold_end(&self, seat: &Seat<Wm>, data: &mut Wm, event: &GestureHoldEndEvent) {
        each!(self, t => PointerTarget::gesture_hold_end(t, seat, data, event))
    }

    fn leave(&self, seat: &Seat<Wm>, data: &mut Wm, serial: Serial, time: u32) {
        each!(self, t => PointerTarget::leave(t, seat, data, serial, time))
    }
}

impl TouchTarget<Wm> for Focus {
    fn down(&self, seat: &Seat<Wm>, data: &mut Wm, event: &DownEvent, seq: Serial) {
        each!(self, t => TouchTarget::down(t, seat, data, event, seq))
    }

    fn up(&self, seat: &Seat<Wm>, data: &mut Wm, event: &UpEvent, seq: Serial) {
        each!(self, t => TouchTarget::up(t, seat, data, event, seq))
    }

    fn motion(&self, seat: &Seat<Wm>, data: &mut Wm, event: &TouchMotionEvent, seq: Serial) {
        each!(self, t => TouchTarget::motion(t, seat, data, event, seq))
    }

    fn frame(&self, seat: &Seat<Wm>, data: &mut Wm, seq: Serial) {
        each!(self, t => TouchTarget::frame(t, seat, data, seq))
    }

    fn cancel(&self, seat: &Seat<Wm>, data: &mut Wm, seq: Serial) {
        each!(self, t => TouchTarget::cancel(t, seat, data, seq))
    }

    fn shape(&self, seat: &Seat<Wm>, data: &mut Wm, event: &ShapeEvent, seq: Serial) {
        each!(self, t => TouchTarget::shape(t, seat, data, event, seq))
    }

    fn orientation(&self, seat: &Seat<Wm>, data: &mut Wm, event: &OrientationEvent, seq: Serial) {
        each!(self, t => TouchTarget::orientation(t, seat, data, event, seq))
    }
}

// A drag's offer to the window under it, Wayland or X11.
pub enum Offer<S: Source> {
    Surface(WlOfferData<S>),
    X11(XwmOfferData<S>),
}

impl<S: Source> OfferData for Offer<S> {
    fn disable(&self) {
        match self {
            Offer::Surface(offer) => offer.disable(),
            Offer::X11(offer) => offer.disable(),
        }
    }

    fn drop(&self) {
        match self {
            Offer::Surface(offer) => offer.drop(),
            Offer::X11(offer) => offer.drop(),
        }
    }

    fn validated(&self) -> bool {
        match self {
            Offer::Surface(offer) => offer.validated(),
            Offer::X11(offer) => offer.validated(),
        }
    }
}

// Each drag event, passed to whichever of the two this is with the offer it
// was given; an offer made to the other kind is none of this one's.
macro_rules! each_offer {
    ($focus:expr, $offer:expr, $target:ident, $o:ident => $call:expr) => {
        match $focus {
            Focus::Surface($target) => {
                let $o = match $offer {
                    Some(Offer::Surface(offer)) => Some(offer),
                    None => None,
                    Some(Offer::X11(_)) => return,
                };
                $call
            }
            Focus::X11($target) => {
                let $o = match $offer {
                    Some(Offer::X11(offer)) => Some(offer),
                    None => None,
                    Some(Offer::Surface(_)) => return,
                };
                $call
            }
        }
    };
}

impl DndFocus<Wm> for Focus {
    type OfferData<S>
        = Offer<S>
    where
        S: Source;

    fn enter<S: Source>(
        &self,
        data: &mut Wm,
        dh: &DisplayHandle,
        source: Arc<S>,
        seat: &Seat<Wm>,
        location: Point<f64, Logical>,
        serial: &Serial,
    ) -> Option<Offer<S>> {
        match self {
            Focus::Surface(t) => DndFocus::enter(t, data, dh, source, seat, location, serial).map(Offer::Surface),
            Focus::X11(t) => DndFocus::enter(t, data, dh, source, seat, location, serial).map(Offer::X11),
        }
    }

    fn motion<S: Source>(
        &self,
        data: &mut Wm,
        offer: Option<&mut Offer<S>>,
        seat: &Seat<Wm>,
        location: Point<f64, Logical>,
        time: u32,
    ) {
        each_offer!(self, offer, t, o => DndFocus::motion(t, data, o, seat, location, time))
    }

    fn leave<S: Source>(&self, data: &mut Wm, offer: Option<&mut Offer<S>>, seat: &Seat<Wm>) {
        each_offer!(self, offer, t, o => DndFocus::leave(t, data, o, seat))
    }

    fn drop<S: Source>(&self, data: &mut Wm, offer: Option<&mut Offer<S>>, seat: &Seat<Wm>) {
        each_offer!(self, offer, t, o => DndFocus::drop(t, data, o, seat))
    }
}
