// Color management (wp_color_manager_v1): How a client says what its
// pixels mean, PQ over BT.2020 with its HDR metadata for a game in HDR,
// and learns what the output takes. smithay carries no implementation, so
// the compositor serves the protocol itself from wayland-protocols'
// generated server side.
//
// Parametric descriptions only: The named transfer functions and
// primaries, custom primaries, luminances and mastering metadata. A
// surface's description is double-buffered as the protocol has it, taking
// effect at the surface's commit, where it is logged. An output answers as
// the sRGB display the compositor composites for, its monitor's own color
// volume from its EDID as the target. A fullscreen surface in PQ over
// BT.2020 whose buffer goes to the screen as it is puts an HDR10 monitor
// in HDR10 with its metadata (render.rs asks it of the screen).

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use demiurge_wl_screen::color::{DisplayColor, HdrMetadata};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum};
use smithay::wayland::compositor::{Cacheable, with_states};
use wayland_protocols::wp::color_management::v1::server::wp_color_management_output_v1::{self, WpColorManagementOutputV1};
use wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_feedback_v1::{
    self, WpColorManagementSurfaceFeedbackV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_v1::{self, WpColorManagementSurfaceV1};
use wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::{
    self, Feature, Primaries, RenderIntent, TransferFunction, WpColorManagerV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_icc_v1::{
    self, WpImageDescriptionCreatorIccV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_params_v1::{
    self, WpImageDescriptionCreatorParamsV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_image_description_info_v1::WpImageDescriptionInfoV1;
use wayland_protocols::wp::color_management::v1::server::wp_image_description_v1::{self, WpImageDescriptionV1};

use crate::wm::Wm;

/// The version served: 2's ready2 and preferred_changed2 for its clients,
/// 1's ready and preferred_changed for theirs.
pub const VERSION: u32 = 2;

const TRANSFER_FUNCTIONS: [TransferFunction; 4] =
    [TransferFunction::Gamma22, TransferFunction::Srgb, TransferFunction::St2084Pq, TransferFunction::ExtLinear];
const NAMED_PRIMARIES: [Primaries; 3] = [Primaries::Srgb, Primaries::Bt2020, Primaries::DisplayP3];
const FEATURES: [Feature; 4] =
    [Feature::Parametric, Feature::SetPrimaries, Feature::SetLuminances, Feature::SetMasteringDisplayPrimaries];

/// An image description's identity: Unique among every description the
/// compositor has made, which the protocol asks of it.
static IDENTITY: AtomicU64 = AtomicU64::new(1);

/// An image description as a client builds it: Each field as the
/// parametric creator's request set it, the primaries named or given as
/// CIE 1931 xy times 1,000,000.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    pub tf: Option<u32>,
    pub tf_power: Option<u32>,
    pub primaries: Option<u32>,
    pub custom_primaries: Option<[i32; 8]>,
    /// Min in 0.0001 cd/m², max and reference in cd/m².
    pub luminances: Option<(u32, u32, u32)>,
    pub mastering_primaries: Option<[i32; 8]>,
    /// Min in 0.0001 cd/m², max in cd/m².
    pub mastering_luminance: Option<(u32, u32)>,
    pub max_cll: Option<u32>,
    pub max_fall: Option<u32>,
}

impl Params {
    /// The display the compositor composites for: sRGB, at the luminances
    /// the protocol gives an SDR display by default.
    fn srgb() -> Params {
        Params {
            tf: Some(TransferFunction::Gamma22 as u32),
            primaries: Some(Primaries::Srgb as u32),
            luminances: Some((2000, 80, 80)),
            ..Params::default()
        }
    }

    /// One line for the compositor's log, the form the guest suite reads.
    pub fn describe(&self) -> String {
        let tf = match (self.tf, self.tf_power) {
            (Some(t), _) => tf_name(t).to_string(),
            (None, Some(p)) => format!("power {}", p as f64 / 10_000.0),
            (None, None) => "unset".to_string(),
        };
        let primaries = match (self.primaries, self.custom_primaries) {
            (Some(p), _) => primaries_name(p).to_string(),
            (None, Some(c)) => format!("{c:?}"),
            (None, None) => "unset".to_string(),
        };
        let mut line = format!("tf={tf} primaries={primaries}");
        if let Some((min, max, reference)) = self.luminances {
            line += &format!(" luminances={min}/{max}/{reference}");
        }
        if let Some((min, max)) = self.mastering_luminance {
            line += &format!(" mastering={min}/{max}");
        }
        if let Some(cll) = self.max_cll {
            line += &format!(" max_cll={cll}");
        }
        if let Some(fall) = self.max_fall {
            line += &format!(" max_fall={fall}");
        }
        line
    }
}

fn tf_name(tf: u32) -> &'static str {
    match tf {
        t if t == TransferFunction::Gamma22 as u32 => "gamma22",
        t if t == TransferFunction::Srgb as u32 => "srgb",
        t if t == TransferFunction::St2084Pq as u32 => "st2084_pq",
        t if t == TransferFunction::ExtLinear as u32 => "ext_linear",
        _ => "other",
    }
}

fn primaries_name(p: u32) -> &'static str {
    match p {
        p if p == Primaries::Srgb as u32 => "srgb",
        p if p == Primaries::Bt2020 as u32 => "bt2020",
        p if p == Primaries::DisplayP3 as u32 => "display_p3",
        _ => "other",
    }
}

/// A made image description: Its identity and what it says, or the reason
/// it could not be made.
pub struct Description {
    identity: u64,
    params: Result<Params, String>,
}

impl Description {
    fn new(params: Result<Params, String>) -> Arc<Description> {
        Arc::new(Description { identity: IDENTITY.fetch_add(1, Ordering::Relaxed), params })
    }
}

// BT.2020's primaries and D65 white as CIE 1931 xy times 1,000,000.
const BT2020: [i32; 8] = [708_000, 292_000, 170_000, 797_000, 131_000, 46_000, 312_700, 329_000];

/// `output`'s description: sRGB, as the compositor composites, with its
/// monitor's color volume from its EDID as the target. One record per
/// monitor, kept with its Output, so every object for it carries one
/// identity.
pub fn output_description(output: &Output, display: Option<DisplayColor>) -> Arc<Description> {
    struct OutputDescription(Arc<Description>);
    output.user_data().insert_if_missing_threadsafe(|| {
        let mut params = Params::srgb();
        if let Some(display) = display {
            params.mastering_primaries = Some(display.primaries);
            params.mastering_luminance = display.luminance;
        }
        OutputDescription(Description::new(Ok(params)))
    });
    output.user_data().get::<OutputDescription>().expect("inserted above").0.clone()
}

fn display_color(wm: &Wm, output: &Output) -> Option<DisplayColor> {
    wm.runtime.as_ref()?.backend.display_color(output)
}

/// Every surface's preferred description: The primary monitor's.
fn preferred(wm: &Wm) -> Arc<Description> {
    output_description(&wm.output, display_color(wm, &wm.output))
}

fn send_preferred(feedback: &WpColorManagementSurfaceFeedbackV1, identity: u64) {
    if feedback.version() >= 2 {
        feedback.preferred_changed2((identity >> 32) as u32, identity as u32);
    } else {
        feedback.preferred_changed(identity as u32);
    }
}

/// The primary monitor changed, and every surface's preferred description
/// with it.
pub fn preferred_changed(wm: &mut Wm) {
    let identity = preferred(wm).identity;
    wm.color_feedbacks.retain(|f| f.is_alive());
    for feedback in &wm.color_feedbacks {
        send_preferred(feedback, identity);
    }
}

/// HDR10 for `surface` if its description is PQ over BT.2020: Its
/// mastering display and light levels, the BT.2020 primaries and PQ's own
/// luminance range where it gives none.
pub fn hdr10(surface: &WlSurface) -> Option<HdrMetadata> {
    let d = with_states(surface, |states| states.cached_state.get::<SurfaceColor>().current().description.clone())?;
    let p = d.params.as_ref().ok()?;
    if p.tf != Some(TransferFunction::St2084Pq as u32) || p.primaries != Some(Primaries::Bt2020 as u32) {
        return None;
    }
    Some(HdrMetadata {
        primaries: p.mastering_primaries.unwrap_or(BT2020),
        mastering_luminance: p.mastering_luminance.or(p.luminances.map(|(min, max, _)| (min, max))).unwrap_or((50, 10_000)),
        max_cll: p.max_cll.unwrap_or(0),
        max_fall: p.max_fall.unwrap_or(0),
    })
}

/// A surface's image description, double-buffered in smithay's cached
/// state so it takes effect with the rest of the surface's state, a
/// synchronized subsurface's at its parent's commit. Pending holds a set
/// or unset since the last commit; current, the description in effect.
#[derive(Default)]
pub struct SurfaceColor {
    change: Option<Option<Arc<Description>>>,
    pub description: Option<Arc<Description>>,
}

impl Cacheable for SurfaceColor {
    fn commit(&mut self, _: &DisplayHandle) -> Self {
        SurfaceColor { change: self.change.take(), description: None }
    }

    fn merge_into(self, into: &mut Self, _: &DisplayHandle) {
        if let Some(change) = self.change {
            into.description = change;
        }
    }
}

/// What a surface's color objects share outside its double-buffered
/// state: Whether a wp_color_management_surface_v1 already speaks for it,
/// and the identity of the description last logged.
#[derive(Default)]
struct Claim {
    claimed: bool,
    logged: Option<u64>,
}

fn with_claim<T>(surface: &WlSurface, f: impl FnOnce(&mut Claim) -> T) -> T {
    with_states(surface, |states| {
        states.data_map.insert_if_missing(|| RefCell::new(Claim::default()));
        f(&mut states.data_map.get::<RefCell<Claim>>().expect("inserted above").borrow_mut())
    })
}

fn set_pending(surface: &WlSurface, change: Option<Arc<Description>>) {
    with_states(surface, |states| states.cached_state.get::<SurfaceColor>().pending().change = Some(change));
}

/// The surface's commit: A description newly in effect is logged, so a
/// test can see what reached the compositor.
pub fn commit(surface: &WlSurface) {
    let now = with_states(surface, |states| states.cached_state.get::<SurfaceColor>().current().description.clone());
    let identity = now.as_ref().map(|d| d.identity);
    if with_claim(surface, |c| std::mem::replace(&mut c.logged, identity)) == identity {
        return;
    }
    match now {
        Some(d) => {
            if let Ok(p) = &d.params {
                tracing::info!(surface = surface.id().protocol_id(), identity = d.identity, "surface image description: {}", p.describe());
            }
        }
        None => tracing::info!(surface = surface.id().protocol_id(), "surface image description unset"),
    }
}

/// A protocol error, logged: A game whose HDR the compositor refuses
/// says so here, where its client sees the error only as its death.
fn refuse<R: Resource>(resource: &R, code: impl Into<u32>, why: &str) {
    tracing::warn!(interface = R::interface().name, "color management refused: {why}");
    resource.post_error(code, why);
}

/// Answer a new description: ready2 or ready by version, or failed.
fn send_made(resource: &WpImageDescriptionV1, d: &Description) {
    match &d.params {
        Ok(_) if resource.version() >= 2 => resource.ready2((d.identity >> 32) as u32, d.identity as u32),
        Ok(_) => resource.ready(d.identity as u32),
        Err(msg) => resource.failed(wp_image_description_v1::Cause::Unsupported, msg.clone()),
    }
}

impl GlobalDispatch<WpColorManagerV1, ()> for Wm {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<WpColorManagerV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        let manager = data_init.init(resource, ());
        manager.supported_intent(RenderIntent::Perceptual);
        for f in FEATURES {
            manager.supported_feature(f);
        }
        for tf in TRANSFER_FUNCTIONS {
            manager.supported_tf_named(tf);
        }
        for p in NAMED_PRIMARIES {
            manager.supported_primaries_named(p);
        }
        manager.done();
    }
}

impl Dispatch<WpColorManagerV1, ()> for Wm {
    fn request(
        wm: &mut Self,
        _: &Client,
        manager: &WpColorManagerV1,
        request: wp_color_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_color_manager_v1::Request;
        match request {
            Request::Destroy => {}
            Request::GetOutput { id, output } => {
                data_init.init(id, Output::from_resource(&output));
            }
            Request::GetSurface { id, surface } => {
                let claimed = with_claim(&surface, |c| std::mem::replace(&mut c.claimed, true));
                data_init.init(id, surface);
                if claimed {
                    refuse(manager, wp_color_manager_v1::Error::SurfaceExists, "the surface already has a color management surface");
                }
            }
            Request::GetSurfaceFeedback { id, surface } => {
                let feedback = data_init.init(id, surface);
                send_preferred(&feedback, preferred(wm).identity);
                wm.color_feedbacks.push(feedback);
            }
            Request::CreateIccCreator { obj } => {
                data_init.init(obj, ());
                refuse(manager, wp_color_manager_v1::Error::UnsupportedFeature, "ICC profiles are not supported");
            }
            Request::CreateParametricCreator { obj } => {
                data_init.init(obj, Mutex::new(Params::default()));
            }
            Request::CreateWindowsScrgb { image_description } => {
                data_init.init(image_description, Description::new(Err("windows-scRGB is not supported".into())));
                refuse(manager, wp_color_manager_v1::Error::UnsupportedFeature, "windows-scRGB is not supported");
            }
            Request::GetImageDescription { image_description, .. } => {
                // No other protocol hands out references here, so none can
                // name a description.
                let d = Description::new(Err("no image description reference is known".into()));
                let object = data_init.init(image_description, d.clone());
                send_made(&object, &d);
            }
            _ => {}
        }
    }
}

impl Dispatch<WpColorManagementOutputV1, Option<Output>> for Wm {
    fn request(
        wm: &mut Self,
        _: &Client,
        _: &WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        output: &Option<Output>,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wp_color_management_output_v1::Request::GetImageDescription { image_description } = request {
            let Some(output) = output else {
                let d = Description::new(Err("the output is gone".into()));
                data_init.init(image_description, d).failed(wp_image_description_v1::Cause::NoOutput, "the output is gone".into());
                return;
            };
            let d = output_description(output, display_color(wm, output));
            let object = data_init.init(image_description, d.clone());
            send_made(&object, &d);
        }
    }
}

impl Dispatch<WpColorManagementSurfaceV1, WlSurface> for Wm {
    fn request(
        _: &mut Self,
        _: &Client,
        object: &WpColorManagementSurfaceV1,
        request: wp_color_management_surface_v1::Request,
        surface: &WlSurface,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        use wp_color_management_surface_v1::{Error, Request};
        match request {
            Request::SetImageDescription { image_description, render_intent } => {
                if !surface.is_alive() {
                    refuse(object, Error::Inert, "the surface is gone");
                    return;
                }
                if render_intent != WEnum::Value(RenderIntent::Perceptual) {
                    refuse(object, Error::RenderIntent, "only the perceptual intent is supported");
                    return;
                }
                let Some(d) = image_description.data::<Arc<Description>>().cloned() else { return };
                if d.params.is_err() {
                    refuse(object, Error::ImageDescription, "the image description failed");
                    return;
                }
                set_pending(surface, Some(d));
            }
            Request::UnsetImageDescription => {
                if surface.is_alive() {
                    set_pending(surface, None);
                }
            }
            // The protocol has the description unset at the next commit.
            Request::Destroy => {
                if surface.is_alive() {
                    with_claim(surface, |c| c.claimed = false);
                    set_pending(surface, None);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WpColorManagementSurfaceFeedbackV1, WlSurface> for Wm {
    fn request(
        wm: &mut Self,
        _: &Client,
        _: &WpColorManagementSurfaceFeedbackV1,
        request: wp_color_management_surface_feedback_v1::Request,
        _: &WlSurface,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_color_management_surface_feedback_v1::Request;
        match request {
            Request::GetPreferred { image_description } | Request::GetPreferredParametric { image_description } => {
                let d = preferred(wm);
                let object = data_init.init(image_description, d.clone());
                send_made(&object, &d);
            }
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionCreatorIccV1, ()> for Wm {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WpImageDescriptionCreatorIccV1,
        request: wp_image_description_creator_icc_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wp_image_description_creator_icc_v1::Request::Create { image_description } = request {
            let d = Description::new(Err("ICC profiles are not supported".into()));
            let object = data_init.init(image_description, d.clone());
            send_made(&object, &d);
        }
    }
}

impl Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Params>> for Wm {
    fn request(
        _: &mut Self,
        _: &Client,
        creator: &WpImageDescriptionCreatorParamsV1,
        request: wp_image_description_creator_params_v1::Request,
        params: &Mutex<Params>,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wp_image_description_creator_params_v1::{Error, Request};
        let mut p = params.lock().unwrap();
        // A parameter set twice is a protocol error, as the protocol says.
        let twice = |set: bool| {
            if set {
                refuse(creator, Error::AlreadySet, "the parameter is already set");
            }
            set
        };
        match request {
            Request::SetTfNamed { tf } => {
                let WEnum::Value(tf) = tf else {
                    refuse(creator, Error::InvalidTf, "an unknown transfer function");
                    return;
                };
                if !TRANSFER_FUNCTIONS.contains(&tf) {
                    refuse(creator, Error::InvalidTf, "the transfer function is not supported");
                    return;
                }
                if !twice(p.tf.is_some() || p.tf_power.is_some()) {
                    p.tf = Some(tf as u32);
                }
            }
            Request::SetTfPower { .. } => refuse(creator, Error::UnsupportedFeature, "set_tf_power is not supported"),
            Request::SetPrimariesNamed { primaries } => {
                let WEnum::Value(primaries) = primaries else {
                    refuse(creator, Error::InvalidPrimariesNamed, "unknown primaries");
                    return;
                };
                if !NAMED_PRIMARIES.contains(&primaries) {
                    refuse(creator, Error::InvalidPrimariesNamed, "the primaries are not supported");
                    return;
                }
                if !twice(p.primaries.is_some() || p.custom_primaries.is_some()) {
                    p.primaries = Some(primaries as u32);
                }
            }
            Request::SetPrimaries { r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y } => {
                if !twice(p.primaries.is_some() || p.custom_primaries.is_some()) {
                    p.custom_primaries = Some([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]);
                }
            }
            Request::SetLuminances { min_lum, max_lum, reference_lum } => {
                if (max_lum as u64) * 10_000 <= min_lum as u64 || (reference_lum as u64) * 10_000 <= min_lum as u64 {
                    refuse(creator, Error::InvalidLuminance, "the luminances are not min < reference, max");
                    return;
                }
                if !twice(p.luminances.is_some()) {
                    p.luminances = Some((min_lum, max_lum, reference_lum));
                }
            }
            Request::SetMasteringDisplayPrimaries { r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y } => {
                if !twice(p.mastering_primaries.is_some()) {
                    p.mastering_primaries = Some([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]);
                }
            }
            Request::SetMasteringLuminance { min_lum, max_lum } => {
                if (max_lum as u64) * 10_000 <= min_lum as u64 {
                    refuse(creator, Error::InvalidLuminance, "the mastering luminance is not min < max");
                    return;
                }
                if !twice(p.mastering_luminance.is_some()) {
                    p.mastering_luminance = Some((min_lum, max_lum));
                }
            }
            Request::SetMaxCll { max_cll } => {
                if !twice(p.max_cll.is_some()) {
                    p.max_cll = Some(max_cll);
                }
            }
            Request::SetMaxFall { max_fall } => {
                if !twice(p.max_fall.is_some()) {
                    p.max_fall = Some(max_fall);
                }
            }
            Request::Create { image_description } => {
                let made = if (p.tf.is_none() && p.tf_power.is_none()) || (p.primaries.is_none() && p.custom_primaries.is_none()) {
                    refuse(creator, Error::IncompleteSet, "a transfer function and primaries are required");
                    Err("incomplete".to_string())
                } else {
                    Ok(p.clone())
                };
                let d = Description::new(made);
                let object = data_init.init(image_description, d.clone());
                send_made(&object, &d);
            }
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionV1, Arc<Description>> for Wm {
    fn request(
        wm: &mut Self,
        _: &Client,
        object: &WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        d: &Arc<Description>,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wp_image_description_v1::Request::GetInformation { information } = request {
            let info = data_init.init(information, ());
            let Ok(p) = d.params.clone() else {
                refuse(object, wp_image_description_v1::Error::NotReady, "the image description failed");
                return;
            };
            // Its done is a destructor event, and an object destroyed in
            // the request that made it is one the backend cannot finish
            // making: The information goes once this dispatch is over.
            if let Some(rt) = &wm.runtime {
                rt.handle.insert_idle(move |_| send_information(&info, &p));
            }
        }
    }
}

/// An image description's parameters as information events, then done.
fn send_information(info: &WpImageDescriptionInfoV1, p: &Params) {
    if let Some(primaries) = p.primaries.and_then(|v| Primaries::try_from(v).ok()) {
        info.primaries_named(primaries);
    }
    if let Some([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]) = p.custom_primaries {
        info.primaries(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y);
    }
    if let Some(tf) = p.tf.and_then(|v| TransferFunction::try_from(v).ok()) {
        info.tf_named(tf);
    }
    if let Some((min, max, reference)) = p.luminances {
        info.luminances(min, max, reference);
    }
    if let Some([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]) = p.mastering_primaries {
        info.target_primaries(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y);
    }
    if let Some((min, max)) = p.mastering_luminance {
        info.target_luminance(min, max);
    }
    if let Some(cll) = p.max_cll {
        info.target_max_cll(cll);
    }
    if let Some(fall) = p.max_fall {
        info.target_max_fall(fall);
    }
    info.done();
}

impl Dispatch<WpImageDescriptionInfoV1, ()> for Wm {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WpImageDescriptionInfoV1,
        _: <WpImageDescriptionInfoV1 as Resource>::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}
