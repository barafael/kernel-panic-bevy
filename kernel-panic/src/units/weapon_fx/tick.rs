//! Per-frame tick of every live weapon visual: fade beams, animate
//! projectile arcs, despawn at end of life.

use bevy::prelude::*;

use super::batch::FxQuadBatches;
use super::shared::{
    BeamVisual, DelayedHit, ExplosionEvent, FadingTrail, Flight, GroundFlash, ImpactBurst,
    LaserBolt, LightningArc, PendingExplosions, ProjectileTrail, ProjectileVisual, SMOKE_COLOR,
    SMOKE_SIZE, SMOKE_TIME_FRAMES, TRAIL_SAMPLE_COUNT, TrailSample,
};
use crate::rendering::camera::RtsCamera;
use crate::sim::{GAME_SPEED, secs_to_frames};
use bevy::ecs::system::SystemParam;

use super::ceg::{CegTrailCtx, spawn_ceg};
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::{CollisionVolume, DamageQueue, PendingDamage};
use crate::units::components::{Faction, TeamId, UnitType, is_friendly};
#[cfg(test)]
use crate::units::content::weapons::WeaponId;
use crate::units::content::weapons::WeaponRegistry;
use crate::units::spatial::SpatialIndex;

/// Upper bound on a guided projectile's life (seconds). Missiles end on
/// the ground or a unit; this only catches one that leaves the map.
const GUIDED_MAX_LIFETIME: f32 = 30.0;

/// Rotation that points a projectile model's S3O +Z along `dir`, with
/// its +Y as close to world up as possible — upstream
/// `CProjectile::GetTransformMatrix` (`Projectile.cpp:209-223`).
pub(super) fn projectile_orientation(dir: Vec3) -> Quat {
    let dir = dir.normalize_or(Vec3::Z);
    let up = if dir.y.abs() < 0.95 { Vec3::Y } else { Vec3::X };
    // `looking_to` aims local −Z; aim it backwards so +Z leads.
    Transform::IDENTITY.looking_to(-dir, up).rotation
}

/// Grouped read-only inputs for the volumetric mid-flight collision
/// pass: target volumes, attacker team/faction (for the friendly
/// filter), and the broad-phase spatial index. Bundled as a
/// `SystemParam` so `tick_weapon_fx` stays under Bevy's 16-arg limit.
#[derive(SystemParam)]
pub(super) struct VolumeHitCtx<'w, 's> {
    target_q: Query<'w, 's, (&'static GlobalTransform, &'static CollisionVolume), With<UnitType>>,
    attacker_q: Query<'w, 's, (&'static TeamId, &'static Faction)>,
    spatial: Res<'w, SpatialIndex>,
    /// Terrain for guided projectiles' ground impacts.
    heightmap: Option<Res<'w, Heightmap>>,
}

/// The camera the ribbon effects face and the quad batches they draw
/// into, bundled so `tick_weapon_fx` stays under Bevy's 16-param limit.
#[derive(SystemParam)]
pub(super) struct RibbonDrawCtx<'w, 's> {
    camera_q: Query<'w, 's, &'static GlobalTransform, With<RtsCamera>>,
    batches: ResMut<'w, FxQuadBatches>,
}

impl RibbonDrawCtx<'_, '_> {
    fn cam_pos(&self) -> Vec3 {
        self.camera_q
            .single()
            .map(|gt| gt.translation())
            .unwrap_or(Vec3::Y * 1000.0)
    }

    /// The camera's right and up axes, for billboards (`camera->GetRight()`
    /// / `GetUp()` in the engine's flare draw).
    fn cam_axes(&self) -> (Vec3, Vec3) {
        self.camera_q
            .single()
            .map(|gt| (gt.right().as_vec3(), gt.up().as_vec3()))
            .unwrap_or((Vec3::X, Vec3::Z))
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn tick_weapon_fx(
    time: Res<Time>,
    mut arcs: Query<(Entity, &mut LightningArc)>,
    mut beams: Query<(Entity, &mut BeamVisual)>,
    mut projectiles: Query<(Entity, &mut ProjectileVisual, &mut Transform)>,
    mut bolts: Query<(Entity, &mut LaserBolt)>,
    mut impacts: Query<(Entity, &mut ImpactBurst, &mut Transform), Without<ProjectileVisual>>,
    mut flashes: Query<
        (Entity, &mut GroundFlash, &mut Transform),
        (Without<ProjectileVisual>, Without<ImpactBurst>),
    >,
    delayed_hits: Query<&DelayedHit>,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_explosions: ResMut<PendingExplosions>,
    weapon_registry: Res<WeaponRegistry>,
    mut draw: RibbonDrawCtx,
    volume_ctx: VolumeHitCtx,
    mut ceg_ctx: CegTrailCtx,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    // Every ribbon and the build-sparkle billboards need the camera's
    // world position; resolve it once per frame.
    let cam_pos = draw.cam_pos();

    // GaussCannon lightning arcs (upstream network_arceffect.lua).
    // Push each segment as a camera-facing quad and fade via vertex
    // colors — the shared additive material is never touched.
    for (entity, mut arc) in &mut arcs {
        arc.lifetime -= dt;
        if arc.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let fade = (arc.lifetime / arc.max_lifetime).clamp(0.0, 1.0);
        let half = arc.width;
        let c = arc.tint.to_f32_array();
        let color = [c[0], c[1], c[2], c[3] * fade];
        for pair in arc.points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let seg_dir = (b - a).try_normalize().unwrap_or(Vec3::Z);
            let to_cam = cam_pos - a;
            let perp = to_cam
                .cross(seg_dir)
                .try_normalize()
                .unwrap_or_else(|| Vec3::Y.cross(seg_dir).try_normalize().unwrap_or(Vec3::X));
            let offset = perp * half;
            draw.batches.push_flat_quad(
                &arc.material,
                [a - offset, a + offset, b + offset, b - offset],
                color,
            );
        }
    }

    // Hit-scan beams (BeamLaser / BuildLaser). Push the 4 corners
    // each frame so the ribbon always faces the camera — same xdir
    // math as the bolt path below.
    let (cam_right, cam_up) = draw.cam_axes();
    for (entity, mut beam) in &mut beams {
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
        let elapsed_frames = secs_to_frames((beam.max_lifetime - beam.lifetime).max(0.0));
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
        // builder to target rather than backwards. The vertex colour
        // carries the `beamdecay` intensity so the cached material is
        // never cloned per beam.
        let color = [intensity, intensity, intensity, 1.0];
        draw.batches.push_flat_quad(
            &beam.material,
            [
                beam.end - offset,
                beam.start - offset,
                beam.start + offset,
                beam.end + offset,
            ],
            color,
        );
        // `BeamLaserProjectile::Draw` with `texture2`: a cap of the
        // ribbon's own half-width at each end, the outer half of the
        // round `laserend` texture facing outward.
        if let Some(caps) = &beam.caps {
            let depth = beam_dir * beam.thickness * thickness_fade;
            const CAP_UVS: [[f32; 2]; 4] = [[0.5, 0.0], [1.0, 0.0], [1.0, 1.0], [0.5, 1.0]];
            draw.batches.push_quad(
                caps,
                [
                    beam.start - offset,
                    beam.start - offset - depth,
                    beam.start + offset - depth,
                    beam.start + offset,
                ],
                CAP_UVS,
                [color; 4],
            );
            draw.batches.push_quad(
                caps,
                [
                    beam.end - offset,
                    beam.end - offset + depth,
                    beam.end + offset + depth,
                    beam.end + offset,
                ],
                CAP_UVS,
                [color; 4],
            );
        }
        // The emitter flare: a camera-facing square of
        // `thickness · laserflaresize` half-size at the start.
        if let Some((flare, half)) = &beam.flare {
            let (r, u) = (cam_right * *half, cam_up * *half);
            draw.batches.push_flat_quad(
                flare,
                [
                    beam.start - r - u,
                    beam.start + r - u,
                    beam.start + r + u,
                    beam.start - r + u,
                ],
                color,
            );
        }
    }

    // Traveling laser bolts — Spring's `CLaserProjectile::Draw`. Lead
    // advances from `origin` at `speed` until reaching the target; the
    // tail trails by up to `max_length`, then catches up once the lead
    // stops. For each live bolt we push a quad with camera-facing
    // corners — `dir1 = ((midpoint - cam) × beam_dir).normalize()`
    // is the width axis; the quad spans `±dir1 * thickness` at lead
    // and tail. Despawns once both ends pass the target.
    for (entity, mut bolt) in &mut bolts {
        let prev_lead_raw = (bolt.speed * bolt.elapsed).min(bolt.total_distance);
        bolt.elapsed += dt;
        let lead_raw = bolt.speed * bolt.elapsed;
        let lead_dist = lead_raw.min(bolt.total_distance);

        // §1.8: sweep this frame's lead segment against (1) the
        // intended target, then (2) the broader spatial-index neighbours
        // for "anyone in path". Triggering removes `DelayedHit`, so
        // the timeout fallback below is a no-op for the same bolt and
        // bolts can't double-trigger.
        let prev_lead_pos = bolt.origin + bolt.direction * prev_lead_raw;
        let curr_lead_pos = bolt.origin + bolt.direction * lead_dist;
        let hit_meta = delayed_hits.get(entity).ok();
        let target_entity = hit_meta.and_then(|h| h.target);
        let attacker_entity = hit_meta.map(|h| h.attacker);
        if let Some(impact_pos) = target_volume_hit(
            target_entity,
            prev_lead_pos,
            curr_lead_pos,
            &volume_ctx.target_q,
        ) {
            trigger_delayed_hit(
                entity,
                HitWho::Intended,
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
        } else if let Some(attacker) = attacker_entity
            && let Some((hit_entity, impact_pos)) = broad_phase_volume_hit(
                attacker,
                target_entity,
                prev_lead_pos,
                curr_lead_pos,
                &volume_ctx.spatial,
                &volume_ctx.target_q,
                &volume_ctx.attacker_q,
            )
        {
            trigger_delayed_hit(
                entity,
                HitWho::Unit(hit_entity),
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
        } else if hit_meta.is_some()
            && let Some(hm) = volume_ctx.heightmap.as_deref()
            && let Some(impact_pos) = hm.ground_hit(prev_lead_pos, curr_lead_pos)
        {
            // `CProjectileHandler::CheckGroundCollisions`: the bolt
            // explodes where it enters the terrain.
            trigger_delayed_hit(
                entity,
                HitWho::Ground,
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
        }
        // A bolt that reaches the end of its `ttl` without touching
        // anything just fades (`CLaserProjectile::Update`): no
        // explosion, no damage.
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

        // Quad UVs: bl=(0,0), br=(u,0), tr=(u,1), tl=(0,1). Upstream
        // assigns `tex1->xstart` (U=0) to the LEAD and `tex1->xend`
        // (U=1) to the TAIL (see `LaserProjectile.cpp::Draw`, where
        // `drawPos` — the lead — gets `tex1->xstart` and `pos2` — the
        // tail — gets `tex1->xend`). The `arrow.tga` atlas has its
        // chevron tips at low U, so that mapping makes the arrows read
        // as `>>>>` pointing at the target. Inverting it (tail-at-U=0)
        // flipped them to face the shooter — the regression the user
        // caught. So: LEAD corners go to bl/tl (U=0), TAIL corners go
        // to br/tr.
        //
        // LaserProjectile.cpp expansion: while still growing
        // (stayTime==0), the tail UV slides from the tile start to the
        // tile end (texEndOffset = (1 - curDrawLen/maxLength) *
        // (xstart - xend)) so the arrow texture materialises at the
        // muzzle and stretches with the bolt. Standalone TGA turns
        // xstart=0, xend=1, so tail U = curDrawLen / maxLength.
        let drawn_len = (lead_dist - tail_dist).max(0.0);
        let grow = (drawn_len / bolt.max_length.max(1e-3)).clamp(0.0, 1.0);
        draw.batches.push_quad(
            &bolt.material,
            [
                lead_pos - offset,
                tail_pos - offset,
                tail_pos + offset,
                lead_pos + offset,
            ],
            [[0.0, 0.0], [grow, 0.0], [grow, 1.0], [0.0, 1.0]],
            [[1.0; 4]; 4],
        );

        // Endcap quads (texture2). Upstream's `dir2` is the
        // camera-aligned forward axis: perpendicular to dir1 and to
        // the camera ray, pointing roughly along the bolt. Each cap
        // is a `2*thickness × thickness` quad anchored at the bolt's
        // tip, extending one thickness *outward* (forward at the
        // lead, backward at the tail).
        if let Some(cap_material) = &bolt.cap_material {
            let dir2 = to_cam.cross(dir1).try_normalize().unwrap_or(bolt.direction);
            let cap_depth = dir2 * bolt.thickness;
            // Lead cap: extends *past* the lead in the forward direction
            // so it reads as a rounded tip at the leading edge.
            draw.batches.push_flat_quad(
                cap_material,
                [
                    lead_pos - offset + cap_depth,
                    lead_pos - offset,
                    lead_pos + offset,
                    lead_pos + offset + cap_depth,
                ],
                [1.0; 4],
            );
            // Tail cap: extends *past* the tail in the backward direction
            // for the trailing tip.
            draw.batches.push_flat_quad(
                cap_material,
                [
                    tail_pos - offset,
                    tail_pos - offset - cap_depth,
                    tail_pos + offset - cap_depth,
                    tail_pos + offset,
                ],
                [1.0; 4],
            );
        }
    }

    let frames = secs_to_frames(dt);
    for (entity, mut proj, mut transform) in &mut projectiles {
        // Plain `&mut` so the flight-state and bookkeeping fields borrow
        // disjointly.
        let proj: &mut ProjectileVisual = &mut proj;
        let total_dist = proj.origin.distance(proj.target);
        let hit_meta = delayed_hits.get(entity).ok();
        let target_entity = hit_meta.and_then(|h| h.target);
        let attacker_entity = hit_meta.map(|h| h.attacker);
        if total_dist < 0.1 && proj.flight == Flight::Direct {
            trigger_delayed_hit(
                entity,
                arrival_hit(target_entity, proj.target, &volume_ctx.target_q),
                proj.target,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
            despawn_projectile(entity, proj, &mut commands);
            continue;
        }

        // —— Integrate this frame's flight model. Every model yields the
        // swept segment (seg_start → seg_end), whether the projectile
        // ended its flight this frame, and where the impact FX should
        // trigger.
        //
        // `Direct` keeps the legacy parametric lerp (arc height baked
        // separately below). Guided flights step the engine's per-frame
        // update ([`super::flight`]) against the target's *current*
        // position (`tracks=1`), and end on the ground — upstream
        // missiles never "arrive"; they hit a unit's volume (swept
        // below) or the terrain. `Ballistic` is `CannonProjectile`
        // (`velocity.y -= gravity·dt`).
        let ground_y = |p: Vec3| {
            volume_ctx
                .heightmap
                .as_deref()
                .map_or(0.0, |hm| hm.sample(p.x, p.z))
        };
        let (seg_start, seg_end, arrived, impact_pos) = match &mut proj.flight {
            Flight::Direct => {
                let prev_progress = proj.progress;
                proj.progress += (proj.speed * dt) / total_dist;
                let seg_start = proj.origin.lerp(proj.target, prev_progress);
                let seg_end = proj.origin.lerp(proj.target, proj.progress.min(1.0));
                (seg_start, seg_end, proj.progress >= 1.0, proj.target)
            }
            Flight::Missile(_) | Flight::Starburst(_) => {
                // `CMissileProjectile::UpdateTargeting`: track the
                // target's aimPos — the model midpoint — not its feet.
                // A missile steered at the ground under a unit dives
                // short and eats terrain before the swept-volume
                // intercept can catch the sphere.
                let target_now = target_entity
                    .and_then(|t| volume_ctx.target_q.get(t).ok())
                    .map(|(gtf, vol)| vol.center(gtf));
                let sample = target_now.map(|pos| super::flight::TargetSample {
                    pos,
                    vel: proj.last_target_pos.map_or(Vec3::ZERO, |last| pos - last),
                });
                proj.last_target_pos = target_now;
                let prev = transform.translation;
                let v = match &mut proj.flight {
                    Flight::Missile(m) => m.step(prev, sample),
                    Flight::Starburst(s) => s.step(prev, sample),
                    Flight::Direct | Flight::Ballistic { .. } => Vec3::ZERO,
                };
                if let Some(t) = target_now {
                    proj.target = t;
                }
                proj.elapsed += dt;
                proj.velocity = v * GAME_SPEED;
                let new_pos = prev + v * frames;
                let ground = ground_y(new_pos);
                let hit_ground = new_pos.y <= ground;
                // A missile that never finds ground (off the map edge)
                // still can't live forever.
                let expired = proj.elapsed > GUIDED_MAX_LIFETIME;
                let impact = Vec3::new(new_pos.x, new_pos.y.max(ground), new_pos.z);
                (prev, new_pos, hit_ground || expired, impact)
            }
            Flight::Ballistic { gravity, .. } => {
                let gravity = *gravity;
                proj.elapsed += dt;
                // `CannonProjectile`: `pos += speed·dt` then
                // `speed.y -= gravity·dt` — the first frame flies on the
                // pure launch velocity so the apex matches the solve.
                let prev = transform.translation;
                let step = proj.velocity * dt;
                let new_pos = prev + step;
                proj.velocity.y -= gravity * dt;
                let to_target = (proj.target - proj.origin).normalize_or(Vec3::ZERO);
                // Terrain collision samples the heightmap like the
                // guided arms above (engine `pos.y < ground`) — a plain
                // sea-level check let overshooting shells tunnel
                // through hills before despawning.
                let arrived =
                    (proj.target - new_pos).dot(to_target) <= 0.0 || new_pos.y <= ground_y(new_pos);
                (prev, new_pos, arrived, new_pos)
            }
        };

        let mut intercepted = false;
        if let Some(impact_pos) =
            target_volume_hit(target_entity, seg_start, seg_end, &volume_ctx.target_q)
        {
            trigger_delayed_hit(
                entity,
                HitWho::Intended,
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
            intercepted = true;
        } else if let Some(attacker) = attacker_entity
            && let Some((hit_entity, impact_pos)) = broad_phase_volume_hit(
                attacker,
                target_entity,
                seg_start,
                seg_end,
                &volume_ctx.spatial,
                &volume_ctx.target_q,
                &volume_ctx.attacker_q,
            )
        {
            trigger_delayed_hit(
                entity,
                HitWho::Unit(hit_entity),
                impact_pos,
                &delayed_hits,
                &weapon_registry,
                &mut damage_queue,
                &mut pending_explosions,
                &mut commands,
            );
            intercepted = true;
        }
        if intercepted || arrived {
            if !intercepted {
                trigger_delayed_hit(
                    entity,
                    arrival_hit(target_entity, impact_pos, &volume_ctx.target_q),
                    impact_pos,
                    &delayed_hits,
                    &weapon_registry,
                    &mut damage_queue,
                    &mut pending_explosions,
                    &mut commands,
                );
            }
            // The trail's last segment ends where the projectile died.
            if let Some(trail) = &mut proj.trail {
                push_trail_sample(trail, seg_end, proj.velocity, true);
            }
            despawn_projectile(entity, proj, &mut commands);
            continue;
        }

        let mut pos = seg_end;
        if proj.flight == Flight::Direct {
            let t = proj.progress;
            pos = proj.origin.lerp(proj.target, t);
            if proj.arc_height > 0.0 {
                let arc = 4.0 * t * (1.0 - t);
                pos.y += proj.arc_height * total_dist * arc;
            }
        }
        transform.translation = pos;
        // Model projectiles face their flight direction
        // (`CProjectile::GetTransformMatrix`: z = dir).
        if let Some(dir) = proj.flight.guided_dir() {
            transform.rotation = projectile_orientation(dir);
        }

        // One smoke-trail segment per sim frame.
        if let Some(trail) = &mut proj.trail {
            push_trail_sample(trail, pos, proj.velocity, false);
            push_trail_quads(&mut draw.batches, trail, cam_pos);
        }

        // `cegTag`: `explGenHandler.GenExplosion(cegID, pos, dir, …)`
        // every sim frame while the projectile has fuel.
        if let Some(weapon) = proj.trail_ceg
            && proj.flight.has_fuel()
        {
            let dir = proj.velocity.normalize_or(Vec3::Y);
            spawn_ceg(
                &weapon_registry.by_id(weapon).ceg_tag,
                pos,
                dir,
                &ceg_ctx.ceg_registry,
                &mut proj.trail_seed,
                &mut commands,
                &mut ceg_ctx.meshes,
                &mut ceg_ctx.materials,
                &mut ceg_ctx.images,
                &mut ceg_ctx.model_cache,
                &mut ceg_ctx.ceg_assets,
            );
        }
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

    // Ground flash ring. Synthetic rings expand outward from 0.25× to
    // 1.5× radius over the lifetime; authored `[groundflash]` discs
    // grow linearly by `growth` elmos/second from `flashSize`. Both
    // collapse to zero in the final quarter to fade out cleanly. Flat
    // (XZ) scale keeps the disc hugging the ground; rotation is set at
    // spawn and never touched.
    for (entity, mut flash, mut transform) in &mut flashes {
        flash.lifetime -= dt;
        if flash.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let life_frac = 1.0 - flash.lifetime / flash.max_lifetime;
        let radius = if flash.growth != 0.0 {
            flash.base_radius + flash.growth * (flash.max_lifetime - flash.lifetime)
        } else {
            flash.base_radius * (0.25 + life_frac * 1.25)
        };
        let fade = if life_frac > 0.75 {
            (1.0 - life_frac) * 4.0
        } else {
            1.0
        };
        let r = radius * fade;
        transform.scale = Vec3::splat(r);
    }
}

/// Sweep a per-frame travel segment against the intended target's
/// `CollisionVolume`. Returns the world-space impact position when the
/// segment crosses the volume, `None` otherwise.
///
/// This is the §1.8 volumetric-hit work for in-flight projectiles: the
/// previous logic waited for the bolt's lead to reach the *predicted*
/// total distance and then relied on `apply_damage`'s spray-angle
/// check, which left targets that moved during flight unhit. With this
/// helper the bolt couples to the target's *current* volume each frame
/// — a moving target gets caught the moment the bolt's segment crosses
/// it. Ground-targeted shots (`target == None`) and shots whose target
/// has despawned fall through to `None` and the existing
/// total-distance fallback fires.
fn target_volume_hit(
    target: Option<Entity>,
    seg_start: Vec3,
    seg_end: Vec3,
    target_q: &Query<(&GlobalTransform, &CollisionVolume), With<UnitType>>,
) -> Option<Vec3> {
    let target = target?;
    let (tf, volume) = target_q.get(target).ok()?;
    let t = volume.ray_segment_hit(volume.center(tf), seg_start, seg_end)?;
    Some(seg_start.lerp(seg_end, t))
}

/// What a projectile that ran its course without crossing a volume hit:
/// its target, if the impact point is inside that unit's collision
/// sphere (a homing shell landing on a unit that stands still), else the
/// ground — splash only. The old behaviour credited every arrival to the
/// intended target, which let a sprayed shell that landed 70 elmos off
/// still deal its full damage.
fn arrival_hit(
    target: Option<Entity>,
    impact_pos: Vec3,
    target_q: &Query<(&GlobalTransform, &CollisionVolume), With<UnitType>>,
) -> HitWho {
    let inside = target
        .and_then(|t| target_q.get(t).ok())
        .is_some_and(|(tf, volume)| {
            volume.center(tf).distance_squared(impact_pos) <= volume.radius * volume.radius
        });
    if inside {
        HitWho::Intended
    } else {
        HitWho::Ground
    }
}

/// Broad-phase second pass: find any non-friendly, non-attacker unit
/// whose `CollisionVolume` the segment crosses, returning the closest
/// (smallest `t`) candidate. Catches the "friendly walks into the
/// bolt" / "an enemy steps into the path" cases the intended-target
/// check misses.
///
/// `skip_target` is the intended target (already covered by
/// [`target_volume_hit`]) — passing it here keeps the broad-phase from
/// double-firing on the same entity. `attacker` is the unit that fired
/// the bolt; we never let a unit shoot itself, and friendlies are
/// skipped so the broad pass doesn't unintentionally introduce
/// friendly fire (matches upstream's default `collidefriendly=0`).
///
/// In the test environment the spatial index is typically empty (no
/// `rebuild_spatial_index` has run), so this returns `None` and the
/// existing intended-target / timeout paths drive behaviour. In a
/// running game the index is rebuilt at the head of every Simulate
/// frame, so this catches anyone in path.
fn broad_phase_volume_hit(
    attacker: Entity,
    skip_target: Option<Entity>,
    seg_start: Vec3,
    seg_end: Vec3,
    spatial: &SpatialIndex,
    target_q: &Query<(&GlobalTransform, &CollisionVolume), With<UnitType>>,
    attacker_q: &Query<(&TeamId, &Faction)>,
) -> Option<(Entity, Vec3)> {
    // Conservative bound on any unit's collision-volume reach; chosen
    // to comfortably exceed every shipped S3O bounding sphere. Used
    // only as a broad-phase culling pad, not as a damage radius.
    const MAX_UNIT_VOLUME_REACH: f32 = 96.0;

    let attacker_info = attacker_q.get(attacker).ok();
    let mid = seg_start.lerp(seg_end, 0.5);
    let half_len = (seg_end - seg_start).length() * 0.5;
    let radius = half_len + MAX_UNIT_VOLUME_REACH;

    let mut best: Option<(Entity, f32)> = None;
    spatial.query_radius(mid, radius, |entry| {
        if entry.entity == attacker {
            return;
        }
        if Some(entry.entity) == skip_target {
            return;
        }
        if let Some((atk_team, _)) = attacker_info
            && is_friendly(entry.team, atk_team.0)
        {
            return;
        }
        let Ok((tf, volume)) = target_q.get(entry.entity) else {
            return;
        };
        if let Some(t) = volume.ray_segment_hit(volume.center(tf), seg_start, seg_end)
            && best.is_none_or(|(_, prev_t)| t < prev_t)
        {
            best = Some((entry.entity, t));
        }
    });

    best.map(|(entity, t)| (entity, seg_start.lerp(seg_end, t)))
}

/// What a projectile's impact struck: the unit it was fired at, another
/// unit that crossed its path, or the ground (splash only).
#[derive(Clone, Copy)]
enum HitWho {
    Intended,
    Unit(Entity),
    Ground,
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
    who: HitWho,
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
    let final_target = match who {
        HitWho::Intended => hit.target,
        HitWho::Unit(unit) => Some(unit),
        HitWho::Ground => None,
    };
    damage_queue.push(PendingDamage {
        target: final_target,
        attacker: hit.attacker,
        weapon: hit.weapon,
        impact_pos,
        attacker_distance: hit.attacker_distance,
    });
    if let Some(super::shared::ImpactEffect::NxZone {
        owner_team,
        owner_faction,
    }) = hit.on_impact
    {
        commands.spawn(crate::units::mechanics::command_fire::nx_zone(
            impact_pos,
            owner_team,
            owner_faction,
        ));
    }
    let weapon_def = weapon_registry.by_id(hit.weapon);
    let (rgb, radius, ceg_name) = (
        weapon_def.rgb_color,
        weapon_def.area_of_effect,
        weapon_def.explosion_generator.clone(),
    );
    pending_explosions.events.push(ExplosionEvent {
        pos: impact_pos,
        rgb,
        radius,
        ceg_name,
    });
    commands.entity(entity).remove::<DelayedHit>();
}

/// Despawn the projectile. Its smoke trail (if any) outlives it: the
/// trail state moves onto its own entity as a [`FadingTrail`] and
/// fades out over `smokeTime` like upstream's free-standing
/// `CSmokeTrailProjectile` segments.
fn despawn_projectile(entity: Entity, proj: &mut ProjectileVisual, commands: &mut Commands) {
    if let Some(trail) = proj.trail.take() {
        commands.spawn(FadingTrail(trail));
    }
    commands.entity(entity).despawn();
}

/// Age every sample by one sim frame, append the projectile's current
/// position as the new head and drop samples older than `smokeTime`.
/// The launch point (first sample) and the impact point (`last`) draw at
/// zero alpha (`firstSegment` / `lastSegment`).
pub(super) fn push_trail_sample(
    trail: &mut ProjectileTrail,
    pos: Vec3,
    velocity: Vec3,
    last: bool,
) {
    age_trail(trail);
    let first = trail.samples.is_empty();
    let dir = velocity
        .try_normalize()
        .or_else(|| trail.samples.back().map(|s| s.dir))
        .unwrap_or(Vec3::Y);
    trail.samples.push_back(TrailSample {
        pos,
        dir,
        age: 0.0,
        hidden: first || last,
    });
    while trail.samples.len() > TRAIL_SAMPLE_COUNT {
        trail.samples.pop_front();
    }
}

/// One sim frame of ageing; expired samples fall off the tail.
fn age_trail(trail: &mut ProjectileTrail) {
    for s in trail.samples.iter_mut() {
        s.age += 1.0;
    }
    while trail
        .samples
        .front()
        .is_some_and(|s| s.age > SMOKE_TIME_FRAMES)
    {
        trail.samples.pop_front();
    }
}

/// Fade out orphaned trails and despawn them once every segment expired.
pub(super) fn tick_fading_trails(
    mut trails: Query<(Entity, &mut FadingTrail)>,
    mut draw: RibbonDrawCtx,
    mut commands: Commands,
) {
    if trails.is_empty() {
        return;
    }
    let cam_pos = draw.cam_pos();
    for (entity, mut fading) in &mut trails {
        age_trail(&mut fading.0);
        if fading.0.samples.len() < 2 {
            commands.entity(entity).despawn();
            continue;
        }
        push_trail_quads(&mut draw.batches, &fading.0, cam_pos);
    }
}

/// Upstream `CSmokeTrailProjectile::Draw`, one vertex pair per sample:
/// offset `±(camDir × dir)·(1 + t·smokeSize)` with `t = age /
/// smokeTime`, faded by `(1 − t)·(0.7 + |camDir·dir|)` and tinted
/// `smokeColor`. Vertex colours are premultiplied (rgb and alpha both
/// carry the fade). Consecutive pairs form one batched quad each; U
/// alternates per sample since every upstream segment spans the whole
/// texture.
fn push_trail_quads(batches: &mut FxQuadBatches, trail: &ProjectileTrail, cam_pos: Vec3) {
    let mut prev: Option<(Vec3, Vec3, [f32; 4], f32)> = None;
    for (i, s) in trail.samples.iter().enumerate() {
        let t = (s.age / SMOKE_TIME_FRAMES).clamp(0.0, 1.0);
        let dif = (s.pos - cam_pos).normalize_or(Vec3::NEG_Y);
        let odir = dif.cross(s.dir).normalize_or(Vec3::X);
        let size = 1.0 + t * SMOKE_SIZE;
        let fade = if s.hidden {
            0.0
        } else {
            ((1.0 - t) * (0.7 + dif.dot(s.dir).abs())).clamp(0.0, 1.0)
        };
        let c = SMOKE_COLOR * fade;
        let lo = s.pos - odir * size;
        let hi = s.pos + odir * size;
        let color = [c, c, c, fade];
        let u = (i % 2) as f32;
        if let Some((prev_lo, prev_hi, prev_color, prev_u)) = prev {
            batches.push_quad(
                &trail.material,
                [prev_lo, lo, hi, prev_hi],
                [[prev_u, 0.0], [u, 0.0], [u, 1.0], [prev_u, 1.0]],
                [prev_color, color, color, prev_color],
            );
        }
        prev = Some((lo, hi, color, u));
    }
}

#[cfg(test)]
mod tests {
    use super::super::ceg::{CegRegistry, CegRenderAssets};
    use super::*;
    use crate::units::assets::meshes::S3OModelCache;
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
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<CegRenderAssets>()
            .init_resource::<FxQuadBatches>()
            .init_resource::<S3OModelCache>()
            .insert_resource(CegRegistry::load());

        let attacker = app.world_mut().spawn_empty().id();
        // The target sits on the flight line, 100 elmos out, with a
        // 5-elmo sphere: the lead crosses it on the second tick.
        let target = app
            .world_mut()
            .spawn((
                GlobalTransform::from_xyz(0.0, 0.0, 100.0),
                UnitType(crate::units::content::definitions::UnitKind::Bit),
                CollisionVolume {
                    radius: 5.0,
                    mid_y: 0.0,
                },
            ))
            .id();

        // Bolt geometry: 100 elmos at 100 elmos/s → impact at t=1.0.
        // max_length=50 so tail takes another 0.5 s to clear.
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
                    material: Handle::default(),
                    cap_material: None,
                },
                Transform::IDENTITY,
                DelayedHit {
                    target: Some(target),
                    attacker,
                    weapon: WeaponId::BUILD_LASER,
                    attacker_distance: 100.0,
                    on_impact: None,
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
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<CegRenderAssets>()
            .init_resource::<FxQuadBatches>()
            .init_resource::<S3OModelCache>()
            .insert_resource(CegRegistry::load());

        app.world_mut().spawn((
            LaserBolt {
                origin: Vec3::ZERO,
                direction: Vec3::Z,
                total_distance: 10.0,
                speed: 100.0,
                max_length: 20.0,
                thickness: 1.0,
                elapsed: 0.0,
                material: Handle::default(),
                cap_material: None,
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

    /// §1.8 mid-flight volumetric hit. Target sits at z=70 with a
    /// 5-elmo collision sphere; the predicted impact at z=100 is
    /// behind it. The first tick advances the bolt past the target
    /// volume — `target_volume_hit` should fire damage at the actual
    /// crossing point (z ≈ 65), NOT at the predicted z=100.
    #[test]
    fn laser_bolt_intercepts_target_volume_before_predicted_impact() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<CegRenderAssets>()
            .init_resource::<FxQuadBatches>()
            .init_resource::<S3OModelCache>()
            .insert_resource(CegRegistry::load());

        let attacker = app.world_mut().spawn_empty().id();
        let target = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, 70.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 70.0))),
                CollisionVolume {
                    radius: 5.0,
                    mid_y: 0.0,
                },
                UnitType(crate::units::content::definitions::UnitKind::Bit),
            ))
            .id();

        app.world_mut().spawn((
            LaserBolt {
                origin: Vec3::ZERO,
                direction: Vec3::Z,
                total_distance: 100.0,
                speed: 100.0,
                max_length: 50.0,
                thickness: 1.0,
                elapsed: 0.0,
                material: Handle::default(),
                cap_material: None,
            },
            Transform::IDENTITY,
            DelayedHit {
                target: Some(target),
                attacker,
                weapon: WeaponId::BUILD_LASER,
                attacker_distance: 100.0,
                on_impact: None,
            },
        ));

        // Tick 1: 0.7 s → lead advances 0..70. Target volume sits at
        // z=70 ± 5, so the segment 0→70 crosses the front of the
        // sphere at z = 65.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(700));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();

        let queue = app.world().resource::<DamageQueue>();
        assert_eq!(queue.len(), 1);
        let impact_z = queue
            .iter_snapshot_for_test()
            .next()
            .expect("damage entry")
            .impact_pos
            .z;
        assert!(
            (impact_z - 65.0).abs() < 0.5,
            "impact should land on the sphere front (~65), got {impact_z}",
        );
    }

    /// Broad-phase: a hostile unit standing in the bolt's path —
    /// not the intended target — should intercept the shot, with the
    /// resolved `PendingDamage::target` pointing at the interloper.
    /// Friendly units sharing the attacker's team are skipped entirely
    /// (matches upstream's default `collidefriendly=0`).
    #[test]
    fn laser_bolt_broad_phase_intercepts_enemy_in_path_skips_friendly() {
        use crate::units::components::Faction;
        use crate::units::content::definitions::UnitKind;
        use crate::units::spatial::SpatialEntry;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<CegRenderAssets>()
            .init_resource::<FxQuadBatches>()
            .init_resource::<S3OModelCache>()
            .insert_resource(CegRegistry::load());

        // Attacker on team 0 / System.
        let attacker = app.world_mut().spawn((TeamId(0), Faction::System)).id();

        // Friendly unit at z=30, directly on the bolt's path. Same
        // team + faction → must be skipped.
        let friendly = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, 30.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 30.0))),
                CollisionVolume {
                    radius: 5.0,
                    mid_y: 0.0,
                },
                UnitType(UnitKind::Bit),
                TeamId(0),
                Faction::System,
            ))
            .id();

        // Hostile interloper at z=50, also on the path — not the
        // intended target. Should soak the broad-phase hit.
        let enemy = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, 50.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 50.0))),
                CollisionVolume {
                    radius: 5.0,
                    mid_y: 0.0,
                },
                UnitType(UnitKind::Bug),
                TeamId(1),
                Faction::Hacker,
            ))
            .id();

        // Intended target way out at z=100.
        let intended = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(0.0, 0.0, 100.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 100.0))),
                CollisionVolume {
                    radius: 5.0,
                    mid_y: 0.0,
                },
                UnitType(UnitKind::Bug),
                TeamId(1),
                Faction::Hacker,
            ))
            .id();

        // Populate the spatial index with all three on-path units.
        let entries = [
            (friendly, Vec3::new(0.0, 0.0, 30.0), 0u8, Faction::System),
            (enemy, Vec3::new(0.0, 0.0, 50.0), 1u8, Faction::Hacker),
            (intended, Vec3::new(0.0, 0.0, 100.0), 1u8, Faction::Hacker),
        ];
        let mut spatial = app.world_mut().resource_mut::<SpatialIndex>();
        for (entity, pos, team, _) in entries {
            spatial.insert_for_test(SpatialEntry {
                entity,
                pos,
                hit_radius: 0.0,
                mid_y: 0.0,
                team,
                kind: UnitKind::Bit,
                hp_positive: true,
                is_flying: false,
                cloaked: false,
                detected_by: 0,
            });
        }

        app.world_mut().spawn((
            LaserBolt {
                origin: Vec3::ZERO,
                direction: Vec3::Z,
                total_distance: 100.0,
                speed: 100.0,
                max_length: 50.0,
                thickness: 1.0,
                elapsed: 0.0,
                material: Handle::default(),
                cap_material: None,
            },
            Transform::IDENTITY,
            DelayedHit {
                target: Some(intended),
                attacker,
                weapon: WeaponId::BUILD_LASER,
                attacker_distance: 100.0,
                on_impact: None,
            },
        ));

        // Tick: 0.6 s — lead advances 0..60. Segment crosses
        // friendly's volume at z=25 first, but the friendly filter
        // skips it; the enemy at z=50 intercepts at z≈45.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(600));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();

        let queue = app.world().resource::<DamageQueue>();
        assert_eq!(queue.len(), 1, "exactly one hit (the enemy interloper)");
        let damage = queue.iter_snapshot_for_test().next().expect("damage entry");
        assert_eq!(
            damage.target,
            Some(enemy),
            "broad-phase must redirect damage to the interloper, NOT the original target",
        );
        assert!(
            (damage.impact_pos.z - 45.0).abs() < 0.5,
            "impact should land on enemy's near face (~45), got {}",
            damage.impact_pos.z,
        );
    }

    /// Mid-flight interception only fires when the bolt's segment
    /// actually crosses the target volume. A target offset off the
    /// bolt's flight line should NOT receive an early hit; the bolt
    /// flies on to the end of its ttl and fades without exploding.
    #[test]
    fn laser_bolt_does_not_intercept_off_axis_target() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<CegRenderAssets>()
            .init_resource::<FxQuadBatches>()
            .init_resource::<S3OModelCache>()
            .insert_resource(CegRegistry::load());

        let attacker = app.world_mut().spawn_empty().id();
        // Target way off to the side — bolt path is along +Z so
        // segment never enters the target's x=50 sphere.
        let target = app
            .world_mut()
            .spawn((
                Transform::from_translation(Vec3::new(50.0, 0.0, 70.0)),
                GlobalTransform::from(Transform::from_translation(Vec3::new(50.0, 0.0, 70.0))),
                CollisionVolume {
                    radius: 5.0,
                    mid_y: 0.0,
                },
                UnitType(crate::units::content::definitions::UnitKind::Bit),
            ))
            .id();

        app.world_mut().spawn((
            LaserBolt {
                origin: Vec3::ZERO,
                direction: Vec3::Z,
                total_distance: 100.0,
                speed: 100.0,
                max_length: 50.0,
                thickness: 1.0,
                elapsed: 0.0,
                material: Handle::default(),
                cap_material: None,
            },
            Transform::IDENTITY,
            DelayedHit {
                target: Some(target),
                attacker,
                weapon: WeaponId::BUILD_LASER,
                attacker_distance: 100.0,
                on_impact: None,
            },
        ));

        // Tick: 0.7 s — lead at z=70, well past the off-axis target.
        // Mid-flight check must miss; lead has not yet reached z=100,
        // so no fallback fires either. Queue stays empty.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(700));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        assert!(
            app.world().resource::<DamageQueue>().is_empty(),
            "off-axis target must not trigger mid-flight interception",
        );

        // Tick 2: another 0.4 s → lead reaches 110 (clamped 100), the
        // end of the bolt's ttl. Nothing was struck, so — like
        // `CLaserProjectile` — the bolt just fades: no damage, no
        // explosion, anywhere.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(400));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
        assert!(
            app.world().resource::<DamageQueue>().is_empty(),
            "a bolt that reaches the end of its flight without a hit does no damage",
        );
        assert!(
            app.world()
                .resource::<PendingExplosions>()
                .events
                .is_empty()
        );
    }

    /// A shell that runs its course without sweeping a volume struck its
    /// target only if it came down inside the target's sphere; a sprayed
    /// shell landing beside it just splashes the ground.
    #[test]
    fn arrival_credits_the_target_only_inside_its_sphere() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        let target = app
            .world_mut()
            .spawn((
                GlobalTransform::from_xyz(100.0, 0.0, 0.0),
                UnitType(crate::units::content::definitions::UnitKind::Bit),
                CollisionVolume {
                    radius: 16.0,
                    mid_y: 16.0,
                },
            ))
            .id();
        let (inside, beside, none) = app
            .world_mut()
            .run_system_once(
                move |q: Query<(&GlobalTransform, &CollisionVolume), With<UnitType>>| {
                    (
                        arrival_hit(Some(target), Vec3::new(104.0, 4.0, 0.0), &q),
                        arrival_hit(Some(target), Vec3::new(140.0, 0.0, 0.0), &q),
                        arrival_hit(None, Vec3::new(100.0, 0.0, 0.0), &q),
                    )
                },
            )
            .unwrap();
        assert!(matches!(inside, HitWho::Intended));
        assert!(matches!(beside, HitWho::Ground));
        assert!(matches!(none, HitWho::Ground));
    }

    /// Shared app for flight-model integration tests: bare resources
    /// the projectile loop touches, no units in the spatial index.
    fn flight_test_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingExplosions>()
            .init_resource::<WeaponRegistry>()
            .init_resource::<SpatialIndex>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<CegRenderAssets>()
            .init_resource::<FxQuadBatches>()
            .init_resource::<S3OModelCache>()
            .insert_resource(CegRegistry::load());
        app
    }

    fn spawn_flight_projectile(
        app: &mut App,
        flight: Flight,
        origin: Vec3,
        target: Vec3,
        speed: f32,
        entity: Option<Entity>,
    ) -> Entity {
        let velocity = match flight {
            Flight::Missile(m) => m.dir * m.speed * GAME_SPEED,
            Flight::Starburst(s) => s.dir * s.speed * GAME_SPEED,
            Flight::Ballistic { velocity, .. } => velocity,
            Flight::Direct => Vec3::ZERO,
        };
        app.world_mut()
            .spawn((
                ProjectileVisual {
                    origin,
                    target,
                    speed,
                    progress: 0.0,
                    arc_height: 0.0,
                    trail: None,
                    flight,
                    velocity,
                    elapsed: 0.0,
                    trail_ceg: None,
                    trail_seed: 0x1234_5678,
                    last_target_pos: None,
                },
                Transform::from_translation(origin),
                DelayedHit {
                    target: None,
                    attacker: entity.unwrap_or(Entity::PLACEHOLDER),
                    weapon: WeaponId::BUILD_LASER,
                    attacker_distance: target.distance(origin),
                    on_impact: None,
                },
            ))
            .id()
    }

    fn sim_tick(app: &mut App) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f64(1.0 / 30.0));
        app.world_mut().run_system_once(tick_weapon_fx).unwrap();
    }

    fn flow_missile() -> spring_tdf::WeaponDef {
        spring_tdf::WeaponDef {
            weapon_type: "StarburstLauncher".into(),
            range: 350.0,
            weapon_velocity: 2000.0,
            start_velocity: 400.0,
            weapon_acceleration: 600.0,
            fixed_launcher: true,
            tracks: true,
            weapon_timer: 0.1,
            turn_rate: 60000.0,
            flight_time: 5.0,
            ..Default::default()
        }
    }

    /// Pointer's Geometric (MissileLauncher, trajectoryheight=1): the
    /// `extraHeight` arc lifts it well above the direct line, it comes
    /// down on the (ground) target and detonates on the terrain.
    #[test]
    fn geometric_missile_arcs_then_lands_on_ground_target() {
        let mut app = flight_test_app();
        let w = spring_tdf::WeaponDef {
            weapon_type: "MissileLauncher".into(),
            range: 1400.0,
            weapon_velocity: 400.0,
            start_velocity: 400.0,
            trajectory_height: 1.0,
            tracks: true,
            turn_rate: 20000.0,
            ..Default::default()
        };
        let target = Vec3::new(400.0, 0.0, 0.0);
        let origin = Vec3::new(0.0, 10.0, 0.0);
        let (flight, pos) =
            super::super::flight::MissileFlight::launch(super::super::flight::Launch {
                weapon: &w,
                muzzle_pos: origin,
                muzzle_dir: Vec3::X,
                target_pos: target,
            });
        let proj =
            spawn_flight_projectile(&mut app, Flight::Missile(flight), pos, target, 400.0, None);

        let mut max_y = 0.0f32;
        let mut arrived = false;
        for _ in 0..300 {
            sim_tick(&mut app);
            let Some(read) = app.world().get::<Transform>(proj) else {
                arrived = true;
                break;
            };
            max_y = max_y.max(read.translation.y);
        }
        assert!(arrived, "missile should come down on the target");
        assert!(max_y > 80.0, "trajectoryheight arc, got max_y={max_y}");
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);
        let dmg = app
            .world()
            .resource::<DamageQueue>()
            .iter_snapshot_for_test()
            .next()
            .expect("damage");
        assert!(
            dmg.impact_pos.distance(target) < 30.0,
            "guided missile should land on/near the target, got {}",
            dmg.impact_pos,
        );
    }

    /// `CMissileProjectile::UpdateTargeting` tracks the target's
    /// `aimPos` — the collision volume's centre — not its ground
    /// position. A Geometric fired at a tall unit (small sphere hanging
    /// well above its root) must steer into the volume mid-air; homing
    /// on the feet would dive past the sphere and eat terrain under it.
    #[test]
    fn geometric_missile_homes_onto_the_targets_midpoint() {
        let mut app = flight_test_app();
        let w = spring_tdf::WeaponDef {
            weapon_type: "MissileLauncher".into(),
            range: 1400.0,
            weapon_velocity: 400.0,
            start_velocity: 400.0,
            trajectory_height: 1.0,
            tracks: true,
            turn_rate: 20000.0,
            ..Default::default()
        };
        // A tall unit: its authored midpoint sits 40 elmos above the
        // root, the collision sphere is small compared to that offset.
        let ground_pos = Vec3::new(400.0, 0.0, 0.0);
        let aim_pos = ground_pos + Vec3::Y * 40.0;
        let target = app
            .world_mut()
            .spawn((
                GlobalTransform::from_translation(ground_pos),
                CollisionVolume {
                    radius: 14.0,
                    mid_y: 40.0,
                },
                UnitType(crate::units::content::definitions::UnitKind::Pointer),
            ))
            .id();
        let origin = Vec3::new(0.0, 10.0, 0.0);
        let (flight, pos) =
            super::super::flight::MissileFlight::launch(super::super::flight::Launch {
                weapon: &w,
                muzzle_pos: origin,
                muzzle_dir: Vec3::X,
                target_pos: aim_pos,
            });
        let proj =
            spawn_flight_projectile(&mut app, Flight::Missile(flight), pos, aim_pos, 412.0, None);
        app.world_mut().get_mut::<DelayedHit>(proj).unwrap().target = Some(target);

        let mut arrived = false;
        for _ in 0..300 {
            sim_tick(&mut app);
            if app.world().get::<Transform>(proj).is_none() {
                arrived = true;
                break;
            }
        }
        assert!(arrived, "missile should reach the target");
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);
        let dmg = app
            .world()
            .resource::<DamageQueue>()
            .iter_snapshot_for_test()
            .next()
            .expect("damage");
        assert_eq!(dmg.target, Some(target));
        assert!(
            dmg.impact_pos.distance(aim_pos) < 32.0,
            "must intercept the elevated volume (centre {aim_pos}), hit at {}",
            dmg.impact_pos,
        );
    }

    /// Flow's FlowMissile, launched level along the muzzle (a flyer's
    /// forward gunpoint) at a unit target *below and behind the launch
    /// line*: it flies out straight for the `weapontimer` frames, swings
    /// round at `turnrate` and hits the target's collision volume — and
    /// the model is oriented along its flight direction.
    #[test]
    fn starburst_from_the_muzzle_swings_onto_a_unit_target() {
        let mut app = flight_test_app();
        let target_pos = Vec3::new(-100.0, 0.0, 150.0);
        let target = app
            .world_mut()
            .spawn((
                GlobalTransform::from_translation(target_pos),
                CollisionVolume {
                    radius: 12.0,
                    mid_y: 0.0,
                },
                UnitType(crate::units::content::definitions::UnitKind::Bit),
            ))
            .id();
        let w = flow_missile();
        let origin = Vec3::new(0.0, 140.0, 0.0);
        let (flight, pos) =
            super::super::flight::StarburstFlight::launch(super::super::flight::Launch {
                weapon: &w,
                muzzle_pos: origin,
                muzzle_dir: Vec3::X,
                target_pos,
            });
        let proj = spawn_flight_projectile(
            &mut app,
            Flight::Starburst(flight),
            pos,
            target_pos,
            400.0,
            None,
        );
        app.world_mut().get_mut::<DelayedHit>(proj).unwrap().target = Some(target);

        let mut path = vec![pos];
        let mut hit = false;
        for _ in 0..200 {
            sim_tick(&mut app);
            let Some(tf) = app.world().get::<Transform>(proj) else {
                hit = true;
                break;
            };
            path.push(tf.translation);
            let model_fwd = tf.rotation * Vec3::Z;
            let vel = app.world().get::<ProjectileVisual>(proj).unwrap().velocity;
            assert!(
                model_fwd.dot(vel.normalize()) > 0.999,
                "model faces its flight"
            );
        }
        assert!(hit, "missile must reach its target");
        // Frames 1-2 go straight along the muzzle (+X).
        assert!(path[2].x > path[1].x && (path[2].z - pos.z).abs() < 1e-3);
        let dmg = app
            .world()
            .resource::<DamageQueue>()
            .iter_snapshot_for_test()
            .next()
            .expect("damage");
        assert_eq!(dmg.target, Some(target));
        assert!(
            dmg.impact_pos.distance(target_pos) < 14.0,
            "hit at {}",
            dmg.impact_pos
        );
    }

    #[test]
    fn ballistic_shell_arcs_and_lands_on_target() {
        // Exploit's BugCannon: ballistic=1, startvelocity 800, myGravity
        // 0.3 × map gravity 50 = 15 → the low-arc lob. The shell must
        // show a real rise mid-flight and detonate at/near the target.
        let mut app = flight_test_app();
        let origin = Vec3::ZERO;
        let target = Vec3::new(1200.0, 0.0, 0.0);
        let v = 800.0_f32;
        let g = 0.3_f32 * 50.0_f32;
        let d = 1200.0_f32;
        let disc = v * v * v * v - g * (g * d * d);
        let tan_theta = (v * v - disc.sqrt()) / (g * d);
        let cos_theta = 1.0_f32 / (1.0 + tan_theta * tan_theta).sqrt();
        let velocity = Vec3::X * (v * cos_theta) + Vec3::Y * (v * cos_theta * tan_theta);
        let proj = spawn_flight_projectile(
            &mut app,
            Flight::Ballistic {
                velocity,
                gravity: g,
            },
            origin,
            target,
            velocity.length(),
            None,
        );

        let mut max_y = 0.0f32;
        let mut arrived = false;
        for _ in 0..240 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(10));
            app.world_mut().run_system_once(tick_weapon_fx).unwrap();
            let Some(read) = app.world().get::<Transform>(proj) else {
                arrived = true;
                break;
            };
            max_y = max_y.max(read.translation.y);
        }
        assert!(arrived, "ballistic shell must land");
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);
        assert!(
            max_y > 2.0,
            "BugCannon shell should visibly arc (gravity 15), got max_y={max_y}",
        );
        let dmg = app
            .world()
            .resource::<DamageQueue>()
            .iter_snapshot_for_test()
            .next()
            .expect("damage");
        assert!(
            dmg.impact_pos.distance(target) < 16.0,
            "low-arc solve should land on the target, got {}/",
            dmg.impact_pos,
        );
    }
}
