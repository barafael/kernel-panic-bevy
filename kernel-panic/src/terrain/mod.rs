pub mod geovent;
pub mod heightmap;
pub mod material;
pub mod mesh;
pub mod smooth_ground;
pub mod terrain_speed;

use bevy::prelude::*;

use crate::map_loading::MatchScopeAppExt;

use geovent::{
    GeoventAssets, VentClaimReleaseTimer, emit_geovent_smoke, reconcile_vent_claims,
    tick_geovent_smoke,
};

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GeoventAssets>()
            .init_match_resource::<VentClaimReleaseTimer>()
            .add_systems(
                Update,
                (
                    reconcile_vent_claims,
                    emit_geovent_smoke,
                    tick_geovent_smoke.after(emit_geovent_smoke),
                ),
            );
    }
}
