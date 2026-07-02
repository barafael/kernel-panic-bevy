//! Per-unit volumetric collision shape.
//!
//! plan.md §1.8 calls out that the port currently models every unit
//! as a sphere — the S3O bounding sphere `hit_radius` for the primary
//! hit gate, and the footprint-derived `radius` for unit-unit
//! separation. Spring's `CCollisionHandler` actually tests against a
//! per-unit primitive: sphere, axis-aligned cylinder, or AABB.
//!
//! [`CollisionVolume`] is the data side of that fix. It is cached at
//! spawn time from the S3O AABB / authored radius so future systems
//! (projectile mid-flight collision, shield interception, per-shot
//! miss semantics) can do volume-aware tests without re-walking the
//! piece tree.
//!
//! The variant currently chosen at spawn is always [`CollisionVolume::Sphere`]
//! — matching today's behaviour. Switching a unit to `Cylinder` or
//! `Aabb` only requires updating the spawn classifier; every consumer
//! reads through the same intersection API and picks up the new
//! geometry transparently.
//!
//! All math is in unit-local coordinates centred on the unit's
//! transform origin: callers pass the unit's world `center` and a
//! world-space ray / point. The volume itself stores only its size.

use bevy::prelude::*;

/// Bounding primitive used for hit / shield / projectile-collision
/// tests. All shapes are origin-centred at the owning unit's transform
/// — orientation is implicit (cylinders are Y-axis aligned, AABB is
/// world-aligned). That mirrors Spring's modelling convention.
///
/// Today only `Sphere` is constructed at spawn time; the other variants
/// are part of the foundation API so a future per-unit classifier can
/// pick them without reshaping the consumer side.
#[allow(dead_code)] // Cylinder / Aabb wired through the public API; spawn-side classifier still picks Sphere only.
#[derive(Component, Copy, Clone, Debug, PartialEq)]
pub enum CollisionVolume {
    Sphere {
        radius: f32,
    },
    /// Y-axis cylinder: any point with `xz_dist <= radius` and
    /// `|y| <= half_height` is inside.
    Cylinder {
        radius: f32,
        half_height: f32,
    },
    /// World-axis-aligned box: any point with `|x| <= half.x &&
    /// |y| <= half.y && |z| <= half.z` (in unit-local space) is inside.
    Aabb {
        half_extents: Vec3,
    },
}

#[allow(dead_code)] // Constructors and bounding-radius helper are part of the public foundation API.
impl CollisionVolume {
    pub fn sphere(radius: f32) -> Self {
        Self::Sphere { radius }
    }

    pub fn cylinder_y(radius: f32, half_height: f32) -> Self {
        Self::Cylinder {
            radius,
            half_height,
        }
    }

    pub fn aabb(half_extents: Vec3) -> Self {
        Self::Aabb { half_extents }
    }

    /// Derive a sphere collision volume from an S3O model's authored
    /// bounding sphere. This is the default classifier — every unit
    /// gets a sphere unless a future per-unit override picks
    /// cylinder / AABB.
    pub fn from_s3o_radius(radius: f32) -> Self {
        Self::Sphere { radius }
    }

    /// Worst-case bounding radius of the volume — useful as a
    /// broad-phase reject before running the precise test.
    pub fn bounding_radius(&self) -> f32 {
        match *self {
            Self::Sphere { radius } => radius,
            Self::Cylinder {
                radius,
                half_height,
            } => (radius * radius + half_height * half_height).sqrt(),
            Self::Aabb { half_extents } => half_extents.length(),
        }
    }

    /// Containment test in world space. `center` is the unit's
    /// transform origin; `point` is the world-space probe.
    pub fn contains(&self, center: Vec3, point: Vec3) -> bool {
        let local = point - center;
        match *self {
            Self::Sphere { radius } => local.length_squared() <= radius * radius,
            Self::Cylinder {
                radius,
                half_height,
            } => {
                let xz_sq = local.x * local.x + local.z * local.z;
                xz_sq <= radius * radius && local.y.abs() <= half_height
            }
            Self::Aabb { half_extents } => {
                local.x.abs() <= half_extents.x
                    && local.y.abs() <= half_extents.y
                    && local.z.abs() <= half_extents.z
            }
        }
    }

    /// Mid-flight projectile collision. Returns the smallest `t` in
    /// `[0, 1]` such that `start.lerp(end, t)` is inside the volume,
    /// or `None` if the segment misses entirely.
    ///
    /// This is the foundation for plan.md §4.2 (projectile physics)
    /// and §4.7 (shield interception): instead of waiting for a bolt
    /// to "reach" its target by distance, the tick system can ask each
    /// candidate unit's `CollisionVolume` whether the bolt's segment
    /// for the current frame *crosses* it.
    pub fn ray_segment_hit(&self, center: Vec3, start: Vec3, end: Vec3) -> Option<f32> {
        let dir = end - start;
        let length_sq = dir.length_squared();
        if length_sq < 1e-12 {
            // Zero-length segment — fall back to a point test.
            return self.contains(center, start).then_some(0.0);
        }

        match *self {
            Self::Sphere { radius } => sphere_segment_hit(center, radius, start, dir, length_sq),
            Self::Cylinder {
                radius,
                half_height,
            } => cylinder_segment_hit(center, radius, half_height, start, dir, length_sq),
            Self::Aabb { half_extents } => aabb_segment_hit(center, half_extents, start, dir),
        }
    }
}

// --- Sphere ----------------------------------------------------------

fn sphere_segment_hit(
    center: Vec3,
    radius: f32,
    start: Vec3,
    dir: Vec3,
    length_sq: f32,
) -> Option<f32> {
    // Solve |start + t*dir - center|² = radius² for t ∈ [0, 1].
    let m = start - center;
    let b = m.dot(dir);
    let c = m.length_squared() - radius * radius;
    if c > 0.0 && b > 0.0 {
        // Origin outside the sphere and ray pointing away.
        return None;
    }
    let discr = b * b - length_sq * c;
    if discr < 0.0 {
        return None;
    }
    // Earliest t along the segment, normalised against |dir|².
    let sqrt_d = discr.sqrt();
    let t_unscaled = -b - sqrt_d;
    let t = if t_unscaled < 0.0 {
        // Origin already inside the sphere.
        0.0
    } else {
        t_unscaled / length_sq
    };
    (t <= 1.0).then_some(t.max(0.0))
}

// --- Cylinder (Y-axis) -----------------------------------------------

fn cylinder_segment_hit(
    center: Vec3,
    radius: f32,
    half_height: f32,
    start: Vec3,
    dir: Vec3,
    _length_sq: f32,
) -> Option<f32> {
    // Test against the infinite XZ-cylinder, then clip to the Y slab.
    let m = start - center;
    let mxz = Vec2::new(m.x, m.z);
    let dxz = Vec2::new(dir.x, dir.z);
    let dxz_len_sq = dxz.length_squared();

    let mut t_enter = 0.0_f32;
    let mut t_exit = 1.0_f32;

    if dxz_len_sq < 1e-12 {
        // Vertical segment: must already be inside the XZ disc.
        if mxz.length_squared() > radius * radius {
            return None;
        }
    } else {
        let b = mxz.dot(dxz);
        let c = mxz.length_squared() - radius * radius;
        let discr = b * b - dxz_len_sq * c;
        if discr < 0.0 {
            return None;
        }
        let sqrt_d = discr.sqrt();
        let t1 = (-b - sqrt_d) / dxz_len_sq;
        let t2 = (-b + sqrt_d) / dxz_len_sq;
        t_enter = t_enter.max(t1);
        t_exit = t_exit.min(t2);
    }

    // Clip to the Y slab.
    let y_top = half_height;
    let y_bot = -half_height;
    if dir.y.abs() < 1e-12 {
        if m.y < y_bot || m.y > y_top {
            return None;
        }
    } else {
        let inv_dy = 1.0 / dir.y;
        let mut ty1 = (y_bot - m.y) * inv_dy;
        let mut ty2 = (y_top - m.y) * inv_dy;
        if ty1 > ty2 {
            std::mem::swap(&mut ty1, &mut ty2);
        }
        t_enter = t_enter.max(ty1);
        t_exit = t_exit.min(ty2);
    }

    if t_enter > t_exit {
        return None;
    }
    let t = t_enter.max(0.0);
    (t <= 1.0).then_some(t)
}

// --- AABB ------------------------------------------------------------

fn aabb_segment_hit(center: Vec3, half: Vec3, start: Vec3, dir: Vec3) -> Option<f32> {
    // Slab method.
    let local = start - center;
    let mut t_enter = 0.0_f32;
    let mut t_exit = 1.0_f32;
    for axis in 0..3 {
        let lo = -half[axis];
        let hi = half[axis];
        let o = local[axis];
        let d = dir[axis];
        if d.abs() < 1e-12 {
            if o < lo || o > hi {
                return None;
            }
        } else {
            let inv = 1.0 / d;
            let mut t1 = (lo - o) * inv;
            let mut t2 = (hi - o) * inv;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            t_enter = t_enter.max(t1);
            t_exit = t_exit.min(t2);
            if t_enter > t_exit {
                return None;
            }
        }
    }
    let t = t_enter.max(0.0);
    (t <= 1.0).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    // -- Sphere ---------------------------------------------------------

    #[test]
    fn sphere_contains_centre_and_surface() {
        let v = CollisionVolume::sphere(10.0);
        assert!(v.contains(Vec3::ZERO, Vec3::ZERO));
        assert!(v.contains(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0)));
        assert!(!v.contains(Vec3::ZERO, Vec3::new(10.001, 0.0, 0.0)));
    }

    #[test]
    fn sphere_segment_hit_through_centre() {
        let v = CollisionVolume::sphere(5.0);
        let t = v
            .ray_segment_hit(
                Vec3::ZERO,
                Vec3::new(-10.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 0.0),
            )
            .expect("must hit");
        // First contact at x = -5 along a 20-unit segment from x = -10.
        assert!(approx(t, 5.0 / 20.0), "t was {t}");
    }

    #[test]
    fn sphere_segment_miss_above() {
        let v = CollisionVolume::sphere(5.0);
        assert!(
            v.ray_segment_hit(
                Vec3::ZERO,
                Vec3::new(-10.0, 6.0, 0.0),
                Vec3::new(10.0, 6.0, 0.0)
            )
            .is_none()
        );
    }

    #[test]
    fn sphere_segment_origin_inside() {
        let v = CollisionVolume::sphere(5.0);
        let t = v
            .ray_segment_hit(Vec3::ZERO, Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0))
            .expect("must hit");
        assert!(approx(t, 0.0), "t was {t}");
    }

    #[test]
    fn sphere_segment_pointing_away() {
        let v = CollisionVolume::sphere(5.0);
        // Origin behind the sphere along +X, ray pointing +X.
        assert!(
            v.ray_segment_hit(
                Vec3::ZERO,
                Vec3::new(20.0, 0.0, 0.0),
                Vec3::new(40.0, 0.0, 0.0)
            )
            .is_none()
        );
    }

    // -- Cylinder -------------------------------------------------------

    #[test]
    fn cylinder_contains_within_xz_and_y() {
        let v = CollisionVolume::cylinder_y(4.0, 8.0);
        assert!(v.contains(Vec3::ZERO, Vec3::new(2.0, 7.0, 2.0))); // |xz| ≈ 2.83, |y| 7 ≤ 8
        assert!(!v.contains(Vec3::ZERO, Vec3::new(3.0, 0.0, 3.0))); // |xz| ≈ 4.24, > 4
        assert!(!v.contains(Vec3::ZERO, Vec3::new(0.0, 9.0, 0.0))); // y too high
        assert!(!v.contains(Vec3::ZERO, Vec3::new(5.0, 0.0, 0.0))); // xz too far
    }

    #[test]
    fn cylinder_segment_hit_horizontal() {
        let v = CollisionVolume::cylinder_y(4.0, 8.0);
        let t = v
            .ray_segment_hit(
                Vec3::ZERO,
                Vec3::new(-10.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 0.0),
            )
            .expect("must hit");
        // First contact at x = -4 over a 20-unit segment from x = -10.
        assert!(approx(t, 6.0 / 20.0), "t was {t}");
    }

    #[test]
    fn cylinder_segment_miss_above_cap() {
        let v = CollisionVolume::cylinder_y(4.0, 8.0);
        assert!(
            v.ray_segment_hit(
                Vec3::ZERO,
                Vec3::new(-10.0, 9.0, 0.0),
                Vec3::new(10.0, 9.0, 0.0)
            )
            .is_none()
        );
    }

    #[test]
    fn cylinder_segment_diagonal_through() {
        let v = CollisionVolume::cylinder_y(4.0, 8.0);
        // Plunge from above-left to below-right; should clip the cylinder.
        let t = v.ray_segment_hit(
            Vec3::ZERO,
            Vec3::new(-10.0, 10.0, 0.0),
            Vec3::new(10.0, -10.0, 0.0),
        );
        assert!(t.is_some(), "diagonal segment should hit");
    }

    // -- AABB -----------------------------------------------------------

    #[test]
    fn aabb_contains_inside_outside() {
        let v = CollisionVolume::aabb(Vec3::new(2.0, 4.0, 6.0));
        assert!(v.contains(Vec3::ZERO, Vec3::new(1.5, 3.0, 5.0)));
        assert!(!v.contains(Vec3::ZERO, Vec3::new(2.5, 0.0, 0.0)));
    }

    #[test]
    fn aabb_segment_hit_face() {
        let v = CollisionVolume::aabb(Vec3::new(2.0, 4.0, 6.0));
        let t = v
            .ray_segment_hit(
                Vec3::ZERO,
                Vec3::new(-10.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 0.0),
            )
            .expect("must hit");
        // First contact at x = -2 over a 20-unit segment from x = -10.
        assert!(approx(t, 8.0 / 20.0), "t was {t}");
    }

    #[test]
    fn aabb_segment_miss_corner() {
        let v = CollisionVolume::aabb(Vec3::new(2.0, 2.0, 2.0));
        assert!(
            v.ray_segment_hit(
                Vec3::ZERO,
                Vec3::new(-10.0, 5.0, 0.0),
                Vec3::new(-3.0, 5.0, 0.0)
            )
            .is_none()
        );
    }

    // -- Bounding radius ------------------------------------------------

    #[test]
    fn bounding_radius_per_shape() {
        assert!(approx(CollisionVolume::sphere(7.0).bounding_radius(), 7.0));
        // 3-4-5 triangle: cylinder radius 3, half-height 4 → 5.
        assert!(approx(
            CollisionVolume::cylinder_y(3.0, 4.0).bounding_radius(),
            5.0
        ));
        // AABB diagonal: √(1+4+4) = 3.
        assert!(approx(
            CollisionVolume::aabb(Vec3::new(1.0, 2.0, 2.0)).bounding_radius(),
            3.0
        ));
    }

    // -- S3O round-trip -------------------------------------------------

    #[test]
    fn from_s3o_radius_yields_sphere() {
        let v = CollisionVolume::from_s3o_radius(13.5);
        assert_eq!(v, CollisionVolume::Sphere { radius: 13.5 });
    }
}
