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
//! SIGTERM, Dispatch …) arm [`Mode::Ability`] so the next
//! ground click casts the aimed abilities, as `D` over that point would
//! ([`deploy_units`] / [`cast_aimed_abilities`] are shared).

use bevy::prelude::*;

use super::movement::{CommandQueue, GuardTarget, MoveTarget, QueuedCommand};
use super::{clear_orders, replace_order};
use super::selection::{
    OrderMarker, PendingMoveIndicators, PickRayCast, Selected, apply_ordered_command, ground_hit,
    unit_hit,
};
use crate::interaction::cursor::{CursorKind, CursorRequest};
use crate::rendering::camera::RtsCamera;
use crate::ui::hud::command_panel::commands::CmdId;
use crate::units::combat::{AttackGroundOrder, ForcedTarget};
use crate::units::components::{TeamId, UnitType, is_friendly};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::mechanics::command_fire::CommandFireEvent;
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
            (
                trigger_aimed_ability_on_hotkey,
                trigger_deploy_on_hotkey,
                trigger_ability_click,
                trigger_patrol_click,
                trigger_attack_ground_click,
                trigger_attack_move_click,
                trigger_guard_click,
                trigger_move_click,
                trigger_set_target_click,
                update_mode_cursor,
            ),
        );
    }
}

/// Sticky order-targeting modes armed from the hotkeys / command panel.
///
/// At most one mode is armed at a time. The armed mode forces the
/// cursor glyph and the next left-click is consumed by that mode's click
/// handler as an order for the selection:
/// - `AttackGround` (`A` / Attack button): fire at a static ground point.
/// - `AttackMove` (`F` / Fight button): march to a point, fighting en
///   route.
/// - `Patrol` (`P` / button): shuttle between the click point and where
///   the unit started.
/// - `Ability` (Ability buttons only — `D` casts at the cursor directly):
///   cast the selection's aimed abilities at the clicked point.
///
/// Modes are cleared by re-pressing the key, Escape, right-click (both
/// handled by the command panel's input, like Spring's `CGuiHandler`),
/// the committing click, or a Stop order.
#[derive(Resource, Default)]
pub struct OrderCursorModes {
    pub mode: Option<Mode>,
}

impl OrderCursorModes {
    pub fn any_active(&self) -> bool {
        self.mode.is_some()
    }

    pub fn is(&self, mode: Mode) -> bool {
        self.mode == Some(mode)
    }

    /// Arm exactly one mode, replacing any other (they share the cursor).
    pub fn arm(&mut self, mode: Mode) {
        self.mode = Some(mode);
    }

    /// Disarm every mode.
    pub fn clear(&mut self) {
        self.mode = None;
    }

    /// After a committing click: Shift keeps the mode armed.
    fn committed(&mut self, keys: &ButtonInput<KeyCode>) {
        if !shift_held(keys) {
            self.clear();
        }
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
    /// Cast the aimed abilities of the units this panel command lists
    /// (NX Flag, SIGTERM, Dispatch …), like a Spring command goes only
    /// to units that list it.
    Ability(CmdId),
}

impl Mode {
    /// The glyph the armed mode forces.
    fn cursor(self) -> CursorKind {
        match self {
            Mode::Patrol => CursorKind::Patrol,
            Mode::Guard => CursorKind::Defend,
            Mode::Move => CursorKind::Move,
            // Spring renders set-target with the attack crosshair too.
            Mode::AttackGround | Mode::AttackMove | Mode::SetTarget | Mode::Ability(_) => {
                CursorKind::Attack
            }
        }
    }
}

/// Force the armed mode's cursor glyph. The high priority beats the
/// default context resolver; with nothing armed it regains control.
fn update_mode_cursor(modes: Res<OrderCursorModes>, mut request: ResMut<CursorRequest>) {
    if let Some(mode) = modes.mode {
        request.set(mode.cursor(), 10);
    }
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

fn shift_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
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
    if matches!(modes.mode, Some(Mode::Ability(_))) {
        modes.clear();
    }
    cast_aimed_abilities(
        selected_q.iter().map(|(e, u)| (e, u.0)),
        target,
        alt_held(&keys),
        &mut command_fire,
        &mut dispatch,
        &mut commands,
    );
}

/// Click handler for [`Mode::Ability`]: the next left-click
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
    let Some(Mode::Ability(cmd)) = modes.mode else {
        return;
    };
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let filter = cmd.ability_caster();
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
    modes.committed(&keys);
}


/// Ground-target click: while [`Mode::AttackGround`] is
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
    if !modes.is(Mode::AttackGround) || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let shift = shift_held(&keys);
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
        modes.clear();
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
    if !modes.is(Mode::Move) || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let shift = shift_held(&keys);
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
        modes.clear();
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
    if !modes.is(Mode::SetTarget) || !mouse.just_pressed(MouseButton::Left) {
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
    modes.committed(&keys);
}

/// Click handler: while [`Mode::Patrol`] is armed, the next
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
    if !modes.is(Mode::Patrol) || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let shift = shift_held(&keys);

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
        let mut queue = CommandQueue::default();
        queue.push(QueuedCommand::Patrol(current_tf.translation));
        let mut ec = commands.entity(entity);
        replace_order(&mut ec, QueuedCommand::Patrol(target));
        ec.insert(queue);
    }
    pending.markers.push((target, OrderMarker::Patrol));
    if !shift {
        modes.clear();
    }
}

/// Click handler: while [`Mode::AttackMove`] is active, the
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
    if !modes.is(Mode::AttackMove) || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let shift = shift_held(&keys);
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
        modes.clear();
    }
}





/// Click handler: while [`Mode::Guard`] is armed, the next
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
    if !modes.is(Mode::Guard) || !mouse.just_pressed(MouseButton::Left) {
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
    modes.committed(&keys);
}

