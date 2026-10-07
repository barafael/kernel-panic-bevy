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
    /// `INBUILDSTANCE`: set once the pad has finished its 2 s lift.
    in_stance: bool,
}

impl UnitAnim for CarrierAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = CarrierPieces::bind(rig);
    }

    fn update(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): `wait-for-move mover along y-axis; INBUILDSTANCE 1`.
        self.in_stance = self.active && rig.at_target(self.pieces.mover, Axis::Y);
    }

    fn activate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): move mover to y-axis [16] speed [8] — the pad
        // rises (2 s) and hovers while the factory is producing.
        self.active = true;
        rig.move_to(self.pieces.mover, Axis::Y, 16.0, 8.0);
    }

    fn deactivate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): move mover to y-axis 0 speed [12].
        self.active = false;
        self.in_stance = false;
        rig.move_to(self.pieces.mover, Axis::Y, 0.0, 12.0);
    }

    fn is_open(&self) -> Option<bool> {
        Some(self.in_stance)
    }
}
