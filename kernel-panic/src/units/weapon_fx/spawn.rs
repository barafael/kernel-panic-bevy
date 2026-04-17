//! Spawning side of weapon visuals: drains `PendingAttacks` and routes each
//! event to the appropriate effect spawner (beam, burst, projectile, melee,
//! plus the bonus nanoframe sparkle for build lasers).

use bevy::prelude::*;

use super::shared::{
    AttackEvent, BeamMaterialCache, BeamVisual, BuildSparkle, BuildSparkleAssets, BurstSegment,
    ImpactBurst, ImpactBurstAssets, PendingAttacks, ProjectileVisual, tdf_color,
};
use crate::units::weapons::WeaponRegistry;

/// True for `BuildLaser` (the upstream build-laser weapon name). The
/// `BuildLaserNoEffect` variant intentionally suppresses the impact particles,
/// so only the bare-name version triggers `BuildSparkle` spawn.
fn is_build_laser(weapon_name: &str) -> bool {
    weapon_name == "BuildLaser"
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_weapon_visuals(
    mut pending: ResMut<PendingAttacks>,
    weapon_registry: Res<WeaponRegistry>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<BeamMaterialCache>,
    mut sparkle_assets: ResMut<BuildSparkleAssets>,
    mut impact_assets: ResMut<ImpactBurstAssets>,
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
        let is_projectile = weapon.ballistic
            || matches!(
                weapon.weapon_type.as_str(),
                "MissileLauncher" | "StarburstLauncher" | "Cannon" | "AircraftBomb"
            )
            || (!weapon.model.is_empty() && weapon.model != ";");

        let is_burst_beam = weapon.beam_burst || weapon.spray_angle > 100.0;
        let is_melee = weapon.weapon_type == "Melee";

        if is_melee {
            spawn_melee_flash(&event, weapon, &mut commands, &mut meshes, &mut materials);
        } else if is_projectile {
            spawn_projectile(&event, weapon, &mut commands, &mut meshes, &mut materials);
        } else if is_burst_beam {
            spawn_burst_beam(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
            );
        } else {
            spawn_beam(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
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
        }
    }
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

/// Beam (Line, MegaBeam, BugShot, DOS_Beam, VirusBeam, GaussCannon).
fn spawn_beam(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
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

    let mat = cache.get_or_create_with_intensity(color, true, weapon.intensity, materials);
    let rotation = Quat::from_rotation_arc(Vec3::Z, dir.normalize());

    // The Byte's MegaBeam fires in bursts of 4 thick rectangles.
    // The Line weapon has corethickness=1 which gives a thinner core + thicker outer.
    // Most beam weapons: single cuboid from A to B.
    let core = weapon.core_thickness * 0.25;
    if core > 0.1 && thickness > 1.0 {
        // Two-layer beam: bright thin core + dimmer outer.
        let core_mat = cache.get_or_create_with_intensity(
            LinearRgba::WHITE,
            true,
            weapon.intensity,
            materials,
        );
        let core_mesh = meshes.add(Cuboid::new(core, core, length));
        commands.spawn((
            BeamVisual {
                lifetime: duration,
                max_lifetime: duration,
            },
            Mesh3d(core_mesh),
            MeshMaterial3d(core_mat),
            Transform::from_translation(midpoint + Vec3::Y * 0.1).with_rotation(rotation),
        ));
    }

    let mesh = meshes.add(Cuboid::new(thickness, thickness, length));
    commands.spawn((
        BeamVisual {
            lifetime: duration,
            max_lifetime: duration,
        },
        Mesh3d(mesh),
        MeshMaterial3d(mat),
        Transform::from_translation(midpoint).with_rotation(rotation),
    ));
}

/// Burst beam (PacketBeam — multiple small beams with spray).
fn spawn_burst_beam(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
) {
    let color = tdf_color(weapon.rgb_color);
    let mat = cache.get_or_create_with_intensity(color, true, weapon.intensity, materials);

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

    let mesh = meshes.add(Cuboid::new(thickness, thickness, length));

    for i in 0..count {
        let angle = spray_rad * (i as f32 / count as f32 - 0.5);
        let perturbed = Quat::from_rotation_y(angle) * base_dir;
        let end = event.attacker_pos + perturbed * length;
        let mid = (event.attacker_pos + end) / 2.0;
        let rotation = Quat::from_rotation_arc(Vec3::Z, perturbed);

        commands.spawn((
            BurstSegment { lifetime: ttl },
            Mesh3d(mesh.clone()),
            MeshMaterial3d(mat.clone()),
            Transform::from_translation(mid).with_rotation(rotation),
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
        meshes.add(Cuboid::new(proj_size, proj_size, proj_size))
    } else {
        meshes.add(Sphere::new(proj_size))
    };

    let material = materials.add(StandardMaterial {
        base_color: Color::LinearRgba(color),
        emissive: color * 6.0,
        unlit: true,
        ..default()
    });

    commands.spawn((
        ProjectileVisual {
            origin: event.attacker_pos,
            target: event.target_pos,
            speed,
            progress: 0.0,
            arc_height,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(event.attacker_pos),
    ));
}

/// Melee flash (Wormbite).
fn spawn_melee_flash(
    event: &AttackEvent,
    _weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let flash_pos = (event.attacker_pos + event.target_pos) / 2.0;
    let mesh = meshes.add(Sphere::new(8.0));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.3, 0.0, 0.8),
        emissive: LinearRgba::new(1.0, 0.3, 0.0, 1.0) * 4.0,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        ..default()
    });
    commands.spawn((
        BeamVisual {
            lifetime: 0.15,
            max_lifetime: 0.15,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(flash_pos),
    ));
}
