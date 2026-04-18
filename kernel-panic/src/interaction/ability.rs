//! Ability hotkeys.
//!
//! Pressing `D` with a caster selected fires that unit's "ability" at
//! the cursor — whatever the unit kind treats as its ability:
//! - Pointer / Obelisk / Firewall / Byte / Terminal → command-fire weapon
//!   (NX Flag, Infection gas, etc.) routed through `CommandFireEvent`.
//! - Port / Connection → Dispatch packets (with ALT modifier for
//!   "drain the buffer", mirroring upstream `network_dispatch.lua`).
//!
//! `E` morphs Bug ↔ Exploit. `R` lets a Packet re-Enter the buffer.

use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use super::selection::{Selected, ground_hit};
use crate::rendering::camera::RtsCamera;
use crate::units::command_fire::CommandFireEvent;
use crate::units::components::UnitType;
use crate::units::definitions::UnitKind;
use crate::units::morph::MorphEvent;
use crate::units::network_buffer::{DispatchEvent, EnterEvent};

pub struct AbilityHotkeyPlugin;

impl Plugin for AbilityHotkeyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                trigger_command_fire_on_hotkey,
                trigger_morph_on_hotkey,
                trigger_dispatch_on_hotkey,
                trigger_enter_on_hotkey,
            ),
        );
    }
}

fn trigger_morph_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    mut ev: MessageWriter<MorphEvent>,
) {
    if !keys.just_pressed(KeyCode::KeyE) {
        return;
    }
    for (entity, unit) in &selected_q {
        if matches!(unit.0, UnitKind::Bug | UnitKind::Exploit) {
            ev.write(MorphEvent { entity });
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
                .insert(crate::units::network_buffer::AutoDispatch { target });
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

    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };

    for (entity, unit) in &selected_q {
        if has_ability(unit.0) {
            ev.write(CommandFireEvent {
                attacker: entity,
                target,
            });
        }
    }
}

fn has_ability(kind: UnitKind) -> bool {
    matches!(
        kind,
        UnitKind::Pointer
            | UnitKind::Obelisk
            | UnitKind::Firewall
            | UnitKind::Byte
            | UnitKind::Terminal
    )
}
