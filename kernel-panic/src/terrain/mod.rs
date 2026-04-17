pub mod geovent;
pub mod material;
pub mod mesh;

use bevy::prelude::*;

use geovent::{GeoventAssets, emit_geovent_smoke, tick_geovent_smoke};

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GeoventAssets>().add_systems(
            Update,
            (
                emit_geovent_smoke,
                tick_geovent_smoke.after(emit_geovent_smoke),
            ),
        );
    }
}
