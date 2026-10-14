//! The sim side of the bus: at the head of every fixed tick, seal what
//! was issued and apply the frames due on this tick, one command at a
//! time (each command's world edits land before the next is applied,
//! so "Shift-queue behind the order given a command ago" sees it).
//!
//! Everything that depends on sim state is decided *here*, not by the
//! issuing input system: whether a unit is busy (Shift appends or
//! replaces), where a patrol route starts, which queued build sites a
//! Shift placement cancels, the attack target's position. The issuer
//! only names units, points and kinds. Units that died, belong to
//! another player or never existed are skipped.

use bevy::ecs::system::{SystemParam, SystemState};
use bevy::prelude::*;

use super::{CommandBus, FactoryOp, NetId, NetIdIndex, Order, PointOrder, SimCommand};
use crate::interaction::movement::{CommandQueue, MoveTarget, Orders, QueuedCommand};
use crate::interaction::selection::apply_ordered_command;
use crate::interaction::selection::right_click::queue_command;
use crate::terrain::geovent::{GeoventSmoker, VentClaim};
use crate::units::combat::{Dying, ForcedTarget, SelfDestructCountdown};
use crate::units::components::{TeamId, UnitType};
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
            ctx.apply(frame.player, cmd, ids.ids());
            state.apply(world);
        }
    }
}

impl ApplyCtx<'_, '_> {
    /// Friend or foe ([`Alliances::friendly`]); a bare world without
    /// alliances counts an owner alone as its friend.
    ///
    /// [`Alliances::friendly`]: crate::units::teams::Alliances::friendly
    fn friendly(&self, a: u8, b: u8) -> bool {
        self.alliances
            .as_ref()
            .map_or(a == b, |al| al.friendly(a, b))
    }

    /// The live units among `ids` that `player` may command, in id order.
    fn owned(&self, player: u8, ids: &[NetId]) -> Vec<(Entity, UnitKind)> {
        ids.iter()
            .filter_map(|id| self.owned_one(player, *id))
            .collect()
    }

    fn owned_one(&self, player: u8, id: NetId) -> Option<(Entity, UnitKind)> {
        let e = self.index.get(id)?;
        let (team, ut, _) = self.units.get(e).ok()?;
        // A unit without a team (bare test worlds) obeys anyone.
        team.is_none_or(|t| t.0 == player).then_some((e, ut.0))
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

    fn give(&mut self, e: Entity, cmd: QueuedCommand, queue: bool) {
        apply_ordered_command(e, cmd, queue, &self.orders, &mut self.commands);
    }

    fn apply(&mut self, player: u8, cmd: &SimCommand, ids: &[NetId]) {
        match cmd {
            SimCommand::Order { order, queue, .. } => self.order(player, ids, *order, *queue),
            SimCommand::PerUnit {
                order,
                units,
                targets,
                queue,
            } => {
                for (id, target) in units.ids().iter().zip(targets.points()) {
                    let Some((e, _)) = self.owned_one(player, *id) else {
                        continue;
                    };
                    let cmd = match order {
                        PointOrder::Move => QueuedCommand::Move(target),
                        PointOrder::AttackMove => QueuedCommand::AttackMove(target),
                    };
                    self.give(e, cmd, *queue);
                }
            }
            SimCommand::Path {
                unit,
                waypoints,
                queue,
            } => {
                let Some((e, _)) = self.owned_one(player, *unit) else {
                    return;
                };
                let mut points = waypoints.points();
                if let Some(first) = points.next() {
                    self.give(e, QueuedCommand::Move(first), *queue);
                }
                for p in points {
                    queue_command(e, QueuedCommand::Move(p), &mut self.commands);
                }
            }
            SimCommand::Stop { .. } => {
                for (e, _) in self.owned(player, ids) {
                    crate::interaction::clear_orders(&mut self.commands.entity(e))
                        .remove::<SelfDestructCountdown>();
                }
            }
            SimCommand::Build {
                kind, sites, queue, ..
            } => {
                let sites: Vec<Vec3> = sites.points().collect();
                self.build(player, ids, *kind, &sites, *queue);
            }
            SimCommand::Factory { kind, op, .. } => {
                for (e, factory) in self.owned(player, ids) {
                    if !factory_roster(factory).contains(kind) {
                        continue;
                    }
                    if let Ok(mut producer) = self.producers.get_mut(e) {
                        apply_factory_op(&mut producer, *kind, *op);
                    }
                }
            }
            SimCommand::SetRepeat { on, .. } => {
                for (e, _) in self.owned(player, ids) {
                    if let Ok(mut p) = self.producers.get_mut(e) {
                        p.set_repeat(*on);
                    }
                }
            }
            SimCommand::SetAutoHold { on, .. } => {
                for (e, _) in self.owned(player, ids) {
                    if let Ok(mut a) = self.autohold.get_mut(e) {
                        a.0 = *on;
                    }
                }
            }
            SimCommand::SetTarget { target, .. } => {
                // Manual fire only ever designates hostiles.
                let Some((t, _, t_team, t_kind)) = self.live(*target) else {
                    return;
                };
                if self.friendly(player, t_team) {
                    return;
                }
                for (e, kind) in self.owned(player, ids) {
                    if self
                        .registry
                        .as_ref()
                        .is_some_and(|r| r.weapon(kind).is_empty() || !r.can_attack(kind, t_kind))
                    {
                        continue;
                    }
                    self.commands.entity(e).insert(ForcedTarget(t));
                }
            }
            SimCommand::UnsetTarget { .. } => {
                for (e, _) in self.owned(player, ids) {
                    self.commands.entity(e).remove::<ForcedTarget>();
                }
            }
            SimCommand::SelfDestruct { .. } => {
                for (e, kind) in self.owned(player, ids) {
                    let countdown = SelfDestructCountdown::ordered(kind, self.registry.as_deref());
                    self.commands.entity(e).insert(countdown);
                }
            }
            SimCommand::Deploy { .. } => {
                for (entity, kind) in self.owned(player, ids) {
                    if kind.deploy_pair().is_some()
                        && let Some(m) = self.deploy.as_mut()
                    {
                        m.write(DeployEvent { entity });
                    }
                }
            }
            SimCommand::Enter { .. } => {
                for (packet, kind) in self.owned(player, ids) {
                    if kind == UnitKind::Packet
                        && let Some(m) = self.enter.as_mut()
                    {
                        m.write(EnterEvent { packet });
                    }
                }
            }
            SimCommand::Cast { target, auto, .. } => {
                let target = target.to_vec3();
                for (e, kind) in self.owned(player, ids) {
                    if kind.has_command_fire_ability() {
                        if let Some(m) = self.command_fire.as_mut() {
                            m.write(CommandFireEvent {
                                attacker: e,
                                target,
                            });
                        }
                    } else if kind.is_teleporter() {
                        // Alt: only the marker — the first batch goes
                        // out next tick via `tick_auto_dispatch`.
                        if *auto {
                            self.commands.entity(e).insert(AutoDispatch { target });
                        } else if let Some(m) = self.dispatch.as_mut() {
                            m.write(DispatchEvent {
                                teleporter: e,
                                target,
                            });
                        }
                    }
                }
            }
        }
    }

    fn order(&mut self, player: u8, ids: &[NetId], order: Order, queue: bool) {
        let units = self.owned(player, ids);
        let cmd = match order {
            Order::Move(p) => QueuedCommand::Move(p.to_vec3()),
            Order::AttackMove(p) => QueuedCommand::AttackMove(p.to_vec3()),
            Order::AttackGround(p) => QueuedCommand::AttackGround(p.to_vec3()),
            Order::AttackUnit(t) => {
                let Some((target, pos, ..)) = self.live(t) else {
                    return;
                };
                QueuedCommand::AttackUnit { target, pos }
            }
            Order::Guard(t) => {
                let Some((target, pos, ..)) = self.live(t) else {
                    return;
                };
                QueuedCommand::Guard { target, pos }
            }
            Order::Patrol(p) => {
                self.patrol(&units, p.to_vec3(), queue);
                return;
            }
        };
        for (e, _) in units {
            if let QueuedCommand::Guard { target, .. } = cmd
                && target == e
            {
                continue;
            }
            self.give(e, cmd, queue);
        }
    }

    /// `CMD_PATROL` (see `interaction::ability::patrol_commands`): a
    /// fresh patrol walks to the point with the start and the point
    /// queued as the route; Shift appends it behind the unit's orders.
    fn patrol(&mut self, units: &[(Entity, UnitKind)], target: Vec3, queue: bool) {
        for &(e, _) in units {
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
        for (e, builder) in self.owned(player, ids) {
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
