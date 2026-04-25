//! Unit groups: bind the current selection to a digit with `Ctrl+1..9`,
//! recall it with bare `1..9`. Plain recall replaces the selection;
//! `Shift+1..9` adds the group to the existing selection.
//!
//! Mirrors the Spring / StarCraft hotkey contract — there's no
//! "center camera on group" yet (would slot in via [`RtsCameraState`]
//! if the group has any live members). Done as a follow-up to plan
//! §10.1.

use bevy::prelude::*;

use super::core::{Selected, SelectionSet};

pub(super) struct UnitGroupsPlugin;

impl Plugin for UnitGroupsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UnitGroups>()
            .add_systems(Update, handle_unit_groups.in_set(SelectionSet::Select));
    }
}

/// Number of recallable groups, indexed by digit key 1..9.
pub const GROUP_COUNT: usize = 9;

/// Stored selections, one per digit. Despawned entities stay until the
/// next assign — `handle_unit_groups` filters via the live entity query
/// at recall time, so a group whose units have all died recalls to
/// nothing rather than panicking on a stale `Entity`.
#[derive(Resource, Default)]
pub struct UnitGroups {
    slots: [Vec<Entity>; GROUP_COUNT],
}

impl UnitGroups {
    pub fn assign(&mut self, slot: usize, entities: impl IntoIterator<Item = Entity>) {
        if slot < GROUP_COUNT {
            self.slots[slot] = entities.into_iter().collect();
        }
    }

    pub fn recall(&self, slot: usize) -> &[Entity] {
        self.slots.get(slot).map(Vec::as_slice).unwrap_or(&[])
    }
}

fn handle_unit_groups(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<Entity, With<Selected>>,
    all_units_q: Query<Entity, With<crate::units::components::UnitType>>,
    mut groups: ResMut<UnitGroups>,
    mut commands: Commands,
) {
    let Some((slot, key)) = digit_just_pressed(&keys) else {
        return;
    };
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

    if ctrl {
        let live: Vec<Entity> = selected_q.iter().collect();
        groups.assign(slot, live);
        info!(
            "Assigned {} units to group {}",
            groups.recall(slot).len(),
            key
        );
        return;
    }

    let members = groups.recall(slot);
    if members.is_empty() {
        return;
    }

    if !shift {
        for entity in &selected_q {
            commands.entity(entity).remove::<Selected>();
        }
    }
    for &entity in members {
        // Skip stale entities — `Commands::entity` panics on despawned
        // ids in some Bevy versions; the unit query gates us cleanly.
        if all_units_q.get(entity).is_ok() {
            commands.entity(entity).insert(Selected);
        }
    }
}

/// `1..=9` map to slots `0..=8`. `0` is intentionally unbound — Spring
/// reserves it for "all units" via `Ctrl+A` (not implemented here).
fn digit_just_pressed(keys: &ButtonInput<KeyCode>) -> Option<(usize, u8)> {
    const DIGITS: [(KeyCode, u8); GROUP_COUNT] = [
        (KeyCode::Digit1, 1),
        (KeyCode::Digit2, 2),
        (KeyCode::Digit3, 3),
        (KeyCode::Digit4, 4),
        (KeyCode::Digit5, 5),
        (KeyCode::Digit6, 6),
        (KeyCode::Digit7, 7),
        (KeyCode::Digit8, 8),
        (KeyCode::Digit9, 9),
    ];
    DIGITS
        .iter()
        .find(|(code, _)| keys.just_pressed(*code))
        .map(|(_, n)| ((*n - 1) as usize, *n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_then_recall_returns_same_entities() {
        let mut groups = UnitGroups::default();
        let e1 = Entity::from_raw_u32(1).unwrap();
        let e2 = Entity::from_raw_u32(2).unwrap();
        groups.assign(0, [e1, e2]);
        assert_eq!(groups.recall(0), &[e1, e2]);
    }

    #[test]
    fn recall_empty_slot_returns_empty() {
        let groups = UnitGroups::default();
        assert!(groups.recall(3).is_empty());
    }

    #[test]
    fn out_of_range_slot_is_no_op() {
        let mut groups = UnitGroups::default();
        let e1 = Entity::from_raw_u32(1).unwrap();
        groups.assign(99, [e1]);
        assert!(groups.recall(99).is_empty());
    }
}
