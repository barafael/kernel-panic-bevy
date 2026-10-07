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
//! The snapshot carries just enough per-entity state (team,
//! hp-is-positive) that the hot loops in `combat_system` and `apply_damage`
//! can skip a second `Query::get` per candidate.
//!
//! Lifecycle: positions stay stable between the start of Simulate (where
//! we rebuild) and the end of Resolve (where `apply_damage` drains) — no
//! movement system runs in between, so the snapshot is live for both
//! phases.

use bevy::platform::collections::HashMap;
use bevy::prelude::*;

use super::combat::Dying;
use super::components::{Health, TeamId, UnitType};
use super::content::definitions::UnitKind;
use super::lifecycle::spawning::Emerging;
use super::mechanics::cloak::{Cloaked, DetectedBy, hidden_from};

/// XZ cell width in elmos. Matches upstream Spring's `CQuadField` default
/// and sits comfortably between the smallest weapon range (~80 elmo melee)
/// and the largest (~700 elmo homebase guns).
pub const SPATIAL_CELL: f32 = 256.0;

/// Horizontal (XZ) distance² — heights differ across terrain but
/// upstream ranges are XZ cylinders (`GetUnitsInCylinder`, weapon range
/// checks). Shared by ability range gates and the AI's range reasoning.
pub fn flat_dist_sq(a: Vec3, b: Vec3) -> f32 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    dx * dx + dz * dz
}

/// Flat snapshot carried in each cell. Shape chosen so the common
/// "is-enemy + is-alive + is-in-range + is-flying" check in target picking
/// runs without any follow-up ECS lookup.
/// Largest radius `query_radius` scans (elmos): past the biggest map's
/// diagonal every bucket is visited anyway.
const MAX_QUERY_RADIUS: f32 = 65_536.0;

#[derive(Clone, Copy)]
pub struct SpatialEntry {
    pub entity: Entity,
    pub pos: Vec3,
    /// Model (collision-volume) radius: explosions measure their
    /// distance to this sphere's surface.
    pub hit_radius: f32,
    /// Height of the collision sphere's centre above `pos` (model
    /// midpoint, `unit->midPos`).
    pub mid_y: f32,
    pub team: u8,
    pub kind: UnitKind,
    pub hp_positive: bool,
    /// Mirrored from the FBI `canFly=1` flag so ground weapons can cheaply
    /// skip flying targets via `NoChaseCategory=VTOL`.
    pub is_flying: bool,
    /// Carries the [`Cloaked`] marker right now (Logic Bomb, buried Worm).
    pub cloaked: bool,
    /// [`DetectedBy`] team mask — which teams' detectors currently see
    /// this cloaked unit. Zero (and ignored) for uncloaked units.
    pub detected_by: u64,
}

impl SpatialEntry {
    /// Whether auto-targeting from `team` may pick this entry: a cloaked
    /// unit is invisible to teams without a detector in range. See
    /// [`crate::units::mechanics::cloak::hidden_from`].
    pub fn targetable_by(&self, team: u8) -> bool {
        !hidden_from(self.cloaked, self.detected_by, team)
    }

    /// World-space centre of the collision sphere (`unit->midPos`).
    pub fn mid_pos(&self) -> Vec3 {
        self.pos + Vec3::Y * self.mid_y
    }

    /// `CollisionVolume::GetPointSurfaceDistance`: distance from `point`
    /// to the sphere's surface, 0 inside it.
    pub fn surface_distance(&self, point: Vec3) -> f32 {
        (self.mid_pos().distance(point) - self.hit_radius).max(0.0)
    }
}

/// Uniform XZ grid of [`SpatialEntry`] lists keyed by cell coordinates.
///
/// Buckets are retained between frames (only the contents are cleared), so
/// steady-state rebuilds don't churn the allocator.
#[derive(Resource, Default)]
pub struct SpatialIndex {
    cells: HashMap<(i32, i32), Vec<SpatialEntry>>,
    /// Largest `hit_radius` in the index: a query for every unit whose
    /// sphere reaches within `r` of a point spans `r + max_hit_radius`.
    max_hit_radius: f32,
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
        self.max_hit_radius = 0.0;
    }

    fn push(&mut self, entry: SpatialEntry) {
        let key = Self::cell(entry.pos.x, entry.pos.z);
        self.max_hit_radius = self.max_hit_radius.max(entry.hit_radius);
        self.cells.entry(key).or_default().push(entry);
    }

    pub fn max_hit_radius(&self) -> f32 {
        self.max_hit_radius
    }

    /// Test-only: insert a fully-formed entry directly. Production
    /// callers should use [`rebuild_spatial_index`] instead, which
    /// derives entries from a live world snapshot.
    #[cfg(test)]
    pub fn insert_for_test(&mut self, entry: SpatialEntry) {
        self.push(entry);
    }

    /// Invoke `f` for every entry whose bucket intersects the XZ square
    /// bounding a circle of `radius` around `center`. Callers still need
    /// to do the real distance check — this only trims the outer loop.
    pub fn query_radius<F: FnMut(&SpatialEntry)>(&self, center: Vec3, radius: f32, mut f: F) {
        // Bounded: an infinite radius (a definition file's `range=inf`)
        // would otherwise walk the whole i32 cell range.
        let radius = if radius.is_finite() {
            radius.min(MAX_QUERY_RADIUS)
        } else {
            MAX_QUERY_RADIUS
        };
        let (x0, z0) = Self::cell(center.x - radius, center.z - radius);
        let (x1, z1) = Self::cell(center.x + radius, center.z + radius);
        for x in x0..=x1 {
            for z in z0..=z1 {
                if let Some(bucket) = self.cells.get(&(x, z)) {
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
    units: Query<
        (
            Entity,
            &UnitType,
            &super::components::UnitStats,
            &TeamId,
            &GlobalTransform,
            &Health,
            Has<Cloaked>,
            Option<&DetectedBy>,
        ),
        // `Without<Emerging>` is a deliberate divergence from upstream:
        // Spring nanoframes are targetable mid-build, but here a Rising
        // unit is up to `EMERGE_DEPTH` elmos underground — shooting at
        // it means aiming through terrain. Kept out of the index (and
        // so out of auto-targeting, AI target scans and splash sweeps)
        // until it surfaces; see `production_system`'s `emerge_lead`
        // note. Revisit with per-piece aim heights if nanoframe
        // harassment becomes wanted.
        (Without<Dying>, Without<Emerging>),
    >,
) {
    index.clear();
    for (entity, unit_type, stats, team, gtf, health, cloaked, detected_by) in &units {
        index.push(SpatialEntry {
            entity,
            pos: gtf.translation(),
            hit_radius: stats.hit_radius,
            mid_y: stats.mid_y,
            team: team.0,
            kind: unit_type.0,
            hp_positive: health.current > 0.0,
            is_flying: stats.can_fly,
            cloaked,
            detected_by: detected_by.map_or(0, |d| d.0),
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
            hit_radius: 0.0,
            mid_y: 0.0,
            team: 0,
            kind: UnitKind::Bit,
            hp_positive: true,
            is_flying: false,
            cloaked: false,
            detected_by: 0,
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
        // Far enough that its cell (11, 11) lies outside the query's
        // bounding cells -3..=2.
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
