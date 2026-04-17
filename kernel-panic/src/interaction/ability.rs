//! Command-fire ability hotkeys.
//!
//! Pressing `Q` with a caster selected (Pointer / Obelisk) fires that
//! unit's command ability at the cursor's ground-hit position. The
//! cast goes through `CommandFireEvent` so `units::command_fire` owns
//! cooldown, radius, and damage resolution — this layer just maps
//! input to an intent.

use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use super::selection::{Selected, ground_hit};
use crate::rendering::camera::RtsCamera;
use crate::units::command_fire::CommandFireEvent;
use crate::units::components::UnitType;
use crate::units::definitions::UnitKind;
use crate::units::morph::MorphEvent;

pub struct AbilityHotkeyPlugin;

impl Plugin for AbilityHotkeyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (trigger_command_fire_on_hotkey, trigger_morph_on_hotkey),
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

fn trigger_command_fire_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    mut ev: MessageWriter<CommandFireEvent>,
) {
    if !keys.just_pressed(KeyCode::KeyQ) {
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
    matches!(kind, UnitKind::Pointer | UnitKind::Obelisk)
}
