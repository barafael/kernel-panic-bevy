//! Per-frame tick of every live weapon visual: fade beams, animate
//! projectile arcs, drift + billboard build-sparkles, despawn at end of life.

use bevy::prelude::*;

use super::shared::{BeamVisual, BuildSparkle, BurstSegment, ProjectileVisual};
use crate::rendering::camera::RtsCamera;

#[allow(clippy::type_complexity)]
pub(super) fn tick_weapon_fx(
    time: Res<Time>,
    mut beams: Query<(Entity, &mut BeamVisual, &mut Transform), Without<ProjectileVisual>>,
    mut bursts: Query<
        (Entity, &mut BurstSegment),
        (Without<BeamVisual>, Without<ProjectileVisual>),
    >,
    mut projectiles: Query<(Entity, &mut ProjectileVisual, &mut Transform)>,
    mut sparkles: Query<
        (Entity, &mut BuildSparkle, &mut Transform),
        (
            Without<BeamVisual>,
            Without<ProjectileVisual>,
            Without<BurstSegment>,
        ),
    >,
    camera_q: Query<&GlobalTransform, With<RtsCamera>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();

    for (entity, mut beam, mut transform) in &mut beams {
        beam.lifetime -= dt;
        if beam.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        // Fade by shrinking cross-section while keeping length.
        let fade = (beam.lifetime / beam.max_lifetime).sqrt();
        let s = transform.scale;
        transform.scale = Vec3::new(s.x.min(1.0) * fade, s.y.min(1.0) * fade, s.z);
    }

    for (entity, mut burst) in &mut bursts {
        burst.lifetime -= dt;
        if burst.lifetime <= 0.0 {
            commands.entity(entity).despawn();
        }
    }

    for (entity, mut proj, mut transform) in &mut projectiles {
        let total_dist = proj.origin.distance(proj.target);
        if total_dist < 0.1 {
            commands.entity(entity).despawn();
            continue;
        }
        proj.progress += (proj.speed * dt) / total_dist;
        if proj.progress >= 1.0 {
            commands.entity(entity).despawn();
            continue;
        }

        let t = proj.progress;
        let mut pos = proj.origin.lerp(proj.target, t);
        if proj.arc_height > 0.0 {
            let arc = 4.0 * t * (1.0 - t);
            pos.y += proj.arc_height * total_dist * arc;
        }
        transform.translation = pos;
    }

    // Build-sparkle particles: drift, decay velocity (airdrag=1 in CEG kills it
    // fast), fade by shrinking the quad, billboard toward camera, despawn at
    // end of life. Material is shared, so per-particle alpha must come from
    // scale rather than mutating colour.
    let cam_pos = camera_q
        .single()
        .ok()
        .map(|gt| gt.translation())
        .unwrap_or(Vec3::Y * 1000.0);
    for (entity, mut sparkle, mut transform) in &mut sparkles {
        sparkle.lifetime -= dt;
        if sparkle.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        // Drift; airdrag=1 → exponential velocity decay (~half-life 0.1s).
        transform.translation += sparkle.velocity * dt;
        sparkle.velocity *= (1.0 - dt * 7.0).max(0.0);

        // Colour map is white, white, transparent — i.e. opaque for first half,
        // then ramps to zero. Approximate by holding full size for the first
        // half of life and shrinking smoothly to zero across the second half.
        let life_frac = sparkle.lifetime / sparkle.max_lifetime;
        let fade = if life_frac > 0.5 {
            1.0
        } else {
            life_frac * 2.0
        };
        let s = sparkle.base_size * fade;

        // Billboard: face the camera while keeping world-up.
        let to_cam = (cam_pos - transform.translation).normalize_or(Vec3::Z);
        let right = Vec3::Y.cross(to_cam).normalize_or(Vec3::X);
        let up = to_cam.cross(right).normalize_or(Vec3::Y);
        transform.rotation = Quat::from_mat3(&Mat3::from_cols(right, up, to_cam));
        transform.scale = Vec3::splat(s);
    }
}
