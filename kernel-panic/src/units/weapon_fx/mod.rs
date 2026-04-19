//! Weapon visual effects — beams, projectiles, and impact flashes.
//!
//! The combat system pushes [`AttackEvent`]s into a [`PendingAttacks`] buffer.
//! `spawn_weapon_visuals` drains the buffer and spawns the right visual;
//! `tick_weapon_fx` fades/moves/despawns them each frame.

mod shared;
mod spawn;
mod tick;

pub use shared::{AttackEvent, ExplosionEvent, PendingAttacks, PendingExplosions};

use bevy::prelude::*;

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
            .add_systems(
                Update,
                (
                    spawn::spawn_weapon_visuals,
                    spawn::spawn_pending_explosions,
                    tick::tick_weapon_fx,
                )
                    .chain()
                    .in_set(GameplaySet::Simulate),
            );
    }
}
