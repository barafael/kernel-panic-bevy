//! The engine's global `smoothGround`: a [`SmoothHeightMesh`] over the
//! live [`Heightmap`], the surface aircraft hold their cruise altitude
//! above (see `interaction::air_movement`).
//!
//! Built at map load (Hex Farm then overwrites it with its flight
//! profile, `SetWholeSmoothMesh`); terrain edits report the touched
//! area through [`SmoothGround::map_changed`] (`CBasicMapDamage::
//! RecalcArea`), and [`update_smooth_ground`] runs the engine's
//! once-per-frame incremental `UpdateSmoothMesh`.

use bevy::prelude::*;
use spring_map::smooth_mesh::SmoothHeightMesh;

use super::heightmap::Heightmap;

#[derive(Resource)]
pub struct SmoothGround {
    pub mesh: SmoothHeightMesh,
    /// Sim frames since the map loaded (`gs->frameNum` for the update
    /// delay).
    frame: u64,
}

impl SmoothGround {
    pub fn new(mesh: SmoothHeightMesh) -> Self {
        Self { mesh, frame: 0 }
    }

    /// Build from the heightmap with the modinfo defaults.
    pub fn from_heightmap(hm: &Heightmap) -> Self {
        let (w, h) = hm.grid_size();
        Self::new(SmoothHeightMesh::new(hm.heights(), w, h))
    }

    /// `smoothGround.GetHeight(x, z)`.
    pub fn get_height(&self, x: f32, z: f32) -> f32 {
        self.mesh.get_height(x, z)
    }

    /// `smoothGround.MapChanged(x1, z1, x2, z2)`: heightmap vertices
    /// `x1..=x2 × z1..=z2` changed this frame.
    pub fn map_changed(&mut self, x1: usize, z1: usize, x2: usize, z2: usize) {
        self.mesh
            .map_changed(x1 as i64, z1 as i64, x2 as i64, z2 as i64, self.frame);
    }
}

/// `smoothGround.UpdateSmoothMesh()`, once per sim frame.
pub fn update_smooth_ground(sg: Option<ResMut<SmoothGround>>, hm: Option<Res<Heightmap>>) {
    let (Some(mut sg), Some(hm)) = (sg, hm) else {
        return;
    };
    let sg = sg.bypass_change_detection();
    sg.frame += 1;
    if sg.mesh.is_updating() {
        let frame = sg.frame;
        sg.mesh.update(hm.heights(), frame);
    }
}
