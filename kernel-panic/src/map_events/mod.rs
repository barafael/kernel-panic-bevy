//! Per-map scripted events. Currently hosts the **Stack Overflow**
//! volcano eruption: every five minutes the central caldera spews bad
//! blocks, cloaked logic bombs, viruses, ICMP packets, and faction
//! nibbles into each enemy base.
//!
//! Activation is gated on a [`EruptionConfig`] resource that
//! [`map_loading`](crate::map_loading) inserts only for matching maps.
//! When the resource is absent, every system here short-circuits — no
//! cost on other maps. The eruption clock runs in the fixed sim
//! (`GameplaySet::Produce`), so it is frame-rate independent and stops
//! with the rest of the gameplay once a match is decided.

use bevy::math::ops;
use bevy::prelude::*;

pub mod circular_flow;
pub mod hex_farm;
pub use circular_flow::CircularFlow;

use crate::map_loading::MatchScopeAppExt;
use crate::rng::{next_f32, next_signed, xorshift32};
use crate::terrain::heightmap::Heightmap;
use crate::units::GameplaySet;
use crate::units::combat::{VirusSpawn, VirusSpawnQueue};
use crate::units::components::Faction;
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::spawning::{SpawnContext, spawn_unit};
use crate::units::mechanics::command_fire::{MineSpawn, MineSpawnQueue};

/// Profile describing a volcano-eruption map. The map_loading code
/// inserts this only when the active map's name matches a known
/// eruption map (currently just `Stack_Overflow`).
#[derive(Resource, Clone, Debug)]
pub struct EruptionConfig {
    /// Volcano center on the X-Z plane (y is sampled from the heightmap).
    pub center_xz: Vec2,
    /// Inner / outer radius for bad-block ring (elmos).
    pub bad_block_ring: (f32, f32),
    /// Inner / outer radius for cloaked-mine ring.
    pub mine_ring: (f32, f32),
    /// Single radius the virus swarm wanders out from.
    pub virus_radius: f32,
    /// ICMP packets path radially outward from this radius.
    pub packet_radius: f32,
    /// Player start positions, used as nibble-drop targets.
    pub player_starts: Vec<Vec3>,
    /// Seconds between eruptions.
    pub cycle_seconds: f32,
}

impl EruptionConfig {
    pub fn stack_overflow(player_starts: Vec<Vec3>) -> Self {
        Self {
            center_xz: Vec2::new(2048.0, 2048.0),
            bad_block_ring: (340.0, 700.0),
            mine_ring: (700.0, 1100.0),
            virus_radius: 900.0,
            packet_radius: 500.0,
            player_starts,
            cycle_seconds: 300.0,
        }
    }
}

/// Synced clock for the eruption cycle. Tracks how long since map start
/// and which sub-step of the current eruption fired most recently.
///
/// A resource rather than a system `Local` so the map cycle can reset
/// it: `configure_map_events` removes it together with
/// [`EruptionConfig`] on every map load, so a restart can't inherit the
/// previous match's clock (a `Local` would keep `elapsed` across world
/// teardowns and ignite a leftover eruption instantly).
#[derive(Debug, Resource)]
pub(crate) struct EruptionState {
    /// Seconds since map load.
    elapsed: f32,
    /// `elapsed` value at which the next eruption will begin.
    next_eruption_at: f32,
    /// `Some(start_time)` while an eruption is in progress.
    current_eruption_started: Option<f32>,
    /// Index of the next sub-step to fire within the current eruption.
    next_step: usize,
    /// How many eruptions have fired so far. Drives intensity scaling.
    eruption_count: u32,
    /// Deterministic xorshift state, advanced per spawn so placements
    /// look noisy without pulling in a full RNG.
    rng: u32,
}

impl EruptionState {
    /// A fresh match: the first eruption comes one full cycle in
    /// ("every five minutes"), not on the first tick — the bases need
    /// time to stand up before the caldera spews into them.
    pub(crate) fn new(config: &EruptionConfig) -> Self {
        Self {
            elapsed: 0.0,
            next_eruption_at: config.cycle_seconds,
            current_eruption_started: None,
            next_step: 0,
            eruption_count: 0,
            rng: 0xCAFEBABE,
        }
    }
}

/// Pending non-mine, non-virus eruption spawns. Drained by
/// [`drain_eruption_queue`] one frame later, mirroring the
/// `VirusSpawnQueue` / `MineSpawnQueue` pattern that already exists.
#[derive(Resource, Default)]
pub struct EruptionSpawnQueue(Vec<EruptionSpawn>);

#[derive(Debug, Clone)]
pub struct EruptionSpawn {
    pub kind: UnitKind,
    pub faction: Faction,
    pub team: u8,
    pub position: Vec3,
}

impl EruptionSpawnQueue {
    pub fn push(&mut self, spawn: EruptionSpawn) {
        self.0.push(spawn);
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Steps that make up one eruption, fired at the given offset (seconds)
/// from eruption start. Spread across ~13 s so the player can read the
/// sequence rather than seeing one giant frame-spike.
const ERUPTION_SCHEDULE: &[(f32, EruptionStep)] = &[
    (0.0, EruptionStep::BadBlocks),
    (2.0, EruptionStep::Mines),
    (4.0, EruptionStep::Viruses),
    (6.5, EruptionStep::Packets),
    (9.0, EruptionStep::Nibbles),
    (13.0, EruptionStep::Done),
];

#[derive(Debug, Clone, Copy)]
enum EruptionStep {
    BadBlocks,
    Mines,
    Viruses,
    Packets,
    Nibbles,
    Done,
}

pub struct MapEventsPlugin;

impl Plugin for MapEventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(hex_farm::HexFarmPlugin);
        app.init_match_resource::<EruptionSpawnQueue>().add_systems(
            FixedUpdate,
            (
                tick_eruption.run_if(
                    resource_exists::<EruptionConfig>.and_then(resource_exists::<EruptionState>),
                ),
                drain_eruption_queue.run_if(|q: Res<EruptionSpawnQueue>| !q.is_empty()),
            )
                .chain()
                // After the factories: every spawned unit draws the
                // next `NetId`, so the spawners' order must be fixed.
                .after(crate::units::lifecycle::spawning::emerge_system)
                .in_set(GameplaySet::Produce),
        );
    }
}

/// Advance the eruption clock and enqueue the appropriate spawns for
/// any sub-step whose offset has just elapsed.
fn tick_eruption(
    time: Res<Time>,
    config: Res<EruptionConfig>,
    heightmap: Option<Res<Heightmap>>,
    mut state: ResMut<EruptionState>,
    mut bad_block_queue: ResMut<EruptionSpawnQueue>,
    mut virus_queue: ResMut<VirusSpawnQueue>,
    mut mine_queue: ResMut<MineSpawnQueue>,
) {
    state.elapsed += time.delta_secs();

    if state.current_eruption_started.is_none() && state.elapsed >= state.next_eruption_at {
        info!(
            "stack-overflow: eruption #{} igniting",
            state.eruption_count + 1
        );
        state.current_eruption_started = Some(state.elapsed);
        state.next_step = 0;
    }

    let Some(started) = state.current_eruption_started else {
        return;
    };
    let Some(heightmap) = heightmap.as_deref() else {
        return;
    };

    let phase = state.elapsed - started;
    while state.next_step < ERUPTION_SCHEDULE.len() {
        let (offset, step) = ERUPTION_SCHEDULE[state.next_step];
        if phase < offset {
            break;
        }
        let intensity = state.eruption_count;
        match step {
            EruptionStep::BadBlocks => {
                fire_bad_blocks(
                    &config,
                    heightmap,
                    intensity,
                    &mut state,
                    &mut bad_block_queue,
                );
            }
            EruptionStep::Mines => {
                fire_mines(&config, heightmap, intensity, &mut state, &mut mine_queue);
            }
            EruptionStep::Viruses => {
                fire_viruses(&config, heightmap, intensity, &mut state, &mut virus_queue);
            }
            EruptionStep::Packets => {
                fire_packets(
                    &config,
                    heightmap,
                    intensity,
                    &mut state,
                    &mut bad_block_queue,
                );
            }
            EruptionStep::Nibbles => {
                fire_nibbles(&config, heightmap, &mut state, &mut bad_block_queue);
            }
            EruptionStep::Done => {
                state.current_eruption_started = None;
                state.eruption_count += 1;
                state.next_eruption_at = state.elapsed + config.cycle_seconds;
                info!(
                    "stack-overflow: eruption complete; next at +{:.0}s",
                    config.cycle_seconds
                );
            }
        }
        state.next_step += 1;
    }
}

/// Cloaked walls: scale 8 → 16 → 24 → ... (2 extra per past eruption).
fn fire_bad_blocks(
    config: &EruptionConfig,
    heightmap: &Heightmap,
    intensity: u32,
    state: &mut EruptionState,
    queue: &mut EruptionSpawnQueue,
) {
    let count = 8 + (intensity * 2).min(24);
    for _ in 0..count {
        let p = ring_point(
            config.center_xz,
            config.bad_block_ring.0,
            config.bad_block_ring.1,
            &mut state.rng,
        );
        let pos = heightmap.place(p.x, p.y);
        queue.push(EruptionSpawn {
            kind: UnitKind::BadBlock,
            // BadBlock has a default faction of System; we keep that so
            // its (non-existent) friendliness rules stay coherent. It
            // physically blocks pathing for everyone regardless of team.
            faction: Faction::System,
            team: ERUPTION_TEAM,
            position: pos,
        });
    }
}

fn fire_mines(
    config: &EruptionConfig,
    heightmap: &Heightmap,
    intensity: u32,
    state: &mut EruptionState,
    queue: &mut MineSpawnQueue,
) {
    let count = 4 + intensity.min(12);
    for _ in 0..count {
        let p = ring_point(
            config.center_xz,
            config.mine_ring.0,
            config.mine_ring.1,
            &mut state.rng,
        );
        let pos = heightmap.place(p.x, p.y);
        queue.push(MineSpawn {
            position: pos,
            faction: Faction::Hacker,
            team: ERUPTION_TEAM,
        });
    }
}

fn fire_viruses(
    config: &EruptionConfig,
    heightmap: &Heightmap,
    _intensity: u32,
    state: &mut EruptionState,
    queue: &mut VirusSpawnQueue,
) {
    let count = 6;
    for _ in 0..count {
        let p = ring_point(config.center_xz, 0.0, config.virus_radius, &mut state.rng);
        let pos = heightmap.place(p.x, p.y);
        queue.push(VirusSpawn {
            position: pos,
            faction: Faction::Hacker,
            team: ERUPTION_TEAM,
        });
    }
}

/// ICMP packets path radially outward; we spawn them in a tight ring
/// near the caldera and rely on regular target selection to send them
/// at the closest visible enemy.
fn fire_packets(
    config: &EruptionConfig,
    heightmap: &Heightmap,
    intensity: u32,
    state: &mut EruptionState,
    queue: &mut EruptionSpawnQueue,
) {
    let count = 12 + (intensity * 4).min(20);
    for _ in 0..count {
        let p = ring_point(
            config.center_xz,
            config.packet_radius * 0.6,
            config.packet_radius,
            &mut state.rng,
        );
        let pos = heightmap.place(p.x, p.y);
        queue.push(EruptionSpawn {
            kind: UnitKind::Packet,
            faction: Faction::Network,
            team: ERUPTION_TEAM,
            position: pos,
        });
    }
}

/// Drop one nibble of each *other* faction's basic combat unit into
/// every player base. A System player gets a Bug + a Packet; the
/// volcano never drops the player's own faction on them.
fn fire_nibbles(
    config: &EruptionConfig,
    heightmap: &Heightmap,
    state: &mut EruptionState,
    queue: &mut EruptionSpawnQueue,
) {
    const NIBBLE_OFFSET: f32 = 80.0;
    for (i, start) in config.player_starts.iter().enumerate() {
        let target_faction = Faction::from_team_id(i as u8);
        for f in [Faction::System, Faction::Hacker, Faction::Network] {
            if f == target_faction {
                continue;
            }
            let dx = next_signed(&mut state.rng) * NIBBLE_OFFSET;
            let dz = next_signed(&mut state.rng) * NIBBLE_OFFSET;
            let pos = heightmap.place(start.x + dx, start.z + dz);
            queue.push(EruptionSpawn {
                kind: f.basic_combat_unit(),
                faction: f,
                team: ERUPTION_TEAM,
                position: pos,
            });
        }
    }
}

/// Synthetic team id far from any player team: eruption spawns are a
/// hostile neutral party that every side can (and will) shoot.
const ERUPTION_TEAM: u8 = 99;

/// Pick a random point on the X-Z plane in the annulus
/// `[inner_r, outer_r]` around `center`. Inner=0 collapses to a disc.
fn ring_point(center: Vec2, inner_r: f32, outer_r: f32, rng: &mut u32) -> Vec2 {
    let theta = next_f32(rng) * std::f32::consts::TAU;
    let r = inner_r + next_f32(rng) * (outer_r - inner_r);
    // Force the rng to advance even if the area math is degenerate, so
    // back-to-back calls with equal inner/outer don't lock to one angle.
    let _ = xorshift32(rng);
    Vec2::new(
        center.x + r * ops::cos(theta),
        center.y + r * ops::sin(theta),
    )
}

pub(crate) fn drain_eruption_queue(mut queue: ResMut<EruptionSpawnQueue>, mut ctx: SpawnContext) {
    for spawn in queue.0.drain(..) {
        spawn_unit(
            spawn.kind,
            spawn.faction,
            spawn.team,
            spawn.position,
            &mut ctx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first eruption lands one cycle into the match, not at t=0.
    #[test]
    fn first_eruption_waits_a_full_cycle() {
        let config = EruptionConfig::stack_overflow(Vec::new());
        let state = EruptionState::new(&config);
        assert_eq!(state.next_eruption_at, config.cycle_seconds);
        assert!(state.current_eruption_started.is_none());
    }
}
