// Output modes follow the panel. Three jobs, each answering a gap X leaves.
// An output's target is its desktop mode, the preferred resolution at the
// highest refresh rate the server offers for it (monitor::desktop_mode):
//
//   - A monitor plugged into a connector X is not driving stays dark, and
//     one pulled out keeps its CRTC and the screen its size. DEMIURGE lights
//     the first at its desktop mode, right of the outputs already lit, and
//     turns the second off. Only a connector seen connected for the first
//     time is lit, so an output turned off by hand, or dark when DEMIURGE
//     starts, stays dark.
//   - When an output's advertised mode list changes (a monitor swapped on
//     the same connector, or switching its input layout, as the LG DualUp
//     does leaving PBP), X keeps scanning out the old mode. DEMIURGE sets
//     the new desktop mode, unless a [[display]] entry pins the output.
//   - X does not revert a RandR mode set when the client that made it dies.
//     A mode change that arrives while a fullscreen client exists belongs to
//     that client, and so does one made before it with no fullscreen client
//     around, however long before, while the output still sits on it (Wine
//     sets the mode before mapping its window; DOOM five seconds before).
//     Once no fullscreen client remains, the output returns to its desktop
//     mode. The desktop mode is the only target, never an earlier mode, so
//     a game's own restore is never undone. A pinned output, or one that
//     reports no preferred mode, returns to its baseline. A change with no
//     fullscreen client around becomes the new baseline until a fullscreen
//     client claims it; the accepted cost is that a mode set by hand is
//     undone after a fullscreen video plays and closes.
//
// Pure state: The caller feeds RandR snapshots and fullscreen transitions
// and carries out the returned mode sets.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputMode {
    pub name: String,
    // Current CRTC mode id, 0 for a connected output no CRTC drives.
    pub mode: u32,
    // The desktop mode.
    pub preferred: Option<u32>,
    // The advertised mode ids; a change means new EDID or a hot-plug.
    pub modes: Vec<u32>,
    // False for an output pulled out while lit, which still holds its CRTC.
    pub connected: bool,
}

// A mode to set; mode 0 turns the output off. Turning off comes first in a
// batch, so a pulled monitor's space is gone before a new one is placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetMode {
    pub output: String,
    pub mode: u32,
}

#[derive(Debug, Default)]
pub struct ModeTracker {
    seen: HashMap<String, Vec<u32>>,
    baseline: HashMap<String, u32>,
    owned: bool,
    // Per output, the mode before the changes made with no fullscreen
    // client present, while the output still sits on one of them; a
    // fullscreen client appearing claims them.
    unowned: HashMap<String, u32>,
}

impl ModeTracker {
    pub fn new(outputs: &[OutputMode]) -> Self {
        let mut t = Self::default();
        for o in outputs {
            t.seen.insert(o.name.clone(), o.modes.clone());
            t.baseline.insert(o.name.clone(), o.mode);
        }
        t
    }

    pub fn on_outputs(
        &mut self,
        outputs: &[OutputMode],
        fullscreen_present: bool,
        pinned: impl Fn(&str) -> bool,
    ) -> Vec<SetMode> {
        let mut sets = Vec::new();
        self.seen.retain(|name, _| outputs.iter().any(|o| &o.name == name));
        self.baseline.retain(|name, _| outputs.iter().any(|o| &o.name == name));
        self.unowned.retain(|name, _| outputs.iter().any(|o| &o.name == name));

        for o in outputs {
            if !o.connected {
                sets.push(SetMode { output: o.name.clone(), mode: 0 });
                continue;
            }
            let Some(seen) = self.seen.get(&o.name) else {
                self.seen.insert(o.name.clone(), o.modes.clone());
                let lit = o.preferred.filter(|_| o.mode == 0);
                self.baseline.insert(o.name.clone(), lit.unwrap_or(o.mode));
                if let Some(mode) = lit {
                    sets.push(SetMode { output: o.name.clone(), mode });
                }
                continue;
            };
            if o.mode == 0 {
                self.seen.insert(o.name.clone(), o.modes.clone());
                continue;
            }
            if *seen != o.modes {
                self.seen.insert(o.name.clone(), o.modes.clone());
                let target = match o.preferred {
                    Some(p) if !pinned(&o.name) => p,
                    _ => o.mode,
                };
                if target != o.mode {
                    sets.push(SetMode { output: o.name.clone(), mode: target });
                }
                self.baseline.insert(o.name.clone(), target);
                self.unowned.remove(&o.name);
                continue;
            }
            let base = self.baseline.get(&o.name).copied().unwrap_or(o.mode);
            if o.mode == base {
                continue;
            }
            if fullscreen_present {
                self.owned = true;
            } else if !self.owned {
                // The first change records the mode before it; a change
                // back to that mode leaves nothing to claim.
                let before = *self.unowned.entry(o.name.clone()).or_insert(base);
                if before == o.mode {
                    self.unowned.remove(&o.name);
                }
                self.baseline.insert(o.name.clone(), o.mode);
            }
        }
        sets.sort_by_key(|s| s.mode != 0);
        sets
    }

    pub fn on_fullscreen_appeared(&mut self) {
        if !self.unowned.is_empty() {
            self.baseline.extend(self.unowned.drain());
            self.owned = true;
        }
    }

    // Called once no fullscreen client remains.
    pub fn on_fullscreen_gone(
        &mut self,
        outputs: &[OutputMode],
        pinned: impl Fn(&str) -> bool,
    ) -> Vec<SetMode> {
        if !std::mem::take(&mut self.owned) {
            return Vec::new();
        }
        outputs
            .iter()
            .filter(|o| o.connected && o.mode != 0)
            .filter_map(|o| {
                let target = match o.preferred {
                    Some(p) if !pinned(&o.name) => p,
                    _ => *self.baseline.get(&o.name)?,
                };
                self.baseline.insert(o.name.clone(), target);
                (target != o.mode && o.modes.contains(&target))
                    .then(|| SetMode { output: o.name.clone(), mode: target })
            })
            .collect()
    }
}
