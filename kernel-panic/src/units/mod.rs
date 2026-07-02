pub mod assets;
pub mod combat;
pub mod components;
pub mod content;
pub mod lifecycle;
pub mod mechanics;
pub mod spatial;
pub mod weapon_fx;

use bevy::prelude::*;

use assets::animation;
use content::{unit_registry, weapons};
use lifecycle::{bookkeeping, construction, game_over, production, script_triggers, spawning};
use mechanics::{cloak, command_fire, deploy, network_buffer, shield};

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
            .init_resource::<assets::meshes::S3OModelCache>()
            .init_resource::<animation::CobFileCache>()
            .insert_resource(weapons::WeaponRegistry::load())
            .insert_resource(unit_registry::UnitRegistry::load())
            .init_resource::<combat::DamageQueue>()
            .init_resource::<combat::VirusSpawnQueue>()
            .init_resource::<shield::OnsMode>()
            .init_resource::<command_fire::MineSpawnQueue>()
            .init_resource::<command_fire::SigTermAssets>()
            .add_message::<command_fire::CommandFireEvent>()
            .add_message::<deploy::DeployEvent>()
            .add_message::<network_buffer::DispatchEvent>()
            .add_message::<network_buffer::EnterEvent>()
            .init_resource::<network_buffer::PacketBuffer>()
            .init_resource::<network_buffer::FlowSpeedTicker>()
            .init_resource::<bookkeeping::SmallBuildingCounts>()
            .init_resource::<cloak::VisibilityRefreshTimer>()
            .init_resource::<cloak::PlayerTeam>()
            .init_resource::<cloak::FogEnabled>()
            .init_resource::<spatial::SpatialIndex>()
            .init_resource::<animation::DeathParticleAssets>()
            .add_plugins(weapon_fx::WeaponFxPlugin)
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
                    (
                        bookkeeping::track_added_buildings,
                        bookkeeping::track_dying_buildings,
                        shield::attach_shields,
                        shield::regen_shields,
                        deploy::process_deploy,
                        network_buffer::tick_port_buffers,
                        network_buffer::tick_spawn_stun,
                        network_buffer::tick_flow_speed,
                        network_buffer::tick_auto_dispatch,
                        network_buffer::process_dispatch,
                        network_buffer::process_enter,
                        production::production_system,
                        production::install_fade_materials,
                        production::animate_connection_hatch,
                        construction::start_construction,
                        construction::tick_construction,
                        spawning::emerge_system,
                    )
                        .chain()
                        .in_set(GameplaySet::Produce),
                    // Why: MuzzlePiece + AimScript must update before
                    // `combat_system` reads them. The Simulate set is
                    // split in two only because Bevy's tuple-arity cap
                    // is 21.
                    (
                        (
                            spatial::rebuild_spatial_index,
                            combat::tick_deploy_state,
                            combat::tick_opening_delay,
                            combat::tick_byte_open,
                            combat::tick_kamikaze,
                            assets::animation::refresh_muzzle_pieces,
                            combat::drive_aim_script,
                            combat::combat_system,
                            combat::attack_ground_system,
                            combat::tick_burst_fire,
                            combat::aim_weapons_system,
                        )
                            .chain(),
                        (
                            combat::tick_infections,
                            command_fire::process_command_fire,
                            command_fire::tick_command_fire_cooldown,
                            command_fire::tick_area_denial,
                            command_fire::tick_sigterm_signals,
                            command_fire::tick_sigterm_bombs,
                            command_fire::tick_protection,
                            script_triggers::trigger_movement_scripts,
                            script_triggers::trigger_production_scripts,
                            script_triggers::trigger_weapon_scripts,
                        )
                            .chain(),
                    )
                        .chain()
                        .in_set(GameplaySet::Simulate),
                    (
                        combat::apply_damage.run_if(|q: Res<combat::DamageQueue>| !q.is_empty()),
                        combat::tick_stun,
                        combat::tick_self_destruct,
                        combat::auto_heal,
                        combat::death_system,
                        spawning::spawn_queued_viruses
                            .run_if(|q: Res<combat::VirusSpawnQueue>| !q.is_empty()),
                        spawning::spawn_queued_mines
                            .run_if(|q: Res<command_fire::MineSpawnQueue>| !q.is_empty()),
                        game_over::check_game_over,
                    )
                        .chain()
                        .in_set(GameplaySet::Resolve),
                    (
                        animation::publish_unit_values,
                        animation::animation_system,
                        // Why: must run after animation_system so the
                        // VM's `ended_threads` snapshot contains this
                        // frame's AimWeapon1 returns.
                        combat::update_aim_script,
                        animation::decay_death_particles,
                        cloak::update_cloak_visibility,
                        cloak::update_fog_visibility,
                        cloak::install_cloak_fade_materials,
                        cloak::restore_cloak_fade_materials,
                    )
                        .chain()
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
