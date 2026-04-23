//! Weapon visual effects — beams, projectiles, and impact flashes.
//!
//! The combat system pushes [`AttackEvent`]s into a [`PendingAttacks`] buffer.
//! `spawn_weapon_visuals` drains the buffer and spawns the right visual;
//! `tick_weapon_fx` fades/moves/despawns them each frame.

mod ceg;
mod shared;
mod spawn;
mod tick;

pub use shared::{AttackEvent, DelayedHitInfo, ExplosionEvent, PendingAttacks, PendingExplosions};

use bevy::prelude::*;

use ceg::{CegParticleMesh, CegRegistry};
use shared::{
    BeamMaterialCache, BuildSparkleAssets, GroundFlashAssets, ImpactBurstAssets, WeaponFxMeshes,
};

use super::GameplaySet;

/// Registers the visual-effect resources and both fx systems.
///
/// The spawn and tick systems both land in `GameplaySet::Simulate` with the
/// spawn system running first so the tick system sees newly-spawned visuals
/// on the same frame.
pub struct WeaponFxPlugin;

impl Plugin for WeaponFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingAttacks>()
            .init_resource::<PendingExplosions>()
            .init_resource::<BeamMaterialCache>()
            .init_resource::<BuildSparkleAssets>()
            .init_resource::<ImpactBurstAssets>()
            .init_resource::<GroundFlashAssets>()
            .init_resource::<WeaponFxMeshes>()
            .init_resource::<CegParticleMesh>()
            .insert_resource(CegRegistry::load())
            .add_systems(
                Update,
                (
                    // tick fires `DelayedHit` into `PendingExplosions`;
                    // the explosion spawner must follow it in the chain
                    // or impact CEGs land a frame late.
                    spawn::spawn_weapon_visuals,
                    tick::tick_weapon_fx,
                    spawn::spawn_pending_explosions,
                    ceg::tick_ceg_particles,
                    ceg::tick_ceg_flames,
                    ceg::tick_ceg_delayed_spawns,
                )
                    .chain()
                    .in_set(GameplaySet::Simulate),
            );
    }
}
