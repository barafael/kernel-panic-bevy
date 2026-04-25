//! Per-frame tick of every live weapon visual: fade beams, animate
//! projectile arcs, drift + billboard build-sparkles, despawn at end of life.

use bevy::mesh::VertexAttributeValues;
use bevy::prelude::*;

use super::shared::{
    BeamVisual, BuildSparkle, DelayedHit, ExplosionEvent, GroundFlash, ImpactBurst, LaserBolt,
    PendingExplosions, ProjectileVisual, TRAIL_SAMPLE_COUNT,
};
use crate::rendering::camera::RtsCamera;
use crate::units::combat::{DamageQueue, PendingDamage};
use crate::units::content::weapons::WeaponRegistry;

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn tick_weapon_fx(
    time: Res<Time>,
    mut beams: Query<(Entity, &mut BeamVisual, &mut Transform), Without<ProjectileVisual>>,
    mut projectiles: Query<(Entity, &mut ProjectileVisual, &mut Transform)>,
    mut bolts: Query<
        (Entity, &mut LaserBolt, &mut Transform),
        (Without<BeamVisual>, Without<ProjectileVisual>),
    >,
    mut sparkles: Query<
        (Entity, &mut BuildSparkle, &mut Transform),
        (
            Without<BeamVisual>,
            Without<ProjectileVisual>,
            Without<LaserBolt>,
        ),
    >,
    mut impacts: Query<
        (Entity, &mut ImpactBurst, &mut Transform),
        (
            Without<BeamVisual>,
            Without<ProjectileVisual>,
            Without<LaserBolt>,
            Without<BuildSparkle>,
        ),
    >,
    mut flashes: Query<
        (Entity, &mut GroundFlash, &mut Transform),
        (
            Without<BeamVisual>,
            Without<ProjectileVisual>,
            Without<LaserBolt>,
            Without<BuildSparkle>,
            Without<ImpactBurst>,
        ),
    >,
    delayed_hits: Query<&DelayedHit>,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_explosions: ResMut<PendingExplosions>,
    weapon_registry: Res<WeaponRegistry>,
    camera_q: Query<&GlobalTransform, With<RtsCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    // Projectile trails + build-sparkle billboards both need the
    // camera's world position; resolve it once per frame.
    let cam_pos = camera_q
        .single()
        .map(|gt| gt.translation())
        .unwrap_or(Vec3::Y * 1000.0);

    // Hit-scan beams (BeamLaser / BuildLaser). Rewrite the 4 corners
    // each frame so the ribbon always faces the camera — same xdir
    // math as the bolt path above.
    for (entity, mut beam, _transform) in &mut beams {
        beam.lifetime -= dt;
        if beam.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let beam_dir = (beam.end - beam.start).try_normalize().unwrap_or(Vec3::Z);
        let mid = (beam.start + beam.end) * 0.5;
        let to_cam = (cam_pos - mid).normalize_or(Vec3::Y);
        let dir1 = to_cam
            .cross(beam_dir)
            .try_normalize()
            .unwrap_or_else(|| Vec3::Y.cross(beam_dir).try_normalize().unwrap_or(Vec3::X));
        // Decay-aware fade. With `beamdecay` < 1.0 the beam is supposed to
        // dim its color each sim frame (Spring's `BeamLaserProjectile::Update`
        // multiplies `coreCol*` / `edgeCol*` by `decay`). We honour that
        // via vertex colors below; thickness stays full. For weapons
        // without authored decay (default 1.0) we keep the legacy
        // sqrt-thickness shrink so a single-frame `beamtime` weapon
        // still reads as a smooth flash rather than a hard pop.
        let elapsed_frames = (beam.max_lifetime - beam.lifetime).max(0.0) * 30.0;
        let intensity = if beam.decay < 1.0 {
            beam.decay.powf(elapsed_frames)
        } else {
            1.0
        };
        let thickness_fade = if beam.decay < 1.0 {
            1.0
        } else {
            (beam.lifetime / beam.max_lifetime).sqrt()
        };
        let offset = dir1 * beam.thickness * thickness_fade;
        // Match the bolt convention: U=0 at `end` (target-side, where
        // any texture's "arrow tip" / "hit end" should read), U=1 at
        // `start` (shooter-side). Keeps a textured BeamLaser like the
        // future DOS_Beam showing its `dosray` stream flowing from
        // builder to target rather than backwards.
        if let Some(mesh) = meshes.get_mut(&beam.mesh) {
            let bl = beam.end - offset;
            let br = beam.start - offset;
            let tr = beam.start + offset;
            let tl = beam.end + offset;
            rewrite_quad_positions(mesh, bl, br, tr, tl);
            rewrite_quad_color(mesh, [intensity, intensity, intensity, 1.0]);
        }
    }

    // Traveling laser bolts — Spring's `CLaserProjectile::Draw`. Lead
    // advances from `origin` at `speed` until reaching the target; the
    // tail trails by up to `max_length`, then catches up once the lead
    // stops. For each live bolt we rewrite the quad's 4 vertices with
    // camera-facing corners — `dir1 = ((midpoint - cam) × beam_dir).normalize()`
    // is the width axis; the quad spans `±dir1 * thickness` at lead
    // and tail. Despawns once both ends pass the target.
    for (entity, mut bolt, _transform) in &mut bolts {
        bolt.elapsed += dt;
        let lead_raw = bolt.speed * bolt.elapsed;
        let lead_dist = lead_raw.min(bolt.total_distance);
        if lead_raw >= bolt.total_distance {
            let impact_pos = bolt.origin + bolt.direction * bolt.total_distance;
            trigger_delayed_hit(
                entity,
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
        }
        let tail_raw = (lead_raw - bolt.max_length).max(0.0);
        if tail_raw >= bolt.total_distance {
            commands.entity(entity).despawn();
            continue;
        }
        let tail_dist = tail_raw.min(bolt.total_distance);
        let lead_pos = bolt.origin + bolt.direction * lead_dist;
        let tail_pos = bolt.origin + bolt.direction * tail_dist;
        let mid = (lead_pos + tail_pos) * 0.5;
        let to_cam = (cam_pos - mid).normalize_or(Vec3::Y);
        // dir1 is the quad's width axis: perpendicular to both the
        // beam direction and the camera-to-bolt ray. If the camera is
        // looking straight down the bolt, fall back to the camera's
        // own right vector so the bolt stays visible head-on.
        let dir1 = to_cam
            .cross(bolt.direction)
            .try_normalize()
            .unwrap_or_else(|| {
                // Bolt viewed end-on: pick any perpendicular that lies in
                // the camera plane.
                Vec3::Y
                    .cross(bolt.direction)
                    .try_normalize()
                    .unwrap_or(Vec3::X)
            });
        let offset = dir1 * bolt.thickness;

        // Mesh UVs are fixed: bl=(0,0), br=(1,0), tr=(1,1), tl=(0,1).
        // Upstream assigns `tex1->xstart` (U=0) to the LEAD and
        // `tex1->xend` (U=1) to the TAIL (see `LaserProjectile.cpp::Draw`,
        // where `drawPos` — the lead — gets `tex1->xstart` and `pos2` —
        // the tail — gets `tex1->xend`). The `arrow.tga` atlas has its
        // chevron tips at low U, so that mapping makes the arrows read
        // as `>>>>` pointing at the target. Inverting it (tail-at-U=0)
        // flipped them to face the shooter — the regression the user
        // caught. So: LEAD corners go to bl/tl (U=0), TAIL corners go
        // to br/tr (U=1).
        if let Some(mesh) = meshes.get_mut(&bolt.mesh) {
            let bl = lead_pos - offset;
            let br = tail_pos - offset;
            let tr = tail_pos + offset;
            let tl = lead_pos + offset;
            rewrite_quad_positions(mesh, bl, br, tr, tl);
        }
    }

    for (entity, mut proj, mut transform) in &mut projectiles {
        let total_dist = proj.origin.distance(proj.target);
        if total_dist < 0.1 {
            trigger_delayed_hit(
                entity,
                proj.target,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
            despawn_projectile(entity, &mut proj, &mut commands);
            continue;
        }
        proj.progress += (proj.speed * dt) / total_dist;
        if proj.progress >= 1.0 {
            trigger_delayed_hit(
                entity,
                proj.target,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
            despawn_projectile(entity, &mut proj, &mut commands);
            continue;
        }

        let t = proj.progress;
        let mut pos = proj.origin.lerp(proj.target, t);
        if proj.arc_height > 0.0 {
            let arc = 4.0 * t * (1.0 - t);
            pos.y += proj.arc_height * total_dist * arc;
        }
        transform.translation = pos;

        // Advance the trail ring-buffer and rewrite the ribbon mesh.
        if let Some(trail) = &mut proj.trail {
            update_trail_samples(&mut trail.samples, pos);
            rewrite_trail_mesh(
                &mut meshes,
                &trail.mesh,
                &trail.samples,
                trail.half_width,
                cam_pos,
            );
        }
    }

    // Build-sparkle particles: drift, decay velocity (airdrag=1 in CEG kills it
    // fast), fade by shrinking the quad, billboard toward camera, despawn at
    // end of life. Material is shared, so per-particle alpha must come from
    // scale rather than mutating colour. `cam_pos` was resolved at the top of
    // the system — shared with the projectile-trail path.
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
        transform.scale = Vec3::splat(r);
    }
}

/// Fire the one-shot impact payload riding on a traveling visual: push
/// the deferred [`PendingDamage`] onto [`DamageQueue`] and enqueue the
/// weapon's impact CEG as an [`ExplosionEvent`], then remove the
/// component so the still-visible bolt tail can't re-trigger. No-op
/// for entities without a [`DelayedHit`] (hitscan beams / build-lasers
/// whose damage settled at spawn time).
#[allow(clippy::too_many_arguments)]
fn trigger_delayed_hit(
    entity: Entity,
    impact_pos: Vec3,
    delayed_hits: &Query<&DelayedHit>,
    weapon_registry: &WeaponRegistry,
    damage_queue: &mut DamageQueue,
    pending_explosions: &mut PendingExplosions,
    commands: &mut Commands,
) {
    let Ok(hit) = delayed_hits.get(entity) else {
        return;
    };
    damage_queue.push(PendingDamage {
        target: hit.target,
        attacker: hit.attacker,
        weapon: hit.weapon.to_string(),
        impact_pos,
        attacker_distance: hit.attacker_distance,
    });
    let (rgb, radius, ceg_name) = weapon_registry
        .get(&hit.weapon)
        .map_or(([0.7; 3], 4.0, String::new()), |w| {
            (w.rgb_color, w.area_of_effect, w.explosion_generator.clone())
        });
    pending_explosions.events.push(ExplosionEvent {
        pos: impact_pos,
        rgb,
        radius,
        ceg_name,
    });
    commands.entity(entity).remove::<DelayedHit>();
}

/// Despawn the projectile and its companion ribbon entity (if any).
/// Called from the projectile loop at both the "arrived at target" and
/// the "origin == target" early-exit paths so the ribbon never
/// dangles without its projectile.
fn despawn_projectile(entity: Entity, proj: &mut ProjectileVisual, commands: &mut Commands) {
    if let Some(trail) = proj.trail.take() {
        commands.entity(trail.ribbon_entity).despawn();
    }
    commands.entity(entity).despawn();
}

/// Advance the ring-buffer by one step: drop the oldest sample and
/// prepend the projectile's current position at the head of the
/// buffer. `samples` is oldest-first so `last()` is always the head
/// (current projectile pos) and `first()` is the tail end.
fn update_trail_samples(samples: &mut [Vec3], head_pos: Vec3) {
    if samples.is_empty() {
        return;
    }
    samples.rotate_left(1);
    if let Some(last) = samples.last_mut() {
        *last = head_pos;
    }
}

/// Rebuild the trail's triangle-strip mesh from the sample buffer.
///
/// The ribbon is a series of 1-quad-wide segments stitched together
/// as a single triangle strip (`N` samples → `2N` vertices). Each
/// sample contributes two vertices offset by `±half_width` along a
/// camera-facing right vector (so the ribbon always reads as a flat
/// strip regardless of viewing angle). UV.u encodes "progress along
/// the trail" from 0 (oldest) to 1 (head), which lets textures with
/// baked-in gradients (firetrail.tga's yellow→transparent fade) feel
/// natural without any material mutation.
fn rewrite_trail_mesh(
    meshes: &mut Assets<Mesh>,
    handle: &Handle<Mesh>,
    samples: &[Vec3],
    half_width: f32,
    cam_pos: Vec3,
) {
    let Some(mesh) = meshes.get_mut(handle) else {
        return;
    };
    let expected = TRAIL_SAMPLE_COUNT * 2;
    let denom = (samples.len().saturating_sub(1).max(1)) as f32;

    // Pre-allocated at spawn (`build_projectile_trail`); mutate in place so
    // we don't allocate `expected × 12` bytes per live projectile per frame.
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    else {
        return;
    };
    let mut write = 0usize;
    for (i, sample) in samples.iter().enumerate() {
        let to_cam = (cam_pos - *sample).normalize_or(Vec3::Y);
        // Tangent: the direction the ribbon runs at this sample. Use
        // the next sample when possible so the normal is authoritative
        // for the segment; fall back to the previous sample at the head.
        let tangent = if i + 1 < samples.len() {
            (samples[i + 1] - *sample).normalize_or(Vec3::Z)
        } else if i > 0 {
            (*sample - samples[i - 1]).normalize_or(Vec3::Z)
        } else {
            Vec3::Z
        };
        let right = tangent.cross(to_cam).normalize_or(Vec3::X) * half_width;
        positions[write] = (*sample - right).to_array();
        positions[write + 1] = (*sample + right).to_array();
        write += 2;
    }
    // Pad any unused tail slots with the last valid vertex so the GPU sees
    // a stable buffer size (trail shorter than the ring buffer capacity).
    let pad = positions
        .get(write.saturating_sub(1))
        .copied()
        .unwrap_or([0.0; 3]);
    for slot in positions[write..expected].iter_mut() {
        *slot = pad;
    }

    // UVs also need rewriting each frame because `samples.len()` determines
    // the `u` gradient. Same in-place pattern.
    if let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute_mut(Mesh::ATTRIBUTE_UV_0) {
        let mut write = 0usize;
        for i in 0..samples.len() {
            let u = i as f32 / denom;
            uvs[write] = [u, 0.0];
            uvs[write + 1] = [u, 1.0];
            write += 2;
        }
        for slot in uvs[write..expected].iter_mut() {
            *slot = [1.0, 0.0];
        }
    }
}

/// Rewrite a 4-vertex quad's positions in place (shared by beam + bolt
/// paths). Uses `Mesh::attribute_mut` to mutate the existing
/// `Float32x3` buffer instead of allocating a fresh `Vec` every frame.
/// The vertex order matches [`super::shared::build_billboard_quad_mesh`].
fn rewrite_quad_positions(mesh: &mut Mesh, bl: Vec3, br: Vec3, tr: Vec3, tl: Vec3) {
    if let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
        && positions.len() >= 4
    {
        positions[0] = bl.to_array();
        positions[1] = br.to_array();
        positions[2] = tr.to_array();
        positions[3] = tl.to_array();
    }
}

/// Set all 4 quad vertex colors to `rgba`. Used by the beam path to
/// apply per-frame `beamdecay` intensity without per-beam material
/// clones: the cached material's `base_color` carries the weapon's
/// authored RGB, vertex color carries the time-varying multiplier,
/// and Bevy's StandardMaterial multiplies them on the GPU.
fn rewrite_quad_color(mesh: &mut Mesh, rgba: [f32; 4]) {
    if let Some(VertexAttributeValues::Float32x4(colors)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_COLOR)
        && colors.len() >= 4
    {
        for slot in colors.iter_mut().take(4) {
            *slot = rgba;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// Traveling weapons MUST defer damage + impact CEG until the bolt's
    /// lead reaches the target. Bolt flies for 1 s: first tick
    /// (mid-flight) leaves queues empty; second tick (impact) pushes
    /// one damage + one explosion and removes the component; third
    /// tick (tail still trailing) must not re-fire.
    #[test]
    fn laser_bolt_defers_damage_until_lead_reaches_target() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<Assets<Mesh>>();

        let attacker = app.world_mut().spawn_empty().id();
        let target = app.world_mut().spawn_empty().id();

        // Bolt geometry: 100 elmos at 100 elmos/s → impact at t=1.0.
        // max_length=50 so tail takes another 0.5 s to clear.
        let mesh_handle = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(super::super::shared::build_billboard_quad_mesh());
        let bolt_entity = app
            .world_mut()
            .spawn((
                LaserBolt {
                    origin: Vec3::ZERO,
                    direction: Vec3::Z,
                    total_distance: 100.0,
                    speed: 100.0,
                    max_length: 50.0,
                    thickness: 1.0,
                    elapsed: 0.0,
                    mesh: mesh_handle,
                },
                Transform::IDENTITY,
                DelayedHit {
                    target: Some(target),
                    attacker,
                    weapon: std::borrow::Cow::Borrowed("TestLaser"),
                    attacker_distance: 100.0,
                },
            ))
            .id();

        // Tick 1: advance 0.5 s — lead at 50 elmos, not yet at target.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(500));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        assert!(app.world().resource::<DamageQueue>().is_empty());
        assert!(
            app.world()
                .resource::<PendingExplosions>()
                .events
                .is_empty()
        );
        assert!(app.world().get::<DelayedHit>(bolt_entity).is_some());

        // Tick 2: advance another 0.5 s → lead now at 100 elmos = impact.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(500));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);
        assert_eq!(app.world().resource::<PendingExplosions>().events.len(), 1);
        assert!(
            app.world().get::<DelayedHit>(bolt_entity).is_none(),
            "component removed after firing so later ticks can't re-trigger",
        );

        // Tick 3: tail still trailing — must not re-fire.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(200));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);
        assert_eq!(app.world().resource::<PendingExplosions>().events.len(), 1);
    }

    /// Bolts without a `DelayedHit` (hitscan beams / build-lasers come
    /// through here too for the width-axis rewrite) must NOT touch
    /// `DamageQueue` or `PendingExplosions` — their damage path settles
    /// at fire time via combat's `PendingDamage` push.
    #[test]
    fn laser_bolt_without_delayed_hit_leaves_queues_empty() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<Assets<Mesh>>();

        let mesh_handle = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(super::super::shared::build_billboard_quad_mesh());
        app.world_mut().spawn((
            LaserBolt {
                origin: Vec3::ZERO,
                direction: Vec3::Z,
                total_distance: 10.0,
                speed: 100.0,
                max_length: 20.0,
                thickness: 1.0,
                elapsed: 0.0,
                mesh: mesh_handle,
            },
            Transform::IDENTITY,
        ));

        // Advance well past impact.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();

        assert!(app.world().resource::<DamageQueue>().is_empty());
        assert!(
            app.world()
                .resource::<PendingExplosions>()
                .events
                .is_empty()
        );
    }
}
