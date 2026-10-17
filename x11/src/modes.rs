// Output modes follow the panel. Two jobs, both answering a gap X leaves:
//
//   - When an output's advertised mode list changes (hot-plug, or a monitor
//     switching its input layout, as the LG DualUp does leaving PBP), X keeps
//     scanning out the old mode. DEMIURGE sets the new preferred mode, unless
//     a [[display]] entry pins the output.
//   - X does not revert a RandR mode set when the client that made it dies.
//     A mode change that arrives while a fullscreen client exists, or within
//     GRACE before one appears (Wine sets the mode before mapping its
//     window), belongs to that client; once no fullscreen client remains,
//     the output returns to its preferred mode. The preferred mode is the
//     only target, never an earlier mode: A game that sets its mode long
//     before its window goes fullscreen makes that mode look hand-set, and
//     restoring to it would undo the game's own restore. A pinned output,
//     or one that reports no preferred mode, returns to its baseline. A
//     change with no fullscreen client around is the user's and becomes the
//     new baseline.
//
// Pure state: The caller feeds RandR snapshots and fullscreen transitions
// and carries out the returned mode sets.

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputMode {
    pub name: String,
    // Current CRTC mode id.
    pub mode: u32,
    pub preferred: Option<u32>,
    // The advertised mode ids; a change means new EDID or a hot-plug.
    pub modes: Vec<u32>,
}

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
    // The baselines replaced by the last change made with no fullscreen
    // client present, and when; a fullscreen client appearing within GRACE
    // claims that change.
    unowned_change: Option<(Instant, HashMap<String, u32>)>,
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
        now: Instant,
    ) -> Vec<SetMode> {
        let mut sets = Vec::new();
        let mut replaced = HashMap::new();
        self.seen.retain(|name, _| outputs.iter().any(|o| &o.name == name));
        self.baseline.retain(|name, _| outputs.iter().any(|o| &o.name == name));

        for o in outputs {
            let Some(seen) = self.seen.get(&o.name) else {
                self.seen.insert(o.name.clone(), o.modes.clone());
                self.baseline.insert(o.name.clone(), o.mode);
                continue;
            };
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
                continue;
            }
            let base = self.baseline.get(&o.name).copied().unwrap_or(o.mode);
            if o.mode == base {
                continue;
            }
            if fullscreen_present {
                self.owned = true;
            } else if !self.owned {
                replaced.insert(o.name.clone(), base);
                self.baseline.insert(o.name.clone(), o.mode);
            }
        }
        if !replaced.is_empty() {
            self.unowned_change = Some((now, replaced));
        }
        sets
    }

    pub fn on_fullscreen_appeared(&mut self, now: Instant) {
        if let Some((at, replaced)) = self.unowned_change.take()
            && now.duration_since(at) <= GRACE
        {
            self.baseline.extend(replaced);
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
