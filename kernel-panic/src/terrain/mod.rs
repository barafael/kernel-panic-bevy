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
            // Vent claims steer the AI's expansion (`ai_brain` skips
            // claimed vents): sim state, so they are reconciled on the
            // fixed tick. In `Update` they followed the render clock,
            // and the AI's choices with them (frame-rate-dependent sim;
            // `headless` determinism experiments).
            .add_systems(
                FixedUpdate,
                reconcile_vent_claims
                    // After this tick's commands: a `Build` claims vents.
                    .after(crate::net::SimCommandSet)
                    .before(crate::units::ai::ai_brain)
                    .before(crate::interaction::movement::advance_command_queue)
                    .run_if(in_state(
                        crate::units::lifecycle::game_over::GameState::Playing,
                    )),
            )
            .add_systems(
                Update,
                (
                    emit_geovent_smoke,
                    tick_geovent_smoke.after(emit_geovent_smoke),
                ),
            );
    }
}
