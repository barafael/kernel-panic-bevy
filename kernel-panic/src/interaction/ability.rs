//! Ability hotkeys.
//!
//! Pressing `D` with a caster selected fires that unit's "ability" at
//! the cursor — whatever the unit kind treats as its ability:
//! - Pointer / Obelisk / Firewall / Byte / Terminal → command-fire weapon
//!   (NX Flag, Infection gas, etc.) routed through `CommandFireEvent`.
//! - Port / Connection → Dispatch packets (with ALT modifier for
//!   "drain the buffer", mirroring upstream `network_dispatch.lua`).
//! - Bug / Exploit → Deploy / Pack Up (the Bug ↔ Exploit morph).
//!
//! The other order hotkeys (Stop, Attack, Move, Fight, Patrol, Guard,
//! Enter, self-destruct …) go through the command panel's single
//! activation path (`ui::hud::command_panel::activation`), exactly like
//! a click on the matching panel button. This module owns the sticky
//! [`OrderCursorModes`] they arm and the map-click handlers that turn an
//! armed mode into an order. The panel's ability buttons (NX Flag,
//! SIGTERM, Dispatch …) arm [`OrderCursorModes::ability`] so the next
//! ground click casts the aimed abilities, as `D` over that point would
//! ([`deploy_units`] / [`cast_aimed_abilities`] are shared).

use bevy::prelude::*;

use super::clear_orders;
use super::movement::{
    AttackMoveActive, CommandQueue, GuardTarget, MovePath, MoveTarget, QueuedCommand,
};
use super::selection::{
    OrderMarker, PendingMoveIndicators, PickRayCast, Selected, apply_ordered_command, ground_hit,
    unit_hit,
};
use crate::rendering::camera::RtsCamera;
use crate::units::combat::{
    AttackGroundOrder, AttackTargetOrder, ForcedTarget,
};
use crate::units::components::{TeamId, UnitType, is_friendly};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::mechanics::command_fire::{CommandFireEvent, PendingCommandFire};
use crate::units::mechanics::deploy::DeployEvent;
use crate::units::mechanics::network_buffer::DispatchEvent;

fn ctrl_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
}

pub struct AbilityHotkeyPlugin;

impl Plugin for AbilityHotkeyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OrderCursorModes>().add_systems(
            Update,
            // Two nested groups: a flat tuple here would exceed
            // Bevy's 21-item tuple-arity cap.
            (
                (
                    trigger_aimed_ability_on_hotkey,
                    trigger_deploy_on_hotkey,
                    trigger_ability_click,
                    update_ability_cursor,
                ),
                (
                    trigger_patrol_click,
                    update_patrol_cursor,
                    trigger_attack_ground_click,
                    update_attack_ground_cursor,
                    trigger_attack_move_click,
                    update_attack_move_cursor,
                    trigger_guard_click,
                    update_guard_cursor,
                    trigger_move_click,
                    update_move_cursor,
                    trigger_set_target_click,
                    update_set_target_cursor,
                ),
            ),
        );
    }
}

/// Sticky order-targeting modes armed from the hotkeys / command panel.
///
/// Only one mode may be active at a time. The active mode forces the
/// cursor glyph (Attack / Attack / Patrol) and the next left-click is
/// consumed by that mode's click handler as an order for the selection:
/// - `attack_ground` (`A` / Attack button): fire at a static ground point.
/// - `attack_move` (`F` / Fight button): march to a point, fighting en
///   route.
/// - `patrol` (`P` / button): shuttle between the click point and where
///   the unit started.
/// - `ability` (Ability button only — `D` casts at the cursor directly):
///   cast the selection's aimed abilities at the clicked point.
///
/// Modes are cleared by re-pressing the key, Escape, right-click (both
/// handled by the command panel's input, like Spring's `CGuiHandler`),
/// the committing click, or a Stop order.
#[derive(Resource, Default)]
pub struct OrderCursorModes {
    pub attack_ground: bool,
    pub attack_move: bool,
    pub patrol: bool,
    pub guard: bool,
    pub move_order: bool,
    pub set_target: bool,
    pub ability: bool,
    /// Which casters an armed [`Self::ability`] click fires: the panel's
    /// per-command buttons (NX Flag, SIGTERM, Dispatch …) only cast their
    /// own units' ability, like a Spring command goes only to units that
    /// list it. `None` casts every aimed ability in the selection.
    pub ability_for: Option<fn(UnitKind) -> bool>,
}

impl OrderCursorModes {
    pub fn any_active(&self) -> bool {
        self.attack_ground
            || self.attack_move
            || self.patrol
            || self.guard
            || self.move_order
            || self.set_target
            || self.ability
    }

    /// Arm exactly one mode, clearing the others (they share the cursor).
    pub fn arm(&mut self, mode: Mode) {
        self.ability_for = None;
        self.attack_ground = mode == Mode::AttackGround;
        self.attack_move = mode == Mode::AttackMove;
        self.patrol = mode == Mode::Patrol;
        self.guard = mode == Mode::Guard;
        self.move_order = mode == Mode::Move;
        self.set_target = mode == Mode::SetTarget;
        self.ability = mode == Mode::Ability;
    }

    /// Disarm every mode.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The sticky order-targeting modes, for [`OrderCursorModes::arm`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    AttackGround,
    AttackMove,
    Patrol,
    Guard,
    Move,
    SetTarget,
    Ability,
}

/// Whether `kind`'s `D` ability is aimed at a map point: command-fire
/// casters (NX Flag, Infection, Protect, Mine Launch, SIGTERM) and the
/// packet teleporters (Dispatch). Bug / Exploit deploy needs no target.
pub(crate) fn ability_is_aimed(kind: UnitKind) -> bool {
    kind.has_command_fire_ability() || kind.is_teleporter()
}

/// Deploy / pack up every Bug / Exploit in `units` — the untargeted half
/// of the `D` ability.
pub(crate) fn deploy_units(
    units: impl IntoIterator<Item = (Entity, UnitKind)>,
    ev: &mut MessageWriter<DeployEvent>,
) {
    for (entity, kind) in units {
        if kind.deploy_pair().is_some() {
            ev.write(DeployEvent { entity });
        }
    }
}

/// Cast the aimed half of the `D` ability at `target` for every unit in
/// `units`: command-fire casters get a [`CommandFireEvent`] (range /
/// approach handled by `process_command_fire`), teleporters dispatch.
///
/// `alt_held` mirrors upstream `network_dispatch.lua`: the dispatch
/// command stays active and re-fires every frame until the team's
/// Packet Buffer is empty, instead of stopping after one 12-batch. With
/// ALT we only insert the `AutoDispatch` marker — the first batch goes
/// out on the next frame via `tick_auto_dispatch`, avoiding a
/// double-fire that would drain up to 24 packets in frame 1.
pub(crate) fn cast_aimed_abilities(
    units: impl IntoIterator<Item = (Entity, UnitKind)>,
    target: Vec3,
    alt_held: bool,
    command_fire: &mut MessageWriter<CommandFireEvent>,
    dispatch: &mut MessageWriter<DispatchEvent>,
    commands: &mut Commands,
) {
    for (entity, kind) in units {
        if kind.has_command_fire_ability() {
            command_fire.write(CommandFireEvent {
                attacker: entity,
                target,
            });
        } else if kind.is_teleporter() {
            if alt_held {
                commands
                    .entity(entity)
                    .insert(crate::units::mechanics::network_buffer::AutoDispatch { target });
            } else {
                dispatch.write(DispatchEvent {
                    teleporter: entity,
                    target,
                });
            }
        }
    }
}

fn alt_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight)
}

/// `D` deploys a selected Bug into an Exploit and packs an Exploit
/// back into a Bug. Co-exists with command-fire / dispatch on the
/// same key because the eligibility sets don't overlap — Bug/Exploit
/// aren't casters, aren't teleporters.
fn trigger_deploy_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    mut ev: MessageWriter<DeployEvent>,
) {
    if !keys.just_pressed(KeyCode::KeyD) {
        return;
    }
    if ctrl_held(&keys) {
        return;
    }
    deploy_units(selected_q.iter().map(|(e, u)| (e, u.0)), &mut ev);
}

/// `D` casts the selection's aimed abilities (command-fire, Dispatch)
/// at the ground point under the cursor.
#[allow(clippy::too_many_arguments)]
fn trigger_aimed_ability_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut command_fire: MessageWriter<CommandFireEvent>,
    mut dispatch: MessageWriter<DispatchEvent>,
    mut commands: Commands,
) {
    if !keys.just_pressed(KeyCode::KeyD) || ctrl_held(&keys) {
        return;
    }
    if !selected_q.iter().any(|(_, u)| ability_is_aimed(u.0)) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    // Why: `D` consumes a click-to-cast the palette button armed — left
    // armed, the next unrelated click would cast again (a second
    // 12-packet Dispatch drains the buffer twice).
    modes.ability = false;
    cast_aimed_abilities(
        selected_q.iter().map(|(e, u)| (e, u.0)),
        target,
        alt_held(&keys),
        &mut command_fire,
        &mut dispatch,
        &mut commands,
    );
}

/// Click handler for [`OrderCursorModes::ability`]: the next left-click
/// on the ground casts the selection's aimed abilities there — exactly
/// what `D` would do with the cursor at that point. Shift stays armed.
#[allow(clippy::too_many_arguments)]
fn trigger_ability_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut command_fire: MessageWriter<CommandFireEvent>,
    mut dispatch: MessageWriter<DispatchEvent>,
    mut commands: Commands,
) {
    if !modes.ability || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let filter = modes.ability_for;
    cast_aimed_abilities(
        selected_q
            .iter()
            .map(|(e, u)| (e, u.0))
            .filter(|(_, k)| filter.is_none_or(|f| f(*k))),
        target,
        alt_held(&keys),
        &mut command_fire,
        &mut dispatch,
        &mut commands,
    );
    pending.markers.push((target, OrderMarker::Attack));
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if !shift {
        modes.ability = false;
    }
}

/// Attack glyph while the Ability button's cast mode is armed.
fn update_ability_cursor(
    modes: Res<OrderCursorModes>,
    mut request: ResMut<crate::interaction::cursor::CursorRequest>,
) {
    if modes.ability {
        request.set(crate::interaction::cursor::CursorKind::Attack, 10);
    }
}

/// Ground-target click: while [`OrderCursorModes::attack_ground`] is
/// armed, the next left-click issues an [`AttackGroundOrder`] for every
/// selected unit. `attack_ground_system` moves the unit into weapon range
/// if needed, then fires each reload cycle at the ground position. Shift
/// queues a move to the same point first. Commits exit the mode (Shift
/// stays in mode).
#[allow(clippy::too_many_arguments)]
fn trigger_attack_ground_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<Entity, With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    move_target_q: Query<(), With<MoveTarget>>,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
) {
    if !modes.attack_ground || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    for entity in &selected_q {
        if shift && move_target_q.contains(entity) {
            // Shift-queue: append a move-then-attack-ground sequence
            // by enqueuing a move order to the target position.
            // The AttackGroundOrder fires once the unit arrives.
            apply_ordered_command(
                entity,
                QueuedCommand::Move(target),
                true,
                &move_target_q,
                &mut commands,
            );
        } else {
            // Immediate: cancel any current order, issue AttackGroundOrder.
            // attack_ground_system handles movement if needed.
            clear_orders(&mut commands.entity(entity)).insert(AttackGroundOrder { pos: target });
        }
    }
    pending.markers.push((target, OrderMarker::Attack));
    if !shift {
        modes.attack_ground = false;
    }
}

/// Force the cursor to the Attack glyph while the ground-target mode
/// is active. Uses a high priority so it beats the default context
/// resolver; returning a lower priority when inactive lets the context
/// resolver regain control.
fn update_attack_ground_cursor(
    modes: Res<OrderCursorModes>,
    mut request: ResMut<crate::interaction::cursor::CursorRequest>,
) {
    if modes.attack_ground {
        request.set(crate::interaction::cursor::CursorKind::Attack, 10);
    }
}

/// Click handler for the move mode: the next left-click issues a plain
/// move order. Shift queues it behind the active order and stays armed.
#[allow(clippy::too_many_arguments)]
fn trigger_move_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    move_target_q: Query<(), With<MoveTarget>>,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
    unit_registry: Res<UnitRegistry>,
) {
    if !modes.move_order || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    for (entity, unit) in &selected_q {
        if unit_registry.speed(unit.0) <= 0.0 {
            continue;
        }
        apply_ordered_command(
            entity,
            QueuedCommand::Move(target),
            shift,
            &move_target_q,
            &mut commands,
        );
    }
    pending.markers.push((target, OrderMarker::Move));
    if !shift {
        modes.move_order = false;
    }
}

/// Click handler for the set-target mode: the next left-click on an enemy
/// unit designates it as the selection's manual target. This is an aim
/// designation only — current orders are left untouched. Shift stays armed.
#[allow(clippy::too_many_arguments)]
fn trigger_set_target_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType, &TeamId), With<Selected>>,
    unit_root_q: Query<Entity, With<UnitType>>,
    parent_q: Query<&ChildOf>,
    unit_info_q: Query<&TeamId>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    target_gtf_q: Query<&GlobalTransform>,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
    unit_registry: Res<UnitRegistry>,
) {
    if !modes.set_target || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = unit_hit(&windows, &camera_q, &mut ray_cast, &unit_root_q, &parent_q) else {
        return;
    };
    // Manual fire must only ever designate hostiles — the forced-target
    // combat path bypasses the auto-targeter's friend filters.
    let Ok(t_team) = unit_info_q.get(target) else {
        return;
    };
    let Some(sel_team) = selected_q.iter().next().map(|(_, _, team)| team.0)
    else {
        return;
    };
    if is_friendly(sel_team, t_team.0) {
        return;
    }
    for (entity, unit, _) in &selected_q {
        if unit_registry.weapon(unit.0).is_empty() {
            continue;
        }
        commands.entity(entity).insert(ForcedTarget(target));
    }
    if let Ok(t_gtf) = target_gtf_q.get(target) {
        pending
            .markers
            .push((t_gtf.translation(), OrderMarker::Target));
    }
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if !shift {
        modes.set_target = false;
    }
}

/// Click handler: while [`OrderCursorModes::patrol`] is armed, the next
/// left-click issues a patrol order for every selected unit. The unit
/// will patrol between its current location and the clicked location. Shift queues a
/// follow-up patrol waypoint behind the unit's active order instead of
/// replacing it (and stays armed for chain-patrolling).
#[allow(clippy::too_many_arguments)]
fn trigger_patrol_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    transform_q: Query<&Transform>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    move_target_q: Query<(), With<MoveTarget>>,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
    unit_registry: Res<crate::units::content::unit_registry::UnitRegistry>,
) {
    if !modes.patrol || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

    for (entity, unit) in &selected_q {
        let speed = unit_registry.speed(unit.0);
        if speed <= 0.0 {
            continue;
        }

        if shift && move_target_q.contains(entity) {
            // Shift-queue: walk to the clicked point once the current
            // order finishes, then patrol back & forth from there.
            apply_ordered_command(
                entity,
                QueuedCommand::Patrol(target),
                true,
                &move_target_q,
                &mut commands,
            );
            continue;
        }

        // Immediate: cancel the current order, march to the target, then
        // return to the starting position — `movement_system` re-queues
        // the opposing waypoint on arrival so this shuttles indefinitely.
        let Ok(current_tf) = transform_q.get(entity) else {
            continue;
        };
        let current_pos = current_tf.translation;
        let mut queue = CommandQueue::default();
        queue.push(QueuedCommand::Patrol(current_pos));
        commands
            .entity(entity)
            .remove::<MovePath>()
            .remove::<AttackMoveActive>()
            .remove::<AttackGroundOrder>()
            .remove::<AttackTargetOrder>()
            .remove::<GuardTarget>()
            .remove::<PendingCommandFire>()
            .insert(MoveTarget(target))
            .insert(queue);
    }
    pending.markers.push((target, OrderMarker::Patrol));
    if !shift {
        modes.patrol = false;
    }
}

/// Click handler: while [`OrderCursorModes::attack_move`] is active, the
/// next left-click issues an attack-move order (march to the point,
/// engaging hostiles en route) for every selected mobile unit. Shift
/// queues the march behind the active order and stays armed.
#[allow(clippy::too_many_arguments)]
fn trigger_attack_move_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    move_target_q: Query<(), With<MoveTarget>>,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
    unit_registry: Res<crate::units::content::unit_registry::UnitRegistry>,
) {
    if !modes.attack_move || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    for (entity, unit) in &selected_q {
        let speed = unit_registry.speed(unit.0);
        if speed <= 0.0 {
            continue;
        }
        apply_ordered_command(
            entity,
            QueuedCommand::AttackMove(target),
            shift,
            &move_target_q,
            &mut commands,
        );
    }
    pending.markers.push((target, OrderMarker::Attack));
    if !shift {
        modes.attack_move = false;
    }
}

/// Force the cursor to the Attack glyph while the attack-move mode is active.
fn update_attack_move_cursor(
    modes: Res<OrderCursorModes>,
    mut request: ResMut<crate::interaction::cursor::CursorRequest>,
) {
    if modes.attack_move {
        request.set(crate::interaction::cursor::CursorKind::Attack, 10);
    }
}

/// Force the cursor to the Defend glyph while the guard mode is armed.
fn update_guard_cursor(
    modes: Res<OrderCursorModes>,
    mut request: ResMut<crate::interaction::cursor::CursorRequest>,
) {
    if modes.guard {
        request.set(crate::interaction::cursor::CursorKind::Defend, 10);
    }
}

/// Force the cursor to the Move glyph while the move mode is armed.
fn update_move_cursor(
    modes: Res<OrderCursorModes>,
    mut request: ResMut<crate::interaction::cursor::CursorRequest>,
) {
    if modes.move_order {
        request.set(crate::interaction::cursor::CursorKind::Move, 10);
    }
}

/// Force the cursor to the Attack glyph while the set-target mode is armed
/// (Spring renders set-target with the attack crosshair too).
fn update_set_target_cursor(
    modes: Res<OrderCursorModes>,
    mut request: ResMut<crate::interaction::cursor::CursorRequest>,
) {
    if modes.set_target {
        request.set(crate::interaction::cursor::CursorKind::Attack, 10);
    }
}

/// Click handler: while [`OrderCursorModes::guard`] is armed, the next
/// left-click on a friendly unit makes every selected mobile unit guard
/// it — trail it at close range while the auto-attack path defends it.
/// Clicking an enemy or bare ground is ignored (the mode stays armed).
#[allow(clippy::too_many_arguments)]
fn trigger_guard_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType, &TeamId), With<Selected>>,
    unit_root_q: Query<Entity, With<UnitType>>,
    parent_q: Query<&ChildOf>,
    unit_info_q: Query<&TeamId>,
    target_gtf_q: Query<&GlobalTransform>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
    unit_registry: Res<UnitRegistry>,
) {
    if !modes.guard || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = unit_hit(&windows, &camera_q, &mut ray_cast, &unit_root_q, &parent_q) else {
        return;
    };
    let Ok(t_team) = unit_info_q.get(target) else {
        return;
    };
    // Guarding makes sense only on a friendly unit.
    let Some(sel_team) = selected_q.iter().next().map(|(_, _, team)| team.0)
    else {
        return;
    };
    if !is_friendly(sel_team, t_team.0) {
        return;
    }

    for (entity, unit, _) in &selected_q {
        if unit_registry.speed(unit.0) <= 0.0 {
            continue;
        }
        clear_orders(&mut commands.entity(entity)).insert(GuardTarget(target));
    }
    if let Ok(t_gtf) = target_gtf_q.get(target) {
        pending
            .markers
            .push((t_gtf.translation(), OrderMarker::Guard));
    }
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if !shift {
        modes.guard = false;
    }
}

/// Force the cursor to the Patrol glyph while the patrol mode is active.
fn update_patrol_cursor(
    modes: Res<OrderCursorModes>,
    mut request: ResMut<crate::interaction::cursor::CursorRequest>,
) {
    if modes.patrol {
        request.set(crate::interaction::cursor::CursorKind::Patrol, 10);
    }
}
