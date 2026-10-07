//! trojan.bos — the Hacker mobile builder. After emerging, its `center`
//! ring spins slowly around the z-axis; the four `piece`s are the
//! build-effect anchors. `StartBuilding(h, p)` pitches `center` to −p at
//! 120°/s while the body steps onto the heading (the host's facing), sets
//! `INBUILDSTANCE` and fires a `BuildLaser` from `center` every 60 ms;
//! `StopBuilding()` levels it again.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};
use super::{DeathFx, turn_time};

/// trojan.bos Create(): `spin center around z-axis speed <-120>`
const CENTER_SPIN_DPS: f32 = -120.0;
/// StartBuilding(): `turn center to x-axis 0-p speed <120>`.
const CENTER_PITCH_SPEED: f32 = 120.0;
/// `emit-sfx 2048 from center; sleep 60`.
const EMIT_INTERVAL: f32 = 0.06;

#[derive(Clone, Copy, Default)]
struct TrojanPieces {
    center: usize,
    piece: [usize; 4],
}

impl TrojanPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            center: rig.bind_piece("center"),
            piece: [
                rig.bind_piece("piece0"),
                rig.bind_piece("piece1"),
                rig.bind_piece("piece2"),
                rig.bind_piece("piece3"),
            ],
        }
    }
}

#[derive(Default)]
pub struct TrojanAnim {
    pieces: TrojanPieces,
    /// The spin starts only once the emerge completes (`sleep 5000`
    /// after BUILD_PERCENT_LEFT drains in the script).
    spinning: bool,
    death: DeathFx,
    /// StartBuilding() thread: seconds until the pitch wait ends, then
    /// `INBUILDSTANCE` and spraying.
    building: Option<f32>,
    in_stance: bool,
    emit_timer: f32,
}

impl UnitAnim for TrojanAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = TrojanPieces::bind(rig);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        self.death.tick(ctx.dt);
        if !self.spinning && ctx.build_percent <= 0 {
            self.spinning = true;
            rig.spin_dps(self.pieces.center, Axis::Z, CENTER_SPIN_DPS);
        }
        if let Some(wait) = &mut self.building {
            if *wait > 0.0 {
                *wait -= ctx.dt;
                if *wait <= 0.0 {
                    self.in_stance = true;
                    self.emit_timer = 0.0;
                }
            } else {
                self.emit_timer -= ctx.dt;
                if self.emit_timer <= 0.0 {
                    self.emit_timer = EMIT_INTERVAL;
                    rig.emit(self.pieces.center, SfxKind::BuildBeam);
                }
            }
        }
    }

    fn start_building(&mut self, rig: &mut AnimRig, _heading: f32, pitch: f32) {
        // StartBuilding(h, p): center x → −p @120 (the body heading is
        // the host's), wait, INBUILDSTANCE, spray.
        rig.turn_rad(
            self.pieces.center,
            Axis::X,
            -pitch,
            CENTER_PITCH_SPEED.to_radians(),
        );
        let wait = turn_time(
            rig,
            self.pieces.center,
            Axis::X,
            -pitch.to_degrees(),
            CENTER_PITCH_SPEED,
        );
        self.building = Some(wait + 1.0 / 30.0);
        self.in_stance = false;
    }

    fn stop_building(&mut self, rig: &mut AnimRig) {
        // StopBuilding(): INBUILDSTANCE off; center level.
        self.building = None;
        self.in_stance = false;
        rig.turn_deg(self.pieces.center, Axis::X, 0.0, CENTER_PITCH_SPEED);
    }

    fn in_build_stance(&self) -> Option<bool> {
        Some(self.in_stance)
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode piece0..3 FALL, then hide them.
        for piece in self.pieces.piece {
            rig.explode(piece, 3);
            rig.hide(piece);
        }
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }
}
