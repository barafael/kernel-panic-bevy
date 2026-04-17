//! Weapon visual effects — beams, projectiles, and impact flashes.
//!
//! The combat system pushes [`AttackEvent`]s into a [`PendingAttacks`] buffer.
//! `spawn_weapon_visuals` drains the buffer and spawns the right visual;
//! `tick_weapon_fx` fades/moves/despawns them each frame.

mod shared;
mod spawn;
mod tick;

pub use shared::{AttackEvent, PendingAttacks};

use bevy::prelude::*;

use shared::{BeamMaterialCache, BuildSparkleAssets, ImpactBurstAssets};

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
            .init_resource::<BeamMaterialCache>()
            .init_resource::<BuildSparkleAssets>()
            .init_resource::<ImpactBurstAssets>()
            .add_systems(
                Update,
                (spawn::spawn_weapon_visuals, tick::tick_weapon_fx)
                    .chain()
                    .in_set(GameplaySet::Simulate),
            );
    }
}
