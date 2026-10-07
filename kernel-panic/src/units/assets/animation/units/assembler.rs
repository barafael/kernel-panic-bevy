//! assembler.bos — the System mobile constructor. Rises while built,
//! then its `body` ring spins slowly around y. `StartBuilding(heading,
//! pitch)`: the nozzle extends 8 elmos, the rotor turns onto the site at
//! 120°/s, the nozzle pitches at 120°/s, `INBUILDSTANCE` is set and the
//! `tip` fires a `BuildLaser` beam every 60 ms. `StopBuilding()` undoes
//! that in reverse (nozzle home at 8 elmos/s).

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};
use super::{DeathFx, move_time, turn_time};

/// assembler.bos Create(): `spin body around y-axis speed <60>`
const BODY_SPIN_DPS: f32 = 60.0;
/// StartBuilding(): `move nozzle to z-axis [8] speed [16]`.
const NOZZLE_OUT: f32 = 8.0;
const NOZZLE_OUT_SPEED: f32 = 16.0;
/// StopBuilding(): `move nozzle to z-axis 0 speed [8]`.
const NOZZLE_HOME_SPEED: f32 = 8.0;
/// Rotor and nozzle turns, `speed <120>`.
const TURN_SPEED: f32 = 120.0;
/// `emit-sfx 2048 from tip; sleep 60`.
const EMIT_INTERVAL: f32 = 0.06;
const WAIT_LATENCY: f32 = 1.0 / 30.0;

#[derive(Clone, Copy, Default)]
struct AssemblerPieces {
    base: usize,
    body: usize,
    rotor: usize,
    nozzle: usize,
    tip: usize,
}

impl AssemblerPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            base: rig.bind_piece("base"),
            body: rig.bind_piece("body"),
            rotor: rig.bind_piece("rotor"),
            nozzle: rig.bind_piece("nozzle"),
            tip: rig.bind_piece("tip"),
        }
    }
}

/// Where the StartBuilding / StopBuilding thread is; each step waits for
/// the move or turn it issued.
#[derive(Clone, Copy, Debug, PartialEq)]
enum BuildPhase {
    Extend,
    Rotor,
    Nozzle,
    /// `INBUILDSTANCE`: spraying from the tip.
    Spraying,
    StowNozzle,
    StowRotor,
    Retract,
}

#[derive(Default)]
pub struct AssemblerAnim {
    pieces: AssemblerPieces,
    /// Spin starts post-emerge.
    spinning: bool,
    death: DeathFx,
    phase: Option<BuildPhase>,
    /// Seconds left in the current phase's wait.
    wait: f32,
    /// Body-relative aim of the build site (radians).
    aim: (f32, f32),
    emit_timer: f32,
}

impl AssemblerAnim {
    fn enter(&mut self, rig: &mut AnimRig, phase: BuildPhase) {
        let p = self.pieces;
        let (heading, pitch) = self.aim;
        self.wait = match phase {
            BuildPhase::Extend => {
                rig.move_to(p.nozzle, Axis::Z, NOZZLE_OUT, NOZZLE_OUT_SPEED);
                move_time(rig, p.nozzle, Axis::Z, NOZZLE_OUT, NOZZLE_OUT_SPEED) + WAIT_LATENCY
            }
            BuildPhase::Rotor => {
                rig.turn_rad(p.rotor, Axis::Y, heading, TURN_SPEED.to_radians());
                turn_time(rig, p.rotor, Axis::Y, heading.to_degrees(), TURN_SPEED) + WAIT_LATENCY
            }
            BuildPhase::Nozzle => {
                rig.turn_rad(p.nozzle, Axis::X, -pitch, TURN_SPEED.to_radians());
                turn_time(rig, p.nozzle, Axis::X, -pitch.to_degrees(), TURN_SPEED) + WAIT_LATENCY
            }
            BuildPhase::Spraying => f32::INFINITY,
            BuildPhase::StowNozzle => {
                rig.turn_deg(p.nozzle, Axis::X, 0.0, TURN_SPEED);
                turn_time(rig, p.nozzle, Axis::X, 0.0, TURN_SPEED) + WAIT_LATENCY
            }
            BuildPhase::StowRotor => {
                rig.turn_deg(p.rotor, Axis::Y, 0.0, TURN_SPEED);
                turn_time(rig, p.rotor, Axis::Y, 0.0, TURN_SPEED) + WAIT_LATENCY
            }
            BuildPhase::Retract => {
                rig.move_to(p.nozzle, Axis::Z, 0.0, NOZZLE_HOME_SPEED);
                move_time(rig, p.nozzle, Axis::Z, 0.0, NOZZLE_HOME_SPEED) + WAIT_LATENCY
            }
        };
        self.phase = Some(phase);
    }
}

impl UnitAnim for AssemblerAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = AssemblerPieces::bind(rig);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create(): base sinks [-8]·pct/100 while building, then the
        // body ring starts spinning.
        if ctx.emerging {
            super::emerge_lift(rig, self.pieces.base, 20.0, ctx.build_percent);
        }
        self.death.tick(ctx.dt);
        if !self.spinning && ctx.build_percent <= 0 {
            self.spinning = true;
            rig.spin_dps(self.pieces.body, Axis::Y, BODY_SPIN_DPS);
        }

        let Some(phase) = self.phase else {
            return;
        };
        if phase == BuildPhase::Spraying {
            self.emit_timer -= ctx.dt;
            if self.emit_timer <= 0.0 {
                self.emit_timer = EMIT_INTERVAL;
                rig.emit(self.pieces.tip, SfxKind::BuildBeam);
            }
            return;
        }
        self.wait -= ctx.dt;
        if self.wait > 0.0 {
            return;
        }
        match phase {
            BuildPhase::Extend => self.enter(rig, BuildPhase::Rotor),
            BuildPhase::Rotor => self.enter(rig, BuildPhase::Nozzle),
            BuildPhase::Nozzle => {
                self.emit_timer = 0.0;
                self.enter(rig, BuildPhase::Spraying);
            }
            BuildPhase::Spraying => {}
            BuildPhase::StowNozzle => self.enter(rig, BuildPhase::StowRotor),
            BuildPhase::StowRotor => self.enter(rig, BuildPhase::Retract),
            BuildPhase::Retract => self.phase = None,
        }
    }

    fn start_building(&mut self, rig: &mut AnimRig, heading: f32, pitch: f32) {
        // StartBuilding(h, p): `signal SIG_BUILD` kills a stow in flight;
        // extend, turn onto the site, then spray.
        self.aim = (heading, pitch);
        self.enter(rig, BuildPhase::Extend);
    }

    fn stop_building(&mut self, rig: &mut AnimRig) {
        // StopBuilding(): INBUILDSTANCE off; nozzle level, rotor home,
        // nozzle in.
        self.enter(rig, BuildPhase::StowNozzle);
    }

    fn in_build_stance(&self) -> Option<bool> {
        Some(self.phase == Some(BuildPhase::Spraying))
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide body/nozzle; explode body SHATTER, nozzle FALL.
        rig.hide(self.pieces.body);
        rig.hide(self.pieces.nozzle);
        rig.explode(self.pieces.body, 4);
        rig.explode(self.pieces.nozzle, 3);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }

    fn aim(&mut self, _rig: &mut AnimRig, _h: f32, _p: f32, _ctx: AnimCtx) -> bool {
        // assembler.bos AimWeapon1: `return 0` — the assembler is a
        // builder and never fires through the weapon pipeline.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// StartBuilding: nozzle out (0.5 s), rotor onto the heading, nozzle
    /// onto the pitch, and only then `INBUILDSTANCE` and beams from the
    /// tip; StopBuilding stows in reverse.
    #[test]
    fn build_stance_follows_the_aim_sequence() {
        let mut rig = AnimRig::for_test(&["base", "body", "rotor", "nozzle", "tip"]);
        let mut anim = AssemblerAnim::default();
        anim.bind(&rig);
        let step = |anim: &mut AssemblerAnim, rig: &mut AnimRig, frames: usize| {
            for _ in 0..frames {
                anim.update(
                    rig,
                    AnimCtx {
                        dt: 1.0 / 30.0,
                        ..AnimCtx::minimal()
                    },
                );
                super::super::super::tick_rig(rig, 1.0 / 30.0);
            }
        };
        // 90° heading at 120°/s = 0.75 s, 30° pitch = 0.25 s.
        anim.start_building(&mut rig, 90f32.to_radians(), 30f32.to_radians());
        assert_eq!(anim.in_build_stance(), Some(false));
        step(&mut anim, &mut rig, 17);
        assert_eq!(rig.piece_translations[3][2], 8.0, "nozzle extended");
        assert_eq!(anim.phase, Some(BuildPhase::Rotor));
        step(&mut anim, &mut rig, 24);
        assert_eq!(anim.phase, Some(BuildPhase::Nozzle));
        step(&mut anim, &mut rig, 10);
        assert_eq!(anim.in_build_stance(), Some(true));
        assert!((rig.piece_rotations[2][1] - 90f32.to_radians()).abs() < 1e-3);
        assert!((rig.piece_rotations[3][0] + 30f32.to_radians()).abs() < 1e-3);
        rig.outbox.clear();
        step(&mut anim, &mut rig, 2);
        assert!(!rig.outbox.is_empty(), "spraying from the tip");

        anim.stop_building(&mut rig);
        assert_eq!(anim.in_build_stance(), Some(false));
        step(&mut anim, &mut rig, 10 + 24 + 32);
        assert_eq!(anim.phase, None);
        assert_eq!(rig.piece_translations[3][2], 0.0);
    }
}
