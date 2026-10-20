pub mod ai;
pub mod assets;
pub mod combat;
pub mod components;
pub mod content;
pub mod features;
pub mod lifecycle;
pub mod mechanics;
pub mod player;
pub mod spatial;
pub mod team_colors;
pub mod teams;
pub mod weapon_fx;

use bevy::prelude::*;

use crate::map_loading::{MatchEntity, MatchScopeAppExt};
use crate::rendering::interpolation::SimPose;
use crate::sim::{SIMULATION_HZ, SimRng};

use assets::animation;
use content::{unit_registry, weapons};
use lifecycle::{bookkeeping, construction, game_over, production, script_triggers, spawning};
use mechanics::{cloak, command_fire, deploy, network_buffer, shield, worm};

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
        // Spring's sim ticks at a fixed 30 Hz (its `GAME_SPEED` frame
        // unit is 1/30 s, and every FBI cooldown/burst/rate constant in
        // the registries is authored against that cadence). Running the
        // gameplay chain on `Time<Fixed>` makes those constants
        // frame-rate-independent; `Res<Time>` inside `FixedUpdate`
        // reads the fixed clock, while rendering/UI keep variable dt.
        // A long frame (map load, a browser tab switch) must not be
        // followed by a burst of catch-up ticks that makes the next
        // frame long too: at most three ticks per frame — the sim
        // briefly runs slow instead of stalling.
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .set_max_delta(std::time::Duration::from_millis(100));
        // The sim runs on the single-threaded executor, always: lockstep
        // peers must run the fixed tick's systems in one fixed order,
        // and the multi-threaded executor (which only parallelises
        // unordered systems) diverged from it in the headless
        // experiments (`net-experiments/x-sim.md` §1, item 11). It is
        // also cheaper at these unit counts (§4: 1.12 vs 1.69 ms median
        // at ~400 units), and wasm has no other.
        app.edit_schedule(FixedUpdate, |s| {
            s.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        app.insert_resource(Time::<Fixed>::from_hz(SIMULATION_HZ))
            // The match's clock and seed (`sim`): reset / set at every
            // match swap.
            .init_match_resource::<crate::sim::SimTick>()
            .init_resource::<crate::sim::MatchSeed>()
            // The local player's result panel: UI only, never gates
            // the sim (`game_over::announce_outcome`).
            .init_state::<game_over::GameState>()
            .init_resource::<assets::meshes::S3OModelCache>()
            .insert_resource(player::LocalTeam(0))
            .init_match_resource::<ai::AiTicker>()
            // Set from the seats at every match swap (`begin_match`).
            .init_resource::<ai::AiSeats>()
            .init_resource::<teams::Alliances>()
            .init_resource::<team_colors::TeamColors>()
            // Which owners are dead and the match result (sim), and the
            // result last shown to the local player (UI).
            .init_match_resource::<teams::TeamFates>()
            .init_match_resource::<SimRng<game_over::LeaveFuseRng>>()
            .init_match_resource::<game_over::AnnouncedOutcome>()
            .insert_resource(weapons::WeaponRegistry::load())
            .insert_resource(unit_registry::UnitRegistry::load())
            .init_resource::<shield::OnsMode>()
            .init_resource::<shield::ShieldShellAssets>()
            .init_resource::<command_fire::SigTermAssets>()
            // Per-match state, re-initialised at every match swap
            // (`map_loading::match_scope`): per-team tallies, queues of
            // pending hits / spawns, timers phased to the match start.
            .init_match_resource::<ai::TeamDamageLog>()
            .init_match_resource::<combat::DamageQueue>()
            .init_match_resource::<combat::DelayedDamage>()
            .init_match_resource::<crate::interaction::ground_move::PendingImpulses>()
            .init_match_resource::<combat::VirusSpawnQueue>()
            .init_match_resource::<command_fire::MineSpawnQueue>()
            .init_match_resource::<SimRng<command_fire::MineLaunchRng>>()
            .init_match_resource::<network_buffer::PacketBuffer>()
            .init_match_resource::<network_buffer::FlowSpeedTicker>()
            .init_match_resource::<bookkeeping::SmallBuildingCounts>()
            .init_match_resource::<bookkeeping::TeamUnitCounts>()
            .init_match_resource::<cloak::VisibilityRefreshTimer>()
            .init_match_resource::<cloak::CloakDetectionTimer>()
            .init_match_resource::<cloak::FogRefreshTimer>()
            .init_match_resource::<spatial::SpatialIndex>()
            .init_match_resource::<crate::game_setup::GameOverDismissed>()
            .add_message::<command_fire::CommandFireEvent>()
            .add_message::<deploy::DeployEvent>()
            .add_message::<network_buffer::DispatchEvent>()
            .add_message::<network_buffer::EnterEvent>()
            .init_resource::<cloak::FogEnabled>()
            .init_resource::<animation::DeathParticleAssets>()
            .init_match_resource::<production::BuggerOffRequests>()
            // The sim's random streams, re-seeded from the match seed
            // at every swap (`sim::SimRng`).
            .init_match_resource::<SimRng<combat::TargetingRng>>()
            .init_match_resource::<SimRng<combat::GroundFireRng>>()
            .init_match_resource::<SimRng<combat::BurstRng>>()
            .init_match_resource::<SimRng<production::FactoryExitRng>>()
            .add_observer(production::kill_orphaned_buildee)
            .add_plugins(weapon_fx::WeaponFxPlugin)
            // Everything the fixed sim moves is drawn interpolated
            // between the last two sim poses (`rendering::interpolation`).
            .register_required_components::<components::UnitType, SimPose>()
            .register_required_components::<animation::PieceIndex, SimPose>()
            .register_required_components::<animation::DeathParticle, SimPose>()
            .register_required_components::<command_fire::SigTermSignal, SimPose>()
            .register_required_components::<command_fire::SigTermBomb, SimPose>()
            .register_required_components::<command_fire::SigTermWreck, SimPose>()
            // A damage zone is plain data (no Transform): tag it match
            // content so the match teardown clears it
            // (`map_loading::match_scope`).
            .register_required_components::<command_fire::AreaDenialZone, MatchEntity>()
            .add_systems(
                FixedUpdate,
                // Also runs in `AppState::Menu`: the attract-mode demo
                // behind the menu is an all-AI skirmish.
                ai::ai_brain
                    .after(crate::net::SimCommandSet)
                    .before(GameplaySet::Produce)
                    // Before the movers: it reads their paths, and an
                    // unordered pair runs in whatever order the
                    // executor picks (single- vs multi-threaded runs
                    // diverged; `headless` determinism experiments).
                    .before(crate::interaction::movement::advance_command_queue),
            )
            .configure_sets(
                FixedUpdate,
                (
                    GameplaySet::Produce,
                    GameplaySet::Simulate,
                    GameplaySet::Resolve,
                    GameplaySet::Animate,
                    GameplaySet::Cleanup,
                )
                    // Never gated on the local player's outcome: a
                    // defeated peer keeps simulating as a spectator
                    // (lockstep), and single player keeps running
                    // behind the result panel, as Spring does after
                    // `GameOver`.
                    .chain(),
            )
            .add_systems(Update, game_over::announce_outcome)
            .add_systems(Startup, validate_registries)
            // Debug builds: the FPU is still in IEEE default mode. Then
            // the tick's frame number.
            .add_systems(
                FixedFirst,
                (crate::sim::assert_float_env, crate::sim::advance_sim_tick),
            )
            .add_systems(
                FixedUpdate,
                (
                    (
                        bookkeeping::track_added_buildings,
                        bookkeeping::track_finished_buildings,
                        bookkeeping::track_added_units,
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
                        // Nested to stay under the tuple-arity cap.
                        (
                            construction::start_construction,
                            production::apply_bugger_off,
                            construction::tick_construction,
                            construction::clear_features,
                            construction::decay_nanoframes,
                        )
                            .chain(),
                        spawning::emerge_system,
                    )
                        .chain()
                        .in_set(GameplaySet::Produce),
                    // Kept as a separate tuple only to stay under
                    // Bevy's 21-system tuple-arity cap; `after` puts
                    // them behind the regen pass so a shell's tint
                    // reflects this frame's regeneration.
                    (shield::spawn_shield_shells, shield::tick_shield_shells)
                        .chain()
                        .after(shield::regen_shields)
                        // Behind every spawner of the set too: they all
                        // add meshes / materials, and asset ids are
                        // handed out in the order they run.
                        .after(spawning::emerge_system)
                        .in_set(GameplaySet::Produce),
                    // Why: MuzzlePiece + AimScript must update before
                    // `combat_system` reads them. The Simulate set is
                    // split in two only because Bevy's tuple-arity cap
                    // is 21.
                    (
                        (
                            // First so every unit whose spawn flushed
                            // before Simulate carries its kind markers
                            // (kamikaze, detector, …) this same tick.
                            bookkeeping::tag_unit_kinds,
                            worm::init_autohold,
                            cloak::update_cloak_detection,
                            spatial::rebuild_spatial_index,
                            combat::sync_deploy_state,
                            combat::tick_kamikaze,
                            combat::drive_aim_script,
                            combat::combat_system,
                            combat::attack_ground_system,
                            combat::attack_target_system,
                            worm::tick_worm_surfacing,
                            combat::tick_burst_fire,
                            combat::aim_weapons_system,
                        )
                            .chain(),
                        (
                            combat::tick_infections,
                            command_fire::advance_pending_casts,
                            command_fire::process_command_fire,
                            command_fire::tick_nx_casts,
                            command_fire::tick_mine_launches,
                            command_fire::tick_command_fire_cooldown,
                            command_fire::tick_area_denial,
                            command_fire::tick_sigterm_signals,
                            command_fire::tick_sigterm_bombs,
                            command_fire::tick_sigterm_wrecks,
                            command_fire::tick_protection,
                            script_triggers::trigger_movement_scripts,
                            script_triggers::trigger_production_scripts,
                            script_triggers::trigger_build_scripts,
                            script_triggers::trigger_weapon_scripts,
                        )
                            .chain(),
                    )
                        .chain()
                        .in_set(GameplaySet::Simulate),
                    (
                        combat::apply_damage.run_if(
                            |q: Res<combat::DamageQueue>, d: Res<combat::DelayedDamage>| {
                                !q.is_empty() || !d.is_empty()
                            },
                        ),
                        combat::tick_stun,
                        combat::tick_self_destruct,
                        game_over::detonate_fuses,
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
                        animation::animation_system,
                        animation::sync_muzzle_pieces,
                        combat::sync_byte_fold_state,
                        animation::decay_death_particles,
                        cloak::update_cloak_visibility,
                        cloak::update_fog_visibility,
                        cloak::install_cloak_fade_materials,
                        cloak::restore_cloak_fade_materials,
                        team_colors::recolor_taken_units,
                    )
                        .chain()
                        .in_set(GameplaySet::Animate),
                    // Last in the tick: a finished Bad Block turns into
                    // its feature; a feature destroyed this tick leaves its
                    // `featureDead` before `cleanup_dying`.
                    (
                        features::turn_into_features,
                        features::destroy_features,
                        combat::cleanup_dying,
                    )
                        .chain()
                        .in_set(GameplaySet::Cleanup),
                ),
            )
            .add_systems(
                FixedUpdate,
                (
                    // The COB `ARMORED` flag, raised on new units and
                    // lowered after their build (`damage::armored_script`).
                    combat::update_armored.in_set(GameplaySet::Produce),
                    // `ExplodeAs` goes off the frame the unit dies, while
                    // it is still a unit to credit the blast to.
                    combat::apply_death_blasts
                        .after(combat::death_system)
                        .in_set(GameplaySet::Resolve)
                        .run_if(|q: Res<combat::DamageQueue>| !q.is_empty()),
                ),
            );
    }
}

fn validate_registries(
    mut weapon_registry: ResMut<weapons::WeaponRegistry>,
    unit_registry: Res<unit_registry::UnitRegistry>,
) {
    weapon_registry.validate_unit_weapon_bindings(&unit_registry);
    weapon_registry.bind_units(&unit_registry);
}
