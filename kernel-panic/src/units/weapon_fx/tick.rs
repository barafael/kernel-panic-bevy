//! Per-frame tick of every live weapon visual: fade beams, animate
//! projectile arcs, drift + billboard build-sparkles, despawn at end of life.

use bevy::prelude::*;

use super::shared::{
    BeamMaterialCache, BeamVisual, BuildSparkle, BurstSegment, GroundFlash, ImpactBurst,
    ImpactBurstAssets, ProjectileVisual, tdf_color,
};
use crate::rendering::camera::RtsCamera;

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
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
    mut impacts: Query<
        (Entity, &mut ImpactBurst, &mut Transform),
        (
            Without<BeamVisual>,
            Without<ProjectileVisual>,
            Without<BurstSegment>,
            Without<BuildSparkle>,
        ),
    >,
    mut flashes: Query<
        (Entity, &mut GroundFlash, &mut Transform),
        (
            Without<BeamVisual>,
            Without<ProjectileVisual>,
            Without<BurstSegment>,
            Without<BuildSparkle>,
            Without<ImpactBurst>,
        ),
    >,
    camera_q: Query<&GlobalTransform, With<RtsCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<BeamMaterialCache>,
    mut impact_assets: ResMut<ImpactBurstAssets>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();

    for (entity, mut beam, mut transform) in &mut beams {
        beam.lifetime -= dt;
        if beam.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        // Fade by shrinking cross-section while holding length constant. Mesh
        // is the shared unit cuboid/sphere, so scale carries both the base
        // dimensions and the animated fade.
        let fade = (beam.lifetime / beam.max_lifetime).sqrt();
        let t = beam.base_thickness * fade;
        transform.scale = Vec3::new(t, t, beam.length);
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

        // Emit trail puffs for smoke_trail / cegTag projectiles. Accumulate
        // dt so frame-rate drops don't thin out the trail, but clamp the
        // catch-up to one puff per tick — a huge `dt` spike shouldn't
        // mint 50 bursts in the same frame.
        if let Some(trail_rgb) = proj.trail_rgb
            && proj.trail_interval > 0.0
        {
            proj.trail_accumulator += dt;
            if proj.trail_accumulator >= proj.trail_interval {
                proj.trail_accumulator -= proj.trail_interval;
                spawn_trail_puff(
                    pos,
                    trail_rgb,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut cache,
                    &mut impact_assets,
                );
            }
        }
    }

    // Build-sparkle particles: drift, decay velocity (airdrag=1 in CEG kills it
    // fast), fade by shrinking the quad, billboard toward camera, despawn at
    // end of life. Material is shared, so per-particle alpha must come from
    // scale rather than mutating colour.
    let cam_pos = camera_q
        .single()
        .inspect_err(
            |error| warn!(%error, "weapon_fx: camera query failed, using Vec3::Y*1000 fallback"),
        )
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

    // Impact bursts: scale up while fading, then despawn. Material is
    // shared (cached by color), so opacity comes from the scale curve.
    for (entity, mut impact, mut transform) in &mut impacts {
        impact.lifetime -= dt;
        if impact.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let life_frac = 1.0 - impact.lifetime / impact.max_lifetime;
        let scale = impact.base_size * (1.0 + life_frac * 1.5);
        transform.scale = Vec3::splat(scale);
    }

    // Ground flash ring: expand outward from 0.25× to 1.5× radius over
    // the lifetime, then collapse to zero in the final quarter to fade
    // out cleanly. Flat (XZ) scale keeps the disc hugging the ground;
    // rotation is set at spawn and never touched.
    for (entity, mut flash, mut transform) in &mut flashes {
        flash.lifetime -= dt;
        if flash.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let life_frac = 1.0 - flash.lifetime / flash.max_lifetime;
        let grow = 0.25 + life_frac * 1.25;
        let fade = if life_frac > 0.75 {
            (1.0 - life_frac) * 4.0
        } else {
            1.0
        };
        let r = flash.base_radius * grow * fade;
        transform.scale = Vec3::new(r, r, r);
    }
}

/// Drop a small faction-coloured puff behind a trailing projectile. The
/// puff reuses the [`ImpactBurst`] component + shared sphere mesh so the
/// existing decay loop shrinks it to nothing without any dedicated tick
/// code. Kept tiny (base size 2) and short (0.35 s life) so a dozen in a
/// row reads as a streak rather than a wall of fireballs.
#[allow(clippy::too_many_arguments)]
fn spawn_trail_puff(
    pos: Vec3,
    rgb: [f32; 3],
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    impact_assets: &mut ImpactBurstAssets,
) {
    let mesh = impact_assets
        .mesh
        .get_or_insert_with(|| meshes.add(Sphere::new(1.0).mesh().ico(2).unwrap()))
        .clone();

    let color = tdf_color(rgb);
    let material = cache.get_or_create(color, true, materials);

    let life = 0.35;
    let base_size = 2.0;

    commands.spawn((
        ImpactBurst {
            lifetime: life,
            max_lifetime: life,
            base_size,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(pos).with_scale(Vec3::splat(base_size)),
    ));
}
