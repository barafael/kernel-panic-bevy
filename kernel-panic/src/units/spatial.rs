//! XZ spatial hash over all living units.
//!
//! Rebuilt once per [`GameplaySet::Simulate`] tick from the unit pool and
//! reused by combat target selection, AoE splash, cloak detection, and the
//! AI's datavent scan. Upstream Spring uses `CQuadField`
//! (`rts/Sim/Misc/QuadField.h`) for the same purpose — a uniform grid over
//! the map with per-cell entity lists. We mirror the cell-size choice
//! (~256 elmos) which matches the engagement distances encoded in
//! Kernel Panic's weapon ranges.
//!
//! The snapshot carries just enough per-entity state (team, faction,
//! hp-is-positive) that the hot loops in `combat_system` and `apply_damage`
//! can skip a second `Query::get` per candidate.
//!
//! Lifecycle: positions stay stable between the start of Simulate (where
//! we rebuild) and the end of Resolve (where `apply_damage` drains) — no
//! movement system runs in between, so the snapshot is live for both
//! phases.

use bevy::prelude::*;
use std::collections::HashMap;

use super::combat::Dying;
use super::components::{Faction, Health, TeamId, UnitType};
use super::spawning::Emerging;
use super::unit_registry::UnitRegistry;

/// XZ cell width in elmos. Matches upstream Spring's `CQuadField` default
/// and sits comfortably between the smallest weapon range (~80 elmo melee)
/// and the largest (~700 elmo homebase guns).
pub const SPATIAL_CELL: f32 = 256.0;

/// Flat snapshot carried in each cell. Shape chosen so the common
/// "is-enemy + is-alive + is-in-range + is-flying" check in target picking
/// runs without any follow-up ECS lookup.
#[derive(Clone, Copy)]
pub struct SpatialEntry {
    pub entity: Entity,
    pub pos: Vec3,
    pub team: u8,
    pub faction: Faction,
    pub hp_positive: bool,
    /// Mirrored from the FBI `canFly=1` flag so ground weapons can cheaply
    /// skip flying targets via `NoChaseCategory=VTOL`.
    pub is_flying: bool,
}

/// Uniform XZ grid of [`SpatialEntry`] lists keyed by cell coordinates.
///
/// Buckets are retained between frames (only the contents are cleared), so
/// steady-state rebuilds don't churn the allocator.
#[derive(Resource, Default)]
pub struct SpatialIndex {
    cells: HashMap<(i32, i32), Vec<SpatialEntry>>,
}

impl SpatialIndex {
    fn cell(x: f32, z: f32) -> (i32, i32) {
        (
            (x / SPATIAL_CELL).floor() as i32,
            (z / SPATIAL_CELL).floor() as i32,
        )
    }

    fn clear(&mut self) {
        for bucket in self.cells.values_mut() {
            bucket.clear();
        }
    }

    fn push(&mut self, entry: SpatialEntry) {
        let key = Self::cell(entry.pos.x, entry.pos.z);
        self.cells.entry(key).or_default().push(entry);
    }

    /// Invoke `f` for every entry whose bucket could intersect a sphere
    /// centered at `center` with XZ `radius`. Callers still need to do
    /// the real distance check — this only trims the outer loop.
    pub fn query_radius<F: FnMut(&SpatialEntry)>(&self, center: Vec3, radius: f32, mut f: F) {
        let (cx, cz) = Self::cell(center.x, center.z);
        // +1 to cover the case where `center` sits near a cell edge and a
        // candidate in the next cell is still within `radius`.
        let reach = (radius / SPATIAL_CELL).ceil() as i32 + 1;
        for dx in -reach..=reach {
            for dz in -reach..=reach {
                if let Some(bucket) = self.cells.get(&(cx + dx, cz + dz)) {
                    for entry in bucket {
                        f(entry);
                    }
                }
            }
        }
    }
}

/// Rebuild the index from the current unit pool. Runs at the head of the
/// Simulate set so every downstream system sees a fresh snapshot.
#[allow(clippy::type_complexity)]
pub fn rebuild_spatial_index(
    mut index: ResMut<SpatialIndex>,
    unit_registry: Res<UnitRegistry>,
    units: Query<
        (
            Entity,
            &UnitType,
            &TeamId,
            &Faction,
            &GlobalTransform,
            &Health,
        ),
        (Without<Dying>, Without<Emerging>),
    >,
) {
    index.clear();
    for (entity, unit_type, team, faction, gtf, health) in &units {
        index.push(SpatialEntry {
            entity,
            pos: gtf.translation(),
            team: team.0,
            faction: *faction,
            hp_positive: health.current > 0.0,
            is_flying: unit_registry.can_fly(unit_type.0),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(entity: Entity, pos: Vec3) -> SpatialEntry {
        SpatialEntry {
            entity,
            pos,
            team: 0,
            faction: Faction::System,
            hp_positive: true,
            is_flying: false,
        }
    }

    fn make_entities(n: usize) -> Vec<Entity> {
        let mut world = World::new();
        (0..n).map(|_| world.spawn_empty().id()).collect()
    }

    #[test]
    fn query_returns_points_within_cell_reach() {
        let ents = make_entities(3);
        let mut index = SpatialIndex::default();
        index.push(entry(ents[0], Vec3::new(10.0, 0.0, 10.0)));
        index.push(entry(ents[1], Vec3::new(500.0, 0.0, 500.0)));
        // Far enough that its cell sits well outside ceil(600/256)+1 = 4
        // cells from the origin cell (cell at ~(11, 11) vs query reach 4).
        index.push(entry(ents[2], Vec3::new(3000.0, 0.0, 3000.0)));

        let mut hit = Vec::new();
        index.query_radius(Vec3::ZERO, 600.0, |e| hit.push(e.entity));

        // Entity 0 is in the origin cell; entity 1's cell overlaps the
        // reach even though its distance (≈707) exceeds 600 — the hash
        // is a conservative filter and the caller does the real distance
        // check. Entity 2 is far enough out that its cell is skipped.
        assert!(hit.contains(&ents[0]));
        assert!(hit.contains(&ents[1]));
        assert!(!hit.contains(&ents[2]));
    }

    #[test]
    fn query_wraps_around_negative_coordinates() {
        let ents = make_entities(2);
        let mut index = SpatialIndex::default();
        index.push(entry(ents[0], Vec3::new(-50.0, 0.0, -50.0)));
        index.push(entry(ents[1], Vec3::new(50.0, 0.0, 50.0)));

        let mut hit = Vec::new();
        index.query_radius(Vec3::ZERO, 200.0, |e| hit.push(e.entity));
        assert!(hit.contains(&ents[0]));
        assert!(hit.contains(&ents[1]));
    }

    #[test]
    fn clear_keeps_capacity_but_drops_entries() {
        let ents = make_entities(10);
        let mut index = SpatialIndex::default();
        for (i, e) in ents.iter().enumerate() {
            index.push(entry(*e, Vec3::new(i as f32 * 10.0, 0.0, 0.0)));
        }
        index.clear();
        let mut hit = 0;
        index.query_radius(Vec3::ZERO, 10_000.0, |_| hit += 1);
        assert_eq!(hit, 0);
        // Buckets retained — steady-state rebuilds shouldn't realloc.
        assert!(!index.cells.is_empty());
    }
}
