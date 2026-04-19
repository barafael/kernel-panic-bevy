//! Spawning side of weapon visuals: drains `PendingAttacks` and routes each
//! event to the appropriate effect spawner (beam, burst, projectile, melee,
//! plus the bonus nanoframe sparkle for build lasers).

use bevy::prelude::*;

use super::shared::{
    AttackEvent, BeamMaterialCache, BeamVisual, BuildSparkle, BuildSparkleAssets, BurstSegment,
    GroundFlash, GroundFlashAssets, ImpactBurst, ImpactBurstAssets, PendingAttacks,
    PendingExplosions, ProjectileVisual, WeaponFxMeshes, tdf_color,
};
use crate::units::assets::meshes::{S3OModelCache, load_raw_bevy_texture};
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
    asset_server: Res<AssetServer>,
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

        // Classify weapon by its TDF properties.
        let is_projectile = weapon.is_projectile();
        let is_burst_beam = weapon.beam_burst || weapon.spray_angle > 100.0;
        let is_melee = weapon.category() == spring_tdf::WeaponCategory::Melee;

        if is_melee {
            spawn_melee_flash(
                &event,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                &mut fx_meshes,
            );
        } else if is_projectile {
            spawn_projectile(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                &mut fx_meshes,
            );
        } else if is_burst_beam {
            spawn_burst_beam(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
                &mut fx_meshes,
            );
        } else {
            spawn_beam(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &mut cache,
                &mut fx_meshes,
            );
        }

        // Muzzle flash at the attacker's muzzle-piece position (or origin
        // if no muzzle was resolved). Re-uses the impact-burst system as
        // a pragmatic `BitmapMuzzleFlame` substitute — a short, bright,
        // weapon-tinted pop so the player sees which of their units just
        // fired. Melee attacks already have their own flash and build
        // lasers shouldn't strobe the builder, so both are excluded.
        if !is_melee && !is_build_laser(&event.weapon_name) {
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
        } else if !is_melee {
            // Every non-melee / non-BuildLaser weapon pops a colored impact
            // burst at the target so the player gets visual feedback even
            // when the full upstream CEG isn't loaded.
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
            // A flat expanding ring at the impact plane adds weight to the
            // sphere-shaped burst and stands in for the upstream `GroundFlash`
            // CEG subsection most KP explosions include. Muzzle flash at the
            // shooter is left ringless — it's at barrel height, not on the
            // ground.
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
    mut cache: ResMut<BeamMaterialCache>,
    mut impact_assets: ResMut<ImpactBurstAssets>,
    mut flash_assets: ResMut<GroundFlashAssets>,
) {
    for event in pending.events.drain(..) {
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
    let material = cache.get_or_create_with_intensity(color, true, 2.0, None, materials);

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

/// Pull the beam texture filename (e.g. `arrow`) off a weapon def and
/// resolve it to a Bevy handle. Upstream quotes the name without an
/// extension — we try `.tga` (the format on disk) and cache the
/// result keyed by the raw name so repeated shots of the same weapon
/// pay disk I/O once. Returns `(name, handle)` ready for the material
/// cache key, or `None` for untextured weapons.
fn beam_texture<'a>(
    tex1: &'a str,
    model_cache: &mut S3OModelCache,
    images: &mut Assets<Image>,
) -> Option<(&'a str, Handle<Image>)> {
    if tex1.is_empty() || tex1 == "none" {
        return None;
    }
    let filename = format!("{tex1}.tga");
    let handle = load_raw_bevy_texture(&filename, model_cache, images)?;
    Some((tex1, handle))
}

/// Beam (Line, MegaBeam, BugShot, DOS_Beam, VirusBeam, GaussCannon).
#[allow(clippy::too_many_arguments)]
fn spawn_beam(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
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
    let midpoint = (event.attacker_pos + event.target_pos) / 2.0;
    let color = tdf_color(weapon.rgb_color);

    // Thickness from TDF, clamped for visibility.
    let thickness = (weapon.thickness * 0.25).clamp(0.3, 6.0);

    // Duration from TDF fields. `beam_decay` controls fade speed — upstream
    // values near 0.8 mean the beam lingers; we stretch the base lifetime
    // by it so PacketBeam and GaussCannon read as longer flashes than
    // their raw duration field suggests.
    let base = if weapon.beam_time > 0.0 {
        weapon.beam_time
    } else if weapon.duration > 0.0 {
        weapon.duration
    } else {
        0.15
    };
    let duration = if weapon.beam_decay > 0.0 {
        base * (1.0 + weapon.beam_decay)
    } else {
        base
    };

    let texture = beam_texture(&weapon.texture1, model_cache, images);
    let mat = cache.get_or_create_with_intensity(color, true, weapon.intensity, texture, materials);
    let rotation = Quat::from_rotation_arc(Vec3::Z, dir.normalize());

    // The Byte's MegaBeam fires in bursts of 4 thick rectangles.
    // The Line weapon has corethickness=1 which gives a thinner core + thicker outer.
    // Most beam weapons: single cuboid from A to B.
    let core = weapon.core_thickness * 0.25;
    if core > 0.1 && thickness > 1.0 {
        // Two-layer beam: bright thin core + dimmer outer. Core stays
        // untextured so the bright white stripe always reads cleanly
        // over the atlased outer.
        let core_mat = cache.get_or_create_with_intensity(
            LinearRgba::WHITE,
            true,
            weapon.intensity,
            None,
            materials,
        );
        let unit_cube = fx_meshes.unit_cube(meshes);
        commands.spawn((
            BeamVisual {
                lifetime: duration,
                max_lifetime: duration,
                base_thickness: core,
                length,
            },
            Mesh3d(unit_cube),
            MeshMaterial3d(core_mat),
            Transform::from_translation(midpoint + Vec3::Y * 0.1)
                .with_rotation(rotation)
                .with_scale(Vec3::new(core, core, length)),
        ));
    }

    let unit_cube = fx_meshes.unit_cube(meshes);
    commands.spawn((
        BeamVisual {
            lifetime: duration,
            max_lifetime: duration,
            base_thickness: thickness,
            length,
        },
        Mesh3d(unit_cube),
        MeshMaterial3d(mat),
        Transform::from_translation(midpoint)
            .with_rotation(rotation)
            .with_scale(Vec3::new(thickness, thickness, length)),
    ));
}

/// Burst beam (PacketBeam — multiple small beams with spray).
#[allow(clippy::too_many_arguments)]
fn spawn_burst_beam(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cache: &mut BeamMaterialCache,
    fx_meshes: &mut WeaponFxMeshes,
) {
    let color = tdf_color(weapon.rgb_color);
    let texture = beam_texture(&weapon.texture1, model_cache, images);
    let mat = cache.get_or_create_with_intensity(color, true, weapon.intensity, texture, materials);

    let dir = event.target_pos - event.attacker_pos;
    let length = dir.length();
    let base_dir = dir.normalize();

    // Number of burst segments.
    let count = if weapon.burst > 1.0 {
        weapon.burst as usize
    } else {
        3
    };

    let spray_rad = (weapon.spray_angle / 65536.0) * std::f32::consts::TAU;
    let thickness = (weapon.thickness * 0.2).clamp(0.3, 2.0);

    let ttl = if weapon.beam_ttl > 0.0 {
        weapon.beam_ttl / 30.0 // beam_ttl is in frames (30fps)
    } else {
        0.12
    };

    let unit_cube = fx_meshes.unit_cube(meshes);
    let scale = Vec3::new(thickness, thickness, length);

    for i in 0..count {
        let angle = spray_rad * (i as f32 / count as f32 - 0.5);
        let perturbed = Quat::from_rotation_y(angle) * base_dir;
        let end = event.attacker_pos + perturbed * length;
        let mid = (event.attacker_pos + end) / 2.0;
        let rotation = Quat::from_rotation_arc(Vec3::Z, perturbed);

        commands.spawn((
            BurstSegment { lifetime: ttl },
            Mesh3d(unit_cube.clone()),
            MeshMaterial3d(mat.clone()),
            Transform::from_translation(mid)
                .with_rotation(rotation)
                .with_scale(scale),
        ));
    }
}

/// Projectile (Geometric, BugCannon, end_game_logic_bomb).
fn spawn_projectile(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    fx_meshes: &mut WeaponFxMeshes,
) {
    let color = tdf_color(weapon.rgb_color);

    let speed = if weapon.weapon_velocity > 0.0 {
        weapon.weapon_velocity
    } else {
        400.0
    };

    let arc_height = weapon.trajectory_height * 0.4;
    let proj_size = (weapon.size * 0.4).clamp(1.5, 6.0);

    // Geometric uses an octahedron model — approximate with a small cube.
    let mesh = if !weapon.model.is_empty() && weapon.model != ";" {
        fx_meshes.unit_cube(meshes)
    } else {
        fx_meshes.unit_sphere(meshes)
    };

    // Route through the shared cache so projectiles that re-use the same
    // color + intensity share a single StandardMaterial handle instead of
    // minting a fresh one per shot.
    let material =
        cache.get_or_create_with_intensity(color, false, weapon.intensity, None, materials);

    // Upstream weapons that configure `smoketrail=1` or a `cegTag=...`
    // (BugCannon → corruption_BCtrail, WMD → corruption_WMDtrail, DOS
    // particle trail) draw a trailing cloud as the shell flies. We can't
    // load the real CEG here, but dropping a tiny faction-coloured puff
    // every few frames reads as the same visual — a streak of motion
    // behind the projectile. Plain laser projectiles (no smoke, no ceg)
    // stay trail-less so the Bit's arrow-beam doesn't get a ghost tail.
    let has_trail = weapon.smoke_trail || !weapon.ceg_tag.is_empty();
    let trail_rgb = has_trail.then_some(weapon.rgb_color);

    commands.spawn((
        ProjectileVisual {
            origin: event.attacker_pos,
            target: event.target_pos,
            speed,
            progress: 0.0,
            arc_height,
            trail_rgb,
            // ~8 puffs/second — dense enough to read as a streak, sparse
            // enough that a long arc shot doesn't mint hundreds of
            // impact-burst entities.
            trail_interval: 0.12,
            trail_accumulator: 0.0,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(event.attacker_pos).with_scale(Vec3::splat(proj_size)),
    ));
}

/// Melee flash (Wormbite).
fn spawn_melee_flash(
    event: &AttackEvent,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
    fx_meshes: &mut WeaponFxMeshes,
) {
    let flash_pos = (event.attacker_pos + event.target_pos) / 2.0;
    let mesh = fx_meshes.unit_sphere(meshes);
    let color = LinearRgba::new(1.0, 0.3, 0.0, 0.8);
    let material = cache.get_or_create_with_intensity(color, true, 1.0, None, materials);
    let size = 8.0;
    commands.spawn((
        BeamVisual {
            lifetime: 0.15,
            max_lifetime: 0.15,
            base_thickness: size,
            length: size,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(flash_pos).with_scale(Vec3::splat(size)),
    ));
}
