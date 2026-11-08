//! Unit groups (`CGroupHandler::GroupCommand`): bind the current
//! selection to a digit with `Ctrl+0..9` (`Ctrl+Shift` adds), recall it
//! with the bare digit — pressed again while the group is the selection,
//! it also centres the camera on it. `Shift+digit` adds the group to the
//! selection, `Alt+digit` toggles its members in it. A unit is in one
//! group at most.

use crate::map_loading::MatchScopeAppExt;
use bevy::prelude::*;

use super::core::{Selected, SelectionSet};

pub(super) struct UnitGroupsPlugin;

impl Plugin for UnitGroupsPlugin {
    fn build(&self, app: &mut App) {
        // Control groups hold the old match's units.
        app.init_match_resource::<UnitGroups>()
            .add_systems(Update, handle_unit_groups.in_set(SelectionSet::Select));
    }
}

/// Number of recallable groups, indexed by digit key 0..9
/// (`KeyBindings.cpp:138` binds `Any+0` to `group0`).
pub const GROUP_COUNT: usize = 10;

/// Stored selections, one per digit. Dead entities stay until the next
/// assign — `handle_unit_groups` filters via the live entity query at
/// recall time (despawned, or still playing a death script: Spring's
/// `CGroup::RemoveUnit` drops a unit at its death), so a group whose
/// units have all died recalls to nothing rather than reselecting
/// corpses or panicking on a stale `Entity`.
///
/// A unit belongs to one group at most (`CUnit::SetGroup` leaves the old
/// one): assigning it elsewhere takes it out of its previous slot.
#[derive(Resource, Default)]
pub struct UnitGroups {
    slots: [Vec<Entity>; GROUP_COUNT],
}

impl UnitGroups {
    /// Ctrl+digit: the group becomes exactly `entities`.
    pub fn assign(&mut self, slot: usize, entities: impl IntoIterator<Item = Entity>) {
        if slot < GROUP_COUNT {
            self.slots[slot].clear();
            self.add(slot, entities);
        }
    }

    /// Ctrl+Shift+digit: `entities` join the group (leaving their old ones).
    pub fn add(&mut self, slot: usize, entities: impl IntoIterator<Item = Entity>) {
        if slot >= GROUP_COUNT {
            return;
        }
        for entity in entities {
            for (i, members) in self.slots.iter_mut().enumerate() {
                if i != slot {
                    members.retain(|&e| e != entity);
                }
            }
            if !self.slots[slot].contains(&entity) {
                self.slots[slot].push(entity);
            }
        }
    }

    pub fn recall(&self, slot: usize) -> &[Entity] {
        self.slots.get(slot).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn handle_unit_groups(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<Entity, With<Selected>>,
    all_units_q: Query<
        (Entity, &Transform),
        (
            With<crate::units::components::UnitType>,
            Without<crate::units::combat::Dying>,
        ),
    >,
    mut groups: ResMut<UnitGroups>,
    mut cameras: Query<&mut crate::rendering::camera::RtsCameraState>,
    mut commands: Commands,
) {
    let Some((slot, key)) = digit_just_pressed(&keys) else {
        return;
    };
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);

    // `CGroupHandler::GroupCommand`: Ctrl sets the group (with Shift it
    // adds), Shift adds the group to the selection, Alt toggles its
    // members in it, a plain press selects it — and focuses the camera
    // when it already is the selection.
    if ctrl {
        let live: Vec<Entity> = selected_q.iter().collect();
        if shift {
            groups.add(slot, live);
        } else {
            groups.assign(slot, live);
        }
        info!(
            "Assigned {} units to group {}",
            groups.recall(slot).len(),
            key
        );
        return;
    }

    let members: Vec<Entity> = groups
        .recall(slot)
        .iter()
        .copied()
        // Skip stale entities — reactivating a slot must not revive
        // despawned ids; the unit query gates us cleanly.
        .filter(|&e| all_units_q.get(e).is_ok())
        .collect();
    if members.is_empty() {
        return;
    }

    if shift {
        for &entity in &members {
            commands.entity(entity).insert(Selected);
        }
        return;
    }
    if alt {
        for &entity in &members {
            if selected_q.get(entity).is_ok() {
                commands.entity(entity).remove::<Selected>();
            } else {
                commands.entity(entity).insert(Selected);
            }
        }
        return;
    }

    let already_selected = selected_q.iter().count() == members.len()
        && members.iter().all(|&e| selected_q.get(e).is_ok());
    if already_selected {
        // `CalculateCenter`: the mean of the members' positions.
        let sum: Vec3 = members
            .iter()
            .filter_map(|&e| all_units_q.get(e).ok())
            .map(|(_, t)| t.translation)
            .sum();
        let center = sum / members.len() as f32;
        for mut cam in &mut cameras {
            cam.focus.x = center.x;
            cam.focus.z = center.z;
        }
    }
    for entity in &selected_q {
        commands.entity(entity).remove::<Selected>();
    }
    for &entity in &members {
        commands.entity(entity).insert(Selected);
    }
}

/// `1..=9` map to slots `1..=9`, `0` to slot `0`.
fn digit_just_pressed(keys: &ButtonInput<KeyCode>) -> Option<(usize, u8)> {
    const DIGITS: [(KeyCode, u8); GROUP_COUNT] = [
        (KeyCode::Digit0, 0),
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
        .map(|(_, n)| (*n as usize, *n))
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

    /// Recalling a group reselects only living members: a unit playing
    /// its death script left the group at the kill (`CGroup::RemoveUnit`).
    #[test]
    fn recall_skips_dying_members() {
        use crate::units::components::UnitType;
        use crate::units::content::definitions::UnitKind;
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        let alive = world.spawn((UnitType(UnitKind::Bit), Transform::default())).id();
        let dying = world
            .spawn((
                UnitType(UnitKind::Bit),
                Transform::default(),
                crate::units::combat::Dying { timer: 1.0 },
            ))
            .id();
        let mut groups = UnitGroups::default();
        groups.assign(1, [alive, dying]);
        world.insert_resource(groups);
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::Digit1);
        world.insert_resource(keys);
        world.run_system_once(handle_unit_groups).unwrap();
        assert!(world.get::<Selected>(alive).is_some());
        assert!(world.get::<Selected>(dying).is_none());
    }

    /// A unit is in one group: assigning it elsewhere moves it; adding
    /// keeps the rest of the group.
    #[test]
    fn a_unit_has_one_group() {
        let mut groups = UnitGroups::default();
        let e1 = Entity::from_raw_u32(1).unwrap();
        let e2 = Entity::from_raw_u32(2).unwrap();
        groups.assign(1, [e1, e2]);
        groups.assign(2, [e1]);
        assert_eq!(groups.recall(1), &[e2]);
        assert_eq!(groups.recall(2), &[e1]);
        groups.add(2, [e2]);
        assert_eq!(groups.recall(2), &[e1, e2]);
        assert!(groups.recall(1).is_empty());
        groups.assign(0, [e1]);
        assert_eq!(groups.recall(0), &[e1]);
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
