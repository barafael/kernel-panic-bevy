//! obelisk.bos — the Hacker infection-gas artillery. Four segments
//! part when aiming, snap back while idle, and the tip smoulders
//! between the 40-second reloads.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};

/// obelisk.bos FireWeapon1(): `sleep 40000` reload.
const RELOAD_SECS: f32 = 40.0;

#[derive(Clone, Copy, Default)]
struct ObeliskPieces {
    segf: usize,
    segb: usize,
    segl: usize,
    segr: usize,
    tip: usize,
}

impl ObeliskPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            segf: rig.bind_piece("segf"),
            segb: rig.bind_piece("segb"),
            segl: rig.bind_piece("segl"),
            segr: rig.bind_piece("segr"),
            tip: rig.bind_piece("tip"),
        }
    }
}

#[derive(Default)]
pub struct ObeliskAnim {
    pieces: ObeliskPieces,
    /// Mirrors the script's `reloading` static.
    reloading: bool,
    reload_timer: f32,
    /// Idle smoulder emit timer.
    charge_timer: f32,
    /// Create() splays the four segments ±16 elmos and closes them once
    /// the emerge completes (`move ... speed [8]` after the build loop).
    post_emerge_done: bool,
}

impl ObeliskAnim {
    /// Segment x/z part/ home targets at a shared speed — the script's
    /// open, close and reload-relax poses all use this one motion.
    fn segments_to(&self, rig: &mut AnimRig, part: f32, speed: f32) {
        rig.move_to(self.pieces.segf, Axis::Z, part, speed);
        rig.move_to(self.pieces.segb, Axis::Z, -part, speed);
        rig.move_to(self.pieces.segr, Axis::X, part, speed);
        rig.move_to(self.pieces.segl, Axis::X, -part, speed);
    }
}

impl UnitAnim for ObeliskAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = ObeliskPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): segs splayed out ±16 elmos (bytecode ±1048576)
        // while the obelisk builds.
        self.segments_to(rig, 16.0, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if !self.post_emerge_done && !ctx.emerging {
            // Create(), post-build: segments close to rest @8 elmos/s.
            self.post_emerge_done = true;
            self.segments_to(rig, 0.0, 8.0);
        }

        if self.reloading {
            self.reload_timer -= ctx.dt;
            if self.reload_timer <= 0.0 {
                // ResetAim()/ChargeFX(): segments home, tip smoulders.
                self.reloading = false;
                self.segments_to(rig, 0.0, 8.0);
            }
        } else {
            // ChargeFX(): emit-sfx 1024 from tip while !reloading
            // (script: every 50ms; throttled here for particle budget).
            self.charge_timer -= ctx.dt;
            if self.charge_timer <= 0.0 {
                self.charge_timer = 0.25;
                rig.emit(self.pieces.tip, SfxKind::Puff);
            }
        }
    }

    fn aim(&mut self, rig: &mut AnimRig, _h: f32, _p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1: `if(reloading) return 0`; otherwise part the four
        // segments and return 1 once segf arrives.
        if self.reloading {
            return false;
        }
        self.segments_to(rig, 8.0, 8.0);
        true
    }

    fn fire(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): reloading=1; sleep 40000.
        self.reloading = true;
        self.reload_timer = RELOAD_SECS;
    }
}
