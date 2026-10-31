// wlr-layer-shell: Surfaces a client places on a monitor's layer rather
// than as a window, MEDUSA's overlay and its pin among them. smithay's layer
// map places each by its anchors, margins and size. The overlay layer draws
// in front of everything but the cursor, the top layer in front of the
// windows and the bar unless a fullscreen window shows, and the bottom and
// background layers behind the windows. A surface on the top or overlay
// layer asking for the keyboard exclusively holds it, and every keybind,
// while it is mapped, as an X11 client's grabs do.

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::utils::under_from_surface_tree;
use smithay::desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::{
    KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
    WlrLayerShellState,
};

use crate::focus::Focus;
use crate::input::Under;
use crate::render::DemiurgeElement;
use crate::wm::Wm;

impl WlrLayerShellHandler for Wm {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.runtime.as_mut().expect("layer-shell is served only with a screen").layer_shell
    }

    // A surface goes on the monitor it names, else on the focused one.
    fn new_layer_surface(&mut self, surface: WlrLayerSurface, output: Option<WlOutput>, _layer: Layer, namespace: String) {
        let output = output.as_ref().and_then(Output::from_resource).or_else(|| self.focused_output());
        let Some(output) = output else {
            surface.send_close();
            return;
        };
        if let Err(e) = layer_map_for_output(&output).map_layer(&LayerSurface::new(surface.clone(), namespace)) {
            tracing::warn!("layer surface refused: {}", e);
            surface.send_close();
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in &self.outputs {
            let mut map = layer_map_for_output(output);
            let layer = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        self.layer_focus();
        self.queue_redraw();
    }
}

// Whether a layer surface has a buffer to show.
fn mapped(layer: &LayerSurface) -> bool {
    with_renderer_surface_state(layer.wl_surface(), |state| state.buffer().is_some()).unwrap_or(false)
}

// Where a layer surface's tree starts on its monitor: Its place less its
// buffer's own offset.
fn origin(map: &smithay::desktop::LayerMap, layer: &LayerSurface) -> Option<Point<i32, Logical>> {
    map.layer_geometry(layer).map(|geo| geo.loc - layer.geometry().loc)
}

impl Wm {
    fn focused_output(&self) -> Option<Output> {
        let name = &self.monitors[self.focused_monitor].name;
        self.outputs.iter().find(|o| &o.name() == name).cloned()
    }

    // A layer surface's commit: Its monitor's layers placed again, its first
    // configure sent, and the keyboard to it or back as it asks.
    pub(crate) fn layer_commit(&mut self, surface: &WlSurface) {
        let Some(output) = self
            .outputs
            .iter()
            .cloned()
            .find(|o| layer_map_for_output(o).layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).is_some())
        else {
            return;
        };
        let configured = with_states(surface, |states| {
            states.data_map.get::<LayerSurfaceData>().is_some_and(|data| data.lock().unwrap().initial_configure_sent)
        });
        {
            let mut map = layer_map_for_output(&output);
            map.arrange();
            if !configured {
                if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
                    layer.layer_surface().send_configure();
                }
            }
        }
        self.layer_focus();
    }

    /// The monitors changed: Each one's layers are placed again on it.
    pub(crate) fn layers_rearranged(&mut self) {
        for output in &self.outputs {
            layer_map_for_output(output).arrange();
        }
    }

    /// A monitor went: Its layer surfaces are told to close.
    pub(crate) fn layers_output_gone(&mut self, output: &Output) {
        let mut map = layer_map_for_output(output);
        let layers: Vec<LayerSurface> = map.layers().cloned().collect();
        for layer in layers {
            layer.layer_surface().send_close();
            map.unmap_layer(&layer);
        }
        drop(map);
        self.layer_focus();
    }

    // The keyboard follows the front surface on the top or overlay layer
    // that holds it exclusively: To it while it is mapped, back to the
    // windows when none does. A lock keeps the keyboard whatever a layer
    // asks.
    pub(crate) fn layer_focus(&mut self) {
        let holder = self.outputs.iter().find_map(|output| {
            let map = layer_map_for_output(output);
            [Layer::Overlay, Layer::Top].into_iter().find_map(|layer| {
                map.layers_on(layer)
                    .rev()
                    .find(|l| l.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive && mapped(l))
                    .map(|l| l.wl_surface().clone())
            })
        });
        if holder == self.exclusive_layer {
            return;
        }
        self.exclusive_layer = holder.clone();
        if !self.locked() {
            match holder {
                Some(surface) => self.keyboard_to(Some(Focus::Surface(surface))),
                None => self.refocus_active_tag(),
            }
        }
        self.queue_redraw();
    }

    // The layers in front of the windows on the monitor `output` shows, front
    // first: The overlay, then the top one unless a fullscreen window hides
    // it.
    fn front_layers(&self, output: &Output) -> &'static [Layer] {
        if self.fullscreen_shown(self.monitor_of(output)) { &[Layer::Overlay] } else { &[Layer::Overlay, Layer::Top] }
    }

    /// The layers drawn in front of the windows but behind the bar on
    /// `output`: The top one, unless a fullscreen window hides it.
    pub(crate) fn top_layers(&self, output: &Output) -> &'static [Layer] {
        if self.fullscreen_shown(self.monitor_of(output)) { &[] } else { &[Layer::Top] }
    }

    fn layer_under(&self, at: Point<f64, Logical>, front: bool) -> Option<Under> {
        self.outputs.iter().find_map(|output| {
            let layers = if front { self.front_layers(output) } else { &[Layer::Bottom, Layer::Background] };
            let map = layer_map_for_output(output);
            layers.iter().find_map(|&layer| {
                map.layers_on(layer).rev().find_map(|l| {
                    let place = output.current_location() + origin(&map, l)?;
                    under_from_surface_tree(l.wl_surface(), at, place, WindowSurfaceType::ALL).map(|(surface, origin)| {
                        Under { focus: Focus::Surface(surface), origin: origin.to_f64(), client: None }
                    })
                })
            })
        })
    }

    /// The layer surface in front of the windows under `at`, if any.
    pub fn front_layer_under(&self, at: Point<f64, Logical>) -> Option<Under> {
        self.layer_under(at, true)
    }

    /// The layer surface behind the windows under `at`, if any.
    pub fn back_layer_under(&self, at: Point<f64, Logical>) -> Option<Under> {
        self.layer_under(at, false)
    }

    /// Every layer surface's tree, for its frame callbacks.
    pub(crate) fn layer_roots(&self) -> Vec<WlSurface> {
        self.outputs.iter().flat_map(|o| layer_map_for_output(o).layers().map(|l| l.wl_surface().clone()).collect::<Vec<_>>()).collect()
    }
}

/// `output`'s layer surfaces on `layers`, front to back, placed from its
/// origin.
pub(crate) fn elements(renderer: &mut GlesRenderer, output: &Output, layers: &[Layer]) -> Vec<DemiurgeElement> {
    let map = layer_map_for_output(output);
    let mut out = Vec::new();
    for &layer in layers {
        for l in map.layers_on(layer).rev() {
            let Some(at) = origin(&map, l) else {
                continue;
            };
            out.extend(
                render_elements_from_surface_tree(renderer, l.wl_surface(), at.to_physical(1), 1.0, 1.0, Kind::Unspecified)
                    .into_iter()
                    .map(DemiurgeElement::Surface),
            );
        }
    }
    out
}
