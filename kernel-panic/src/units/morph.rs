//! Bug ↔ Exploit morph.
//!
//! Bugs can deploy into stationary Exploits (long-range artillery that
//! deal more damage the farther their target is). The player triggers
//! morph via the `E` hotkey; health is transferred proportionally so
//! a 50%-HP Bug becomes a 50%-HP Exploit.

use bevy::prelude::*;

use super::components::{Faction, Health, TeamId, UnitType};
use super::definitions::UnitKind;

/// Event: the listed unit should morph to its paired form. The morph
/// system resolves the target kind from the source.
#[derive(Message, Debug, Clone, Copy)]
pub struct MorphEvent {
    pub entity: Entity,
}

/// Determine the target UnitKind when morphing `kind`, if any.
fn morph_target(kind: UnitKind) -> Option<UnitKind> {
    match kind {
        UnitKind::Bug => Some(UnitKind::Exploit),
        UnitKind::Exploit => Some(UnitKind::Bug),
        _ => None,
    }
}

/// System: process morph events by despawning the source and spawning
/// its pair with proportional HP at the same position.
pub fn process_morph(
    mut events: MessageReader<MorphEvent>,
    query: Query<(&UnitType, &Faction, &TeamId, &Transform, &Health)>,
    unit_registry: Res<super::unit_registry::UnitRegistry>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<super::meshes::S3OModelCache>,
    mut cob_cache: ResMut<super::animation::CobFileCache>,
    invisible_mat: Res<super::spawning::SelectionVolumeMaterial>,
    mut commands: Commands,
) {
    for event in events.read() {
        let Ok((unit, faction, team, transform, health)) = query.get(event.entity) else {
            continue;
        };
        let Some(target_kind) = morph_target(unit.0) else {
            continue;
        };

        // HP carry-over: preserve the health fraction. A 1-HP Bug
        // morphs into a 1-HP-equivalent Exploit, not a pristine one.
        let hp_fraction = health.fraction().clamp(0.01, 1.0);
        let new_max = unit_registry.max_health(target_kind);
        let new_current = (new_max * hp_fraction).max(1.0);

        commands.entity(event.entity).despawn();

        let spawned = super::spawning::spawn_unit(
            target_kind,
            *faction,
            team.0,
            transform.translation,
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut model_cache,
            &mut cob_cache,
            &invisible_mat,
            &unit_registry,
        );
        commands.entity(spawned).insert(Health {
            current: new_current,
            max: new_max,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bug_and_exploit_are_mutual_morph_pairs() {
        assert_eq!(morph_target(UnitKind::Bug), Some(UnitKind::Exploit));
        assert_eq!(morph_target(UnitKind::Exploit), Some(UnitKind::Bug));
    }

    #[test]
    fn other_units_do_not_morph() {
        assert_eq!(morph_target(UnitKind::Bit), None);
        assert_eq!(morph_target(UnitKind::Kernel), None);
        assert_eq!(morph_target(UnitKind::Packet), None);
    }
}
