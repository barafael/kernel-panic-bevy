//! Weapon visual effects — beams, projectiles, and impact flashes.
//!
//! The combat system pushes [`AttackEvent`]s into a [`PendingAttacks`] buffer.
//! `spawn_weapon_visuals` drains the buffer and spawns the right visual;
//! `tick_weapon_fx` fades/moves/despawns them each frame.

pub(crate) mod batch;
mod ceg;
mod flight;
mod shared;
mod spawn;
mod tick;

pub use shared::{AttackEvent, DelayedHitInfo, ExplosionEvent, PendingAttacks, PendingExplosions};

/// Live effect counts for the `KP_PROFILE` census.
#[cfg(not(target_arch = "wasm32"))]
pub fn effect_counts(world: &mut World) -> [(&'static str, usize); 5] {
    [
        ("ceg_particles", world.query::<&ceg::CegParticle>().iter(world).count()),
        ("ceg_flames", world.query::<&ceg::CegFlame>().iter(world).count()),
        ("ceg_spikes", world.query::<&ceg::CegSpike>().iter(world).count()),
        ("ceg_delayed", world.query::<&ceg::CegDelayedSpawn>().iter(world).count()),
        ("laser_bolts", world.query::<&shared::LaserBolt>().iter(world).count()),
    ]
}

use bevy::prelude::*;

use crate::rendering::interpolation::SimPose;
use batch::FxQuadBatches;
pub(crate) use ceg::CegRegistry;
use ceg::CegRenderAssets;
use shared::{
    BeamMaterialCache, BuildSparkleAssets, GroundFlashAssets, ImpactBurstAssets, WeaponFxMeshes,
};

use super::GameplaySet;

/// Registers the visual-effect resources and both fx systems.
///
/// The spawn and tick systems both land in `GameplaySet::Simulate` (the
/// fixed 30 Hz sim schedule) with the spawn system running first so the
/// tick system sees newly-spawned visuals on the same frame.
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
            .init_resource::<CegRenderAssets>()
            .init_resource::<FxQuadBatches>()
            .insert_resource(CegRegistry::load())
            // Projectiles and the synthesised bursts move in the fixed
            // sim; draw them interpolated (`rendering::interpolation`).
            // Beams, bolts, arcs, spikes, trails and CEG particles /
            // flames have no Transform at all: they are quads in the
            // world-space batch meshes (`batch`), rewritten every tick.
            .register_required_components::<shared::ProjectileVisual, SimPose>()
            .register_required_components::<shared::BuildSparkle, SimPose>()
            .register_required_components::<shared::ImpactBurst, SimPose>()
            .register_required_components::<shared::GroundFlash, SimPose>()
            .add_systems(
                FixedUpdate,
                (
                    // tick fires `DelayedHit` into `PendingExplosions`;
                    // the explosion spawner must follow it in the chain
                    // or impact CEGs land a frame late.
                    spawn::spawn_weapon_visuals,
                    tick::tick_weapon_fx,
                    tick::tick_fading_trails,
                    spawn::spawn_pending_explosions,
                    ceg::tick_ceg_particles,
                    ceg::tick_ceg_flames,
                    ceg::tick_ceg_spikes,
                    ceg::tick_ceg_delayed_spawns,
                    // Last: every ribbon has pushed its quads by now.
                    batch::flush_fx_quad_batches,
                )
                    .chain()
                    // Why the explicit anchors: this chain lives in
                    // `GameplaySet::Simulate` but outside its combat /
                    // command-fire sub-chains, so without edges the
                    // scheduler may drain `PendingAttacks` before the
                    // frame's shots are queued — a nondeterministic
                    // one-frame FX lag. Both anchors are the *last*
                    // systems of their sub-chain, so ordering against
                    // them places the fx chain after all of Simulate's
                    // producers.
                    .after(crate::units::combat::aim_weapons_system)
                    .after(crate::units::lifecycle::script_triggers::trigger_weapon_scripts)
                    .in_set(GameplaySet::Simulate),
            );
    }
}
