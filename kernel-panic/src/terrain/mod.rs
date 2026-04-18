pub mod geovent;
pub mod heightmap;
pub mod material;
pub mod mesh;

use bevy::prelude::*;

use geovent::{
    GeoventAssets, VentClaimReleaseTimer, emit_geovent_smoke, release_stale_vent_claims,
    tick_geovent_smoke,
};

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GeoventAssets>()
            .init_resource::<VentClaimReleaseTimer>()
            .add_systems(
                Update,
                (
                    release_stale_vent_claims,
                    emit_geovent_smoke,
                    tick_geovent_smoke.after(emit_geovent_smoke),
                ),
            );
    }
}
