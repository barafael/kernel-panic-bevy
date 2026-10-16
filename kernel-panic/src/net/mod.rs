//! The command layer every networking model needs: player (and AI)
//! input that changes the simulation becomes an explicit, serializable
//! [`SimCommand`], scheduled for a known sim tick and applied by one
//! system at the head of that tick ([`apply::apply_sim_commands`]).
//! Input systems never touch sim state themselves any more; they
//! *issue* into the [`CommandBus`].
//!
//! - Units are named by [`NetId`], a per-match spawn counter assigned
//!   in the sim (a component hook on the first insert), not by `Entity`,
//!   whose index depends on every render / UI entity a machine happens
//!   to spawn.
//! - Commands issued during a frame are staged and sealed into one
//!   [`CommandFrame`] for tick `current + input_delay` by the apply
//!   system; with `input_delay == 0` (single player) they apply on the
//!   very tick they would have landed on before the bus existed.
//! - The in-sim AI issues through the same bus as its team's player
//!   (frames for the next tick, not broadcast: every lockstep peer runs
//!   the AI itself).
//! - Lockstep (M1, `net-experiments/m1-runtime.md`): the tick driver
//!   ([`driver`]) runs the fixed sim one turn at a time. Sealed local
//!   frames leave through the outbox, the host ([`session`]) sequences
//!   every peer's into turns, and only the turns come back into the bus
//!   ([`CommandBus::deliver_turn`]) — for the host and in single player
//!   too (a solo host with delay 0). [`transport`] is what a network
//!   must provide; [`sim_net`] simulates one for the tests,
//!   [`matchbox`] (WebRTC, M2) is the real one.
//! - The room (M2, `net-experiments/m2-lobby.md`): [`lobby`] seats the
//!   players, checks every peer's build and map ([`compat`]) and starts
//!   the networked match with one setup and seed for all.
//!
//! See `net-experiments/x-commands.md` for the inventory, the wire
//! format and the measured frame sizes.

pub(crate) mod apply;
#[cfg(test)]
mod audit;
pub mod chat;
pub(crate) mod codec;
pub mod compat;
pub(crate) mod control;
pub mod driver;
#[cfg(all(any(test, feature = "replay"), not(target_arch = "wasm32")))]
pub(crate) mod fuzz;
#[cfg(test)]
mod leave_tests;
pub mod lobby;
#[cfg(test)]
mod lobby_tests;
#[cfg(test)]
mod lockstep_tests;
pub mod matchbox;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod matchbox_tests;
#[cfg(test)]
mod multipeer_tests;
pub mod rejoin;
#[cfg(test)]
mod rejoin_tests;
pub mod replay_log;
pub mod session;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
pub mod sim_net;
pub mod speed;
#[cfg(test)]
mod speed_tests;
pub mod state;
pub mod transport;
pub mod web_tab;
pub mod wire;

use std::collections::{BTreeMap, HashMap};

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

pub use codec::{IdList, NetPos, PosList};
pub use control::{DevCommand, MatchControl, MatchRules};
pub use driver::LockstepPlugin;

use crate::map_loading::MatchScopeAppExt;
use crate::units::content::definitions::UnitKind;

/// A unit's id: the per-match spawn counter value it got when its
/// `UnitType` was first inserted (`UnitType` requires it,
/// numbered by the `on_add` hook) — Spring's unit id. Deterministic as
/// long as the sim spawns units in the same order everywhere (it must
/// anyway): command buffers apply in schedule order, so the hooks fire
/// in the same order on every peer.
///
/// Anything the sim derives from "the unit's id" uses this, never the
/// `Entity`, whose index is shared with every UI / FX entity the frame
/// loop spawns and so differs between machines: network references in
/// [`SimCommand`]s, the `SlowUpdate` phase ([`rephase_ground_mover`]),
/// the `HoverAir` RNG seed, the AI's canonical iteration order and the
/// headless state hash.
///
/// `0` = unassigned (a world without a [`NetIdAllocator`], i.e. unit
/// tests); a non-zero id inserted explicitly (a client mirroring a
/// server's ids) is kept.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
)]
#[component(on_add = on_add_net_id, on_remove = on_remove_net_id)]
pub struct NetId(pub u32);

/// The spawn counter behind [`NetId`]. Match-scoped: ids restart at 1
/// with every match.
#[derive(Resource, Debug)]
pub struct NetIdAllocator {
    next: u32,
}

impl Default for NetIdAllocator {
    fn default() -> Self {
        Self { next: 1 }
    }
}

/// `NetId` → live entity. Lookups only — never iterated, so the hash
/// order can't leak into the sim.
#[derive(Resource, Debug, Default)]
pub struct NetIdIndex(HashMap<NetId, Entity>);

impl NetIdIndex {
    pub fn get(&self, id: NetId) -> Option<Entity> {
        self.0.get(&id).copied()
    }
}

fn on_add_net_id(mut world: DeferredWorld, ctx: HookContext) {
    let entity = ctx.entity;
    let mut id = world.get::<NetId>(entity).copied().unwrap_or_default();
    if id.0 == 0 {
        let Some(mut alloc) = world.get_resource_mut::<NetIdAllocator>() else {
            return;
        };
        id = NetId(alloc.next);
        alloc.next += 1;
        if let Some(mut slot) = world.get_mut::<NetId>(entity) {
            *slot = id;
        }
    }
    if let Some(mut index) = world.get_resource_mut::<NetIdIndex>() {
        index.0.insert(id, entity);
    }
    rephase(&mut world, entity, id);
}

/// The `SlowUpdate` phase of a ground mover is its `NetId` modulo the
/// slow-update rate, Spring's stagger by unit id. It used to be the
/// *entity index*, which differs between machines (every render / UI
/// entity a machine spawned shifts it): two lockstep peers ran their
/// slow updates on different ticks and drifted apart within ~7 ticks of
/// the first group move. Whichever of the two components lands second
/// sets the phase (`spawn_unit` adds the mover after the `UnitType`).
pub(crate) fn rephase_ground_mover(mut world: DeferredWorld, ctx: HookContext) {
    if let Some(id) = world.get::<NetId>(ctx.entity).copied() {
        rephase(&mut world, ctx.entity, id);
    }
}

fn rephase(world: &mut DeferredWorld, entity: Entity, id: NetId) {
    if id.0 == 0 {
        return;
    }
    if let Some(mut mover) = world.get_mut::<crate::interaction::ground_move::GroundMover>(entity) {
        mover.frame = id.0 % crate::sim::SLOW_UPDATE_RATE;
    }
}

fn on_remove_net_id(mut world: DeferredWorld, ctx: HookContext) {
    let Some(id) = world.get::<NetId>(ctx.entity).copied() else {
        return;
    };
    if let Some(mut index) = world.get_resource_mut::<NetIdIndex>()
        && index.0.get(&id) == Some(&ctx.entity)
    {
        index.0.remove(&id);
    }
}

/// The units a command is for. `Prev` reuses the list of this player's
/// previous `List` (the bus substitutes it automatically when a player
/// orders the same group again, as Spring sends a selection once and
/// then bare commands).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Units {
    List(IdList),
    Prev,
}

impl Units {
    pub fn of(ids: impl IntoIterator<Item = NetId>) -> Self {
        Units::List(IdList::new(ids))
    }

    pub(crate) fn list(&self) -> Option<&IdList> {
        match self {
            Units::List(l) => Some(l),
            Units::Prev => None,
        }
    }
}

/// A positional or unit-targeted order (`QueuedCommand` minus the
/// draw-only positions, which the sim recomputes when applying).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Order {
    Move(NetPos),
    AttackMove(NetPos),
    /// Patrol between the unit (or the end of its queue, Shift) and the
    /// point; the route is worked out from sim state when applied.
    Patrol(NetPos),
    AttackGround(NetPos),
    AttackUnit(NetId),
    Guard(NetId),
}

/// The order of a [`SimCommand::PerUnit`] batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointOrder {
    Move,
    AttackMove,
}

/// A factory build-option click (`CFactoryCAI::GiveCommandReal`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactoryOp {
    /// Append `n`.
    Add(u16),
    /// Put `n` at the front (Alt).
    AddFront(u16),
    /// Remove `n`, newest first (right click) or oldest first (Alt).
    Remove { count: u16, oldest: bool },
}

/// Everything a player (or the AI) can do to the simulation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimCommand {
    /// One order for a group; `queue` = Shift (append behind what each
    /// unit is doing).
    Order {
        units: Units,
        order: Order,
        queue: bool,
    },
    /// A different point per unit (drawn formation, the AI's scattered
    /// pushes): `targets[i]` belongs to `units.ids()[i]`.
    PerUnit {
        order: PointOrder,
        units: IdList,
        targets: PosList,
        queue: bool,
    },
    /// One unit following a drawn path.
    Path {
        unit: NetId,
        waypoints: PosList,
        queue: bool,
    },
    Stop {
        units: Units,
    },
    /// Building placement: every builder gets `sites` (a row is several).
    Build {
        builders: Units,
        #[serde(with = "codec::kind_index")]
        kind: UnitKind,
        sites: PosList,
        queue: bool,
    },
    Factory {
        factories: Units,
        #[serde(with = "codec::kind_index")]
        kind: UnitKind,
        op: FactoryOp,
    },
    SetRepeat {
        units: Units,
        on: bool,
    },
    SetAutoHold {
        units: Units,
        on: bool,
    },
    /// Manual target designation (T).
    SetTarget {
        units: Units,
        target: NetId,
    },
    UnsetTarget {
        units: Units,
    },
    SelfDestruct {
        units: Units,
    },
    /// Bug ↔ Exploit.
    Deploy {
        units: Units,
    },
    /// Packets into the nearest teleporter.
    Enter {
        units: Units,
    },
    /// Aimed abilities at a point: command-fire casts, Dispatch
    /// (`auto` = Alt, keep dispatching until the buffer is empty).
    Cast {
        units: Units,
        target: NetPos,
        auto: bool,
    },
    // M1. New variants go at the end: postcard encodes the variant
    // index, and recorded replays must keep decoding.
    /// Game speed in percent of real time (`NETMSG_USER_SPEED`); see
    /// [`control`] for what the driver does with it.
    SetSpeed {
        percent: u16,
    },
    /// Pause / unpause everyone (`NETMSG_PAUSE`; [`control`]).
    Pause {
        on: bool,
    },
    /// The player gives up (`TEAMMSG_RESIGN`): their units
    /// self-destruct, their later commands are refused.
    Resign,
    /// Dev tools and cheats, refused unless the match allows cheats.
    Dev(DevCommand),
    /// The TAKE button: every remaining unit of `from`, an allied owner
    /// that has left play, becomes the sender's (2026-10-15).
    Take {
        from: u8,
    },
    // M2.
    /// The player's peer left the match (disconnected or timed out):
    /// the host puts it in the turn stream in that player's name, so
    /// every peer applies the leave on the same tick. Then as
    /// [`SimCommand::Resign`] (`TeamFates::leave`, the user's leaving
    /// rules). Spring makes the dropped player inactive and leaves its
    /// team standing (`CPlayerHandler::PlayerLeft`,
    /// `Game/Players/PlayerHandler.cpp:66`). Only the host issues it: a
    /// guest's own is refused at sequencing.
    Dropped,
    // M3.
    /// The host's speed control (Spring's lag protection,
    /// `NETMSG_INTERNAL_SPEED`; [`speed`]): the game runs at `percent`
    /// of real time, at most the players' speed, while a player's
    /// machine can't keep up. Only from [`speed::SERVER_PLAYER`], whose
    /// frames carry nothing else.
    InternalSpeed {
        percent: u16,
    },
    /// The player's peer came back after its seat was dropped
    /// (`net::rejoin`): the host puts it in the turn stream in that
    /// player's name, after the `Dropped`. The seat returns to play if
    /// nothing has happened to it meanwhile — its side still has an
    /// active owner (so its units were not fused), nobody took its
    /// units, the match is not decided; otherwise it is refused and the
    /// player watches. Spring lets a dropped player reconnect to its
    /// team (`CGameServer::BindConnection`, `Net/GameServer.cpp:2875`;
    /// `CTeam::AddPlayer` revives the team, `Sim/Misc/Team.cpp:283`).
    /// Only the host issues it: a guest's own is refused at sequencing.
    Returned,
}

impl SimCommand {
    /// A [`SimCommand::PerUnit`] from (unit, target) pairs, sorted by id
    /// as the wire format wants.
    pub fn per_unit(order: PointOrder, mut pairs: Vec<(NetId, Vec3)>, queue: bool) -> Self {
        pairs.retain(|(id, _)| id.0 != 0);
        pairs.sort_by_key(|(id, _)| *id);
        pairs.dedup_by_key(|(id, _)| *id);
        SimCommand::PerUnit {
            order,
            units: IdList::new(pairs.iter().map(|(id, _)| *id)),
            targets: PosList::quantize(pairs.iter().map(|(_, p)| *p)),
            queue,
        }
    }

    /// One order for one unit (the AI's usual shape).
    pub fn order_one(unit: NetId, order: Order) -> Self {
        SimCommand::Order {
            units: Units::of([unit]),
            order,
            queue: false,
        }
    }

    fn units_mut(&mut self) -> Option<&mut Units> {
        use SimCommand::*;
        match self {
            Order { units, .. }
            | Stop { units }
            | Build {
                builders: units, ..
            }
            | Factory {
                factories: units, ..
            }
            | SetRepeat { units, .. }
            | SetAutoHold { units, .. }
            | SetTarget { units, .. }
            | UnsetTarget { units }
            | SelfDestruct { units }
            | Deploy { units }
            | Enter { units }
            | Cast { units, .. } => Some(units),
            PerUnit { .. }
            | Path { .. }
            | SetSpeed { .. }
            | Pause { .. }
            | Resign
            | Dev(_)
            | Take { .. }
            | Dropped
            | InternalSpeed { .. }
            | Returned => None,
        }
    }
}

/// One player's commands for one sim tick — the unit a lockstep peer
/// sends (an empty frame still says "nothing from me on this tick").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandFrame {
    pub tick: u32,
    pub player: u8,
    pub commands: Vec<SimCommand>,
}

impl CommandFrame {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("frames always serialize")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// The command pipeline of one match.
///
/// Issue side (input systems, the AI): [`CommandBus::issue`] /
/// [`CommandBus::issue_as`]. Lockstep side: the tick driver drains
/// [`CommandBus::outbox`] (our sealed frames) and hands back the turns
/// ([`CommandBus::deliver_turn`]); a recorded log comes in frame by
/// frame ([`CommandBus::deliver`]). Sim side:
/// [`apply::apply_sim_commands`] seals what was issued, then applies
/// every frame due this tick, ordered by player.
#[derive(Resource, Debug, Default)]
pub struct CommandBus {
    /// The tick the next apply runs (== ticks applied this match).
    pub tick: u32,
    /// Ticks between issuing a command and applying it. 0 in single
    /// player; a lockstep match needs it to cover the round trip.
    pub input_delay: u32,
    /// Lockstep: our sealed frames go to [`Self::outbox`] only and come
    /// back through the turn stream ([`Self::deliver_turn`]); nothing
    /// local is applied on its own. The tick driver sets it; a bare
    /// world (unit tests) applies its frames directly.
    pub routed: bool,
    /// Record every applied frame (replay / measurements).
    pub log: Option<Vec<CommandFrame>>,
    pub outbox: Vec<CommandFrame>,
    staged: Vec<SimCommand>,
    staged_ai: Vec<(u8, SimCommand)>,
    pending: BTreeMap<(u32, u8), Vec<SimCommand>>,
    /// Seal side of the `Units::Prev` substitution, per player.
    last_sent: HashMap<u8, IdList>,
    /// Apply side of `Units::Prev`, per player.
    last_applied: HashMap<u8, IdList>,
    /// The tick the last seal ran for: the driver seals before running
    /// a tick, so the apply system's own seal finds nothing left.
    sealed: Option<u32>,
}

impl CommandBus {
    /// Issue a command as the local player. Commands for no units are
    /// dropped here.
    pub fn issue(&mut self, cmd: SimCommand) {
        if !is_empty(&cmd) {
            self.staged.push(cmd);
        }
    }

    /// Issue a command for `player` from inside the sim (the AI).
    pub fn issue_as(&mut self, player: u8, cmd: SimCommand) {
        if !is_empty(&cmd) {
            self.staged_ai.push((player, cmd));
        }
    }

    /// A frame from outside (a recorded log: the golden replays, the
    /// command fuzz). A frame for a tick already simulated is dropped
    /// with an error.
    pub fn deliver(&mut self, frame: CommandFrame) {
        if frame.tick < self.tick {
            error!(
                "command frame for tick {} from player {} arrived at tick {}",
                frame.tick, frame.player, self.tick
            );
            return;
        }
        if frame.commands.is_empty() {
            return;
        }
        self.pending
            .entry((frame.tick, frame.player))
            .or_default()
            .extend(frame.commands);
    }

    /// Seal what was issued since the last tick into frames: the local
    /// player's for `tick + input_delay`, the AI's for `tick`. Called
    /// first thing each tick (by
    /// the tick driver, else by the apply system), so the frames sealed
    /// for `tick` itself apply now. Once per tick: a second call is a
    /// no-op.
    pub fn seal(&mut self, local_player: Option<u8>) {
        if self.sealed == Some(self.tick) {
            return;
        }
        self.sealed = Some(self.tick);
        let staged = std::mem::take(&mut self.staged);
        if let Some(player) = local_player {
            let tick = self.tick + self.input_delay;
            let frame = self.seal_frame(tick, player, staged);
            if self.routed {
                self.outbox.push(frame);
            } else if !frame.commands.is_empty() {
                self.pending
                    .entry((frame.tick, frame.player))
                    .or_default()
                    .extend(frame.commands);
            }
        }
        let mut by_player: BTreeMap<u8, Vec<SimCommand>> = BTreeMap::new();
        for (player, cmd) in std::mem::take(&mut self.staged_ai) {
            by_player.entry(player).or_default().push(cmd);
        }
        for (player, cmds) in by_player {
            let frame = self.seal_frame(self.tick, player, cmds);
            self.pending
                .entry((frame.tick, frame.player))
                .or_default()
                .extend(frame.commands);
        }
    }

    /// [`Self::seal`] even if this tick was sealed already: the driver
    /// seals once per tick, and again each frame while paused, when no
    /// tick runs but the commands must still go out.
    pub fn reseal(&mut self, local_player: Option<u8>) {
        self.sealed = None;
        self.seal(local_player);
    }

    /// The host's turn for the current tick arrived: its frames apply
    /// now, after anything already pending for this tick (the AI's).
    pub fn deliver_turn(&mut self, turn: &wire::Turn) {
        debug_assert_eq!(turn.tick, self.tick, "turns apply in tick order");
        for (player, commands) in &turn.frames {
            if commands.is_empty() {
                continue;
            }
            self.pending
                .entry((turn.tick, *player))
                .or_default()
                .extend(commands.iter().cloned());
        }
    }

    fn seal_frame(&mut self, tick: u32, player: u8, mut commands: Vec<SimCommand>) -> CommandFrame {
        for cmd in &mut commands {
            let Some(units) = cmd.units_mut() else {
                continue;
            };
            let Some(list) = units.list() else {
                continue;
            };
            if self.last_sent.get(&player) == Some(list) {
                *units = Units::Prev;
            } else {
                self.last_sent.insert(player, list.clone());
            }
        }
        CommandFrame {
            tick,
            player,
            commands,
        }
    }

    /// The frames due on the current tick, by player.
    fn take_due(&mut self) -> Vec<CommandFrame> {
        let tick = self.tick;
        let keys: Vec<(u32, u8)> = self
            .pending
            .range((tick, 0)..=(tick, u8::MAX))
            .map(|(k, _)| *k)
            .collect();
        keys.into_iter()
            .map(|key| CommandFrame {
                tick: key.0,
                player: key.1,
                commands: self.pending.remove(&key).unwrap_or_default(),
            })
            .collect()
    }

    /// Resolve `Units::Prev` against what `player` last applied, and
    /// remember explicit lists.
    fn resolve_units(&mut self, player: u8, units: &Units) -> IdList {
        match units {
            Units::List(list) => {
                self.last_applied.insert(player, list.clone());
                list.clone()
            }
            Units::Prev => self.last_applied.get(&player).cloned().unwrap_or_default(),
        }
    }
}

fn is_empty(cmd: &SimCommand) -> bool {
    match cmd {
        SimCommand::PerUnit { units, .. } => units.is_empty(),
        SimCommand::Path { waypoints, .. } => waypoints.0.is_empty(),
        SimCommand::Build { sites, .. } if sites.0.is_empty() => true,
        other => {
            let mut other = other.clone();
            other
                .units_mut()
                .and_then(|u| u.list().map(IdList::is_empty))
                .unwrap_or(false)
        }
    }
}

/// The input-side helper: the bus plus the `NetId` of any entity.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Issue<'w, 's> {
    pub bus: ResMut<'w, CommandBus>,
    ids: Query<'w, 's, &'static NetId>,
}

impl Issue<'_, '_> {
    pub fn id(&self, entity: Entity) -> Option<NetId> {
        self.ids.get(entity).ok().copied()
    }

    /// The units among `entities` that have an id.
    pub fn units(&self, entities: impl IntoIterator<Item = Entity>) -> Units {
        Units::of(entities.into_iter().filter_map(|e| self.id(e)))
    }

    pub fn issue(&mut self, cmd: SimCommand) {
        self.bus.issue(cmd);
    }
}

/// The apply system's place at the head of the fixed tick: before unit
/// motion, the AI and the gameplay chain.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SimCommandSet;

pub struct CommandBusPlugin;

impl Plugin for CommandBusPlugin {
    fn build(&self, app: &mut App) {
        app.init_match_resource::<NetIdAllocator>()
            .init_match_resource::<NetIdIndex>()
            .init_match_resource::<CommandBus>()
            .configure_sets(
                FixedUpdate,
                SimCommandSet.before(crate::units::GameplaySet::Produce),
            )
            .add_systems(FixedUpdate, apply::apply_sim_commands.in_set(SimCommandSet))
            .add_plugins(control::MatchControlPlugin);
    }
}

/// Register the bus on a bare test `World` (no `App`).
#[cfg(test)]
pub(crate) fn init_test_world(world: &mut World) {
    world.init_resource::<NetIdAllocator>();
    world.init_resource::<NetIdIndex>();
    world.init_resource::<CommandBus>();
    world.init_resource::<MatchControl>();
}

/// Seal and apply whatever was issued, right now (tests that drive an
/// input system and check the resulting orders).
#[cfg(test)]
pub(crate) fn apply_now(world: &mut World) {
    use bevy::ecs::system::RunSystemOnce;
    world.run_system_once(apply::apply_sim_commands).unwrap();
}
