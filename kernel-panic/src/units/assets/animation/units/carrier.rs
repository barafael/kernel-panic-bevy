//! carrier.bos — the Network homebase. The `mover` pad lifts when
//! production starts and lowers when it stops; the shoulder/arm/finger
//! tree carries the host-driven build beam.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Clone, Copy, Default)]
struct CarrierPieces {
    mover: usize,
}

impl CarrierPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            mover: rig.bind_piece("mover"),
        }
    }
}

#[derive(Default)]
pub struct CarrierAnim {
    pieces: CarrierPieces,
    /// Activate() has run and Deactivate() has not.
    active: bool,
    /// `INBUILDSTANCE`: set once the pad has finished its first 2 s
    /// lift, and never cleared — carrier.bos `Deactivate()` only lowers
    /// the pad — so a Carrier that reopens builds at once.
    in_stance: bool,
}

impl UnitAnim for CarrierAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = CarrierPieces::bind(rig);
    }

    fn update(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): `wait-for-move mover along y-axis; INBUILDSTANCE 1`.
        if self.active && rig.at_target(self.pieces.mover, Axis::Y) {
            self.in_stance = true;
        }
    }

    fn activate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): move mover to y-axis [16] speed [8] — the pad
        // rises (2 s) and hovers while the factory is producing.
        self.active = true;
        rig.move_to(self.pieces.mover, Axis::Y, 16.0, 8.0);
    }

    fn deactivate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): `signal SIG_ActDeact` (a lift still under way
        // never sets the stance); move mover to y-axis 0 speed [12].
        self.active = false;
        rig.move_to(self.pieces.mover, Axis::Y, 0.0, 12.0);
    }

    fn is_open(&self) -> Option<bool> {
        Some(self.in_stance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// carrier.bos: the first finished lift sets `INBUILDSTANCE`, and
    /// `Deactivate()` only lowers the pad, so a Carrier that reopens is
    /// in its build stance at once. A lift cut short sets nothing.
    #[test]
    fn the_build_stance_outlives_a_deactivate() {
        let mut rig = AnimRig::for_test(&["root", "mover", "pad"]);
        let mut anim = CarrierAnim::default();
        anim.bind(&rig);
        let step = |anim: &mut CarrierAnim, rig: &mut AnimRig, frames: usize| {
            for _ in 0..frames {
                anim.update(rig, AnimCtx::minimal());
                super::super::super::tick_rig(rig, 1.0 / 30.0);
            }
        };
        // Cut short: 16 elmos at 8 a second take 60 frames.
        anim.activate(&mut rig, AnimCtx::minimal());
        step(&mut anim, &mut rig, 30);
        anim.deactivate(&mut rig, AnimCtx::minimal());
        step(&mut anim, &mut rig, 90);
        assert_eq!(anim.is_open(), Some(false));

        anim.activate(&mut rig, AnimCtx::minimal());
        step(&mut anim, &mut rig, 59);
        assert_eq!(anim.is_open(), Some(false));
        step(&mut anim, &mut rig, 3);
        assert_eq!(anim.is_open(), Some(true));
        anim.deactivate(&mut rig, AnimCtx::minimal());
        step(&mut anim, &mut rig, 90);
        assert_eq!(anim.is_open(), Some(true), "kept through the close");
        anim.activate(&mut rig, AnimCtx::minimal());
        step(&mut anim, &mut rig, 1);
        assert_eq!(anim.is_open(), Some(true), "reopening builds at once");
    }
}
