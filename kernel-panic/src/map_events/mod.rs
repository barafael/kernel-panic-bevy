//! Per-map scripted events. Currently hosts the **Stack Overflow**
//! volcano eruption: every five minutes the central caldera spews bad
//! blocks, cloaked logic bombs, viruses, ICMP packets, and faction
//! nibbles into each enemy base.
//!
//! Activation is gated on a [`EruptionConfig`] resource that
//! [`map_loading`](crate::map_loading) inserts only for matching maps.
//! When the resource is absent, every system here short-circuits — no
//! cost on other maps.

use bevy::prelude::*;

pub mod circular_flow;
pub use circular_flow::CircularFlow;

use crate::rng::{next_f32, next_signed, xorshift32};
use crate::terrain::heightmap::Heightmap;
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
/// Held as a [`Local`] inside [`tick_eruption`] — there's only one
/// active volcano at a time, so a per-system Local is enough.
#[derive(Debug)]
struct EruptionState {
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

impl Default for EruptionState {
    fn default() -> Self {
        Self {
            elapsed: 0.0,
            next_eruption_at: 0.0,
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
        app.init_resource::<EruptionSpawnQueue>().add_systems(
            Update,
            (
                tick_eruption.run_if(resource_exists::<EruptionConfig>),
                drain_eruption_queue.run_if(|q: Res<EruptionSpawnQueue>| !q.is_empty()),
            )
                .chain(),
        );
    }
}

/// Advance the eruption clock and enqueue the appropriate spawns for
/// any sub-step whose offset has just elapsed.
fn tick_eruption(
    time: Res<Time>,
    config: Res<EruptionConfig>,
    heightmap: Option<Res<Heightmap>>,
    mut state: Local<EruptionState>,
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

/// Synthetic team id far from any player team. Eruption units share a
/// faction with one of the players (and `is_friendly` returns true on
/// faction match), so a System player will see eruption-spawned bad
/// blocks as allied — but bad blocks are inert walls, mines auto-target
/// non-allies, and the asymmetry evens out across the three factions.
const ERUPTION_TEAM: u8 = 99;

/// Pick a random point on the X-Z plane in the annulus
/// `[inner_r, outer_r]` around `center`. Inner=0 collapses to a disc.
fn ring_point(center: Vec2, inner_r: f32, outer_r: f32, rng: &mut u32) -> Vec2 {
    let theta = next_f32(rng) * std::f32::consts::TAU;
    let r = inner_r + next_f32(rng) * (outer_r - inner_r);
    // Force the rng to advance even if the area math is degenerate, so
    // back-to-back calls with equal inner/outer don't lock to one angle.
    let _ = xorshift32(rng);
    Vec2::new(center.x + r * theta.cos(), center.y + r * theta.sin())
}

fn drain_eruption_queue(mut queue: ResMut<EruptionSpawnQueue>, mut ctx: SpawnContext) {
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
