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

use super::movement::{MoveTarget, QueuedCommand};
use super::selection::{Selected, apply_ordered_command, ground_hit};
use crate::rendering::camera::RtsCamera;
use crate::units::combat::{SELF_DESTRUCT_DELAY, SelfDestructCountdown};
use crate::units::components::UnitType;
use crate::units::content::definitions::UnitKind;
use crate::units::mechanics::command_fire::CommandFireEvent;
use crate::units::mechanics::deploy::DeployEvent;
use crate::units::mechanics::network_buffer::{DispatchEvent, EnterEvent};

fn ctrl_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
}

pub struct AbilityHotkeyPlugin;

impl Plugin for AbilityHotkeyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AttackGroundMode>().add_systems(
            Update,
            (
                trigger_command_fire_on_hotkey,
                trigger_deploy_on_hotkey,
                trigger_dispatch_on_hotkey,
                trigger_enter_on_hotkey,
                trigger_self_destruct_on_hotkey,
                toggle_attack_ground_mode,
                trigger_attack_ground_click,
                update_attack_ground_cursor,
            ),
        );
    }
}

/// Sticky ground-attack targeting mode: toggled on by pressing `A`,
/// cleared by pressing `A` again, Escape, right-click, or issuing the
/// click that commits the ground target. While `active` the cursor is
/// forced to [`CursorKind::Attack`] and the next left-click is consumed
/// by [`trigger_attack_ground_click`] as a ground-target order for
/// every selected unit.
#[derive(Resource, Default)]
pub struct AttackGroundMode {
    pub active: bool,
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

/// Toggle [`AttackGroundMode`]. `A` flips the flag; Escape and
/// right-click hard-cancel. While active the cursor renders as
/// [`CursorKind::Attack`] (see [`update_attack_ground_cursor`]) and the
/// next left-click on the ground commits the order.
fn toggle_attack_ground_mode(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut mode: ResMut<AttackGroundMode>,
) {
    // Ignore A while Ctrl is down so Ctrl+A (reserved for select-all
    // in future) doesn't toggle this mode.
    if !ctrl_held(&keys) && keys.just_pressed(KeyCode::KeyA) {
        mode.active = !mode.active;
        return;
    }
    if mode.active && (keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right))
    {
        mode.active = false;
    }
}

/// Ground-target click: while [`AttackGroundMode`] is active, the next
/// left-click on the ground issues a move order to that point for every
/// selected unit. Since `combat_system` auto-targets enemies in range
/// every frame, the moving unit opens fire on anything along the way
/// and keeps firing after it arrives — Spring's "attack-move"
/// semantics without a dedicated command kind. Shift queues, matching
/// the move-order behaviour of right-click.
///
/// Commits exit the mode. The selection system short-circuits while
/// `AttackGroundMode.active` so the same click doesn't also clear the
/// current selection.
#[allow(clippy::too_many_arguments)]
fn trigger_attack_ground_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<Entity, With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    move_target_q: Query<(), With<MoveTarget>>,
    mut mode: ResMut<AttackGroundMode>,
    mut commands: Commands,
) {
    if !mode.active || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    for entity in &selected_q {
        apply_ordered_command(
            entity,
            QueuedCommand::Move(target),
            shift,
            &move_target_q,
            &mut commands,
        );
    }
    // Single-shot: one click finishes the order, exit the mode so the
    // next click reverts to normal selection semantics. Shift-click
    // stays in the mode so the player can fan several targets without
    // re-pressing `A` for each.
    if !shift {
        mode.active = false;
    }
}

/// Force the cursor to the Attack glyph while the ground-target mode
/// is active. Uses a high priority so it beats the default context
/// resolver; returning a lower priority when inactive lets the context
/// resolver regain control.
fn update_attack_ground_cursor(
    mode: Res<AttackGroundMode>,
    mut request: ResMut<crate::interaction::cursor::CursorRequest>,
) {
    if mode.active {
        request.set(crate::interaction::cursor::CursorKind::Attack, 10);
    }
}
