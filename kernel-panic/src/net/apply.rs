//! The sim side of the bus: at the head of every fixed tick, seal what
//! was issued and apply the frames due on this tick, one command at a
//! time (each command's world edits land before the next is applied,
//! so "Shift-queue behind the order given a command ago" sees it).
//!
//! Everything that depends on sim state is decided *here*, not by the
//! issuing input system: whether a unit is busy (Shift appends or
//! replaces), where a patrol route starts, which queued build sites a
//! Shift placement cancels, the attack target's position. The issuer
//! only names units, points and kinds.
//!
//! ## Validation
//!
//! Every peer applies every frame, including a modified client's, so
//! a command is checked when it applies, as the original checked it
//! on every receiving client:
//!
//! - **The sender.** A frame from a player without a seat in the match
//!   is refused (`CGame::ClientReadNet` throws on
//!   `!playerHandler.IsValidPlayer(playerNum)` for `NETMSG_COMMAND`,
//!   `Net/NetCommands.cpp:690`), so is anything from a player who
//!   resigned (a spectator: `TEAMMSG_RESIGN` clears their net
//!   selection, `Net/NetCommands.cpp:1225`).
//! - **The units.** Only live units the sender controls take part
//!   (`NETMSG_SELECT` keeps the ids `unitHandler.GetUnit` knows and
//!   `CanControlTeam` allows, `Net/NetCommands.cpp:730`;
//!   `CSelectedUnitsHandler::AINetOrder` the same for AI orders,
//!   `Game/SelectedUnitsHandler.cpp:785`). Dead, unknown and foreign
//!   units are dropped silently — a unit dying between issue and apply
//!   is normal latency, which Spring doesn't warn about either.
//! - **What the units can do** (`CCommandAI::AllowedCommand`,
//!   `Sim/Units/CommandAI/CommandAI.cpp:635`): attacks need a weapon
//!   (`IsAttackCapable`) and a target the weapon may hit, fight /
//!   patrol / guard / drawn moves need a mobile unit, builds an option
//!   the builder has, factory ops a kind the factory builds, abilities
//!   a unit that has them. Units that can't are dropped from the
//!   command; the rest carry it out.
//! - **The parameters.** Points must lie on the map (`IsCommandInMap`,
//!   `CommandAI.cpp:564`); a vent building's site must be a vent, any
//!   building's footprint on the map (`TestUnitBuildSquare`); unit
//!   lists sorted and unique (the encoder's invariant), per-unit lists
//!   as long as their units, counts within what the UI can send.
//!   A bad point drops that point; a malformed list or count the whole
//!   command.
//! - **Cheats** ([`DevCommand`]) only where the match allows them
//!   ([`cheats_allowed`]).
//! - **The server** ([`SERVER_PLAYER`], the host's speed control,
//!   `net::speed`) sends `InternalSpeed` and nothing else; nobody else
//!   sends `InternalSpeed`.
//!
//! Every check reads only replicated state (the seats, the match
//! control, sim components, the map), so all peers decide alike.
//! Refusals are counted in [`Refusals`] and logged — as warnings when
//! no honest client sends them (malformed, unseated, resigned, cheats),
//! at debug level when they happen in play (points off the map: the
//! AI's scattered pushes near an edge, which Spring drops silently
//! too). Nothing panics.
//! Alliance questions go through [`ApplyCtx::friendly`] only
//! ([`Alliances::friendly`](crate::units::teams::Alliances::friendly)).

use bevy::ecs::system::{SystemParam, SystemState};
use bevy::prelude::*;

use super::control::{MAX_SPEED_PERCENT, MIN_SPEED_PERCENT};
use super::speed::SERVER_PLAYER;
use super::{
    CommandBus, DevCommand, FactoryOp, MatchControl, NetId, NetIdIndex, NetPos, Order, PointOrder,
    SimCommand,
};
use crate::game_setup::ActiveMatch;
use crate::interaction::movement::{CommandQueue, MoveTarget, Orders, QueuedCommand};
use crate::interaction::selection::apply_ordered_command;
use crate::interaction::selection::right_click::queue_command;
use crate::sim::DetFloat;
use crate::terrain::geovent::{GeoventSmoker, VentClaim};
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::{Dying, ForcedTarget, SelfDestructCountdown};
use crate::units::components::{Health, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::construction::{
    Constructing, PendingBuild, QueuedBuild, buildings_for, queue_build,
};
use crate::units::lifecycle::production::{FactoryOrders, Producer, factory_roster};
use crate::units::mechanics::command_fire::CommandFireEvent;
use crate::units::mechanics::deploy::DeployEvent;
use crate::units::mechanics::network_buffer::{AutoDispatch, DispatchEvent, EnterEvent};
use crate::units::mechanics::worm::AutoHold;

/// Most units one factory command may add or remove: the UI's largest
/// multiplier (Ctrl+Shift, `CCommandAI::GetCountMultiplierFromOptions`).
pub const MAX_FACTORY_COUNT: u16 = 100;
/// Most sites in one build command (a long Shift row is a few dozen).
pub const MAX_BUILD_SITES: usize = 256;
/// Most targets one area attack expands to (Spring has no cap; keeps a
/// map-wide drag from queueing hundreds of attacks per unit).
pub const MAX_AREA_TARGETS: usize = 64;
/// Largest area-order radius in elmos (`CGuiHandler::GetCommand`'s
/// `maxRadius`).
pub const MAX_AREA_RADIUS: f32 = 100_000.0;
/// Most units one `/give` spawns.
pub const MAX_DEV_SPAWN: u16 = 100;

/// Whether the match allows [`DevCommand`]s: at most one human seat
/// (single player and the dev tools' sandbox), or the host's
/// [`MatchRules::cheats`]. A bare world without a match (unit tests)
/// allows them.
///
/// [`MatchRules::cheats`]: super::MatchRules::cheats
pub(crate) fn cheats_allowed(active: Option<&ActiveMatch>) -> bool {
    active.is_none_or(|m| m.0.rules.cheats || m.0.players.iter().filter(|p| !p.ai).count() <= 1)
}

/// The host's [`MatchRules`](super::MatchRules) for one command of
/// `player`: pauses where the player may pause, speeds inside the
/// range, cheats where the match allows them; the speed control's
/// `InternalSpeed` from the server ([`SERVER_PLAYER`]) and nothing else
/// from it. The host asks before it
/// sequences a command (`driver::host_admit`), every peer again when it
/// applies ([`apply_sim_commands`]), so both refuse the same.
pub(crate) fn rules_admit(
    active: Option<&ActiveMatch>,
    player: u8,
    cmd: &SimCommand,
) -> Result<(), Refusal> {
    // The host's speed control is the server's only command, and only
    // it sends as the server (`net::speed`).
    if (player == SERVER_PLAYER) != matches!(cmd, SimCommand::InternalSpeed { .. }) {
        return Err(Refusal::ServerOnly);
    }
    let rules = active.map(|m| m.0.rules).unwrap_or_default();
    match cmd {
        SimCommand::Pause { .. } if !rules.may_pause(player) => {
            Err(Refusal::Rules("pausing is off"))
        }
        SimCommand::SetSpeed { percent } => {
            let (lo, hi) = rules.speed_range();
            if *percent < lo || *percent > hi {
                Err(Refusal::Rules("speed outside the match's range"))
            } else {
                Ok(())
            }
        }
        SimCommand::Dev(_) if !cheats_allowed(active) => Err(Refusal::NoCheats),
        _ => Ok(()),
    }
}

/// Why a command, or a part of it, was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The frame's player has no seat in the match.
    Unseated,
    /// The player has left play (resigned, taken out) and spectates.
    Inactive,
    /// Impossible as sent: a unit list not sorted and unique, per-unit
    /// lists of different lengths, a count or speed out of range.
    Malformed(&'static str),
    /// A point off the map.
    OffMap,
    /// A build site the building can't stand on.
    BadSite,
    /// An attack on a friend.
    Friendly,
    /// A cheat in a match without cheats.
    NoCheats,
    /// A `Take` of units that can't be taken: from an enemy, from an
    /// owner still in play, or (lost the race) nothing left to take.
    NotTakeable(&'static str),
    /// Against the host's [`MatchRules`](super::MatchRules): a pause
    /// where pausing is off, a speed outside the match's range.
    Rules(&'static str),
    /// A `Returned` for a seat that cannot come back: it did not drop,
    /// its side is out, its units were taken, the match is decided.
    NotReturnable(&'static str),
    /// `InternalSpeed` from a seat, or anything else from
    /// [`SERVER_PLAYER`].
    ServerOnly,
}

/// Command refusals (diagnostics and tests; the sim never reads it).
#[derive(Resource, Debug, Default)]
pub struct Refusals {
    /// Commands refused whole or in part.
    pub count: u64,
    /// The latest: tick, player, why.
    pub last: Option<(u32, u8, Refusal)>,
}

/// The rules side of applying: match control, seats, the map.
#[derive(SystemParam)]
#[allow(clippy::type_complexity)]
pub(crate) struct Rules<'w, 's> {
    control: Option<ResMut<'w, MatchControl>>,
    active: Option<Res<'w, ActiveMatch>>,
    heightmap: Option<Res<'w, Heightmap>>,
    refusals: Option<ResMut<'w, Refusals>>,
    fates: Option<ResMut<'w, crate::units::teams::TeamFates>>,
    /// The Network's packet buffers (a `Take` moves the leaver's).
    buffer: Option<ResMut<'w, crate::units::mechanics::network_buffer::PacketBuffer>>,
    all_vents: Query<'w, 's, &'static GeoventSmoker>,
    /// Every live unit with its tallies: what a `Take` moves.
    owned_units: Query<
        'w,
        's,
        (
            Entity,
            &'static NetId,
            &'static TeamId,
            Has<crate::units::lifecycle::bookkeeping::CountedUnit>,
            Has<crate::units::lifecycle::bookkeeping::CountedSmallBuilding>,
        ),
        (With<UnitType>, Without<Dying>),
    >,
    health: Query<'w, 's, &'static mut Health, Without<Dying>>,
}

impl Rules<'_, '_> {
    /// Whether `player` commands units of `team`: its own, or any with
    /// god mode on.
    fn controls(&self, player: u8, team: u8) -> bool {
        team == player || self.control.as_ref().is_some_and(|c| c.god_mode)
    }

    /// `IsCommandInMap`: on the map in the plane. No map: anywhere.
    fn in_map(&self, p: Vec3) -> bool {
        self.heightmap.as_ref().is_none_or(|h| {
            let (w, d) = h.world_size();
            (0.0..=w).contains(&p.x) && (0.0..=d).contains(&p.z)
        })
    }

    /// `TestUnitBuildSquare`'s fixed part: the footprint on the map, and
    /// a vent building (KP's yardmap needs a geothermal vent) on a vent.
    fn site_ok(&self, kind: UnitKind, site: Vec3, registry: Option<&UnitRegistry>) -> bool {
        let half = registry.map_or(Vec2::ZERO, |r| r.footprint_elmos(kind) * 0.5);
        let on_map = self.heightmap.as_ref().is_none_or(|h| {
            let (w, d) = h.world_size();
            site.x - half.x >= 0.0
                && site.z - half.y >= 0.0
                && site.x + half.x <= w
                && site.z + half.y <= d
        });
        let vent_ok = !kind.is_small_building()
            || self
                .all_vents
                .iter()
                .any(|v| v.pos.xz().distance_squared(site.xz()) < 1.0);
        on_map && vent_ok
    }

    /// Whether `player` has a seat in the match.
    fn admit_seat(&self, player: u8) -> Result<(), Refusal> {
        if self
            .active
            .as_ref()
            // Seat `i` is owner `i` (`units::teams`).
            .is_some_and(|m| usize::from(player) >= m.0.players.len())
        {
            return Err(Refusal::Unseated);
        }
        Ok(())
    }

    /// Whether `player` may send anything at all.
    fn admit_player(&self, player: u8) -> Result<(), Refusal> {
        // The host's own voice (speed control): no seat, never resigns.
        if player == SERVER_PLAYER {
            return Ok(());
        }
        self.admit_seat(player)?;
        if self
            .control
            .as_ref()
            .is_some_and(|c| c.resigned.contains(&player))
            || self.fates.as_ref().is_some_and(|f| f.is_dead(player))
        {
            return Err(Refusal::Inactive);
        }
        Ok(())
    }

    fn refuse(&mut self, tick: u32, player: u8, cmd: &SimCommand, why: Refusal) {
        let what = command_name(cmd);
        match why {
            Refusal::OffMap
            | Refusal::BadSite
            | Refusal::Friendly
            | Refusal::NotTakeable("nothing left") => {
                debug!("refused (part of) {what} from player {player} at tick {tick}: {why:?}");
            }
            _ => warn!("refused {what} from player {player} at tick {tick}: {why:?}"),
        }
        if let Some(r) = self.refusals.as_mut() {
            r.count += 1;
            r.last = Some((tick, player, why));
        }
    }
}

/// Everything applying a command reads or writes.
#[derive(SystemParam)]
#[allow(clippy::type_complexity)]
pub(crate) struct ApplyCtx<'w, 's> {
    commands: Commands<'w, 's>,
    index: Res<'w, NetIdIndex>,
    units: Query<
        'w,
        's,
        (
            Option<&'static TeamId>,
            &'static UnitType,
            Option<&'static Transform>,
        ),
        Without<Dying>,
    >,
    orders: Orders<'w, 's>,
    queues: Query<
        'w,
        's,
        (
            Option<&'static CommandQueue>,
            Option<&'static MoveTarget>,
            Option<&'static PendingBuild>,
            Option<&'static Constructing>,
        ),
    >,
    producers: Query<'w, 's, &'static mut Producer>,
    /// The orders factories give the units they build.
    factory_orders: Query<'w, 's, &'static mut FactoryOrders>,
    autohold: Query<'w, 's, &'static mut AutoHold>,
    vents: Query<'w, 's, (Entity, &'static GeoventSmoker), Without<VentClaim>>,
    /// Last tick's spatial snapshot (area orders expand through it);
    /// bare test worlds have none.
    spatial: Option<Res<'w, crate::units::spatial::SpatialIndex>>,
    net_ids: Query<'w, 's, &'static NetId>,
    /// The standing map features (clear targets).
    features: Query<'w, 's, (&'static crate::units::features::Feature, &'static Transform)>,
    registry: Option<Res<'w, UnitRegistry>>,
    command_fire: Option<ResMut<'w, Messages<CommandFireEvent>>>,
    deploy: Option<ResMut<'w, Messages<DeployEvent>>>,
    dispatch: Option<ResMut<'w, Messages<DispatchEvent>>>,
    enter: Option<ResMut<'w, Messages<EnterEvent>>>,
    rules: Rules<'w, 's>,
    alliances: Option<Res<'w, crate::units::teams::Alliances>>,
}

/// Seal the commands issued since the last tick, then apply every frame
/// due on this tick (by player, each in issue order).
pub(crate) fn apply_sim_commands(world: &mut World, state: &mut SystemState<ApplyCtx>) {
    let local = super::driver::local_player(world);
    let Some(mut bus) = world.get_resource_mut::<CommandBus>() else {
        return;
    };
    bus.seal(local);
    let frames = bus.take_due();
    bus.tick += 1;
    if let Some(log) = bus.log.as_mut() {
        log.extend(frames.iter().cloned());
    }
    for frame in frames {
        for cmd in &frame.commands {
            // `Units::Prev` resolves against the bus's per-player memory.
            // Every explicit list goes into it, refused command or not:
            // it mirrors the sender's seal-side memory, which recorded
            // the list when it sent it (else an honest `Prev` after a
            // refused command would name another group).
            let ids = {
                let mut bus = world.resource_mut::<CommandBus>();
                cmd.units()
                    .map(|u| bus.resolve_units(frame.player, u))
                    .unwrap_or_default()
            };
            let Ok(mut ctx) = state.get_mut(world) else {
                return;
            };
            // Checked per command: a resignation mid-frame refuses the
            // rest of the frame.
            // A returning player is out of play until its `Returned`
            // applies: only its seat is checked here (`returned`).
            let admitted = if matches!(cmd, SimCommand::Returned) {
                ctx.rules.admit_seat(frame.player)
            } else {
                ctx.rules.admit_player(frame.player)
            };
            if let Err(why) = admitted
                .and_then(|()| rules_admit(ctx.rules.active.as_deref(), frame.player, cmd))
                .and_then(|()| ctx.check_shape(cmd, &ids))
            {
                // A drop for a player who already left (resigned, then
                // disconnected: "Leave match") is the normal way out,
                // not a refusal.
                if !(matches!(cmd, SimCommand::Dropped) && why == Refusal::Inactive) {
                    ctx.rules.refuse(frame.tick, frame.player, cmd, why);
                }
                continue;
            }
            // The post-game statistics' actions per minute: what every
            // peer applied (`stats`; not sim state).
            if let Some(mut stats) = world.get_resource_mut::<crate::stats::MatchStats>() {
                stats.count_action(frame.player, cmd);
            }
            if let SimCommand::Dev(DevCommand::Spawn {
                kind,
                team,
                at,
                count,
                spacing,
            }) = cmd
            {
                let spawn = DevSpawn {
                    kind: *kind,
                    team: *team,
                    at: *at,
                    count: *count,
                    spacing: *spacing,
                };
                if let Err(e) = world.run_system_cached_with(dev_spawn, spawn) {
                    warn!("dev spawn failed: {e}");
                }
                continue;
            }
            let Ok(mut ctx) = state.get_mut(world) else {
                return;
            };
            ctx.apply(frame.tick, frame.player, cmd, ids.ids());
            state.apply(world);
        }
    }
}

/// A unit the sender controls: entity, kind, team.
#[derive(Clone, Copy)]
struct Own {
    e: Entity,
    kind: UnitKind,
    team: u8,
}

impl ApplyCtx<'_, '_> {
    /// Structural checks: whole-command refusals that need no unit.
    fn check_shape(&self, cmd: &SimCommand, ids: &super::IdList) -> Result<(), Refusal> {
        // The resolved list (a `Units::Prev` naming a malformed one is
        // malformed too).
        if !canonical(ids.ids()) {
            return Err(Refusal::Malformed("unit list not sorted and unique"));
        }
        match cmd {
            SimCommand::PerUnit { units, targets, .. } => {
                if !canonical(units.ids()) {
                    return Err(Refusal::Malformed("unit list not sorted and unique"));
                }
                if units.ids().len() != targets.0.len() {
                    return Err(Refusal::Malformed("one target per unit"));
                }
            }
            SimCommand::Path { unit, .. } if unit.0 == 0 => {
                return Err(Refusal::Malformed("unit 0"));
            }
            SimCommand::Build { sites, .. } if sites.0.len() > MAX_BUILD_SITES => {
                return Err(Refusal::Malformed("too many build sites"));
            }
            SimCommand::Factory { op, .. } => {
                let n = match *op {
                    FactoryOp::Add(n) | FactoryOp::AddFront(n) => n,
                    FactoryOp::Remove { count, .. } => count,
                };
                if n > MAX_FACTORY_COUNT {
                    return Err(Refusal::Malformed("factory count"));
                }
            }
            SimCommand::SetSpeed { percent } | SimCommand::InternalSpeed { percent }
                if !(MIN_SPEED_PERCENT..=MAX_SPEED_PERCENT).contains(percent) =>
            {
                return Err(Refusal::Malformed("speed out of range"));
            }
            // One point for the whole command: off the map, all of it.
            SimCommand::Order {
                order:
                    Order::Move(p)
                    | Order::AttackMove(p)
                    | Order::Patrol(p)
                    | Order::AttackGround(p)
                    | Order::AttackArea { center: p, .. }
                    | Order::ClearArea { center: p, .. }
                    | Order::Bombard {
                        target: super::BombardTarget::Ground(p),
                        ..
                    },
                ..
            }
            | SimCommand::Cast { target: p, .. }
                if !self.rules.in_map(p.to_vec3()) =>
            {
                return Err(Refusal::OffMap);
            }
            SimCommand::Order {
                order: Order::AttackArea { radius, .. } | Order::ClearArea { radius, .. },
                ..
            } if radius.0 == 0 || radius.to_f32() > MAX_AREA_RADIUS => {
                return Err(Refusal::Malformed("area radius"));
            }
            SimCommand::Dev(dev) => {
                // (Cheats at all: `rules_admit`.)
                match dev {
                    DevCommand::Spawn { count, at, .. } => {
                        if *count == 0 || *count > MAX_DEV_SPAWN {
                            return Err(Refusal::Malformed("spawn count"));
                        }
                        if !self.rules.in_map(at.to_vec3()) {
                            return Err(Refusal::OffMap);
                        }
                    }
                    DevCommand::Kill { units } | DevCommand::SetHealth { units, .. }
                        if !canonical(units.ids()) =>
                    {
                        return Err(Refusal::Malformed("unit list not sorted and unique"));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }
    /// Friend or foe ([`Alliances::friendly`]); a bare world without
    /// alliances counts an owner alone as its friend.
    ///
    /// [`Alliances::friendly`]: crate::units::teams::Alliances::friendly
    fn friendly(&self, a: u8, b: u8) -> bool {
        self.alliances
            .as_ref()
            .map_or(a == b, |al| al.friendly(a, b))
    }

    /// The live units among `ids` that `player` controls, in id order.
    fn owned(&self, player: u8, ids: &[NetId]) -> Vec<Own> {
        ids.iter()
            .filter_map(|id| self.owned_one(player, *id))
            .collect()
    }

    fn owned_one(&self, player: u8, id: NetId) -> Option<Own> {
        let e = self.index.get(id)?;
        let (team, ut, _) = self.units.get(e).ok()?;
        // A unit without a team (bare test worlds) obeys anyone.
        let team = team.map_or(player, |t| t.0);
        self.rules.controls(player, team).then_some(Own {
            e,
            kind: ut.0,
            team,
        })
    }

    /// Any live unit (an attack or guard target).
    fn live(&self, id: NetId) -> Option<(Entity, Vec3, u8, UnitKind)> {
        let e = self.index.get(id)?;
        let (team, ut, tf) = self.units.get(e).ok()?;
        Some((
            e,
            tf.map_or(Vec3::ZERO, |t| t.translation),
            team.map_or(0, |t| t.0),
            ut.0,
        ))
    }

    fn pos(&self, e: Entity) -> Vec3 {
        self.units
            .get(e)
            .ok()
            .and_then(|(_, _, tf)| tf.map(|t| t.translation))
            .unwrap_or_default()
    }

    /// `canMove` (a speed): fight, patrol, guard and drawn moves.
    fn mobile(&self, kind: UnitKind) -> bool {
        self.registry.as_ref().is_none_or(|r| r.speed(kind) > 0.0)
    }

    /// `IsAttackCapable`: a weapon.
    fn armed(&self, kind: UnitKind) -> bool {
        self.registry
            .as_ref()
            .is_none_or(|r| r.weapon(kind).is_some())
    }

    /// Armed and able to hit `target` (`OnlyTargetCategory`).
    fn can_hit(&self, kind: UnitKind, target: UnitKind) -> bool {
        self.armed(kind)
            && self
                .registry
                .as_ref()
                .is_none_or(|r| r.can_attack(kind, target))
    }

    fn give(&mut self, e: Entity, cmd: QueuedCommand, queue: bool) {
        apply_ordered_command(e, cmd, queue, &self.orders, &mut self.commands);
    }

    /// A factory that keeps move, fight, patrol and guard orders for the
    /// units it builds ([`FactoryOrders`]).
    fn takes_rally(&self, u: &Own) -> bool {
        u.kind.takes_rally_orders() && self.factory_orders.contains(u.e)
    }

    /// Give factory `e` an order for its new units: it replaces their
    /// orders, or with `queue` (Shift) goes behind them.
    fn give_factory(&mut self, e: Entity, cmd: QueuedCommand, queue: bool) {
        /// Plenty for any rally route; keeps a spammed Shift from
        /// growing a factory's list without bound.
        const MAX_FACTORY_ORDERS: usize = 64;
        if let Ok(mut orders) = self.factory_orders.get_mut(e) {
            if !queue {
                orders.commands.clear();
            }
            if orders.commands.len() < MAX_FACTORY_ORDERS {
                orders.commands.push(cmd);
            }
        }
    }

    fn apply(&mut self, tick: u32, player: u8, cmd: &SimCommand, ids: &[NetId]) {
        match cmd {
            SimCommand::Order { order, queue, .. } => {
                self.order(tick, player, cmd, ids, *order, *queue);
            }
            SimCommand::PerUnit {
                order,
                units,
                targets,
                queue,
            } => {
                let mut off_map = false;
                for (id, target) in units.ids().iter().zip(targets.points()) {
                    let Some(own) = self.owned_one(player, *id) else {
                        continue;
                    };
                    if !self.mobile(own.kind) {
                        continue;
                    }
                    if !self.rules.in_map(target) {
                        off_map = true;
                        continue;
                    }
                    let cmd = match order {
                        PointOrder::Move => QueuedCommand::Move(target),
                        PointOrder::AttackMove => QueuedCommand::AttackMove(target),
                    };
                    self.give(own.e, cmd, *queue);
                }
                if off_map {
                    self.rules.refuse(tick, player, cmd, Refusal::OffMap);
                }
            }
            SimCommand::Path {
                unit,
                waypoints,
                queue,
            } => {
                let Some(own) = self.owned_one(player, *unit) else {
                    return;
                };
                let factory = self.takes_rally(&own);
                if !self.mobile(own.kind) && !factory {
                    return;
                }
                let points: Vec<Vec3> = waypoints.points().collect();
                let on_map: Vec<Vec3> = points
                    .iter()
                    .copied()
                    .filter(|p| self.rules.in_map(*p))
                    .collect();
                if on_map.len() != points.len() {
                    self.rules.refuse(tick, player, cmd, Refusal::OffMap);
                }
                let mut points = on_map.into_iter();
                if factory {
                    if let Some(first) = points.next() {
                        self.give_factory(own.e, QueuedCommand::Move(first), *queue);
                    }
                    for p in points {
                        self.give_factory(own.e, QueuedCommand::Move(p), true);
                    }
                    return;
                }
                if let Some(first) = points.next() {
                    self.give(own.e, QueuedCommand::Move(first), *queue);
                }
                for p in points {
                    queue_command(own.e, QueuedCommand::Move(p), &mut self.commands);
                }
            }
            SimCommand::Stop { .. } => {
                for own in self.owned(player, ids) {
                    crate::interaction::clear_orders(&mut self.commands.entity(own.e))
                        .remove::<SelfDestructCountdown>();
                    if let Ok(mut orders) = self.factory_orders.get_mut(own.e) {
                        orders.commands.clear();
                    }
                }
            }
            SimCommand::Build {
                kind, sites, queue, ..
            } => {
                let all: Vec<Vec3> = sites.points().collect();
                let ok: Vec<Vec3> = all
                    .iter()
                    .copied()
                    .filter(|s| self.rules.site_ok(*kind, *s, self.registry.as_deref()))
                    .collect();
                if ok.len() != all.len() {
                    self.rules.refuse(tick, player, cmd, Refusal::BadSite);
                }
                if !ok.is_empty() {
                    self.build(player, ids, *kind, &ok, *queue);
                }
            }
            SimCommand::Factory { kind, op, .. } => {
                for own in self.owned(player, ids) {
                    if !factory_roster(own.kind).contains(kind) {
                        continue;
                    }
                    if let Ok(mut producer) = self.producers.get_mut(own.e) {
                        apply_factory_op(&mut producer, *kind, *op);
                    }
                }
            }
            SimCommand::SetRepeat { on, .. } => {
                for own in self.owned(player, ids) {
                    if let Ok(mut p) = self.producers.get_mut(own.e) {
                        p.set_repeat(*on);
                    }
                }
            }
            SimCommand::SetAutoHold { on, .. } => {
                for own in self.owned(player, ids) {
                    if let Ok(mut a) = self.autohold.get_mut(own.e) {
                        a.0 = *on;
                    }
                }
            }
            SimCommand::SetTarget { target, .. } => {
                // Manual fire only ever designates hostiles.
                let Some((t, _, t_team, t_kind)) = self.live(*target) else {
                    return;
                };
                for own in self.owned(player, ids) {
                    if self.friendly(own.team, t_team) || !self.can_hit(own.kind, t_kind) {
                        continue;
                    }
                    self.commands.entity(own.e).insert(ForcedTarget(t));
                }
            }
            SimCommand::UnsetTarget { .. } => {
                for own in self.owned(player, ids) {
                    self.commands.entity(own.e).remove::<ForcedTarget>();
                }
            }
            SimCommand::SelfDestruct { .. } => {
                for own in self.owned(player, ids) {
                    let countdown =
                        SelfDestructCountdown::ordered(own.kind, self.registry.as_deref());
                    self.commands.entity(own.e).insert(countdown);
                }
            }
            SimCommand::Deploy { .. } => {
                for own in self.owned(player, ids) {
                    if own.kind.deploy_pair().is_some()
                        && let Some(m) = self.deploy.as_mut()
                    {
                        m.write(DeployEvent {
                            entity: own.e,
                            then: None,
                        });
                    }
                }
            }
            SimCommand::Enter { .. } => {
                for own in self.owned(player, ids) {
                    if own.kind == UnitKind::Packet
                        && let Some(m) = self.enter.as_mut()
                    {
                        m.write(EnterEvent { packet: own.e });
                    }
                }
            }
            SimCommand::Cast { target, auto, .. } => {
                let target = target.to_vec3();
                for own in self.owned(player, ids) {
                    if own.kind.has_command_fire_ability() {
                        if let Some(m) = self.command_fire.as_mut() {
                            m.write(CommandFireEvent {
                                attacker: own.e,
                                target,
                            });
                        }
                    } else if own.kind.is_teleporter() {
                        // Alt: only the marker — the first batch goes
                        // out next tick via `tick_auto_dispatch`.
                        if *auto {
                            self.commands.entity(own.e).insert(AutoDispatch { target });
                        } else if let Some(m) = self.dispatch.as_mut() {
                            m.write(DispatchEvent {
                                teleporter: own.e,
                                target,
                            });
                        }
                    }
                }
            }
            SimCommand::SetSpeed { percent } => {
                if let Some(c) = self.rules.control.as_mut() {
                    c.set_wanted(*percent);
                }
            }
            SimCommand::InternalSpeed { percent } => {
                if let Some(c) = self.rules.control.as_mut() {
                    c.set_internal(*percent);
                }
            }
            SimCommand::Pause { on } => {
                if let Some(c) = self.rules.control.as_mut() {
                    c.paused = *on;
                    c.paused_by = on.then_some(player);
                }
            }
            SimCommand::Resign => self.resign(player),
            SimCommand::Dropped => {
                self.resign(player);
                if let Some(c) = self.rules.control.as_mut() {
                    c.dropped.insert(player);
                }
            }
            SimCommand::Take { from } => self.take(tick, player, cmd, *from),
            SimCommand::Returned => self.returned(tick, player, cmd),
            SimCommand::Dev(dev) => self.dev(dev),
        }
    }

    /// `TEAMMSG_RESIGN`. The player becomes a spectator (their later
    /// commands are refused) and their owner leaves play
    /// ([`TeamFates::leave`], the one way out, shared with being taken
    /// out and, later, dropping). The user's rule (2026-10-15) decides
    /// the units in this tick's game-over pass: with an ally still
    /// active they stay for it to take (`Take`); as the ally team's last
    /// active owner they blow up within 4 s
    /// (`units::lifecycle::game_over`).
    ///
    /// Upstream differs: Recoil only drops the player to spectating and
    /// leaves the team leaderless, its units standing
    /// (`Net/NetCommands.cpp:1195`); Spring's old quit box
    /// self-destructed every unit ("resign, so self-d all units",
    /// `Game/UI/QuitBox.cpp:329`).
    ///
    /// [`TeamFates::leave`]: crate::units::teams::TeamFates::leave
    fn resign(&mut self, player: u8) {
        if let Some(c) = self.rules.control.as_mut() {
            c.resigned.insert(player);
        }
        if let Some(fates) = self.rules.fates.as_mut() {
            fates.leave(player);
        }
    }

    /// The flashing TAKE button (the user's rule, 2026-10-15): `player`,
    /// still active, takes every remaining unit of `from`, an allied
    /// owner that has left play. All of them, at once, in `NetId` order:
    /// the owner changes ([`TeamId`]) and the unit and small-building
    /// tallies move with it (`bookkeeping::retally`); orders were
    /// cancelled when `from` left. Everything else keyed by owner reads
    /// [`TeamId`] each tick (the AI's snapshot, cloak detection, the
    /// spatial index). Unit caps don't move: the taker may end up over
    /// its cap, which stops its factories until it is back under.
    ///
    /// The first valid `Take` in turn order wins; later ones find
    /// nothing left and are refused, as is a `Take` from an enemy, from
    /// an owner still in play, or from oneself. Spring has no such
    /// command: its leaderless teams can be taken over with `/take` by
    /// an ally (`TEAMMSG_GIVEAWAY`, `Net/NetCommands.cpp:1160`), team
    /// and all, which is the closest thing.
    fn take(&mut self, tick: u32, player: u8, cmd: &SimCommand, from: u8) {
        let seated = self
            .rules
            .active
            .as_ref()
            .is_none_or(|m| usize::from(from) < m.0.players.len());
        let left = self.rules.fates.as_ref().is_some_and(|f| f.is_dead(from));
        let why = if from == player {
            Some("from oneself")
        } else if !seated || !self.friendly(player, from) {
            Some("not an ally")
        } else if !left {
            Some("still in play")
        } else {
            None
        };
        if let Some(why) = why {
            self.rules
                .refuse(tick, player, cmd, Refusal::NotTakeable(why));
            return;
        }
        let mut theirs: Vec<_> = self
            .rules
            .owned_units
            .iter()
            .filter(|(_, _, team, ..)| team.0 == from)
            .map(|(e, id, _, counted, small)| (*id, e, counted, small))
            .collect();
        if theirs.is_empty() {
            self.rules
                .refuse(tick, player, cmd, Refusal::NotTakeable("nothing left"));
            return;
        }
        theirs.sort_unstable_by_key(|(id, ..)| *id);
        for (_, e, counted, small) in theirs {
            let mut ec = self.commands.entity(e);
            ec.insert(TeamId(player));
            crate::units::lifecycle::bookkeeping::retally(&mut ec, player, counted, small);
        }
        // The packet buffer goes with the units: Spring's hand-over
        // gives the team's resources along with its units
        // (`CTeam::GiveEverythingTo`, `Sim/Misc/Team.cpp:216-233`), and
        // the buffer is the Network's resource. KP's buffer gadget has no
        // hand-over of its own (`network_buffer.lua` keeps `bufferSize`
        // per team, no `UnitGiven` / `TeamDied`), so there the packets
        // stayed with the dead team, out of anyone's reach.
        if let Some(buffer) = self.rules.buffer.as_mut() {
            let packets = buffer.take(from, u32::MAX);
            buffer.add(player, packets);
        }
    }

    /// A dropped player's peer is back (`SimCommand::Returned`,
    /// `net::rejoin`; the host sequences it after the seat's `Dropped`).
    /// The seat returns to play only if the drop changed nothing for
    /// good: its side still has an active owner (else its units were
    /// fused with the side's), it still owns units (nobody took them;
    /// an AI ally takes them within a second when no human ally is
    /// left), and the match is not decided. Then it is active again
    /// ([`TeamFates::rejoin`]): no longer resigned, its idle units its
    /// own to command, the TAKE offer gone. Otherwise the player
    /// watches, as after a resignation.
    ///
    /// Spring keeps a dropped player's team standing and lets the
    /// player reconnect to it (`CGameServer::BindConnection`,
    /// `Net/GameServer.cpp:2875-3053`), reviving a dead team
    /// (`CTeam::AddPlayer`, `Sim/Misc/Team.cpp:283`); here the user's
    /// leaving rules have already acted on the drop, so only a seat
    /// they left untouched comes back.
    ///
    /// [`TeamFates::rejoin`]: crate::units::teams::TeamFates::rejoin
    fn returned(&mut self, tick: u32, player: u8, cmd: &SimCommand) {
        let dropped = self
            .rules
            .control
            .as_ref()
            .is_some_and(|c| c.dropped.contains(&player));
        let decided = self
            .rules
            .fates
            .as_ref()
            .is_some_and(|f| f.winners().is_some());
        let side_in_play = match (self.alliances.as_deref(), self.rules.fates.as_deref()) {
            (Some(a), Some(f)) => a.allies_of(player).any(|o| o != player && !f.is_dead(o)),
            _ => true,
        };
        let owns = self
            .rules
            .owned_units
            .iter()
            .any(|(_, _, team, ..)| team.0 == player);
        let why = if !dropped {
            Some("did not drop")
        } else if decided {
            Some("the match is decided")
        } else if !side_in_play {
            Some("its side is out")
        } else if !owns {
            Some("its units were taken")
        } else {
            None
        };
        if let Some(why) = why {
            self.rules
                .refuse(tick, player, cmd, Refusal::NotReturnable(why));
            return;
        }
        if let Some(c) = self.rules.control.as_mut() {
            c.resigned.remove(&player);
            c.dropped.remove(&player);
        }
        if let Some(fates) = self.rules.fates.as_mut() {
            fates.rejoin(player);
        }
        info!("player {player} returned to play at tick {tick}");
    }

    /// The cheats other than `Spawn` (which needs the spawn machinery,
    /// [`dev_spawn`]).
    fn dev(&mut self, dev: &DevCommand) {
        match dev {
            DevCommand::Spawn { .. } => {}
            DevCommand::Kill { units } => {
                for id in units.ids() {
                    if let Some((e, ..)) = self.live(*id) {
                        self.commands.entity(e).try_insert(Dying { timer: 0.0 });
                    }
                }
            }
            DevCommand::SetHealth { units, hp } => {
                for id in units.ids() {
                    if let Some((e, ..)) = self.live(*id)
                        && let Ok(mut health) = self.rules.health.get_mut(e)
                    {
                        health.current = (*hp as f32).det_min(health.max);
                    }
                }
            }
            DevCommand::GodMode { on } => {
                if let Some(c) = self.rules.control.as_mut() {
                    c.god_mode = *on;
                }
            }
        }
    }

    fn order(
        &mut self,
        tick: u32,
        player: u8,
        sim_cmd: &SimCommand,
        ids: &[NetId],
        order: Order,
        queue: bool,
    ) {
        // (A point off the map refused the command in `check_shape`.)
        let mut units = self.owned(player, ids);
        // Factories keep move, fight, patrol and guard orders for the
        // units they build (`CFactoryCAI`'s new-unit commands; KP's
        // single right click is the engine's, so a plain move reaches
        // them too). They take no attack orders.
        let factories: Vec<Own> = units
            .iter()
            .copied()
            .filter(|u| self.takes_rally(u))
            .collect();
        units.retain(|u| !self.takes_rally(u));
        if !factories.is_empty() {
            let factory_cmd = match order {
                Order::Move(p) => Some(QueuedCommand::Move(p.to_vec3())),
                Order::AttackMove(p) => Some(QueuedCommand::AttackMove(p.to_vec3())),
                Order::Patrol(p) => Some(QueuedCommand::Patrol(p.to_vec3())),
                Order::Guard(t) => self
                    .live(t)
                    .map(|(target, pos, ..)| QueuedCommand::Guard { target, pos }),
                Order::AttackGround(_)
                | Order::AttackUnit(_)
                | Order::AttackArea { .. }
                | Order::ClearFeature(_)
                | Order::ClearArea { .. }
                | Order::Bombard { .. }
                | Order::Enter(_) => None,
            };
            if let Some(cmd) = factory_cmd {
                for f in &factories {
                    let guards_itself =
                        matches!(cmd, QueuedCommand::Guard { target, .. } if target == f.e);
                    if !guards_itself {
                        self.give_factory(f.e, cmd, queue);
                    }
                }
            }
        }
        let cmd = match order {
            Order::Move(p) => QueuedCommand::Move(p.to_vec3()),
            Order::AttackMove(p) => {
                units.retain(|u| self.mobile(u.kind));
                QueuedCommand::AttackMove(p.to_vec3())
            }
            Order::AttackGround(p) => {
                units.retain(|u| self.armed(u.kind));
                QueuedCommand::AttackGround(p.to_vec3())
            }
            Order::AttackUnit(t) => {
                let Some((target, pos, t_team, t_kind)) = self.live(t) else {
                    return;
                };
                let before = units.len();
                units.retain(|u| !self.friendly(u.team, t_team));
                if units.len() != before {
                    self.rules.refuse(tick, player, sim_cmd, Refusal::Friendly);
                }
                units.retain(|u| self.can_hit(u.kind, t_kind));
                QueuedCommand::AttackUnit { target, pos }
            }
            Order::Guard(t) => {
                let Some((target, pos, ..)) = self.live(t) else {
                    return;
                };
                units.retain(|u| self.mobile(u.kind) && u.e != target);
                QueuedCommand::Guard { target, pos }
            }
            Order::Patrol(p) => {
                units.retain(|u| self.mobile(u.kind));
                self.patrol(&units, p.to_vec3(), queue);
                return;
            }
            Order::AttackArea { center, radius } => {
                units.retain(|u| self.armed(u.kind));
                self.attack_area(&units, center.to_vec3(), radius.to_f32(), queue);
                return;
            }
            Order::ClearFeature(id) => {
                // A feature already gone (latency) is no refusal.
                let Some((feature, pos)) = self.feature(id) else {
                    return;
                };
                units.retain(|u| u.kind.is_constructor());
                QueuedCommand::ClearFeature { feature, pos }
            }
            Order::ClearArea { center, radius } => {
                units.retain(|u| u.kind.is_constructor());
                QueuedCommand::ClearArea {
                    center: center.to_vec3(),
                    radius: radius.to_f32(),
                }
            }
            Order::Bombard {
                target,
                deploy_only,
            } => {
                let (target, pos) = match target {
                    super::BombardTarget::Ground(p) => (None, p.to_vec3()),
                    super::BombardTarget::Unit(t) => {
                        let Some((target, pos, t_team, _)) = self.live(t) else {
                            return;
                        };
                        let before = units.len();
                        units.retain(|u| !self.friendly(u.team, t_team));
                        if units.len() != before {
                            self.rules.refuse(tick, player, sim_cmd, Refusal::Friendly);
                        }
                        (Some(target), pos)
                    }
                };
                units.retain(|u| u.kind == UnitKind::Bug);
                QueuedCommand::Bombard {
                    target,
                    pos,
                    deploy_only,
                }
            }
            Order::Enter(t) => {
                let Some((target, pos, t_team, t_kind)) = self.live(t) else {
                    return;
                };
                if !t_kind.is_teleporter() {
                    return;
                }
                // `network_enter.lua`: the Packet's own team's teleporter.
                units.retain(|u| u.kind == UnitKind::Packet && u.team == t_team);
                QueuedCommand::Enter { target, pos }
            }
        };
        for u in units {
            self.give(u.e, cmd, queue);
        }
    }

    /// A standing, clearable map feature: a clear target.
    fn feature(&self, id: NetId) -> Option<(Entity, Vec3)> {
        let e = self.index.get(id)?;
        let (feature, tf) = self.features.get(e).ok()?;
        feature.def.clearable.then_some((e, tf.translation))
    }

    /// `CSelectedUnitsHandlerAI::SelectAttackNet` for the circle form:
    /// every hostile unit in the circle that the issuing side can see,
    /// nearest the group's centre first (with Shift, the centre of the
    /// ends of their queues), becomes one attack per unit — the first
    /// replacing its orders unless Shift, the rest queued behind.
    fn attack_area(&mut self, units: &[Own], center: Vec3, radius: f32, queue: bool) {
        let Some(spatial) = self.spatial.as_deref() else {
            return;
        };
        if units.is_empty() {
            return;
        }
        // The group's centre, or with Shift where its queues end
        // (`LastQueuePosition`): the end of the queue, else the move
        // target, else the unit.
        let mid = units
            .iter()
            .map(|u| {
                let own = self.pos(u.e);
                if !queue {
                    return own;
                }
                let (q, mt, ..) = self.queues.get(u.e).unwrap_or_default();
                q.and_then(|q| q.commands.last().map(|c| c.position()))
                    .or(mt.map(|t| t.0))
                    .unwrap_or(own)
            })
            .sum::<Vec3>()
            / units.len() as f32;
        // Hidden from the issuers: a cloaked unit none of their teams
        // has detected (`SelectCircleUnits` takes LOS / radar).
        let hidden = |e: &crate::units::spatial::SpatialEntry| {
            units.iter().all(|u| {
                crate::units::mechanics::cloak::hidden_from(e.cloaked, e.detected_by, u.team)
            })
        };
        let friendly_to_all = |team: u8| units.iter().all(|u| self.friendly(u.team, team));
        let mut found: Vec<(f32, NetId, Entity, Vec3, UnitKind, u8)> = Vec::new();
        spatial.query_radius(center, radius, |e| {
            if e.feature
                || !e.hp_positive
                || crate::units::spatial::flat_dist_sq(e.pos, center) > radius * radius
                || friendly_to_all(e.team)
                || hidden(e)
            {
                return;
            }
            let Ok(id) = self.net_ids.get(e.entity) else {
                return;
            };
            found.push((
                crate::units::spatial::flat_dist_sq(e.pos, mid),
                *id,
                e.entity,
                e.pos,
                e.kind,
                e.team,
            ));
        });
        // Deterministic: distance, then the unit's id (the index is in
        // id order already; the tie-break makes it explicit).
        found.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        found.truncate(MAX_AREA_TARGETS);
        if found.is_empty() {
            return;
        }
        for u in units {
            let mut first = true;
            for &(_, _, target, pos, kind, team) in &found {
                if self.friendly(u.team, team) || !self.can_hit(u.kind, kind) {
                    continue;
                }
                let cmd = QueuedCommand::AttackUnit { target, pos };
                if first {
                    self.give(u.e, cmd, queue);
                } else {
                    // Behind the one just given: `Orders::busy` cannot
                    // see that deferred insert yet.
                    queue_command(u.e, cmd, &mut self.commands);
                }
                first = false;
            }
        }
    }

    /// `CMD_PATROL` (see `interaction::ability::patrol_commands`): a
    /// fresh patrol walks to the point with the start and the point
    /// queued as the route; Shift appends it behind the unit's orders.
    fn patrol(&mut self, units: &[Own], target: Vec3, queue: bool) {
        for &Own { e, .. } in units {
            let unit_pos = self.pos(e);
            let (q, move_target, ..) = self.queues.get(e).unwrap_or_default();
            let append = queue && self.orders.busy(e);
            let cmds = crate::interaction::ability::patrol_commands(
                append,
                q.map_or(&[][..], |q| &q.commands),
                move_target.map(|t| t.0),
                unit_pos,
                target,
            );
            for cmd in cmds {
                self.give(e, cmd, append);
            }
            if !append {
                self.commands.entity(e).insert(CommandQueue {
                    commands: vec![
                        QueuedCommand::Patrol(unit_pos),
                        QueuedCommand::Patrol(target),
                    ],
                });
            }
        }
    }

    /// Building placement (`ui::hud::placement`'s committing click):
    /// every capable builder gets the sites; with Shift each site is a
    /// queued command that may cancel the builder's own orders on that
    /// spot ([`queue_build`]). The vents taken are claimed.
    fn build(&mut self, player: u8, ids: &[NetId], kind: UnitKind, sites: &[Vec3], queue: bool) {
        let mut ordered: Vec<Vec3> = Vec::new();
        for Own {
            e, kind: builder, ..
        } in self.owned(player, ids)
        {
            if !buildings_for(builder).contains(&kind) {
                continue;
            }
            if queue && self.orders.busy(e) {
                let (q, _, pending, constructing) = self.queues.get(e).unwrap_or_default();
                let running = pending
                    .map(|p| (p.kind, p.site))
                    .or(constructing.map(|c| (c.kind, c.site)));
                let mut active = running;
                let mut queued = q.map(|q| q.commands.clone()).unwrap_or_default();
                for &site in sites {
                    let outcome = queue_build(&mut active, &mut queued, kind, site, |k| {
                        self.registry
                            .as_ref()
                            .map_or(Vec2::splat(32.0), |r| r.footprint_elmos(k))
                    });
                    if outcome == QueuedBuild::Queued {
                        ordered.push(site);
                    }
                }
                let mut ec = self.commands.entity(e);
                if running.is_some() && active.is_none() {
                    crate::interaction::finish_active_order(&mut ec);
                }
                ec.insert(CommandQueue { commands: queued });
                continue;
            }
            let mut orders = sites
                .iter()
                .map(|&site| QueuedCommand::BuildAt { kind, site });
            if let Some(first) = orders.next() {
                self.give(e, first, false);
            }
            let rest: Vec<QueuedCommand> = orders.collect();
            if !rest.is_empty() {
                self.commands
                    .entity(e)
                    .insert(CommandQueue { commands: rest });
            }
            ordered.extend_from_slice(sites);
        }
        // Claim the vents so no second constructor can stack on them.
        for (vent_entity, vent) in &self.vents {
            if ordered.iter().any(|s| vent.pos.distance_squared(*s) < 1.0) {
                self.commands.entity(vent_entity).insert(VentClaim);
            }
        }
    }
}

/// Sorted, unique, no id 0: what [`IdList::new`](super::IdList::new) produces.
fn canonical(ids: &[NetId]) -> bool {
    ids.first().is_none_or(|id| id.0 != 0) && ids.windows(2).all(|w| w[0] < w[1])
}

/// A short name for logs.
fn command_name(cmd: &SimCommand) -> &'static str {
    match cmd {
        SimCommand::Order { .. } => "Order",
        SimCommand::PerUnit { .. } => "PerUnit",
        SimCommand::Path { .. } => "Path",
        SimCommand::Stop { .. } => "Stop",
        SimCommand::Build { .. } => "Build",
        SimCommand::Factory { .. } => "Factory",
        SimCommand::SetRepeat { .. } => "SetRepeat",
        SimCommand::SetAutoHold { .. } => "SetAutoHold",
        SimCommand::SetTarget { .. } => "SetTarget",
        SimCommand::UnsetTarget { .. } => "UnsetTarget",
        SimCommand::SelfDestruct { .. } => "SelfDestruct",
        SimCommand::Deploy { .. } => "Deploy",
        SimCommand::Enter { .. } => "Enter",
        SimCommand::Cast { .. } => "Cast",
        SimCommand::SetSpeed { .. } => "SetSpeed",
        SimCommand::Pause { .. } => "Pause",
        SimCommand::Resign => "Resign",
        SimCommand::Dropped => "Dropped",
        SimCommand::InternalSpeed { .. } => "InternalSpeed",
        SimCommand::Returned => "Returned",
        SimCommand::Take { .. } => "Take",
        SimCommand::Dev(_) => "Dev",
    }
}

/// A factory build-option click on one factory (`CFactoryCAI`).
pub(crate) fn apply_factory_op(producer: &mut Producer, kind: UnitKind, op: FactoryOp) {
    match op {
        FactoryOp::Add(n) => {
            for _ in 0..n {
                producer.enqueue(kind);
            }
        }
        FactoryOp::AddFront(n) => producer.enqueue_front(kind, n as u32),
        FactoryOp::Remove { count, oldest } => producer.remove_kind(kind, count as u32, oldest),
    }
}

/// [`DevCommand::Spawn`]'s parameters.
struct DevSpawn {
    kind: UnitKind,
    team: u8,
    at: NetPos,
    count: u16,
    spacing: u16,
}

/// `/give`: the units on a square grid, the team's seat faction (the
/// kind's own for a team without a seat). The local dev tool learns the
/// entities through [`super::control::DevSpawned`].
fn dev_spawn(
    In(spawn): In<DevSpawn>,
    mut ctx: crate::units::lifecycle::spawning::SpawnContext,
    heightmap: Option<Res<Heightmap>>,
    active: Option<Res<ActiveMatch>>,
    mut spawned: Option<ResMut<super::control::DevSpawned>>,
) {
    let faction = active
        .as_ref()
        .and_then(|m| m.0.players.get(usize::from(spawn.team)))
        .map_or(spawn.kind.faction(), |p| p.faction);
    let at = spawn.at.to_vec3();
    let spacing = f32::from(spawn.spacing);
    let count = usize::from(spawn.count);
    let cols = (1..=count).find(|c| c * c >= count).unwrap_or(1);
    for i in 0..count {
        let (cx, cz) = ((i % cols) as f32, (i / cols) as f32);
        let (x, z) = (at.x + cx * spacing, at.z + cz * spacing);
        let pos = heightmap
            .as_ref()
            .map_or(Vec3::new(x, at.y, z), |h| h.place(x, z));
        let e = crate::units::lifecycle::spawning::spawn_unit(
            spawn.kind, faction, spawn.team, pos, &mut ctx,
        );
        if let Some(s) = spawned.as_mut() {
            s.0.push(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_setup::{GameSetup, PlayerSpec};
    use crate::net::{CommandFrame, IdList, PosList, Units};
    use crate::units::combat::AttackTargetOrder;
    use crate::units::components::Faction;

    fn world() -> World {
        let mut world = World::new();
        crate::net::init_test_world(&mut world);
        world.init_resource::<Refusals>();
        world
    }

    fn unit(world: &mut World, kind: UnitKind, team: u8, x: f32) -> (Entity, NetId) {
        let e = world
            .spawn((
                UnitType(kind),
                TeamId(team),
                Transform::from_xyz(x, 0.0, 100.0),
                Health::full(100.0),
            ))
            .id();
        (e, *world.get::<NetId>(e).unwrap())
    }

    /// Deliver `commands` from `player` for the coming tick and apply.
    fn send(world: &mut World, player: u8, commands: Vec<SimCommand>) {
        let mut bus = world.resource_mut::<CommandBus>();
        let tick = bus.tick;
        bus.deliver(CommandFrame {
            tick,
            player,
            commands,
        });
        crate::net::apply_now(world);
    }

    fn move_to(units: Units, x: f32) -> SimCommand {
        SimCommand::Order {
            units,
            order: Order::Move(NetPos::quantize(Vec3::new(x, 0.0, 100.0))),
            queue: false,
        }
    }

    fn refused(world: &World) -> u64 {
        world.resource::<Refusals>().count
    }

    /// Four seats, teams 0..4, the first `humans` of them human.
    fn seats(world: &mut World, humans: u8) {
        let players = (0..4)
            .map(|seat| PlayerSpec {
                faction: Faction::System,
                ally_team: seat,
                ai: seat >= humans,
            })
            .collect();
        world.insert_resource(ActiveMatch(GameSetup {
            players,
            ..default()
        }));
    }

    /// `NETMSG_SELECT`'s filter: dead, unknown and other teams' units
    /// drop out silently; the sender's own still take the order.
    #[test]
    fn only_own_live_units_obey() {
        let mut world = world();
        let (mine, a) = unit(&mut world, UnitKind::Bit, 0, 100.0);
        let (theirs, b) = unit(&mut world, UnitKind::Bit, 1, 200.0);
        let (dead, c) = unit(&mut world, UnitKind::Bit, 0, 300.0);
        world.entity_mut(dead).insert(Dying { timer: 1.0 });
        send(
            &mut world,
            0,
            vec![move_to(Units::of([a, b, c, NetId(999)]), 50.0)],
        );
        assert!(world.get::<MoveTarget>(mine).is_some());
        assert!(world.get::<MoveTarget>(theirs).is_none());
        assert!(world.get::<MoveTarget>(dead).is_none());
        assert_eq!(refused(&world), 0, "latency, not a refusal");
    }

    /// Senders without a seat are refused; a resignation kills the
    /// owner and refuses what the player sends after it, even later in
    /// the same frame.
    #[test]
    fn unseated_and_resigned_players_are_refused() {
        use crate::units::teams::TeamFates;
        let mut world = world();
        world.init_resource::<TeamFates>();
        seats(&mut world, 1);
        let (mine, a) = unit(&mut world, UnitKind::Bit, 0, 100.0);
        send(&mut world, 7, vec![move_to(Units::of([a]), 50.0)]);
        assert!(world.get::<MoveTarget>(mine).is_none());
        assert_eq!(refused(&world), 1);

        send(
            &mut world,
            0,
            vec![SimCommand::Resign, move_to(Units::of([a]), 50.0)],
        );
        let fates = world.resource::<TeamFates>();
        assert!(fates.is_dead(0) && !fates.is_dead(1));
        assert!(world.get::<MoveTarget>(mine).is_none());
        assert!(world.resource::<MatchControl>().resigned.contains(&0));
        assert_eq!(refused(&world), 2);
    }

    /// Lists not sorted and unique, per-unit lists of the wrong length,
    /// counts and speeds out of range: refused whole. The `Units::Prev`
    /// memory follows the sender's: a `Prev` after a malformed list is
    /// malformed too.
    #[test]
    fn malformed_commands_are_refused_whole() {
        let mut world = world();
        let (e, a) = unit(&mut world, UnitKind::Bit, 0, 100.0);
        let (_, b) = unit(&mut world, UnitKind::Bit, 0, 120.0);
        send(&mut world, 0, vec![move_to(Units::of([a]), 50.0)]);
        world.entity_mut(e).remove::<MoveTarget>();
        send(
            &mut world,
            0,
            vec![
                move_to(Units::List(IdList::raw(vec![b, a])), 60.0),
                move_to(Units::List(IdList::raw(vec![a, a])), 60.0),
                move_to(Units::List(IdList::raw(vec![NetId(0), a])), 60.0),
                SimCommand::PerUnit {
                    order: PointOrder::Move,
                    units: IdList::new([a, b]),
                    targets: PosList::quantize([Vec3::ZERO]),
                    queue: false,
                },
                SimCommand::Factory {
                    factories: Units::of([a]),
                    kind: UnitKind::Bit,
                    op: FactoryOp::Add(MAX_FACTORY_COUNT + 1),
                },
                SimCommand::SetSpeed { percent: 5 },
                SimCommand::SetSpeed { percent: 5000 },
            ],
        );
        assert_eq!(refused(&world), 7);
        assert!(world.get::<MoveTarget>(e).is_none());
        assert_eq!(world.resource::<MatchControl>().speed_percent, 100);
        // `Prev` names the last list sent, `[a]` from the `Factory`.
        send(&mut world, 0, vec![move_to(Units::Prev, 70.0)]);
        assert_eq!(world.get::<MoveTarget>(e).map(|t| t.0.x), Some(70.0));
        send(
            &mut world,
            0,
            vec![
                move_to(Units::List(IdList::raw(vec![a, a])), 80.0),
                move_to(Units::Prev, 80.0),
            ],
        );
        assert_eq!(refused(&world), 9);
        assert_eq!(world.get::<MoveTarget>(e).map(|t| t.0.x), Some(70.0));
    }

    /// `IsCommandInMap`: an order to a point off the map is refused;
    /// per-unit targets and path waypoints off it drop alone.
    #[test]
    fn points_must_lie_on_the_map() {
        let mut world = world();
        // 64 squares of 8 elmos: 512 × 512.
        world.insert_resource(Heightmap::from_raw(vec![0.0; 65 * 65], 65, 65));
        let (e1, a) = unit(&mut world, UnitKind::Bit, 0, 100.0);
        let (e2, b) = unit(&mut world, UnitKind::Bit, 0, 120.0);
        send(&mut world, 0, vec![move_to(Units::of([a, b]), -8.0)]);
        assert!(world.get::<MoveTarget>(e1).is_none());
        assert_eq!(refused(&world), 1);
        send(
            &mut world,
            0,
            vec![SimCommand::per_unit(
                PointOrder::Move,
                vec![
                    (a, Vec3::new(600.0, 0.0, 10.0)),
                    (b, Vec3::new(500.0, 0.0, 10.0)),
                ],
                false,
            )],
        );
        assert!(world.get::<MoveTarget>(e1).is_none());
        assert_eq!(world.get::<MoveTarget>(e2).map(|t| t.0.x), Some(500.0));
        send(
            &mut world,
            0,
            vec![SimCommand::Path {
                unit: a,
                waypoints: PosList::quantize([
                    Vec3::new(10.0, 0.0, 10.0),
                    Vec3::new(10.0, 0.0, 900.0),
                    Vec3::new(20.0, 0.0, 10.0),
                ]),
                queue: false,
            }],
        );
        assert_eq!(world.get::<MoveTarget>(e1).map(|t| t.0.x), Some(10.0));
        let queue = &world.get::<CommandQueue>(e1).unwrap().commands;
        assert!(matches!(queue[..], [QueuedCommand::Move(p)] if p.x == 20.0));
    }

    /// A vent building's site must be a vent.
    #[test]
    fn vent_buildings_need_a_vent() {
        let mut world = world();
        let (builder, a) = unit(&mut world, UnitKind::Assembler, 0, 100.0);
        let vent = Vec3::new(160.0, 0.0, 160.0);
        world.spawn(GeoventSmoker {
            facing: 0,
            pos: vent,
            emit_timer: 0.0,
            rng: 0,
        });
        let build = |site: Vec3| SimCommand::Build {
            builders: Units::of([a]),
            kind: UnitKind::Socket,
            sites: PosList::quantize([site]),
            queue: false,
        };
        send(&mut world, 0, vec![build(vent + Vec3::X * 40.0)]);
        assert!(world.get::<PendingBuild>(builder).is_none());
        assert_eq!(refused(&world), 1);
        send(&mut world, 0, vec![build(vent)]);
        assert_eq!(
            world.get::<PendingBuild>(builder).map(|p| p.site),
            Some(vent)
        );
    }

    /// Attacks need a weapon that can hit the target, and an enemy.
    #[test]
    fn attacks_need_a_weapon_and_an_enemy() {
        let mut world = world();
        world.insert_resource(UnitRegistry::load());
        let (bit, a) = unit(&mut world, UnitKind::Bit, 0, 100.0);
        let (builder, b) = unit(&mut world, UnitKind::Assembler, 0, 120.0);
        let (_, own) = unit(&mut world, UnitKind::Byte, 0, 140.0);
        let (_, enemy) = unit(&mut world, UnitKind::Bit, 1, 300.0);
        let attack = |target| SimCommand::Order {
            units: Units::of([a, b]),
            order: Order::AttackUnit(target),
            queue: false,
        };
        send(&mut world, 0, vec![attack(own)]);
        assert!(world.get::<AttackTargetOrder>(bit).is_none());
        assert_eq!(refused(&world), 1);
        send(&mut world, 0, vec![attack(enemy)]);
        assert!(world.get::<AttackTargetOrder>(bit).is_some());
        assert!(world.get::<AttackTargetOrder>(builder).is_none());
    }

    /// One spatial entry per unit, as the index would hold it.
    fn index_unit(world: &mut World, e: Entity, cloaked: bool, feature: bool) {
        let (team, kind, pos) = {
            let team = world.get::<TeamId>(e).map_or(0, |t| t.0);
            let kind = world.get::<UnitType>(e).map_or(UnitKind::BadBlock, |u| u.0);
            let pos = world.get::<Transform>(e).unwrap().translation;
            (team, kind, pos)
        };
        world
            .get_resource_or_init::<crate::units::spatial::SpatialIndex>()
            .insert_for_test(crate::units::spatial::SpatialEntry {
                entity: e,
                pos,
                hit_radius: 10.0,
                mid_y: 0.0,
                up: Vec3::Y,
                team,
                kind,
                hp_positive: true,
                is_flying: false,
                cloaked,
                detected_by: 0,
                feature,
            });
    }

    fn attack_area(units: Units, x: f32, radius: f32, queue: bool) -> SimCommand {
        SimCommand::Order {
            units,
            order: Order::AttackArea {
                center: NetPos::quantize(Vec3::new(x, 0.0, 100.0)),
                radius: crate::net::NetLen::quantize(radius),
            },
            queue,
        }
    }

    fn queued(world: &World, e: Entity) -> Vec<QueuedCommand> {
        world
            .get::<CommandQueue>(e)
            .map_or(Vec::new(), |q| q.commands.clone())
    }

    /// `SelectAttackNet`: an area attack becomes one attack per hostile
    /// in the circle, nearest the group first, the first replacing the
    /// orders and the rest queued; a friend, an undetected cloaked unit,
    /// a feature and anything outside the circle are left out.
    #[test]
    fn area_attack_expands_to_sorted_targets() {
        let mut world = world();
        world.insert_resource(UnitRegistry::load());
        let (bit, a) = unit(&mut world, UnitKind::Bit, 0, 100.0);
        let (near, _) = unit(&mut world, UnitKind::Bit, 1, 300.0);
        let (far, _) = unit(&mut world, UnitKind::Bit, 1, 500.0);
        let (friend, _) = unit(&mut world, UnitKind::Bit, 0, 350.0);
        let (ghost, _) = unit(&mut world, UnitKind::LogicBomb, 1, 320.0);
        let (away, _) = unit(&mut world, UnitKind::Bit, 1, 2000.0);
        let (block, _) = unit(&mut world, UnitKind::BadBlock, 1, 330.0);
        for (e, cloaked, feature) in [
            (bit, false, false),
            (near, false, false),
            (far, false, false),
            (friend, false, false),
            (ghost, true, false),
            (away, false, false),
            (block, false, true),
        ] {
            index_unit(&mut world, e, cloaked, feature);
        }
        send(
            &mut world,
            0,
            vec![attack_area(Units::of([a]), 400.0, 300.0, false)],
        );
        assert_eq!(
            world.get::<AttackTargetOrder>(bit).map(|o| o.target),
            Some(near),
            "the nearest first"
        );
        assert_eq!(
            queued(&world, bit),
            vec![QueuedCommand::AttackUnit {
                target: far,
                pos: Vec3::new(500.0, 0.0, 100.0),
            }]
        );
        assert_eq!(refused(&world), 0);

        // Shift: behind the current orders, sorted from where they end.
        let mut world = self::tests::world();
        world.insert_resource(UnitRegistry::load());
        let (bit, a) = unit(&mut world, UnitKind::Bit, 0, 100.0);
        let (near, _) = unit(&mut world, UnitKind::Bit, 1, 300.0);
        let (far, _) = unit(&mut world, UnitKind::Bit, 1, 500.0);
        for e in [bit, near, far] {
            index_unit(&mut world, e, false, false);
        }
        world
            .entity_mut(bit)
            .insert(MoveTarget(Vec3::new(600.0, 0.0, 100.0)));
        send(
            &mut world,
            0,
            vec![attack_area(Units::of([a]), 400.0, 300.0, true)],
        );
        assert!(world.get::<MoveTarget>(bit).is_some(), "the move stays");
        assert_eq!(
            queued(&world, bit)
                .iter()
                .map(|c| match c {
                    QueuedCommand::AttackUnit { target, .. } => *target,
                    other => panic!("{other:?}"),
                })
                .collect::<Vec<_>>(),
            vec![far, near],
            "from the end of the queue, 500 is nearer than 300"
        );
    }

    /// An area order needs a radius inside the map's measure.
    #[test]
    fn area_orders_refuse_a_bad_radius() {
        let mut world = world();
        let (bit, a) = unit(&mut world, UnitKind::Bit, 0, 100.0);
        send(
            &mut world,
            0,
            vec![attack_area(Units::of([a]), 400.0, 0.0, false)],
        );
        assert_eq!(refused(&world), 1);
        send(
            &mut world,
            0,
            vec![attack_area(
                Units::of([a]),
                400.0,
                MAX_AREA_RADIUS * 2.0,
                false,
            )],
        );
        assert_eq!(refused(&world), 2);
        assert!(world.get::<AttackTargetOrder>(bit).is_none());
    }

    /// A standing clearable feature, as the clear orders look it up.
    fn block(world: &mut World, x: f32) -> (Entity, NetId) {
        let def = crate::units::content::defs::defs()
            .features
            .get("badblock")
            .unwrap();
        let e = world
            .spawn((
                crate::units::features::Feature { def, owner: None },
                NetId::default(),
                Health::full(def.health),
                Transform::from_xyz(x, 0.0, 100.0),
            ))
            .id();
        (e, *world.get::<NetId>(e).unwrap())
    }

    /// Clear orders reach constructors only; a feature already gone is
    /// no refusal, a radius of nothing is.
    #[test]
    fn clear_orders_go_to_constructors() {
        use crate::units::lifecycle::construction::ClearOrder;
        let mut world = world();
        let (builder, a) = unit(&mut world, UnitKind::Assembler, 0, 100.0);
        let (bit, b) = unit(&mut world, UnitKind::Bit, 0, 120.0);
        let (block, id) = block(&mut world, 300.0);
        let clear = SimCommand::Order {
            units: Units::of([a, b]),
            order: Order::ClearFeature(id),
            queue: false,
        };
        send(&mut world, 0, vec![clear.clone()]);
        assert_eq!(
            world.get::<ClearOrder>(builder),
            Some(&ClearOrder::Feature { feature: block })
        );
        assert!(world.get::<ClearOrder>(bit).is_none());
        assert_eq!(refused(&world), 0);

        world.entity_mut(builder).remove::<ClearOrder>();
        world.despawn(block);
        send(&mut world, 0, vec![clear]);
        assert!(world.get::<ClearOrder>(builder).is_none());
        assert_eq!(refused(&world), 0, "gone: latency, not a refusal");

        let area = |radius: f32| SimCommand::Order {
            units: Units::of([a, b]),
            order: Order::ClearArea {
                center: NetPos::quantize(Vec3::new(300.0, 0.0, 100.0)),
                radius: crate::net::NetLen::quantize(radius),
            },
            queue: false,
        };
        send(&mut world, 0, vec![area(0.0)]);
        assert_eq!(refused(&world), 1);
        send(&mut world, 0, vec![area(200.0)]);
        assert!(matches!(
            world.get::<ClearOrder>(builder),
            Some(ClearOrder::Area { current: None, .. })
        ));
        assert!(world.get::<ClearOrder>(bit).is_none());
    }

    /// Bombard reaches Bugs only, refuses a friendly target like an
    /// attack, and takes a ground point.
    #[test]
    fn bombard_reaches_bugs_only() {
        use crate::net::BombardTarget;
        use crate::units::mechanics::deploy::BombardOrder;
        let mut world = world();
        let (bug, a) = unit(&mut world, UnitKind::Bug, 0, 100.0);
        let (bit, b) = unit(&mut world, UnitKind::Bit, 0, 120.0);
        let (enemy, e) = unit(&mut world, UnitKind::Bit, 1, 900.0);
        let (_, own) = unit(&mut world, UnitKind::Byte, 0, 140.0);
        let bombard = |target, deploy_only| SimCommand::Order {
            units: Units::of([a, b]),
            order: Order::Bombard {
                target,
                deploy_only,
            },
            queue: false,
        };
        send(
            &mut world,
            0,
            vec![bombard(BombardTarget::Unit(own), false)],
        );
        assert!(world.get::<BombardOrder>(bug).is_none());
        assert_eq!(refused(&world), 1);
        send(&mut world, 0, vec![bombard(BombardTarget::Unit(e), false)]);
        let order = world.get::<BombardOrder>(bug).expect("the Bug takes it");
        assert_eq!(order.target, Some(enemy));
        assert!(!order.deploy_only);
        assert!(world.get::<BombardOrder>(bit).is_none());
        let ground = NetPos::quantize(Vec3::new(700.0, 0.0, 100.0));
        send(
            &mut world,
            0,
            vec![bombard(BombardTarget::Ground(ground), true)],
        );
        let order = world.get::<BombardOrder>(bug).unwrap();
        assert_eq!((order.target, order.pos), (None, ground.to_vec3()));
        assert!(order.deploy_only);
    }

    /// An Enter order sends Packets to their own side's teleporter and
    /// nobody anywhere else.
    #[test]
    fn enter_orders_walk_packets_to_their_own_teleporter() {
        use crate::units::mechanics::network_buffer::EnterOrder;
        let mut world = world();
        let (packet, a) = unit(&mut world, UnitKind::Packet, 0, 100.0);
        let (bit, b) = unit(&mut world, UnitKind::Bit, 0, 120.0);
        let (port, mine) = unit(&mut world, UnitKind::Port, 0, 600.0);
        let (_, theirs) = unit(&mut world, UnitKind::Port, 1, 700.0);
        let enter = |target| SimCommand::Order {
            units: Units::of([a, b]),
            order: Order::Enter(target),
            queue: false,
        };
        send(&mut world, 0, vec![enter(theirs)]);
        assert!(world.get::<EnterOrder>(packet).is_none());
        send(&mut world, 0, vec![enter(b)]);
        assert!(
            world.get::<EnterOrder>(packet).is_none(),
            "a Bit is no door"
        );
        send(&mut world, 0, vec![enter(mine)]);
        assert_eq!(
            world.get::<EnterOrder>(packet).map(|o| o.target),
            Some(port)
        );
        assert!(world.get::<EnterOrder>(bit).is_none());
        assert_eq!(refused(&world), 0);
    }

    /// Cheats only in a match with at most one human; god mode lets the
    /// player command every team.
    #[test]
    fn cheats_need_a_single_player_match() {
        let mut world = world();
        seats(&mut world, 2);
        let (theirs, b) = unit(&mut world, UnitKind::Bit, 1, 200.0);
        let kill = SimCommand::Dev(DevCommand::Kill {
            units: IdList::new([b]),
        });
        send(&mut world, 0, vec![kill.clone()]);
        assert!(world.get::<Dying>(theirs).is_none());
        assert_eq!(refused(&world), 1);

        seats(&mut world, 1);
        send(&mut world, 0, vec![move_to(Units::of([b]), 50.0)]);
        assert!(world.get::<MoveTarget>(theirs).is_none());
        send(
            &mut world,
            0,
            vec![
                SimCommand::Dev(DevCommand::GodMode { on: true }),
                move_to(Units::of([b]), 50.0),
                SimCommand::Dev(DevCommand::SetHealth {
                    units: IdList::new([b]),
                    hp: 40,
                }),
            ],
        );
        assert!(world.get::<MoveTarget>(theirs).is_some());
        assert_eq!(world.get::<Health>(theirs).unwrap().current, 40.0);
        send(&mut world, 0, vec![kill]);
        assert!(world.get::<Dying>(theirs).is_some());
    }

    /// `Take` (the user's rule, 2026-10-15): all of a resigned ally's
    /// units, with their tallies; the cap doesn't move, so the taker may
    /// end up over it. Refused: from an enemy, from oneself, from an
    /// owner still in play, and a second time (nothing left).
    #[test]
    fn take_moves_every_unit_and_its_tally() {
        use crate::units::lifecycle::bookkeeping::{
            MAX_UNITS_PER_TEAM, TeamUnitCounts, track_added_units,
        };
        use crate::units::teams::{Alliances, TeamFates};
        use bevy::ecs::system::RunSystemOnce;
        let mut world = world();
        let setup = GameSetup {
            players: [0, 0, 1]
                .into_iter()
                .map(|ally_team| PlayerSpec {
                    faction: Faction::System,
                    ally_team,
                    ai: false,
                })
                .collect(),
            ..default()
        };
        world.insert_resource(Alliances::from_setup(&setup));
        world.insert_resource(ActiveMatch(setup));
        world.init_resource::<TeamFates>();
        world.init_resource::<TeamUnitCounts>();
        let n = MAX_UNITS_PER_TEAM * 3 / 5;
        let theirs: Vec<Entity> = (0..n)
            .map(|i| unit(&mut world, UnitKind::Bit, 1, i as f32).0)
            .collect();
        for i in 0..n {
            unit(&mut world, UnitKind::Bit, 0, i as f32);
        }
        world.run_system_once(track_added_units).unwrap();
        world.flush();
        let counts = |w: &World| {
            let c = w.resource::<TeamUnitCounts>();
            (c.get(0), c.get(1))
        };
        assert_eq!(counts(&world), (n, n));

        let take = SimCommand::Take { from: 1 };
        send(&mut world, 0, vec![take.clone()]);
        assert_eq!(refused(&world), 1, "owner 1 is still in play");
        world.resource_mut::<TeamFates>().leave(1);
        send(&mut world, 2, vec![take.clone()]);
        send(&mut world, 1, vec![SimCommand::Take { from: 1 }]);
        assert_eq!(refused(&world), 3, "an enemy can't, a leaver can't");
        send(&mut world, 0, vec![take.clone(), take]);
        assert!(
            theirs
                .iter()
                .all(|e| world.get::<TeamId>(*e).unwrap().0 == 0)
        );
        assert_eq!(refused(&world), 4, "the second take finds nothing");
        assert_eq!(counts(&world), (2 * n, 0));
        let c = world.resource::<TeamUnitCounts>();
        assert_eq!(c.limit(0), MAX_UNITS_PER_TEAM);
        assert!(c.at_limit(0), "over the cap: its factories wait");
    }

    /// The host's `MatchRules`, checked where commands apply: pausing
    /// off (the host's seat still may, as Spring's host), the speed
    /// range, cheats for a match with several humans.
    #[test]
    fn match_rules_bound_pause_speed_and_cheats() {
        use crate::net::MatchRules;
        let mut world = world();
        seats(&mut world, 2);
        world.resource_mut::<ActiveMatch>().0.rules = MatchRules {
            pausable: false,
            min_speed: 50,
            max_speed: 200,
            cheats: false,
            host: Some(0),
        };
        send(&mut world, 1, vec![SimCommand::Pause { on: true }]);
        assert!(!world.resource::<MatchControl>().paused);
        assert_eq!(
            world.resource::<Refusals>().last.map(|l| l.2),
            Some(Refusal::Rules("pausing is off"))
        );
        send(&mut world, 0, vec![SimCommand::Pause { on: true }]);
        assert!(world.resource::<MatchControl>().paused, "the host may");
        send(
            &mut world,
            1,
            vec![
                SimCommand::SetSpeed { percent: 300 },
                SimCommand::SetSpeed { percent: 40 },
                SimCommand::SetSpeed { percent: 200 },
            ],
        );
        assert_eq!(world.resource::<MatchControl>().speed_percent, 200);
        assert_eq!(refused(&world), 3);

        let (theirs, b) = unit(&mut world, UnitKind::Bit, 1, 200.0);
        let kill = SimCommand::Dev(DevCommand::Kill {
            units: IdList::new([b]),
        });
        send(&mut world, 0, vec![kill.clone()]);
        assert!(
            world.get::<Dying>(theirs).is_none(),
            "two humans: no cheats"
        );
        world.resource_mut::<ActiveMatch>().0.rules.cheats = true;
        send(&mut world, 0, vec![kill]);
        assert!(
            world.get::<Dying>(theirs).is_some(),
            "the host allowed them"
        );
        // The default rules are single player's: everything goes.
        let solo = Some(&ActiveMatch(GameSetup::default()));
        for cmd in [
            SimCommand::Pause { on: true },
            SimCommand::SetSpeed { percent: 10 },
            SimCommand::SetSpeed { percent: 2000 },
            SimCommand::Dev(DevCommand::GodMode { on: true }),
        ] {
            assert_eq!(rules_admit(solo, 0, &cmd), Ok(()), "{cmd:?}");
        }
    }

    /// A peer that dropped out leaves play like a resignation, marked as
    /// dropped; a drop after the player resigned ("Leave match") is no
    /// refusal.
    #[test]
    fn dropped_players_leave_play() {
        use crate::units::teams::TeamFates;
        let mut world = world();
        world.init_resource::<TeamFates>();
        seats(&mut world, 3);
        send(&mut world, 1, vec![SimCommand::Dropped]);
        assert!(world.resource::<TeamFates>().is_dead(1));
        let c = world.resource::<MatchControl>();
        assert!(c.dropped.contains(&1) && c.resigned.contains(&1));
        send(&mut world, 2, vec![SimCommand::Resign]);
        send(&mut world, 2, vec![SimCommand::Dropped]);
        assert!(!world.resource::<MatchControl>().dropped.contains(&2));
        assert_eq!(refused(&world), 0);
    }

    /// M3: a dropped player's `Returned` puts its seat back in play while
    /// its ally plays on and its units are its own; not once they were
    /// taken, nor for a seat that resigned or never left.
    #[test]
    fn returned_players_come_back_if_nothing_took_their_seat() {
        use crate::units::teams::{Alliances, TeamFates};
        let mut world = world();
        // Owners 0, 1 allied against 2.
        let setup = GameSetup {
            players: [0, 0, 1]
                .into_iter()
                .map(|ally_team| PlayerSpec {
                    faction: Faction::System,
                    ally_team,
                    ai: false,
                })
                .collect(),
            ..default()
        };
        world.insert_resource(Alliances::from_setup(&setup));
        world.insert_resource(ActiveMatch(setup));
        world.init_resource::<TeamFates>();
        for owner in 0..3 {
            unit(&mut world, UnitKind::Bit, owner, f32::from(owner));
        }
        world.flush();
        send(&mut world, 0, vec![SimCommand::Returned]);
        assert_eq!(refused(&world), 1, "never left");
        send(&mut world, 1, vec![SimCommand::Dropped]);
        send(&mut world, 1, vec![SimCommand::Returned]);
        assert_eq!(refused(&world), 1, "back");
        assert!(!world.resource::<TeamFates>().is_dead(1));
        let c = world.resource::<MatchControl>();
        assert!(!c.dropped.contains(&1) && !c.resigned.contains(&1));
        // Its orders count again.
        let (_, id) = unit(&mut world, UnitKind::Bit, 1, 9.0);
        world.flush();
        send(
            &mut world,
            1,
            vec![SimCommand::Stop {
                units: Units::of([id]),
            }],
        );
        assert_eq!(refused(&world), 1);
        // Dropped again, and its units taken: it watches.
        send(&mut world, 1, vec![SimCommand::Dropped]);
        send(&mut world, 0, vec![SimCommand::Take { from: 1 }]);
        world.flush();
        send(&mut world, 1, vec![SimCommand::Returned]);
        assert_eq!(refused(&world), 2, "its units were taken");
        assert!(world.resource::<TeamFates>().is_dead(1));
        // A resignation is for good.
        send(&mut world, 2, vec![SimCommand::Resign]);
        send(&mut world, 2, vec![SimCommand::Returned]);
        assert_eq!(refused(&world), 3, "did not drop");
    }

    /// A `Take` hands the leaver's packet buffer over with its units
    /// (Spring's `GiveEverythingTo` gives the resources too).
    #[test]
    fn take_hands_over_the_packet_buffer() {
        use crate::units::mechanics::network_buffer::PacketBuffer;
        use crate::units::teams::{Alliances, TeamFates};
        let mut world = world();
        let setup = GameSetup {
            players: [0, 0]
                .into_iter()
                .map(|ally_team| PlayerSpec {
                    faction: Faction::Network,
                    ally_team,
                    ai: false,
                })
                .collect(),
            ..default()
        };
        world.insert_resource(Alliances::from_setup(&setup));
        world.insert_resource(ActiveMatch(setup));
        world.init_resource::<TeamFates>();
        let mut buffer = PacketBuffer::default();
        buffer.add(0, 3);
        buffer.add(1, 17);
        world.insert_resource(buffer);
        unit(&mut world, UnitKind::Bit, 1, 100.0);
        world.resource_mut::<TeamFates>().leave(1);
        send(&mut world, 0, vec![SimCommand::Take { from: 1 }]);
        let b = world.resource::<PacketBuffer>();
        assert_eq!((b.get(0), b.get(1)), (20, 0));
    }

    /// Speed and pause land in the match control.
    #[test]
    fn speed_and_pause_set_the_match_control() {
        let mut world = world();
        send(
            &mut world,
            1,
            vec![
                SimCommand::SetSpeed { percent: 250 },
                SimCommand::Pause { on: true },
            ],
        );
        let control = world.resource::<MatchControl>();
        assert_eq!(control.speed_percent, 250);
        assert!(control.paused);
        assert_eq!(control.paused_by, Some(1));
    }

    fn factory(world: &mut World, kind: UnitKind, x: f32) -> (Entity, NetId) {
        let (e, id) = unit(world, kind, 0, x);
        world.entity_mut(e).insert(Producer::new());
        (e, id)
    }

    fn factory_orders(world: &World, e: Entity) -> Vec<QueuedCommand> {
        world.get::<FactoryOrders>(e).unwrap().commands.clone()
    }

    fn order(units: Units, order: Order, queue: bool) -> SimCommand {
        SimCommand::Order {
            units,
            order,
            queue,
        }
    }

    fn at(x: f32) -> NetPos {
        NetPos::quantize(Vec3::new(x, 0.0, 100.0))
    }

    /// A move to a selection with a factory: the unit walks, the factory
    /// keeps the move for the units it builds and doesn't move itself.
    #[test]
    fn a_factory_keeps_orders_for_its_units() {
        let mut world = world();
        let (kernel, k) = factory(&mut world, UnitKind::Kernel, 100.0);
        let (bit, b) = unit(&mut world, UnitKind::Bit, 0, 200.0);
        send(&mut world, 0, vec![move_to(Units::of([k, b]), 50.0)]);
        assert!(world.get::<MoveTarget>(bit).is_some());
        assert!(world.get::<MoveTarget>(kernel).is_none());
        assert_eq!(
            factory_orders(&world, kernel),
            vec![QueuedCommand::Move(at(50.0).to_vec3())]
        );
    }

    /// Shift appends, a plain order replaces; attacks aren't kept; a
    /// factory doesn't guard itself; Stop clears the orders.
    #[test]
    fn factory_orders_queue_replace_and_stop() {
        let mut world = world();
        let (socket, s) = factory(&mut world, UnitKind::Socket, 100.0);
        let (_, b) = unit(&mut world, UnitKind::Bit, 0, 200.0);
        let units = || Units::of([s]);
        send(
            &mut world,
            0,
            vec![
                order(units(), Order::Move(at(10.0)), false),
                order(units(), Order::AttackMove(at(20.0)), true),
                order(units(), Order::Patrol(at(30.0)), true),
                order(units(), Order::Guard(b), true),
                order(units(), Order::Guard(s), true),
                order(units(), Order::AttackGround(at(40.0)), true),
            ],
        );
        let kinds: Vec<&str> = factory_orders(&world, socket)
            .iter()
            .map(|c| match c {
                QueuedCommand::Move(_) => "move",
                QueuedCommand::AttackMove(_) => "fight",
                QueuedCommand::Patrol(_) => "patrol",
                QueuedCommand::Guard { .. } => "guard",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["move", "fight", "patrol", "guard"]);

        send(
            &mut world,
            0,
            vec![order(units(), Order::Move(at(60.0)), false)],
        );
        assert_eq!(
            factory_orders(&world, socket),
            vec![QueuedCommand::Move(at(60.0).to_vec3())]
        );
        send(&mut world, 0, vec![SimCommand::Stop { units: units() }]);
        assert!(factory_orders(&world, socket).is_empty());
    }

    /// A drawn line for a factory: a move per point.
    #[test]
    fn a_factory_takes_a_drawn_path() {
        let mut world = world();
        let (hole, h) = factory(&mut world, UnitKind::Hole, 100.0);
        send(
            &mut world,
            0,
            vec![SimCommand::Path {
                unit: h,
                waypoints: PosList::quantize([10.0, 20.0, 30.0].map(|x| at(x).to_vec3())),
                queue: false,
            }],
        );
        assert_eq!(
            factory_orders(&world, hole),
            [10.0, 20.0, 30.0].map(|x| QueuedCommand::Move(at(x).to_vec3()))
        );
    }
}
