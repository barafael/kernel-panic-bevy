//! Per-unit volumetric collision shape.
//!
//! Spring's `CCollisionHandler` tests hits against a per-unit primitive
//! (sphere, axis-aligned cylinder or box). Every KP unit uses the S3O
//! model's authored bounding sphere, so [`CollisionVolume`] is that
//! sphere, cached at spawn time so projectile mid-flight collision can
//! run a segment test without re-walking the piece tree.
//!
//! The sphere is centred on the model midpoint (`unit->midPos`), `mid_y`
//! above the unit's transform origin; [`CollisionVolume::center`] gives
//! the world-space centre for a unit transform.

use bevy::prelude::*;

/// Bounding sphere used for projectile-collision tests.
#[derive(Component, Copy, Clone, Debug, PartialEq)]
pub struct CollisionVolume {
    pub radius: f32,
    /// Height of the sphere's centre above the unit origin (S3O
    /// `midpoint.y`).
    pub mid_y: f32,
}

impl CollisionVolume {
    /// The S3O model's authored bounding sphere.
    pub fn from_s3o(radius: f32, mid_y: f32) -> Self {
        Self { radius, mid_y }
    }

    /// World-space centre of the sphere for a unit at `tf`.
    pub fn center(&self, tf: &GlobalTransform) -> Vec3 {
        tf.translation() + Vec3::Y * self.mid_y
    }

    /// Mid-flight projectile collision. Returns the smallest `t` in
    /// `[0, 1]` such that `start.lerp(end, t)` is inside the sphere, or
    /// `None` if the segment misses entirely — so the tick asks whether
    /// a bolt's segment for the current frame *crosses* a unit instead
    /// of waiting for it to "reach" its target by distance.
    pub fn ray_segment_hit(&self, center: Vec3, start: Vec3, end: Vec3) -> Option<f32> {
        let radius = self.radius;
        let dir = end - start;
        let length_sq = dir.length_squared();
        let m = start - center;
        let c = m.length_squared() - radius * radius;
        if length_sq < 1e-12 {
            // Zero-length segment — fall back to a point test.
            return (c <= 0.0).then_some(0.0);
        }
        // Solve |start + t*dir - center|² = radius² for t ∈ [0, 1].
        let b = m.dot(dir);
        if c > 0.0 && b > 0.0 {
            // Origin outside the sphere and ray pointing away.
            return None;
        }
        let discr = b * b - length_sq * c;
        if discr < 0.0 {
            return None;
        }
        // Earliest t along the segment, normalised against |dir|².
        let t_unscaled = -b - discr.sqrt();
        let t = if t_unscaled < 0.0 {
            // Origin already inside the sphere.
            0.0
        } else {
            t_unscaled / length_sq
        };
        (t <= 1.0).then_some(t.max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn sphere(radius: f32) -> CollisionVolume {
        CollisionVolume::from_s3o(radius, 0.0)
    }

    #[test]
    fn zero_length_segment_is_a_point_test() {
        let v = sphere(10.0);
        assert_eq!(
            v.ray_segment_hit(Vec3::ZERO, Vec3::X * 10.0, Vec3::X * 10.0),
            Some(0.0)
        );
        let outside = Vec3::new(10.001, 0.0, 0.0);
        assert!(v.ray_segment_hit(Vec3::ZERO, outside, outside).is_none());
    }

    #[test]
    fn sphere_segment_hit_through_centre() {
        let v = sphere(5.0);
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
        let v = sphere(5.0);
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
        let v = sphere(5.0);
        let t = v
            .ray_segment_hit(Vec3::ZERO, Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0))
            .expect("must hit");
        assert!(approx(t, 0.0), "t was {t}");
    }

    #[test]
    fn sphere_segment_pointing_away() {
        let v = sphere(5.0);
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
}
