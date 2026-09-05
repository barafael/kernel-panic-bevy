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
//! `R` lets a Packet re-Enter the buffer. `Ctrl+D` is self-destruct.

use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use super::movement::{AttackMoveActive, CommandQueue, GuardTarget, MovePath, MoveTarget, QueuedCommand};
use super::selection::{
    OrderMarker, PendingMoveIndicators, Selected, apply_ordered_command, ground_hit, unit_hit,
};
use crate::rendering::camera::RtsCamera;
use crate::units::combat::{
    AttackGroundOrder, AttackTargetOrder, ForcedTarget, SelfDestructCountdown, SELF_DESTRUCT_DELAY,
};
use crate::units::components::{Faction, TeamId, UnitType, is_friendly};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::mechanics::command_fire::CommandFireEvent;
use crate::units::mechanics::deploy::DeployEvent;
use crate::units::mechanics::network_buffer::{DispatchEvent, EnterEvent};

fn ctrl_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
}

pub struct AbilityHotkeyPlugin;

impl Plugin for AbilityHotkeyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OrderCursorModes>()
            .add_systems(
                Update,
                // Two nested groups: a flat tuple here would exceed
                // Bevy's 21-item tuple-arity cap.
                (
                    (
                        trigger_command_fire_on_hotkey,
                        trigger_deploy_on_hotkey,
                        trigger_dispatch_on_hotkey,
                        trigger_enter_on_hotkey,
                        trigger_self_destruct_on_hotkey,
                        trigger_unset_target_on_hotkey,
                    ),
                    (
                        toggle_patrol_cursor_mode,
                        trigger_patrol_click,
                        update_patrol_cursor,
                        toggle_attack_ground_mode,
                        trigger_attack_ground_click,
                        update_attack_ground_cursor,
                        toggle_attack_move_mode,
                        trigger_attack_move_click,
                        update_attack_move_cursor,
                        toggle_guard_mode,
                        trigger_guard_click,
                        update_guard_cursor,
                        toggle_move_mode,
                        trigger_move_click,
                        update_move_cursor,
                        toggle_set_target_mode,
                        trigger_set_target_click,
                        update_set_target_cursor,
                    ),
                ),
            );
    }
}

/// Sticky order-targeting modes armed from the hotkeys / order palette.
///
/// Only one mode may be active at a time. The active mode forces the
/// cursor glyph (Attack / Attack / Patrol) and the next left-click is
/// consumed by that mode's click handler as an order for the selection:
/// - `attack_ground` (`A` / Attack button): fire at a static ground point.
/// - `attack_move` (`F` / Fight button): march to a point, fighting en
///   route.
/// - `patrol` (`P` / button): shuttle between the click point and where
///   the unit started.
///
/// Modes are cleared by re-pressing the key, Escape, right-click, the
/// committing click, or a Stop order.
#[derive(Resource, Default)]
pub struct OrderCursorModes {
    pub attack_ground: bool,
    pub attack_move: bool,
    pub patrol: bool,
    pub guard: bool,
    pub move_order: bool,
    pub set_target: bool,
}

impl OrderCursorModes {
    pub fn any_active(&self) -> bool {
        self.attack_ground
            || self.attack_move
            || self.patrol
            || self.guard
            || self.move_order
            || self.set_target
    }

    /// Arm exactly one mode, clearing the others (they share the cursor).
    fn arm(&mut self, mode: Mode) {
        self.attack_ground = mode == Mode::AttackGround;
        self.attack_move = mode == Mode::AttackMove;
        self.patrol = mode == Mode::Patrol;
        self.guard = mode == Mode::Guard;
        self.move_order = mode == Mode::Move;
        self.set_target = mode == Mode::SetTarget;
    }
}

/// The sticky order-targeting modes, for [`OrderCursorModes::arm`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    AttackGround,
    AttackMove,
    Patrol,
    Guard,
    Move,
    SetTarget,
}

/// `Ctrl+D` starts a 5 s self-destruct countdown on every selected
/// unit. The countdown is aborted by `Stop` (handled by the order
/// palette, which removes `SelfDestructCountdown` alongside the rest
/// of the order state) so the player can cancel before detonation.
fn trigger_self_destruct_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<Entity, With<Selected>>,
    mut commands: Commands,
) {
    if !ctrl_held(&keys) || !keys.just_pressed(KeyCode::KeyD) {
        return;
    }
    for entity in &selected_q {
        commands.entity(entity).insert(SelfDestructCountdown {
            remaining: SELF_DESTRUCT_DELAY,
        });
    }
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
    for (entity, unit) in &selected_q {
        if unit.0.deploy_pair().is_some() {
            ev.write(DeployEvent { entity });
        }
    }
}

fn trigger_dispatch_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: bevy::picking::mesh_picking::ray_cast::MeshRayCast,
    mut ev: MessageWriter<DispatchEvent>,
    mut commands: Commands,
) {
    if !keys.just_pressed(KeyCode::KeyD) {
        return;
    }
    if ctrl_held(&keys) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    // Holding ALT mirrors upstream `network_dispatch.lua`: the dispatch
    // command stays active and re-fires every frame until the team's
    // Packet Buffer is empty, instead of stopping after one 12-batch.
    // When ALT is held we only insert the `AutoDispatch` marker — the
    // first batch goes out on the next frame via `tick_auto_dispatch`,
    // avoiding a double-fire that would drain up to 24 packets in frame 1.
    let alt_held = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    for (entity, unit) in &selected_q {
        if !unit.0.is_teleporter() {
            continue;
        }
        if alt_held {
            commands
                .entity(entity)
                .insert(crate::units::mechanics::network_buffer::AutoDispatch { target });
        } else {
            ev.write(DispatchEvent {
                teleporter: entity,
                target,
            });
        }
    }
}

fn trigger_enter_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    mut ev: MessageWriter<EnterEvent>,
) {
    if !keys.just_pressed(KeyCode::KeyR) {
        return;
    }
    for (entity, unit) in &selected_q {
        if unit.0 == UnitKind::Packet {
            ev.write(EnterEvent { packet: entity });
        }
    }
}

fn trigger_command_fire_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    mut ev: MessageWriter<CommandFireEvent>,
) {
    if !keys.just_pressed(KeyCode::KeyD) {
        return;
    }
    if ctrl_held(&keys) {
        return;
    }

    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };

    for (entity, unit) in &selected_q {
        if unit.0.has_command_fire_ability() {
            ev.write(CommandFireEvent {
                attacker: entity,
                target,
            });
        }
    }
}

/// Toggle [`OrderCursorModes::attack_ground`]. `A` flips the flag; Escape
/// and right-click hard-cancel. While armed the cursor renders as
/// [`CursorKind::Attack`] (see [`update_attack_ground_cursor`]) and the
/// next left-click on the ground commits the order.
fn toggle_attack_ground_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut modes: ResMut<OrderCursorModes>,
) {
    // Ignore A while Ctrl is down so Ctrl+A (reserved for select-all
    // in future) doesn't toggle this mode.
    if !ctrl_held(&keys) && keys.just_pressed(KeyCode::KeyA) {
        if modes.attack_ground {
            modes.attack_ground = false;
        } else {
            modes.arm(Mode::AttackGround);
        }
        return;
    }
    if modes.attack_ground
        && (keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right))
    {
        modes.attack_ground = false;
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
    mut ray_cast: MeshRayCast,
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
            commands
                .entity(entity)
                .remove::<MoveTarget>()
                .remove::<crate::interaction::movement::MovePath>()
                .remove::<crate::interaction::movement::CommandQueue>()
                .remove::<AttackTargetOrder>()
                .remove::<GuardTarget>()
                .insert(AttackGroundOrder { pos: target });
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

/// Toggle [`OrderCursorModes::patrol`]. `P` flips the flag; Escape and
/// right-click hard-cancel. While armed the cursor renders as
/// [`CursorKind::Patrol`] (see [`update_patrol_cursor`]) and the next
/// left-click commits the patrol order.
fn toggle_patrol_cursor_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut modes: ResMut<OrderCursorModes>,
) {
    if keys.just_pressed(KeyCode::KeyP) {
        if modes.patrol {
            modes.patrol = false;
        } else {
            modes.arm(Mode::Patrol);
        }
        return;
    }
    if modes.patrol
        && (keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right))
    {
        modes.patrol = false;
    }
}

/// Toggle [`OrderCursorModes::attack_move`] with `F` (Fight). Escape and
/// right-click hard-cancel, mirroring the other order modes. At most one
/// cursor mode stays armed at a time.
fn toggle_attack_move_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut modes: ResMut<OrderCursorModes>,
) {
    if keys.just_pressed(KeyCode::KeyF) {
        if modes.attack_move {
            modes.attack_move = false;
        } else {
            modes.arm(Mode::AttackMove);
        }
        return;
    }
    if modes.attack_move
        && (keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right))
    {
        modes.attack_move = false;
    }
}

/// Toggle [`OrderCursorModes::guard`] with `G` (Guard). While armed the
/// cursor renders as [`CursorKind::Defend`] and the next left-click on a
/// friendly unit makes every selected unit guard it.
fn toggle_guard_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut modes: ResMut<OrderCursorModes>,
) {
    if keys.just_pressed(KeyCode::KeyG) {
        if modes.guard {
            modes.guard = false;
        } else {
            modes.arm(Mode::Guard);
        }
        return;
    }
    if modes.guard && (keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right))
    {
        modes.guard = false;
    }
}

/// Toggle [`OrderCursorModes::move_order`] with `M` (Spring's `CMD_MOVE`):
/// while armed the cursor renders as [`CursorKind::Move`] and the next
/// left-click issues a plain move for the selection.
fn toggle_move_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut modes: ResMut<OrderCursorModes>,
) {
    if keys.just_pressed(KeyCode::KeyM) {
        if modes.move_order {
            modes.move_order = false;
        } else {
            modes.arm(Mode::Move);
        }
        return;
    }
    if modes.move_order
        && (keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right))
    {
        modes.move_order = false;
    }
}

/// Toggle [`OrderCursorModes::set_target`] with `T` (Spring's
/// `CMD_SET_TARGET`): while armed the next left-click on a unit designates
/// it as the selected units' manual target — preferred over auto-target
/// in range, turret-tracked out of range, never chased.
fn toggle_set_target_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut modes: ResMut<OrderCursorModes>,
) {
    if keys.just_pressed(KeyCode::KeyT) {
        if modes.set_target {
            modes.set_target = false;
        } else {
            modes.arm(Mode::SetTarget);
        }
        return;
    }
    if modes.set_target
        && (keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right))
    {
        modes.set_target = false;
    }
}

/// `X` (Spring's `CMD_UNSET_TARGET`): drop the manual target designation
/// from every selected unit.
fn trigger_unset_target_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<Entity, With<Selected>>,
    mut commands: Commands,
) {
    if !keys.just_pressed(KeyCode::KeyX) {
        return;
    }
    for entity in &selected_q {
        commands.entity(entity).remove::<ForcedTarget>();
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
    mut ray_cast: MeshRayCast,
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
    selected_q: Query<(Entity, &UnitType, &TeamId, &Faction), With<Selected>>,
    unit_root_q: Query<Entity, With<UnitType>>,
    parent_q: Query<&ChildOf>,
    unit_info_q: Query<(&TeamId, &Faction)>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    target_gtf_q: Query<&GlobalTransform>,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
    unit_registry: Res<UnitRegistry>,
) {
    if !modes.set_target || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = unit_hit(&windows, &camera_q, &mut ray_cast, &unit_root_q, &parent_q)
    else {
        return;
    };
    // Manual fire must only ever designate hostiles — the forced-target
    // combat path bypasses the auto-targeter's friend filters.
    let Ok((t_team, t_faction)) = unit_info_q.get(target) else {
        return;
    };
    let Some((sel_team, sel_faction)) = selected_q
        .iter()
        .next()
        .map(|(_, _, team, faction)| (team.0, *faction))
    else {
        return;
    };
    if is_friendly(sel_team, sel_faction, t_team.0, *t_faction) {
        return;
    }
    for (entity, unit, _, _) in &selected_q {
        if unit_registry.weapon(unit.0).is_empty() {
            continue;
        }
        commands.entity(entity).insert(ForcedTarget(target));
    }
    if let Ok(t_gtf) = target_gtf_q.get(target) {
        pending.markers.push((t_gtf.translation(), OrderMarker::Target));
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
    mut ray_cast: MeshRayCast,
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
    mut ray_cast: MeshRayCast,
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
    selected_q: Query<(Entity, &UnitType, &TeamId, &Faction), With<Selected>>,
    unit_root_q: Query<Entity, With<UnitType>>,
    parent_q: Query<&ChildOf>,
    unit_info_q: Query<(&TeamId, &Faction)>,
    target_gtf_q: Query<&GlobalTransform>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
    unit_registry: Res<UnitRegistry>,
) {
    if !modes.guard || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = unit_hit(&windows, &camera_q, &mut ray_cast, &unit_root_q, &parent_q)
    else {
        return;
    };
    let Ok((t_team, t_faction)) = unit_info_q.get(target) else {
        return;
    };
    // Guarding makes sense only on a friendly unit.
    let Some((sel_team, sel_faction)) = selected_q
        .iter()
        .next()
        .map(|(_, _, team, faction)| (team.0, *faction))
    else {
        return;
    };
    if !is_friendly(sel_team, sel_faction, t_team.0, *t_faction) {
        return;
    }

    for (entity, unit, _, _) in &selected_q {
        if unit_registry.speed(unit.0) <= 0.0 {
            continue;
        }
        commands
            .entity(entity)
            .remove::<MoveTarget>()
            .remove::<MovePath>()
            .remove::<CommandQueue>()
            .remove::<AttackGroundOrder>()
            .remove::<AttackTargetOrder>()
            .remove::<AttackMoveActive>()
            .remove::<crate::units::lifecycle::construction::PendingBuild>()
            .insert(GuardTarget(target));
    }
    if let Ok(t_gtf) = target_gtf_q.get(target) {
        pending.markers.push((t_gtf.translation(), OrderMarker::Guard));
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
