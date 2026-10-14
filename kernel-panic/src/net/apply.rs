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
use crate::units::lifecycle::production::{Producer, factory_roster};
use crate::units::mechanics::command_fire::CommandFireEvent;
use crate::units::mechanics::deploy::DeployEvent;
use crate::units::mechanics::network_buffer::{AutoDispatch, DispatchEvent, EnterEvent};
use crate::units::mechanics::worm::AutoHold;

/// Most units one factory command may add or remove: the UI's largest
/// multiplier (Ctrl+Shift, `CCommandAI::GetCountMultiplierFromOptions`).
pub const MAX_FACTORY_COUNT: u16 = 100;
/// Most sites in one build command (a long Shift row is a few dozen).
pub const MAX_BUILD_SITES: usize = 256;
/// Most units one `/give` spawns.
pub const MAX_DEV_SPAWN: u16 = 100;

/// Whether the match allows [`DevCommand`]s: at most one human seat
/// (single player and the dev tools' sandbox). A bare world without a
/// match (unit tests) allows them.
pub(crate) fn cheats_allowed(active: Option<&ActiveMatch>) -> bool {
    active.is_none_or(|m| m.0.players.iter().filter(|p| !p.ai).count() <= 1)
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

    /// Whether `player` may send anything at all.
    fn admit_player(&self, player: u8) -> Result<(), Refusal> {
        if self
            .active
            .as_ref()
            // Seat `i` is owner `i` (`units::teams`).
            .is_some_and(|m| usize::from(player) >= m.0.players.len())
        {
            return Err(Refusal::Unseated);
        }
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
    autohold: Query<'w, 's, &'static mut AutoHold>,
    vents: Query<'w, 's, (Entity, &'static GeoventSmoker), Without<VentClaim>>,
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
    let local = world
        .get_resource::<crate::units::player::LocalTeam>()
        .map_or(Some(0), |t| {
            (t.0 != crate::game_setup::SPECTATOR_TEAM).then_some(t.0)
        });
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
                let mut cmd = cmd.clone();
                cmd.units_mut()
                    .map(|u| bus.resolve_units(frame.player, u))
                    .unwrap_or_default()
            };
            let Ok(mut ctx) = state.get_mut(world) else {
                return;
            };
            // Checked per command: a resignation mid-frame refuses the
            // rest of the frame.
            if let Err(why) = ctx
                .rules
                .admit_player(frame.player)
                .and_then(|()| ctx.check_shape(cmd, &ids))
            {
                ctx.rules.refuse(frame.tick, frame.player, cmd, why);
                continue;
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
            SimCommand::SetSpeed { percent }
                if !(MIN_SPEED_PERCENT..=MAX_SPEED_PERCENT).contains(percent) =>
            {
                return Err(Refusal::Malformed("speed out of range"));
            }
            // One point for the whole command: off the map, all of it.
            SimCommand::Order {
                order:
                    Order::Move(p) | Order::AttackMove(p) | Order::Patrol(p) | Order::AttackGround(p),
                ..
            }
            | SimCommand::Cast { target: p, .. }
                if !self.rules.in_map(p.to_vec3()) =>
            {
                return Err(Refusal::OffMap);
            }
            SimCommand::Dev(dev) => {
                if !cheats_allowed(self.rules.active.as_deref()) {
                    return Err(Refusal::NoCheats);
                }
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
            .is_none_or(|r| !r.weapon(kind).is_empty())
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
                if !self.mobile(own.kind) {
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
                        m.write(DeployEvent { entity: own.e });
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
                    c.speed_percent = *percent;
                }
            }
            SimCommand::Pause { on } => {
                if let Some(c) = self.rules.control.as_mut() {
                    c.paused = *on;
                    c.paused_by = on.then_some(player);
                }
            }
            SimCommand::Resign => self.resign(player),
            SimCommand::Take { from } => self.take(tick, player, cmd, *from),
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
        let cmd = match order {
            // A plain move goes to the whole selection, factories too
            // (KP's single right click is the engine's, and Spring
            // factories take `CMD_MOVE` as their rally order).
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
        };
        for u in units {
            self.give(u.e, cmd, queue);
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
}
