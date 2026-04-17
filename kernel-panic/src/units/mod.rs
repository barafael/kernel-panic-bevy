pub mod animation;
pub mod combat;
pub mod components;
pub mod definitions;
pub mod game_over;
pub mod meshes;
pub mod production;
pub mod script_triggers;
pub mod spawning;
mod tdf_loader;
pub mod unit_registry;
pub mod weapon_fx;
pub mod weapons;

use bevy::prelude::*;

/// Logical buckets for gameplay systems. Systems inside a set run after the
/// previous set completes; inside a set they are unordered unless they declare
/// their own `.after()` edges.
#[derive(SystemSet, Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum GameplaySet {
    /// Factories tick, queued production emerges into the world.
    Produce,
    /// Combat target selection + damage queueing; triggers fire based on state.
    Simulate,
    /// Damage/deaths/infections applied; game-over check; virus spawns drain.
    Resolve,
    /// COB animation + death-particle decay.
    Animate,
    /// Despawns and other end-of-frame cleanup.
    Cleanup,
}

/// Registers all unit resources and gameplay systems.
pub struct UnitsPlugin;

impl Plugin for UnitsPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<game_over::GameState>()
            .init_resource::<meshes::S3OModelCache>()
            .init_resource::<animation::CobFileCache>()
            .insert_resource(weapons::WeaponRegistry::load())
            .insert_resource(unit_registry::UnitRegistry::load())
            .init_resource::<combat::DamageQueue>()
            .init_resource::<combat::VirusSpawnQueue>()
            .add_plugins(weapon_fx::WeaponFxPlugin)
            .init_resource::<game_over::PlayerTeam>()
            .configure_sets(
                Update,
                (
                    GameplaySet::Produce,
                    GameplaySet::Simulate,
                    GameplaySet::Resolve,
                    GameplaySet::Animate,
                    GameplaySet::Cleanup,
                )
                    .chain()
                    .run_if(in_state(game_over::GameState::Playing)),
            )
            .add_systems(Startup, validate_registries)
            .add_systems(
                Update,
                (
                    (production::production_system, spawning::emerge_system)
                        .chain()
                        .in_set(GameplaySet::Produce),
                    (
                        combat::tick_deploy_state,
                        combat::combat_system,
                        combat::tick_infections,
                        script_triggers::trigger_movement_scripts,
                        script_triggers::trigger_production_scripts,
                        script_triggers::trigger_weapon_scripts,
                    )
                        .chain()
                        .in_set(GameplaySet::Simulate),
                    (
                        combat::apply_damage,
                        combat::death_system,
                        spawning::spawn_queued_viruses,
                        game_over::check_game_over,
                    )
                        .chain()
                        .in_set(GameplaySet::Resolve),
                    (
                        animation::animation_system,
                        animation::decay_death_particles,
                    )
                        .in_set(GameplaySet::Animate),
                    combat::cleanup_dying.in_set(GameplaySet::Cleanup),
                ),
            );
    }
}

fn validate_registries(
    weapon_registry: Res<weapons::WeaponRegistry>,
    unit_registry: Res<unit_registry::UnitRegistry>,
) {
    weapon_registry.validate_unit_weapon_bindings(&unit_registry);
}
