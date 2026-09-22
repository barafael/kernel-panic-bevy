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
}

impl UnitAnim for CarrierAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = CarrierPieces::bind(rig);
    }

    fn activate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): move mover to y-axis [16] speed [8] — the pad
        // rises and hovers while the factory is producing.
        rig.move_to(self.pieces.mover, Axis::Y, 16.0, 8.0);
    }

    fn deactivate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): move mover to y-axis 0 speed [12].
        rig.move_to(self.pieces.mover, Axis::Y, 0.0, 12.0);
    }
}
