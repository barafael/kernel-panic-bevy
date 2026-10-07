//! window.bos — the Hacker datavent factory. While it is being built the
//! `Anim()` thread flips the hourglass: the three sand pieces turn 180°
//! a quarter second apart, the hourglass itself turns over at 480°/s,
//! everything resets and the loop repeats every 1.375 s. Once built the
//! bar and flap appear and the hourglass is hidden. `Activate()` swings
//! the flap open at 120°/s (0.75 s) and only then sets `INBUILDSTANCE`;
//! `Deactivate()` closes it.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// Anim(): `sleep 250` between the sand flips, hourglass @480°/s,
/// reset after the hourglass turn, `sleep 250`, loop.
const SAND_STEP: f32 = 0.25;
const HOURGLASS_SPEED: f32 = 480.0;
const LOOP_PERIOD: f32 = 1.375;
/// Activate(): `turn flap to x-axis <-90> speed <120>`.
const FLAP_OPEN_DEG: f32 = -90.0;
const FLAP_SPEED: f32 = 120.0;

#[derive(Clone, Copy, Default)]
struct WindowPieces {
    bar: usize,
    flap: usize,
    hourglass: usize,
    hglass: [usize; 2],
    sand: [usize; 3],
}

impl WindowPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            bar: rig.bind_piece("bar"),
            flap: rig.bind_piece("flap"),
            hourglass: rig.bind_piece("hourglass"),
            hglass: [rig.bind_piece("hglass0"), rig.bind_piece("hglass1")],
            sand: [
                rig.bind_piece("sand0"),
                rig.bind_piece("sand1"),
                rig.bind_piece("sand2"),
            ],
        }
    }
}

/// One `Anim()` command, fired when the lap clock passes its time.
type AnimStep = fn(&mut AnimRig, &WindowPieces);

#[derive(Default)]
pub struct WindowAnim {
    pieces: WindowPieces,
    /// Anim() loop clock, 0..LOOP_PERIOD.
    anim_t: f32,
    /// Anim() steps already fired this lap (0..=4).
    anim_step: usize,
    /// Post-emerge cleanup has run (show bar/flap, hide the hourglass).
    finished: bool,
    /// Activate() has been called and not yet undone.
    active: bool,
    /// `INBUILDSTANCE`: the flap is fully open.
    in_stance: bool,
}

impl UnitAnim for WindowAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = WindowPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide bar; hide flap; start-script Anim().
        rig.hide(self.pieces.bar);
        rig.hide(self.pieces.flap);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if ctx.emerging && !self.finished {
            // Anim(): sand2, sand1, sand0 flip 180° "now" a quarter
            // second apart, the hourglass turns over at 480°/s, then
            // everything snaps back and the lap restarts.
            self.anim_t += ctx.dt;
            let p = self.pieces;
            let steps: [(f32, AnimStep); 5] = [
                (0.0, |rig, p| rig.turn_deg(p.sand[2], Axis::Z, 180.0, 0.0)),
                (SAND_STEP, |rig, p| {
                    rig.turn_deg(p.sand[1], Axis::Z, 180.0, 0.0)
                }),
                (2.0 * SAND_STEP, |rig, p| {
                    rig.turn_deg(p.sand[0], Axis::Z, 180.0, 0.0)
                }),
                (3.0 * SAND_STEP, |rig, p| {
                    rig.turn_deg(p.hourglass, Axis::Z, 180.0, HOURGLASS_SPEED)
                }),
                (3.0 * SAND_STEP + 180.0 / HOURGLASS_SPEED, |rig, p| {
                    for piece in [p.hourglass, p.sand[0], p.sand[1], p.sand[2]] {
                        rig.turn_deg(piece, Axis::Z, 0.0, 0.0);
                    }
                }),
            ];
            while self.anim_step < steps.len() && self.anim_t >= steps[self.anim_step].0 {
                (steps[self.anim_step].1)(rig, &p);
                self.anim_step += 1;
            }
            if self.anim_t >= LOOP_PERIOD {
                self.anim_t -= LOOP_PERIOD;
                self.anim_step = 0;
            }
        } else if !self.finished && !ctx.emerging {
            // Create(), post-build: show bar/flap; hide the hourglass.
            self.finished = true;
            rig.show(self.pieces.bar);
            rig.show(self.pieces.flap);
            for piece in self.pieces.hglass.into_iter().chain(self.pieces.sand) {
                rig.hide(piece);
            }
        }

        // Activate(): `wait-for-turn flap around x-axis; INBUILDSTANCE 1`.
        self.in_stance = self.active && rig.at_target(self.pieces.flap, Axis::X);
    }

    fn activate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): flap to x <-90> speed <120> — the window opens.
        self.active = true;
        rig.turn_deg(self.pieces.flap, Axis::X, FLAP_OPEN_DEG, FLAP_SPEED);
    }

    fn deactivate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): INBUILDSTANCE 0; flap back to 0.
        self.active = false;
        self.in_stance = false;
        rig.turn_deg(self.pieces.flap, Axis::X, 0.0, FLAP_SPEED);
    }

    fn is_open(&self) -> Option<bool> {
        Some(self.in_stance)
    }
}
