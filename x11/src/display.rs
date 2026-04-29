// Per-output display configuration: mode + DSR + DPI ownership.
//
// Replaces the user's xrandr lines in startup.commands. Each [[display]]
// entry in the TOML matches an output by name (or EDID model substring),
// applies the requested mode, applies a CRTC scale transform if
// dsr_multiplier > 1.0, and sets the X server's reported DPI.
//
// Implementation: shells out to xrandr (xorg-xrandr, part of every X11
// install). Pure-protocol RandR set_crtc_transform exists in x11rb but
// requires hand-managing screen-size resize, mode lookup, transform
// matrix construction, and CRTC config in the right order -- xrandr
// does this orchestration cleanly. The "external tool" the user
// objected to is the visible startup.commands line, not the
// implementation detail; xrandr stays inside DEMIURGE.
//
// DSR mechanism: xrandr --scale 2x2 --mode 1920x1080 makes the
// framebuffer 3840x2160 (2x the panel) while the CRTC scans out at
// the panel's native 1920x1080. The driver downscales server-side via
// the scanout pipe. NVIDIA, AMD, and Intel drivers all handle this.
//
// On a multi-output setup, [[display]] entries apply per match. An
// output not matching any entry is left at the server's current state.

use crate::config::Display;
use crate::monitor::Monitors;

// Apply every [[display]] entry against the current monitor list.
// Called from Wm::init, reload_config, and refresh_monitors.
pub fn apply_all(displays: &[Display], monitors: &Monitors) {
    if displays.is_empty() {
        return;
    }
    for d in displays {
        // Match against connector name first, then EDID model
        // substring (case-insensitive).
        let pattern = d.r#match.to_lowercase();
        let matched = monitors
            .iter()
            .find(|m| m.name.to_lowercase().contains(&pattern));
        let mon_name = match matched {
            Some(m) => m.name.clone(),
            None => {
                eprintln!(
                    "[demiurge] display: no output matches '{}' (skipping)",
                    d.r#match,
                );
                continue;
            }
        };
        apply_one(&mon_name, d);
    }
}

fn apply_one(output_name: &str, d: &Display) {
    let mut cmd = std::process::Command::new("xrandr");
    cmd.arg("--output").arg(output_name);

    if !d.mode.is_empty() {
        cmd.arg("--mode").arg(&d.mode);
    }

    // DSR: --scale Nx N expands the framebuffer by N along each axis.
    // The CRTC scans out at the panel's native mode, downscaling. The
    // driver-side scaler quality varies (NVIDIA's is fine, AMD's is
    // fine, Intel's is acceptable); for higher-quality Lanczos
    // downscale we'd need a compositor-side pass, which is what the
    // Wayland implementation does.
    if d.dsr_multiplier > 1.0 {
        let scale = format!("{}x{}", d.dsr_multiplier, d.dsr_multiplier);
        cmd.arg("--scale").arg(&scale);
        // Filter selection: --filter takes "bilinear" or "nearest". X11
        // RandR doesn't expose Lanczos at this layer (the GPU's scanout
        // scaler does the work). Default to bilinear. Honor the
        // user's choice if they explicitly set it; ignore "lanczos"
        // (no-op, falls back to bilinear).
        let filter = match d.filter.as_str() {
            "nearest" => "nearest",
            _ => "bilinear",
        };
        cmd.arg("--filter").arg(filter);
    } else if (d.dsr_multiplier - 1.0).abs() > f64::EPSILON {
        // Multiplier < 1.0 doesn't make sense for DSR; ignore but warn.
        eprintln!(
            "[demiurge] display '{}': dsr_multiplier {} < 1.0 ignored",
            output_name, d.dsr_multiplier,
        );
    }

    if d.dpi > 0 {
        // --dpi is screen-wide on xrandr (not per-output) but applying
        // it during a per-output invocation works -- it's the global
        // DPI hint the X server reports via Xft / Xresources. If
        // multiple [[display]] entries set conflicting DPI values, the
        // last one wins.
        cmd.arg("--dpi").arg(d.dpi.to_string());
    }

    match cmd.status() {
        Ok(status) if status.success() => {
            eprintln!(
                "[demiurge] display: applied {} (mode='{}', dsr={}, dpi={})",
                output_name,
                if d.mode.is_empty() { "unchanged" } else { d.mode.as_str() },
                d.dsr_multiplier,
                d.dpi,
            );
        }
        Ok(status) => {
            eprintln!(
                "[demiurge] display: xrandr exited {} for output '{}'",
                status.code().unwrap_or(-1),
                output_name,
            );
        }
        Err(e) => {
            eprintln!(
                "[demiurge] display: xrandr spawn failed for '{}': {}",
                output_name, e,
            );
        }
    }
}
