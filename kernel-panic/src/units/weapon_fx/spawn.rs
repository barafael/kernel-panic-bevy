//! Spawning side of weapon visuals: drains `PendingAttacks` and routes each
//! event to the appropriate effect spawner (beam, burst, projectile, melee,
//! plus the bonus nanoframe sparkle for build lasers).

use bevy::prelude::*;

use super::ceg::{CegParticleMesh, CegRegistry, spawn_ceg};
use super::shared::{
    AttackEvent, BeamMaterialCache, BeamVisual, BuildSparkle, BuildSparkleAssets, DelayedHit,
    GroundFlash, GroundFlashAssets, ImpactBurst, ImpactBurstAssets, LaserBolt, PendingAttacks,
    PendingExplosions, ProjectileTrail, ProjectileVisual, TRAIL_SAMPLE_COUNT, WeaponFxMeshes,
    build_billboard_quad_mesh, tdf_color, weapon_core_color, weapon_edge_color,
};
use crate::units::assets::meshes::{S3OModelCache, load_beam_texture, load_s3o_mesh};
use crate::units::content::weapons::WeaponRegistry;

/// True for `BuildLaser` (the upstream build-laser weapon name). The
/// `BuildLaserNoEffect` variant intentionally suppresses the impact particles,
/// so only the bare-name version triggers `BuildSparkle` spawn.
fn is_build_laser(weapon_name: &str) -> bool {
    weapon_name == "BuildLaser"
}

/// Radius (elmos) of the muzzle-flash burst at the firing unit. Small
/// enough that it reads as "that unit just shot" without obscuring the
/// unit itself; the underlying [`ImpactBurst`] decays over its fixed
/// lifetime so there's nothing to tune per weapon.
const MUZZLE_FLASH_RADIUS: f32 = 6.0;

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_weapon_visuals(
    mut pending: ResMut<PendingAttacks>,
    weapon_registry: Res<WeaponRegistry>,
    ceg_registry: Res<CegRegistry>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cache: ResMut<BeamMaterialCache>,
    mut sparkle_assets: ResMut<BuildSparkleAssets>,
    mut impact_assets: ResMut<ImpactBurstAssets>,
    mut flash_assets: ResMut<GroundFlashAssets>,
    mut fx_meshes: ResMut<WeaponFxMeshes>,
    mut particle_mesh: ResMut<CegParticleMesh>,
    asset_server: Res<AssetServer>,
    mut rng: Local<u32>,
) {
    for event in pending.events.drain(..) {
        let Some(weapon) = weapon_registry.get(&event.weapon_name) else {
            continue;
        };

        let dir = event.target_pos - event.attacker_pos;
        let length = dir.length();
        if length < 0.1 {
            continue;
        }

        // Classify through the typed category — `derived_weapon_type`
        // runs the `weapondefs_post.lua` legacy-tag shim so weapons that
        // authored `beamweapon=1 lineofsight=1` without a literal
        // `weaponType=` still land on `LaserCannon` (Bit `Line`, Byte
        // `MegaBeam`, RetroDeath family, MineLauncher). Shim details
        // live on [`spring_tdf::WeaponDef::derived_weapon_type`].
        let category = weapon.category();
        let is_melee = category == spring_tdf::WeaponCategory::Melee;
        let is_projectile = weapon.is_projectile();
        let is_beam_laser = category == spring_tdf::WeaponCategory::BeamLaser;
        let is_laser_cannon = category == spring_tdf::WeaponCategory::LaserCannon;

        // Primary visual entity — the projectile / bolt that carries
        // the `DelayedHit` if this attack has deferred damage.
        let mut primary_visual: Option<Entity> = None;

        if is_melee {
            spawn_melee_flash(
                &event,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                &mut fx_meshes,
                &mut impact_assets,
            );
        } else if is_projectile {
            primary_visual = Some(spawn_projectile(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
                &mut fx_meshes,
            ));
        } else if is_laser_cannon {
            primary_visual = Some(spawn_laser_bolt(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
                &mut fx_meshes,
            ));
        } else if is_beam_laser {
            spawn_textured_beam(
                &event,
                weapon,
                true,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
                &mut fx_meshes,
            );
        } else {
            // Untyped weapon (shouldn't happen for KP's roster). Fall
            // back to a flat untextured beam so there's *some* feedback.
            spawn_textured_beam(
                &event,
                weapon,
                false,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
                &mut fx_meshes,
            );
        }

        // Hand the deferred payload to the visual `tick_weapon_fx`
        // will process on impact. The weapon's name is enough to
        // recover rgb / AoE / explosion CEG via the registry then.
        if let (Some(visual), Some(delayed)) = (primary_visual, event.delayed_hit.as_ref()) {
            commands.entity(visual).insert(DelayedHit {
                target: delayed.target,
                attacker: delayed.attacker,
                weapon: event.weapon_name.clone(),
                attacker_distance: delayed.attacker_distance,
            });
        }

        // Muzzle flash at the attacker's muzzle-piece position.
        //
        // When the unit's FBI declared a `[SFXTypes]` table and combat
        // filled in `event.muzzle_ceg`, replay that CEG verbatim — this
        // is the path that gives Bit the cyan `arrowflare` muzzle
        // (`custom:oldskool_shot2`) and Byte/Pointer the soft-blue
        // `oldskool_shot1` puff. When no SFX CEG was authored, fall
        // back to the synthesised coloured sphere so there's still a
        // "something fired" signal. Melee / BuildLaser skip both — see
        // `is_melee` / `is_build_laser` filters.
        if !is_melee && !is_build_laser(&event.weapon_name) {
            let ceg_spawned = if let Some(muzzle_ceg) = event.muzzle_ceg.as_deref() {
                let muzzle_dir = (event.target_pos - event.attacker_pos).normalize_or(Vec3::Y);
                spawn_ceg(
                    muzzle_ceg,
                    event.attacker_pos,
                    muzzle_dir,
                    &ceg_registry,
                    &mut rng,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut images,
                    &mut model_cache,
                    &mut particle_mesh,
                )
            } else {
                false
            };
            if !ceg_spawned {
                spawn_impact_burst(
                    event.attacker_pos,
                    weapon.rgb_color,
                    MUZZLE_FLASH_RADIUS,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut cache,
                    &mut impact_assets,
                );
            }
        }

        // Build lasers also drop a short-lived "nanoframe pixel" sprite at
        // the target end (upstream `oldskool_build` CEG). The NoEffect variant
        // intentionally skips this.
        if is_build_laser(&event.weapon_name) {
            spawn_build_sparkle(
                event.target_pos,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut sparkle_assets,
                &asset_server,
            );
        } else if !is_melee && event.delayed_hit.is_none() {
            // Upstream CEG is the source of truth for impact particles:
            // the weapon's `explosiongenerator=custom:NAME` resolves to a
            // CSimpleParticleSystem definition in `gamedata/explosions/`.
            // Replay those particles faithfully — colormap, size growth,
            // directional spread, lifetime all come from the authored
            // script. Fall back to the synthesised burst + ground flash
            // only when the CEG is missing or references a class we
            // don't yet support (CBitmapMuzzleFlame etc).
            let dir = (event.target_pos - event.attacker_pos).normalize_or(Vec3::Y);
            let used_ceg = !weapon.explosion_generator.is_empty()
                && spawn_ceg(
                    &weapon.explosion_generator,
                    event.target_pos,
                    dir,
                    &ceg_registry,
                    &mut rng,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut images,
                    &mut model_cache,
                    &mut particle_mesh,
                );
            if !used_ceg {
                let aoe = weapon.area_of_effect.max(4.0);
                spawn_impact_burst(
                    event.target_pos,
                    weapon.rgb_color,
                    aoe,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut cache,
                    &mut impact_assets,
                );
                spawn_ground_flash(
                    event.target_pos,
                    weapon.rgb_color,
                    aoe,
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut cache,
                    &mut flash_assets,
                );
            }
        }
    }
}

/// Drain every [`PendingExplosions`] entry and spawn a matching
/// burst + ground flash. No beam, no muzzle flash, no projectile — this
/// path is for standalone detonations (unit-death `ExplodeAs`, kamikaze
/// triggers, command-fire area blasts). Scaling matches the per-weapon
/// impact so a Logic Bomb's death boom reads the same visual language
/// as its in-flight hit.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_pending_explosions(
    mut pending: ResMut<PendingExplosions>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cache: ResMut<BeamMaterialCache>,
    mut impact_assets: ResMut<ImpactBurstAssets>,
    mut flash_assets: ResMut<GroundFlashAssets>,
    mut particle_mesh: ResMut<CegParticleMesh>,
    ceg_registry: Res<CegRegistry>,
    mut rng: Local<u32>,
) {
    for event in pending.events.drain(..) {
        // Drive standalone explosions through the same CEG lookup: if
        // the caller passed a named CEG (death `ExplodeAs`, mine kill,
        // SIGTERM), replay its emitters; otherwise fall back to the
        // synthesised burst so there's still feedback for unscripted
        // detonations.
        let used_ceg = !event.ceg_name.is_empty()
            && spawn_ceg(
                &event.ceg_name,
                event.pos,
                Vec3::Y,
                &ceg_registry,
                &mut rng,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut particle_mesh,
            );
        if !used_ceg {
            let aoe = event.radius.max(4.0);
            spawn_impact_burst(
                event.pos,
                event.rgb,
                aoe,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                &mut impact_assets,
            );
            spawn_ground_flash(
                event.pos,
                event.rgb,
                aoe,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                &mut flash_assets,
            );
        }
    }
}

/// Flat horizontal emissive ring at `pos`. Radius animates from 0.25× to
/// 1.5× `aoe` over the lifetime so the ring visibly "blooms" outward from
/// the impact point before fading. Mesh is a shared unit-circle; the
/// spawn-time scale carries the initial radius so the tick system only
/// needs to grow `base_radius` — no material mutation per frame.
///
/// The ground plane is approximated at `pos.y + 0.5` so the ring sits
/// slightly above terrain without z-fighting. Units that explode mid-air
/// (Flow, command-fire projectiles) still project their ring close to
/// the blast center — good enough; a world-space ground snap would
/// require a heightmap sample and isn't worth the dependency on this
/// visual-only path.
#[allow(clippy::too_many_arguments)]
fn spawn_ground_flash(
    pos: Vec3,
    rgb: [f32; 3],
    aoe: f32,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    flash_assets: &mut GroundFlashAssets,
) {
    let mesh = flash_assets
        .mesh
        .get_or_insert_with(|| meshes.add(Circle::new(1.0)))
        .clone();

    let color = tdf_color(rgb);
    let material = cache.get_or_create_tiled(color, true, 2.0, None, 0, materials);

    // Clamp so even mines / SIGTERM don't swallow the screen, but keep
    // beam pings (AoE=8) readable. Small ring radius feels snappier than
    // the full blast sphere.
    let base_radius = (aoe * 0.5).clamp(4.0, 80.0);
    let life = 0.45;

    // `Circle` is XY by default; rotate to lie flat on XZ so it hugs the ground.
    let flat = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);

    commands.spawn((
        GroundFlash {
            lifetime: life,
            max_lifetime: life,
            base_radius,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(pos + Vec3::Y * 0.5)
            .with_rotation(flat)
            .with_scale(Vec3::splat(base_radius * 0.25)),
    ));
}

#[allow(clippy::too_many_arguments)]
fn spawn_impact_burst(
    target_pos: Vec3,
    rgb: [f32; 3],
    aoe: f32,
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

    // Ground-hit weapons benefit from a bigger puff; single-target hit-scan
    // (AoE=8) gets a small blip, while Logic Bomb (AoE=512) bursts large.
    // Clamp so even the largest explosions stay readable.
    let base_size = (aoe * 0.25).clamp(3.0, 24.0);
    let life = 0.35;

    commands.spawn((
        ImpactBurst {
            lifetime: life,
            max_lifetime: life,
            base_size,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(target_pos + Vec3::Y * 2.0).with_scale(Vec3::splat(base_size)),
    ));
}

/// Mirrors the upstream CEG params:
///   particleLife=16 ± 8 frames @ 30 fps      → 0.27–0.80 s
///   particleSize=3 ± 4                       → ~world units across
///   particleSpeed=2 ± .1, emitVector=(0,1,0) → slight upward drift
///   airdrag=1                                → kills velocity fast
///   colorMap=white, white, transparent black → fade to nothing
fn spawn_build_sparkle(
    target_pos: Vec3,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sparkle_assets: &mut BuildSparkleAssets,
    asset_server: &AssetServer,
) {
    let mesh = sparkle_assets
        .mesh
        .get_or_insert_with(|| meshes.add(Rectangle::new(1.0, 1.0)))
        .clone();
    let material = sparkle_assets
        .material
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::WHITE,
                base_color_texture: Some(asset_server.load("sfx/hollowsquare.png")),
                emissive: LinearRgba::WHITE * 4.0,
                unlit: true,
                alpha_mode: AlphaMode::Add,
                cull_mode: None,
                ..default()
            })
        })
        .clone();

    // Cheap deterministic-ish jitter: hash position bits + frame for variety
    // without pulling in `rand`. Stable enough for the eye.
    let h = (target_pos.x.to_bits() ^ target_pos.z.to_bits().rotate_left(13)) as f32;
    let r0 = (h * 0.000_000_2).fract();
    let r1 = ((h * 0.000_001_3).fract() * 7.0).fract();
    let r2 = ((h * 0.000_011_1).fract() * 13.0).fract();
    let r3 = ((h * 0.000_111_1).fract() * 17.0).fract();

    // particleSize=3 ± 4 → roughly 1..7 world units. Clamp so we don't get tiny invisible specks.
    let size = (3.0 + (r0 - 0.5) * 4.0).clamp(1.5, 7.0);
    // particleLife=16 ± 8 frames @ 30fps → 0.27..0.80s.
    let life = (16.0 + (r1 - 0.5) * 8.0) / 30.0;
    // Slight horizontal scatter and upward drift (emitVector y=1, speed≈2 elmos/frame).
    let scatter = Vec3::new((r2 - 0.5) * 4.0, 1.0, (r3 - 0.5) * 4.0);
    let velocity = scatter.normalize_or(Vec3::Y) * 30.0; // ~2 elmos/frame * 30fps

    let spawn_pos = target_pos + Vec3::Y * 1.0; // pos=0,1.0,0 in CEG

    commands.spawn((
        BuildSparkle {
            lifetime: life,
            max_lifetime: life,
            velocity,
            base_size: size,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(spawn_pos).with_scale(Vec3::splat(size)),
    ));
}

/// Resolve a TDF `texture1=...` name to a Bevy handle *with repeat
/// sampling enabled* plus the texture's pixel dimensions, so callers
/// can compute a tile count that preserves the texture's native
/// aspect ratio along the beam length. Returns `None` for textures
/// that couldn't be loaded (disk miss) or weapons that set `texture1=`
/// to empty / `none`.
///
/// Upstream `RESOURCES.TDF` declares atlas aliases — `bytelasermid`
/// points at the on-disk `bytemegabeammid.tga`, not a file named after
/// the alias itself. We consult [`CegRegistry::resolve_texture`] first
/// (which mirrors the same alias table) and fall back to a literal
/// `{name}.tga` lookup for weapon textures that don't happen to be
/// aliased (e.g. `circle.tga` already lives on disk under that name).
fn beam_texture<'a>(
    tex1: &'a str,
    model_cache: &mut S3OModelCache,
    images: &mut Assets<Image>,
) -> Option<(&'a str, Handle<Image>, f32)> {
    if tex1.is_empty() || tex1.eq_ignore_ascii_case("none") {
        return None;
    }
    let resolved = CegRegistry::resolve_texture(tex1)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{tex1}.tga"));
    let (handle, w, h) = load_beam_texture(&resolved, model_cache, images)?;
    let aspect = if h > 0 { w as f32 / h as f32 } else { 1.0 };
    Some((tex1, handle, aspect))
}

/// Spawn a `BeamLaser` hit-scan ribbon from attacker to target.
///
/// Mirrors the upstream two-pass draw in
/// `rts/Sim/Projectiles/WeaponProjectiles/BeamLaserProjectile.cpp`:
/// the outer pass draws the full-thickness quad with the texture
/// tinted by `rgbColor` ([`weapon_edge_color`]); the core pass draws
/// a `thickness * corethickness` quad on top with `rgbColor2` =
/// white ([`weapon_core_color`]), which is what preserves baked-colour
/// textures unchanged in the center. Both passes use the same beam
/// texture (`texture1`); end caps (`texture2`) would go through
/// `visuals.texture2` but we haven't ported that detail yet.
#[allow(clippy::too_many_arguments)]
fn spawn_textured_beam(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    is_beam_laser: bool,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cache: &mut BeamMaterialCache,
    fx_meshes: &mut WeaponFxMeshes,
) {
    let dir = event.target_pos - event.attacker_pos;
    let length = dir.length();
    if length < 0.1 {
        return;
    }
    // Trust the TDF-authored thickness verbatim. The earlier 0.9×
    // squeeze + clamp(1.5, 12.0) suppressed Byte's authored 16-elmo
    // MegaBeam down to a 12-unit stripe, which is why the impact
    // flare and core were reading as a unit hairline rather than the
    // fat upstream ribbon.
    let thickness = weapon.thickness.max(1.0);

    // BeamLaser lifetime comes from `beamtime`/`beamttl`. Color decay is
    // a separate per-frame multiplier on the vertex colors (see
    // `BeamVisual.decay`); it doesn't extend the lifetime, contrary to
    // an earlier port-ism. 0.08 s is a pragmatic floor so single-frame
    // shots still visibly flash.
    let lifetime = if is_beam_laser {
        let ttl_sec = weapon.beam_ttl / 30.0; // beam_ttl is frames @ 30fps
        weapon.beam_time.max(ttl_sec).max(0.08)
    } else {
        weapon.duration.max(0.08)
    };

    // Tile the texture along beam length so `arrow`'s `>>>>` or
    // `dosray`'s `01010101` stream reads as a sequence of glyphs
    // rather than one stretched smear. 56 elmos/tile matches the
    // on-screen glyph size in upstream footage at default zoom.
    const ARROW_TILE_LENGTH: f32 = 56.0;
    let texture = beam_texture(&weapon.texture1, model_cache, images);
    let has_texture = texture.is_some();
    let tile_count = if has_texture {
        ((length / ARROW_TILE_LENGTH).round() as u32).clamp(1, 24)
    } else {
        0
    };

    // Outer (edge) pass: each beam entity owns a 4-vertex mesh that
    // the tick system rewrites per frame with camera-facing corners.
    let edge_color = weapon_edge_color(weapon);
    let outer_mat = cache.get_or_create_tiled(
        edge_color,
        true,
        weapon.intensity,
        texture
            .as_ref()
            .map(|(name, handle, _)| (*name, handle.clone())),
        tile_count,
        materials,
    );
    let outer_mesh = meshes.add(build_billboard_quad_mesh());
    commands.spawn((
        BeamVisual {
            start: event.attacker_pos,
            end: event.target_pos,
            thickness,
            lifetime,
            max_lifetime: lifetime,
            mesh: outer_mesh.clone(),
            decay: weapon.beam_decay,
        },
        Mesh3d(outer_mesh),
        MeshMaterial3d(outer_mat),
        Transform::IDENTITY,
    ));

    // Core pass: `corethickness × rgbColor2 (white) × texture`. Always
    // drawn when authored > 0. For `corethickness=1` the core fully
    // covers the outer and baked-colour textures (arrow cyan,
    // bytemegabeam magenta) come through intact. Lower ratios let the
    // outer halo peek around for a two-tone look.
    let core_ratio = weapon.core_thickness.clamp(0.0, 1.0);
    if core_ratio > 0.01 {
        let core_thickness = thickness * core_ratio;
        let core_color = weapon_core_color(weapon);
        let core_mat = cache.get_or_create_tiled(
            core_color,
            true,
            weapon.intensity.max(1.0),
            texture.map(|(name, handle, _)| (name, handle)),
            tile_count,
            materials,
        );
        let core_mesh = meshes.add(build_billboard_quad_mesh());
        commands.spawn((
            BeamVisual {
                start: event.attacker_pos,
                end: event.target_pos,
                thickness: core_thickness,
                lifetime,
                max_lifetime: lifetime,
                mesh: core_mesh.clone(),
                decay: weapon.beam_decay,
            },
            Mesh3d(core_mesh),
            MeshMaterial3d(core_mat),
            Transform::IDENTITY,
        ));
    }
    // `fx_meshes` is still shared with other spawners; this path no
    // longer pulls the old `beam_quad` handle.
    let _ = fx_meshes;
    let _ = length;
}

/// Spawn a traveling laser bolt for `LaserCannon` weapons (Bit `Line`,
/// Byte `MegaBeam`, RetroDeath death streaks, Bug `BugShot`, Virus
/// `VirusBeam`, MineLauncher). Upstream's
/// `rts/Sim/Projectiles/WeaponProjectiles/LaserProjectile.cpp`
/// renders a short segment of length
/// `max_length = duration * weapon_velocity` flying at
/// `weapon_velocity` elmos/sec: the lead extends from the muzzle,
/// the tail trails by up to `max_length`, then contracts after
/// impact. [`tick_weapon_fx`] animates position + length.
///
/// The atlas stretches once across the moving bolt — we don't tile,
/// matching upstream's `tex1->xstart..xend` assignment at tail/lead.
/// Same two-pass draw as `spawn_textured_beam`: outer edge with
/// `rgbColor × texture`, core with white × texture (covered fully
/// when `corethickness=1`).
#[allow(clippy::too_many_arguments)]
fn spawn_laser_bolt(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cache: &mut BeamMaterialCache,
    fx_meshes: &mut WeaponFxMeshes,
) -> Entity {
    let delta = event.target_pos - event.attacker_pos;
    let distance = delta.length().max(0.1);
    let direction = delta / distance;

    // Trust the authored thickness — see spawn_textured_beam.
    let thickness = weapon.thickness.max(1.0);

    let speed = if weapon.weapon_velocity > 0.0 {
        weapon.weapon_velocity
    } else {
        256.0
    };
    let duration = weapon.duration.max(0.05);
    // `max_length = duration * speed` per upstream, with a floor of
    // `thickness * 3` so very short ticks still read as a bolt.
    let max_length = (speed * duration).max(thickness * 3.0);

    let texture = beam_texture(&weapon.texture1, model_cache, images);
    let has_texture = texture.is_some();
    let tile_count: u32 = if has_texture { 1 } else { 0 };

    // Each bolt instance owns its own 4-vertex mesh; the tick system
    // rewrites the corners each frame using the current camera. No
    // transform scaling, no rotation — positions go in world space.
    let edge_color = weapon_edge_color(weapon);
    let outer_mat = cache.get_or_create_tiled(
        edge_color,
        true,
        weapon.intensity,
        texture
            .as_ref()
            .map(|(name, handle, _)| (*name, handle.clone())),
        tile_count,
        materials,
    );
    let outer_mesh = meshes.add(build_billboard_quad_mesh());
    let outer_entity = commands
        .spawn((
            LaserBolt {
                origin: event.attacker_pos,
                direction,
                total_distance: distance,
                speed,
                max_length,
                thickness,
                elapsed: 0.0,
                mesh: outer_mesh.clone(),
            },
            Mesh3d(outer_mesh),
            MeshMaterial3d(outer_mat),
            Transform::IDENTITY,
        ))
        .id();

    // Core pass: `corethickness × white × texture`. For Bit's
    // `Line` (corethickness=1) the core is full width and fully covers
    // the outer pass, leaving the arrow texture's baked cyan intact.
    // For Byte's MegaBeam (corethickness=0.5) the outer magenta halo
    // surrounds a hot-white core.
    let core_ratio = weapon.core_thickness.clamp(0.0, 1.0);
    if core_ratio > 0.01 {
        let core_thickness = thickness * core_ratio;
        let core_color = weapon_core_color(weapon);
        let core_mat = cache.get_or_create_tiled(
            core_color,
            true,
            weapon.intensity.max(1.0),
            texture.map(|(name, handle, _)| (name, handle)),
            tile_count,
            materials,
        );
        let core_mesh = meshes.add(build_billboard_quad_mesh());
        commands.spawn((
            LaserBolt {
                origin: event.attacker_pos,
                direction,
                total_distance: distance,
                speed,
                max_length,
                thickness: core_thickness,
                elapsed: 0.0,
                mesh: core_mesh.clone(),
            },
            Mesh3d(core_mesh),
            MeshMaterial3d(core_mat),
            Transform::IDENTITY,
        ));
    }
    // `fx_meshes` remains in the signature for the other spawners;
    // this path no longer pulls the shared `beam_quad` handle.
    let _ = fx_meshes;
    outer_entity
}

/// Projectile (Pointer Geometric / NX, BugCannon, SigTerm bomb,
/// Logic Bomb end-game blast, Cannon shells).
///
/// Loads the weapon's authored [`spring_tdf::WeaponDef::model`]
/// (`octashot.s3o` for the Pointer, `sigterm.s3o` for the Terminal's
/// airstrike) through the shared [`S3OModelCache`] so every shot of
/// the same weapon reuses one mesh handle. When no model is set the
/// projectile falls back to a small unit sphere — this covers plain
/// cannon/plasma weapons that upstream Spring renders as a sprite
/// billboard.
///
/// `arc_height` is the authored `trajectoryHeight` scaled to look
/// right at typical map ranges (upstream stores a fraction of target
/// distance; we bake in a gentler 0.4× factor so pointer shots
/// don't arc into orbit). A full 1.0× curve puts the apex at the
/// same Y as the distance — too bouncy in 3D camera; the tick system
/// applies a 4·t·(1-t) parabola on top which is already a full arc.
#[allow(clippy::too_many_arguments)]
fn spawn_projectile(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cache: &mut BeamMaterialCache,
    fx_meshes: &mut WeaponFxMeshes,
) -> Entity {
    let color = tdf_color(weapon.rgb_color);

    let speed = if weapon.weapon_velocity > 0.0 {
        weapon.weapon_velocity
    } else {
        400.0
    };

    let arc_height = weapon.trajectory_height * 0.4;

    // Prefer the authored S3O model (octashot, sigterm, etc.). The s3o
    // meshes are authored at their real upstream size in elmos, so the
    // `proj_size` scale we apply to the fallback sphere/cube (derived
    // from the weapon's `size=`) is inappropriate here — leave real
    // models at 1.0× and the model's own geometry dictates on-screen
    // size. Unknown / missing models drop to a small sphere sized by
    // the weapon's authored `size=`, which matches upstream Spring's
    // sprite-projectile fallback for Cannon weapons.
    let model_name = weapon.model.trim().trim_end_matches(';');
    let (mesh, visual_scale) = if !model_name.is_empty() && model_name != ";" {
        if let Some(handle) = load_s3o_mesh(model_name, meshes, model_cache) {
            (handle, 1.0)
        } else {
            (
                fx_meshes.unit_sphere(meshes),
                (weapon.size * 0.4).clamp(1.5, 6.0),
            )
        }
    } else {
        (
            fx_meshes.unit_sphere(meshes),
            (weapon.size * 0.4).clamp(1.5, 6.0),
        )
    };

    // Route through the shared cache so projectiles that re-use the same
    // color + intensity share a single StandardMaterial handle instead of
    // minting a fresh one per shot.
    let material = cache.get_or_create_tiled(color, false, weapon.intensity, None, 0, materials);

    // Upstream weapons with `smoketrail=1` or a `cegTag=...` leave a
    // trailing ribbon along the flight path. Build it as a dedicated
    // triangle-strip entity textured with the weapon's `texture2`
    // (`pointertrail` / `firetrail` / …) and keep its mesh handle on
    // the projectile so the tick system can rewrite it each frame.
    let has_trail = weapon.smoke_trail || !weapon.ceg_tag.is_empty();
    let trail = if has_trail {
        build_projectile_trail(
            event.attacker_pos,
            weapon,
            commands,
            meshes,
            materials,
            images,
            model_cache,
        )
    } else {
        None
    };

    commands
        .spawn((
            ProjectileVisual {
                origin: event.attacker_pos,
                target: event.target_pos,
                speed,
                progress: 0.0,
                arc_height,
                trail,
            },
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_translation(event.attacker_pos).with_scale(Vec3::splat(visual_scale)),
        ))
        .id()
}

/// Spawn a companion entity that carries the projectile's trail mesh.
///
/// The mesh starts empty — the tick system rewrites it every frame
/// from the projectile's sample ring-buffer. The material picks up
/// the weapon's `texture2` (the designated smoke-trail atlas); when
/// that texture can't be loaded the trail falls back to an untextured
/// coloured strip so something still reads as "motion behind the
/// shell".
#[allow(clippy::too_many_arguments)]
fn build_projectile_trail(
    origin: Vec3,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
) -> Option<ProjectileTrail> {
    // Half-width: trails look natural at ~0.5× the weapon's authored
    // size (or a minimum of 2 elmos so a cegTag with size=0 still
    // leaves a visible streak).
    let half_width = (weapon.size * 0.5).max(2.0);

    // Build an empty triangle-strip mesh sized for `TRAIL_SAMPLE_COUNT`
    // samples (one quad per segment, two tris per quad). The tick
    // system fills in vertex positions each frame.
    let mut mesh = Mesh::new(
        bevy::mesh::PrimitiveTopology::TriangleStrip,
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
    );
    let vert_count = TRAIL_SAMPLE_COUNT * 2;
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0_f32; 3]; vert_count]);
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 1.0, 0.0_f32]; vert_count],
    );
    // UV.u runs along the trail (0 at head, 1 at tail); UV.v alternates
    // 0/1 across the ribbon width. The tick system writes .u per
    // sample; .v stays constant so we initialise once here.
    let mut uvs = Vec::with_capacity(vert_count);
    for _ in 0..TRAIL_SAMPLE_COUNT {
        uvs.push([0.0, 0.0]);
        uvs.push([0.0, 1.0]);
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);

    let mesh_handle = meshes.add(mesh);

    // Material: tint by weapon's resolved edge color so the `texture2`
    // multiplies by the intended hue. For Pointer's Geometric the
    // rgbColor is (0,0,0) → defaults to Cannon orange, which overlays
    // `pointertrail.tga`'s baked orange-red beautifully.
    let color = weapon_edge_color(weapon);
    let texture = if !weapon.texture2.is_empty() && weapon.texture2 != "none" {
        super::super::weapon_fx::ceg::CegRegistry::resolve_texture(&weapon.texture2)
            .and_then(|filename| {
                crate::units::assets::meshes::load_beam_texture(filename, model_cache, images)
            })
            .map(|(handle, _, _)| handle)
    } else {
        None
    };
    let material = materials.add(StandardMaterial {
        base_color: Color::LinearRgba(color),
        base_color_texture: texture,
        emissive: color * 3.0,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        ..default()
    });

    let ribbon_entity = commands
        .spawn((
            Mesh3d(mesh_handle.clone()),
            MeshMaterial3d(material),
            // Mesh is already in world space — the ribbon doesn't ride
            // the projectile's transform.
            Transform::IDENTITY,
        ))
        .id();

    // Seed the sample buffer with the origin so the very first frame
    // draws a zero-length strip rather than a degenerate fan from the
    // origin; segments fill in as the projectile moves.
    let samples = vec![origin; TRAIL_SAMPLE_COUNT];

    Some(ProjectileTrail {
        ribbon_entity,
        mesh: mesh_handle,
        samples,
        half_width,
    })
}

/// Melee flash (Wormbite): a short-lived orange `ImpactBurst` at the
/// midpoint. Reuses the impact-burst component so `tick_weapon_fx`
/// handles fade/despawn uniformly; no beam geometry involved.
fn spawn_melee_flash(
    event: &AttackEvent,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    _fx_meshes: &mut WeaponFxMeshes,
    impact_assets: &mut ImpactBurstAssets,
) {
    let flash_pos = (event.attacker_pos + event.target_pos) / 2.0;
    spawn_impact_burst(
        flash_pos,
        [1.0, 0.3, 0.0],
        16.0,
        commands,
        meshes,
        materials,
        cache,
        impact_assets,
    );
}
