//! Network faction's packet-buffer mechanic.
//!
//! Ports don't spawn Packets directly. Instead each finished Port adds
//! one to a shared per-team counter every `PORT_TICK_INTERVAL`; the player
//! dispatches stored packets from any teleporter (Port or Connection)
//! ([`DispatchOrder`]). Packets near a friendly teleporter can return to
//! the buffer, giving Network players mobile spawn points.
//!
//! Mirrors upstream `LuaRules/Gadgets/network_buffer.lua` (and the
//! dispatch / enter companion gadgets).

use std::collections::BTreeMap;

use bevy::prelude::*;

use crate::interaction::movement::{MoveTarget, NavGridSet};
use crate::net::NetId;
use crate::sim::{
    DetFloat, GAME_SPEED, Heading, SQUARE_SIZE, SimTick, cob_sleep_frames, countdown_secs,
    frames_to_secs, slow_update_due,
};
use crate::units::combat::{Armored, Dying};
use crate::units::components::{Faction, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::bookkeeping::{CreateBudget, TeamUnitCounts};
use crate::units::lifecycle::spawning::{Emerging, SpawnContext, spawn_unit_facing};

/// Seconds between per-Port buffer increments. network_buffer.lua
/// re-arms a Port `delayFrames=164` after each packet but only checks
/// on every 8th frame (`(t + 1) % 8`), so the period rounds up to the
/// next multiple of 8: 168 frames (5.6 s).
const PORT_TICK_INTERVAL: f32 = frames_to_secs(168.0);

/// Max packets that can spawn in a single Dispatch — matches upstream's
/// 12-slot offset list.
pub const DISPATCH_MAX: usize = 12;

/// The spots of a dispatch pass, `network_dispatch.lua`'s `offsetList`:
/// `(x, z)` in [`DISPATCH_SPOT_STEP`]s from the teleporter — a square
/// ring on a 7×7 grid, clockwise from the north-west corner — and the
/// facing (`0` south … `3` west) a Packet takes there, away from the
/// teleporter.
const DISPATCH_SPOTS: [(i8, i8, u8); DISPATCH_MAX] = [
    (-3, -3, 2),
    (-1, -3, 2),
    (1, -3, 2),
    (3, -3, 1),
    (3, -1, 1),
    (3, 1, 1),
    (3, 3, 0),
    (1, 3, 0),
    (-1, 3, 0),
    (-3, 3, 3),
    (-3, 1, 3),
    (-3, -1, 3),
];

/// Elmos per step of [`DISPATCH_SPOTS`] (`offsetList[…]*16`).
const DISPATCH_SPOT_STEP: f32 = 16.0;

/// network_buffer.lua clamps every team's buffer to the room under its
/// unit limit on the frames it pays the Ports (`(t + 1) % 8`).
const BUFFER_CAP_PERIOD: u64 = 8;

/// Seconds a freshly-spawned Packet must wait before it can re-enter
/// the buffer via the Enter command. Upstream `stunTime=180` frames.
pub const SPAWN_STUN_SECONDS: f32 = frames_to_secs(180.0);

/// How long a dispatched Packet keeps its `ARMORED` spawn protection.
/// network_dispatch.lua calls packet.bos `Spawned()` right after
/// `Spring.CreateUnit`, and that clears the flag after `sleep 500`. COB
/// time moves 33 ms a frame and a thread wakes once it is past its wake
/// time, so the sleep is 16 frames. `Create()`'s own 5 s grace then
/// clears a flag that is already down.
const SPAWNED_ARMORED_SECONDS: f32 = countdown_secs(cob_sleep_frames(500));

/// Max distance (elmos) at which a Packet can enter a teleporter.
/// Upstream `enterDist=150`.
pub const ENTER_DISTANCE: f32 = 150.0;

/// A Packet's running Enter order on a teleporter (`network_enter.lua`'s
/// `CMD_ENTER` with a unit: walk within `enterDist * .75`, then enter).
/// Ends with the entry, or when the teleporter is gone.
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct EnterOrder {
    pub target: Entity,
    /// The order has had its first tick. The engine runs a command the
    /// moment it becomes the unit's order (`CCommandAI::GiveAllowedCommand`
    /// on an empty queue, `FinishCommand` on a promotion: both call
    /// `SlowUpdate`) and after that only on the unit's slow updates. A
    /// unit that is paralysed or unfinished at that moment misses the
    /// run and waits for a slow update like any other.
    ///
    /// Kept in the order, never read from Bevy's change detection: a
    /// sorted query answers `is_added` from the world's ticks, which
    /// move with the app's frames, not with the sim's — two peers (or a
    /// frame that runs two sim ticks) would disagree.
    started: bool,
}

impl EnterOrder {
    pub fn new(target: Entity) -> Self {
        Self {
            target,
            started: false,
        }
    }
}

/// `network_enter.lua`'s `CommandFallback` for a Packet with an
/// [`EnterOrder`], run as the engine runs it: the moment the order
/// becomes the Packet's order and then on each of its slow updates. Within [`ENTER_DISTANCE`] of the teleporter on the
/// ground plane and past its spawn stun, the Packet asks to enter that
/// teleporter ([`process_enter`] takes it this tick). Otherwise the
/// order stays (`return true, false`) and the Packet is sent to within
/// three quarters of that distance (`SetUnitMoveGoal(…, enterDist *
/// .75)`: a move goal with a radius, which the walk ends at on whatever
/// frame it gets there), where it waits out what is left of the stun. A
/// target that is gone ends the order; a paralysed Packet's orders
/// don't run.
#[allow(clippy::type_complexity)]
pub fn tick_enter_orders(
    tick: Res<SimTick>,
    mut packets: Query<
        (
            Entity,
            &Transform,
            &mut EnterOrder,
            Has<PacketSpawnStun>,
            Has<crate::units::combat::Stunned>,
            Has<crate::interaction::movement::MoveTarget>,
            Option<&crate::interaction::movement::MovePath>,
            &crate::net::NetId,
        ),
        Without<Dying>,
    >,
    teleporters: Query<&Transform, Without<Dying>>,
    mut enter: MessageWriter<EnterEvent>,
    mut commands: Commands,
) {
    use crate::interaction::movement::{MovePath, MoveTarget, chase_path_stale};
    let frame = tick.frame();
    let reach = ENTER_DISTANCE * 0.75;
    for (packet, tf, mut order, spawn_stunned, stunned, has_move, path, id) in
        packets.iter_mut().sort::<&crate::net::NetId>()
    {
        let first = !order.started;
        if first {
            order.started = true;
        }
        if stunned {
            continue;
        }
        let due = first || slow_update_due(frame, id.0);
        let Ok(target_tf) = teleporters.get(order.target) else {
            if due {
                commands
                    .entity(packet)
                    .remove::<(EnterOrder, MoveTarget, MovePath)>();
            }
            continue;
        };
        let at = target_tf.translation;
        let dist_sq = crate::units::spatial::flat_dist_sq(tf.translation, at);
        if due {
            if dist_sq < ENTER_DISTANCE * ENTER_DISTANCE && !spawn_stunned {
                enter.write(EnterEvent {
                    packet,
                    into: Some(order.target),
                });
                continue;
            }
            if dist_sq > reach * reach && chase_path_stale(path, at) {
                commands.entity(packet).insert(MoveTarget(at));
            }
        }
        if dist_sq <= reach * reach && (has_move || path.is_some()) {
            // Inside the move goal's radius: stand and wait.
            commands.entity(packet).remove::<(MoveTarget, MovePath)>();
        }
    }
}

/// Shared across-team packet counters. A team's Ports tick into it;
/// dispatch drains it; Packets re-entering top it back up.
#[derive(Resource, Default)]
pub struct PacketBuffer(BTreeMap<u8, u32>);

impl PacketBuffer {
    pub fn add(&mut self, team: u8, n: u32) {
        *self.0.entry(team).or_default() += n;
    }

    /// Read the team's buffer without modifying it.
    pub fn peek(&self, team: u8) -> u32 {
        self.0.get(&team).copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub fn get(&self, team: u8) -> u32 {
        self.peek(team)
    }

    /// Drain up to `want` packets from the team's buffer, returning how
    /// many were actually available.
    pub fn take(&mut self, team: u8, want: u32) -> u32 {
        let entry = self.0.entry(team).or_default();
        let taken = (*entry).min(want);
        *entry -= taken;
        taken
    }
}

/// Per-Port timer component: seconds until the Port's next packet.
/// Starts due: network_buffer.lua's `UnitFinished` sets
/// `portList[u] = 0`, so a Port pays its first packet as soon as it is
/// built, then one every [`PORT_TICK_INTERVAL`].
#[derive(Component, Default)]
pub struct PortTimer(pub f32);

/// Attached to freshly-dispatched Packets; blocks re-entry until the
/// timer expires, matching upstream's spawnStun behaviour.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct PacketSpawnStun {
    pub remaining: f32,
}

/// A teleporter's Dispatch order (`CMD_DISPATCH` in its queue). The
/// gadget runs it from `CommandFallback`, which the engine calls when
/// the order starts and on the unit's slow updates: each pass puts a
/// buffered Packet on every free
/// one of [`DISPATCH_SPOTS`] and sends it walking to `target`. The order
/// ends after its first pass, or with Alt once the buffer is empty
/// (`return true, not opts.alt or bufferSize[team]==0`), and goes with
/// any order that replaces it (it is part of the unit's active order).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
#[component(storage = "SparseSet")]
pub struct DispatchOrder {
    pub target: Vec3,
    /// Alt: dispatch until the buffer is empty.
    pub until_empty: bool,
    /// The order has had its first tick (as [`EnterOrder`]'s).
    started: bool,
}

impl DispatchOrder {
    pub fn new(target: Vec3, until_empty: bool) -> Self {
        Self {
            target,
            until_empty,
            started: false,
        }
    }
}

/// Event: send the given Packet into a teleporter's buffer — `into`
/// (its [`EnterOrder`]'s target), or with `None` the nearest teleporter
/// of its team. Silently ignored if the Packet is still spawn-stunned,
/// paralysed or too far.
#[derive(Message, Debug, Clone, Copy)]
pub struct EnterEvent {
    pub packet: Entity,
    pub into: Option<Entity>,
}

/// Pay out every finished Port's packets. A Port under construction
/// (still [`Emerging`]) is not in network_buffer.lua's `portList` yet
/// — it joins on `UnitFinished` — so its timer doesn't run.
#[allow(clippy::type_complexity)]
pub fn tick_port_buffers(
    time: Res<Time>,
    mut ports: Query<(&UnitType, &TeamId, &mut PortTimer), (Without<Dying>, Without<Emerging>)>,
    mut buffer: ResMut<PacketBuffer>,
) {
    let dt = time.delta_secs();
    for (unit, team, mut timer) in &mut ports {
        if unit.0 != UnitKind::Port {
            continue;
        }
        timer.0 -= dt;
        if timer.0 <= 0.0 {
            timer.0 += PORT_TICK_INTERVAL;
            buffer.add(team.0, 1);
        }
    }
}

pub fn tick_spawn_stun(
    time: Res<Time>,
    mut query: Query<(Entity, &mut PacketSpawnStun)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut stun) in &mut query {
        stun.remaining -= dt;
        if stun.remaining <= 0.0 {
            commands.entity(entity).remove::<PacketSpawnStun>();
        }
    }
}

/// network_buffer.lua's clamp, every [`BUFFER_CAP_PERIOD`] frames
/// after the Ports are paid: a team's buffer never holds more Packets
/// than its unit limit has room for (`math.min(bufferSize[team],
/// MaxUnits - Spring.GetTeamUnitCount(team))`).
pub fn cap_packet_buffers(
    tick: Res<SimTick>,
    counts: Res<TeamUnitCounts>,
    mut buffer: ResMut<PacketBuffer>,
) {
    if !(tick.frame() + 1).is_multiple_of(BUFFER_CAP_PERIOD) {
        return;
    }
    for (team, packets) in &mut buffer.0 {
        let room = counts.limit(*team).saturating_sub(counts.get(*team));
        *packets = (*packets).min(room);
    }
}

/// Run the [`DispatchOrder`]s that are due this frame: one pass each
/// (`CommandFallback`'s `while index < 12 and bufferSize[team] > 0`).
/// The engine runs a command the moment it becomes the unit's order
/// and then on each of the unit's slow updates; a paralysed or
/// unfinished teleporter runs neither (`CUnit::SlowUpdate` returns
/// before its command AI), so its order waits.
/// A spot that is taken costs nothing; each free one gets a Packet,
/// spawned facing out and sent to the order's target (the gadget's
/// `CMD.MOVE`), spawn-stunned and briefly ARMORED (packet.bos
/// `Spawned()`). A team at its unit limit still pays for the spot, but
/// `Spring.CreateUnit` makes nothing: that Packet is lost
/// ([`CreateBudget`]). Unit-id order: one team's teleporters share its
/// buffer.
#[allow(clippy::type_complexity)]
pub fn tick_dispatch_orders(
    tick: Res<SimTick>,
    mut orders: Query<
        (
            Entity,
            &NetId,
            &UnitType,
            &TeamId,
            &Faction,
            &Transform,
            &mut DispatchOrder,
            Has<crate::units::combat::Stunned>,
            Has<Emerging>,
        ),
        Without<Dying>,
    >,
    ground_units: Query<(&UnitType, &Transform), Without<Dying>>,
    nav: Option<Res<NavGridSet>>,
    mut buffer: ResMut<PacketBuffer>,
    counts: Option<Res<TeamUnitCounts>>,
    mut ctx: SpawnContext,
) {
    let frame = tick.frame();
    let mut blockers = None;
    let mut budget = CreateBudget::new(counts.as_deref());
    for (entity, id, kind, team, faction, tf, mut order, stunned, unfinished) in
        orders.iter_mut().sort::<&NetId>()
    {
        let first = !order.started;
        if first {
            order.started = true;
        }
        if stunned || unfinished {
            continue;
        }
        if !first && !slow_update_due(frame, id.0) {
            continue;
        }
        if !kind.0.is_teleporter() {
            ctx.commands.entity(entity).remove::<DispatchOrder>();
            continue;
        }
        // The ground units' blocked squares, once a frame.
        let blockers =
            blockers.get_or_insert_with(|| ground_blockers(&ground_units, &ctx.unit_registry));
        let slope_cap = ctx.unit_registry.max_slope_ratio(UnitKind::Packet);
        let origin = tf.translation;
        for &(dx, dz, facing) in &DISPATCH_SPOTS {
            if buffer.peek(team.0) == 0 {
                break;
            }
            let spot = Vec2::new(
                origin.x + f32::from(dx) * DISPATCH_SPOT_STEP,
                origin.z + f32::from(dz) * DISPATCH_SPOT_STEP,
            );
            if !dispatch_spot_free(spot, nav.as_deref(), slope_cap, blockers) {
                continue;
            }
            buffer.take(team.0, 1);
            if !budget.admit(team.0) {
                continue;
            }
            let packet = spawn_unit_facing(
                UnitKind::Packet,
                *faction,
                team.0,
                Vec3::new(spot.x, origin.y, spot.y),
                Heading::from_facing(facing),
                &mut ctx,
            );
            ctx.commands.entity(packet).insert((
                MoveTarget(order.target),
                PacketSpawnStun {
                    remaining: SPAWN_STUN_SECONDS,
                },
                Armored {
                    after_build: SPAWNED_ARMORED_SECONDS,
                },
            ));
        }
        if !order.until_empty || buffer.peek(team.0) == 0 {
            ctx.commands.entity(entity).remove::<DispatchOrder>();
        }
    }
}

/// The heightmap squares each ground unit blocks, as half-open
/// rectangles: its footprint (`xsize` × `zsize` squares, twice the FBI
/// footprint) from `CSolidObject::GetMapPos`, `(int(pos + SQUARE_SIZE /
/// 2) / SQUARE_SIZE) - size / 2`. Buildings are on the structure layer;
/// Flows fly.
fn ground_blockers(
    units: &Query<(&UnitType, &Transform), Without<Dying>>,
    registry: &UnitRegistry,
) -> Vec<IRect> {
    units
        .iter()
        .filter(|(kind, _)| registry.speed(kind.0) > 0.0 && kind.0 != UnitKind::Flow)
        .map(|(kind, tf)| {
            let size = (registry.footprint_elmos(kind.0) / SQUARE_SIZE).as_ivec2();
            let corner = IVec2::new(
                (tf.translation.x + SQUARE_SIZE / 2.0) as i32 / SQUARE_SIZE as i32,
                (tf.translation.z + SQUARE_SIZE / 2.0) as i32 / SQUARE_SIZE as i32,
            ) - size / 2;
            IRect::from_corners(corner, corner + size)
        })
        .collect()
}

/// `network_dispatch.lua`'s test of a spot: nothing on the ground
/// within 8 elmos (`GetGroundBlocked(tx-8, tz-8, tx+8, tz+8)`: no unit,
/// structure or feature on those squares) and ground a Packet can stand
/// on (`TestBuildOrder(packet) == 2`; `slope_cap` is the Packet's).
fn dispatch_spot_free(
    spot: Vec2,
    nav: Option<&NavGridSet>,
    slope_cap: f32,
    blockers: &[IRect],
) -> bool {
    if spot.x < 0.0 || spot.y < 0.0 {
        return false;
    }
    // The squares from `int((t-8) / 8)` to `int((t+8) / 8)`, half-open.
    let half = SQUARE_SIZE;
    let squares = IRect::new(
        ((spot.x - half).det_max(0.0) / SQUARE_SIZE) as i32,
        ((spot.y - half).det_max(0.0) / SQUARE_SIZE) as i32,
        ((spot.x + half) / SQUARE_SIZE) as i32 + 1,
        ((spot.y + half) / SQUARE_SIZE) as i32 + 1,
    );
    if blockers.iter().any(|b| !b.intersect(squares).is_empty()) {
        return false;
    }
    let Some(nav) = nav else {
        return true;
    };
    // No crush strength: a Bad Block blocks the spot as well.
    let structure = (squares.min.y..squares.max.y)
        .any(|z| (squares.min.x..squares.max.x).any(|x| nav.structure_square(x, z, 0.0)));
    if structure {
        return false;
    }
    nav.bucket(slope_cap).is_some_and(|b| {
        b.speed_map
            .get((spot.x / SQUARE_SIZE) as u32, (spot.y / SQUARE_SIZE) as u32)
            > 0.0
    })
}

/// Drain `EnterEvent`s — absorb Packets that are close to a
/// friendly teleporter and have finished their post-dispatch stun
/// (`network_enter.lua`: `dist < enterDist`, measured on the ground
/// plane, and `spawnStun[u] < GetGameFrame()`). A paralysed Packet
/// executes no command.
#[allow(clippy::type_complexity)]
pub fn process_enter(
    mut events: MessageReader<EnterEvent>,
    packets: Query<
        (
            &UnitType,
            &TeamId,
            &Transform,
            Has<PacketSpawnStun>,
            Has<crate::units::combat::Stunned>,
            Option<&crate::units::combat::Infected>,
        ),
        Without<Dying>,
    >,
    teleporters: Query<(Entity, &UnitType, &TeamId, &Transform), Without<Dying>>,
    mut buffer: ResMut<PacketBuffer>,
    mut virus_spawns: ResMut<crate::units::combat::VirusSpawnQueue>,
    mut commands: Commands,
) {
    let enter_sq = ENTER_DISTANCE * ENTER_DISTANCE;
    // A Packet named twice in one tick (its order and a direct Enter)
    // enters once.
    let mut absorbed: Vec<Entity> = Vec::new();
    for event in events.read() {
        let Ok((packet_unit, packet_team, packet_tf, spawn_stunned, stunned, infected)) =
            packets.get(event.packet)
        else {
            continue;
        };
        if packet_unit.0 != UnitKind::Packet
            || spawn_stunned
            || stunned
            || absorbed.contains(&event.packet)
        {
            continue;
        }
        // The ordered teleporter (its team was checked when the order was
        // given, `AllowCommand`), else any teleporter of the Packet's
        // team; within range either way.
        let in_range = teleporters
            .iter()
            .filter(|(entity, t_unit, t_team, _)| {
                t_unit.0.is_teleporter()
                    && event
                        .into
                        .map_or(t_team.0 == packet_team.0, |into| into == *entity)
            })
            .any(|(_, _, _, t_tf)| {
                crate::units::spatial::flat_dist_sq(packet_tf.translation, t_tf.translation)
                    < enter_sq
            });
        if in_range {
            absorbed.push(event.packet);
            buffer.add(packet_team.0, 1);
            // Absorbed, not killed: no `Dying`, but
            // its counters' marker hooks run on the despawn itself.
            // infection.lua's `UnitDestroyed` fires for any removal, so
            // an infected Packet still releases its Virus in the port.
            if let Some(infected) = infected {
                virus_spawns.push(crate::units::combat::VirusSpawn::of(
                    infected,
                    packet_tf.translation,
                    packet_tf.forward().as_vec3(),
                ));
            }
            commands.entity(event.packet).despawn();
        }
    }
}

/// Extra elmos-per-second added to Flow's base speed per *small
/// building* (Socket, Window, Port, Terminal, Obelisk, Firewall) the
/// team owns. Matches upstream `network_flowspeed.lua::bonusPerFac`.
pub const FLOW_BONUS_PER_BUILDING: f32 = 30.0;

/// Upstream caps Flow at MAX_SPEED=75; our registry speed lookup is in
/// elmos/second (MaxVelocity * 30), so we cap the *bonus* portion so
/// the combined speed never exceeds 75 elmos/frame equivalents.
const FLOW_MAX_SPEED: f32 = 75.0 * GAME_SPEED;

/// Per-Flow multiplier applied on top of `unit_registry.speed`. Updated
/// once per second by `tick_flow_speed` based on the Flow's team's
/// small-building count.
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct SpeedBoost(pub f32);

/// How often to recompute each team's small-building count. One second
/// matches upstream's 27-frame cadence well enough without rescanning
/// every frame.
const FLOW_TICK_INTERVAL: f32 = 1.0;

#[derive(Resource)]
pub struct FlowSpeedTicker(pub f32);

impl Default for FlowSpeedTicker {
    /// Starts half an interval in so the recount lands between the AI
    /// brain's tick (phase 0) and the game-over census (phase 5/6)
    /// instead of on the same frame as both.
    fn default() -> Self {
        Self(FLOW_TICK_INTERVAL * 0.5)
    }
}

/// Periodically recount small buildings per team and update
/// every Flow's `SpeedBoost` so movement can apply the bonus.
pub fn tick_flow_speed(
    time: Res<Time>,
    mut ticker: ResMut<FlowSpeedTicker>,
    mut flows: Query<(&UnitType, &TeamId, &mut SpeedBoost)>,
    unit_registry: Res<crate::units::content::unit_registry::UnitRegistry>,
    small_building_counts: Res<crate::units::lifecycle::bookkeeping::SmallBuildingCounts>,
) {
    ticker.0 += time.delta_secs();
    if ticker.0 < FLOW_TICK_INTERVAL {
        return;
    }
    ticker.0 = 0.0;

    for (unit, team, mut boost) in &mut flows {
        if unit.0 != UnitKind::Flow {
            continue;
        }
        let base = unit_registry.speed(UnitKind::Flow);
        let bonus = small_building_counts.get(team.0) as f32 * FLOW_BONUS_PER_BUILDING;
        boost.0 = (base + bonus).det_min(FLOW_MAX_SPEED) - base;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// `network_enter.lua`: a Packet with an Enter order walks to its
    /// teleporter and asks to enter it once within `enterDist` on the
    /// ground plane, checked when the order arrives and then on the
    /// Packet's slow updates. While it is spawn-stunned the order stays:
    /// it walks to within three quarters of `enterDist` and waits. A
    /// teleporter that is gone ends the order.
    #[test]
    fn an_enter_order_walks_in_then_enters() {
        use crate::interaction::movement::MoveTarget;
        let mut world = World::new();
        world.init_resource::<Messages<EnterEvent>>();
        world.insert_resource(SimTick(1));
        let port = world
            .spawn((
                UnitType(UnitKind::Port),
                Transform::from_xyz(500.0, 0.0, 0.0),
            ))
            .id();
        let packet = world
            .spawn((
                UnitType(UnitKind::Packet),
                crate::net::NetId(1),
                Transform::IDENTITY,
                EnterOrder::new(port),
            ))
            .id();
        let system = world.register_system(tick_enter_orders);
        let run = |world: &mut World, frame: u64, x: f32| -> Vec<EnterEvent> {
            world.insert_resource(SimTick(frame));
            world.get_mut::<Transform>(packet).unwrap().translation = Vec3::new(x, 40.0, 0.0);
            world.run_system(system).unwrap();
            world
                .resource_mut::<Messages<EnterEvent>>()
                .drain()
                .collect()
        };
        // The order runs on arrival: the walk starts.
        assert!(run(&mut world, 1, 0.0).is_empty());
        assert_eq!(
            world.get::<MoveTarget>(packet).map(|t| t.0),
            Some(Vec3::new(500.0, 0.0, 0.0))
        );
        // 140 elmos out on the ground (higher up, which does not count):
        // nothing until the Packet's slow update (frame + id ≡ 0 mod 15).
        assert!(run(&mut world, 2, 360.0).is_empty());
        let events = run(&mut world, 14, 360.0);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].packet, events[0].into), (packet, Some(port)));
        assert!(world.get::<EnterOrder>(packet).is_some());

        // Spawn-stunned: the order is kept, the walk goes on to 112.5…
        world
            .entity_mut(packet)
            .insert(PacketSpawnStun { remaining: 1.0 });
        assert!(run(&mut world, 29, 360.0).is_empty());
        assert!(world.get::<EnterOrder>(packet).is_some());
        assert!(world.get::<MoveTarget>(packet).is_some());
        // …where it stands and waits for the stun to end.
        assert!(run(&mut world, 44, 400.0).is_empty());
        assert!(world.get::<EnterOrder>(packet).is_some());
        assert!(world.get::<MoveTarget>(packet).is_none());
        world.entity_mut(packet).remove::<PacketSpawnStun>();
        assert_eq!(run(&mut world, 59, 400.0).len(), 1);

        world.despawn(port);
        assert!(run(&mut world, 74, 400.0).is_empty());
        assert!(world.get::<EnterOrder>(packet).is_none());
    }

    /// `process_enter`: the ordered teleporter within 150 elmos on the
    /// ground takes the Packet (height does not count); a spawn-stunned
    /// or paralysed Packet stays out, and a Packet named twice enters
    /// once.
    #[test]
    fn a_packet_enters_its_teleporter_within_ground_range() {
        let mut world = World::new();
        world.init_resource::<Messages<EnterEvent>>();
        world.init_resource::<PacketBuffer>();
        world.init_resource::<crate::units::combat::VirusSpawnQueue>();
        let port = world
            .spawn((
                UnitType(UnitKind::Port),
                TeamId(0),
                Transform::from_xyz(0.0, 0.0, 0.0),
            ))
            .id();
        let packet_at = |world: &mut World, x: f32| {
            world
                .spawn((
                    UnitType(UnitKind::Packet),
                    TeamId(0),
                    Transform::from_xyz(x, 120.0, 0.0),
                ))
                .id()
        };
        let near = packet_at(&mut world, 140.0);
        let far = packet_at(&mut world, 160.0);
        let fresh = packet_at(&mut world, 100.0);
        world
            .entity_mut(fresh)
            .insert(PacketSpawnStun { remaining: 1.0 });
        let paralysed = packet_at(&mut world, 100.0);
        world
            .entity_mut(paralysed)
            .insert(crate::units::combat::Stunned);
        for packet in [near, near, far, fresh, paralysed] {
            world.write_message(EnterEvent {
                packet,
                into: Some(port),
            });
        }
        world.run_system_once(process_enter).unwrap();
        assert_eq!(world.resource::<PacketBuffer>().get(0), 1);
        assert!(world.get_entity(near).is_err(), "absorbed");
        for kept in [far, fresh, paralysed] {
            assert!(world.get_entity(kept).is_ok());
        }
    }

    #[test]
    fn buffer_add_and_take_round_trip() {
        let mut b = PacketBuffer::default();
        assert_eq!(b.get(0), 0);
        b.add(0, 5);
        assert_eq!(b.get(0), 5);
        assert_eq!(b.take(0, 3), 3);
        assert_eq!(b.get(0), 2);
        assert_eq!(b.take(0, 10), 2);
        assert_eq!(b.get(0), 0);
    }

    /// Why: the Port's timer used to run from the moment its nanoframe
    /// appeared. network_buffer.lua only lists a Port on `UnitFinished`
    /// (`portList[u] = 0`): nothing while it is built, one packet as it
    /// finishes, then one per 168-frame period.
    #[test]
    fn port_pays_only_once_finished() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        app.init_resource::<Time>().init_resource::<PacketBuffer>();
        let port = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::Port),
                TeamId(2),
                PortTimer::default(),
                Emerging {
                    target_y: 0.0,
                    remaining: 30.0,
                    total: 30.0,
                    rally_point: None,
                    rally_then: None,
                    style: crate::units::lifecycle::spawning::EmergeStyle::Fade,
                },
            ))
            .id();
        let step = |app: &mut App, secs: f32| {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(secs));
            app.world_mut().run_system_once(tick_port_buffers).unwrap();
            app.world().resource::<PacketBuffer>().get(2)
        };
        assert_eq!(step(&mut app, 20.0), 0, "under construction");
        app.world_mut().entity_mut(port).remove::<Emerging>();
        assert_eq!(step(&mut app, 0.1), 1, "first packet on completion");
        assert_eq!(step(&mut app, 5.0), 1);
        assert_eq!(step(&mut app, 0.6), 2, "then every 5.6 s");
    }

    /// network_buffer.lua clamps each team's buffer to the room under its
    /// unit limit, on its 8th frames only.
    #[test]
    fn the_buffer_never_outgrows_the_unit_limit() {
        use crate::units::lifecycle::bookkeeping::{MAX_UNITS_PER_TEAM, track_added_units};
        let mut world = World::new();
        world.init_resource::<TeamUnitCounts>();
        let mut buffer = PacketBuffer::default();
        buffer.add(0, 50);
        buffer.add(1, 50);
        world.insert_resource(buffer);
        for _ in 0..MAX_UNITS_PER_TEAM - 20 {
            world.spawn((UnitType(UnitKind::Packet), TeamId(0)));
        }
        world.run_system_once(track_added_units).unwrap();
        world.insert_resource(SimTick(5));
        world.run_system_once(cap_packet_buffers).unwrap();
        assert_eq!(
            world.resource::<PacketBuffer>().get(0),
            50,
            "every 8th frame"
        );
        world.insert_resource(SimTick(7));
        world.run_system_once(cap_packet_buffers).unwrap();
        assert_eq!(world.resource::<PacketBuffer>().get(0), 20);
        assert_eq!(world.resource::<PacketBuffer>().get(1), 50, "per team");
    }

    /// `GetGroundBlocked(tx-8, tz-8, tx+8, tz+8)`: a ground unit whose
    /// footprint squares touch the spot's blocks it; off the map, or with
    /// the unit a square further, the spot is free (no nav grid here).
    #[test]
    fn a_spot_is_taken_by_a_footprint_on_its_squares() {
        // A 2×2 footprint from square (x, z) (a Packet at x = 100: 12).
        let at = |x: i32, z: i32| IRect::from_corners(IVec2::new(x, z), IVec2::new(x + 2, z + 2));
        // The spot (100, 100) covers squares 11..=13.
        assert!(!dispatch_spot_free(
            Vec2::splat(100.0),
            None,
            0.0,
            &[at(12, 12)]
        ));
        assert!(!dispatch_spot_free(
            Vec2::splat(100.0),
            None,
            0.0,
            &[at(10, 10)]
        ));
        assert!(dispatch_spot_free(
            Vec2::splat(100.0),
            None,
            0.0,
            &[at(14, 12)]
        ));
        assert!(dispatch_spot_free(Vec2::splat(100.0), None, 0.0, &[]));
        assert!(!dispatch_spot_free(Vec2::new(-4.0, 100.0), None, 0.0, &[]));
    }

    #[test]
    fn buffers_are_team_scoped() {
        let mut b = PacketBuffer::default();
        b.add(0, 3);
        b.add(1, 7);
        assert_eq!(b.get(0), 3);
        assert_eq!(b.get(1), 7);
        assert_eq!(b.take(0, 2), 2);
        assert_eq!(b.get(0), 1);
        assert_eq!(b.get(1), 7);
    }
}
