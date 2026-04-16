//! Weapon visual effects — beams, projectiles, and impact flashes.
//!
//! The combat system pushes [`AttackEvent`]s into a [`PendingAttacks`] buffer.
//! [`spawn_weapon_visuals`] drains the buffer, looks up the weapon in the TDF
//! registry, and spawns the appropriate effect entity.
//! [`tick_weapon_fx`] fades/moves/despawns the visuals over time.

use bevy::prelude::*;

use super::weapons::WeaponRegistry;

// ── Shared types ────────────────────────────────────────────────────

/// Describes a single attack for the visual system.
pub struct AttackEvent {
    pub attacker_pos: Vec3,
    pub target_pos: Vec3,
    pub weapon_name: &'static str,
}

/// Buffer written by the combat system, drained by visual systems.
#[derive(Resource, Default)]
pub struct PendingAttacks {
    pub events: Vec<AttackEvent>,
}

// ── Effect components ───────────────────────────────────────────────

/// A beam visual that fades over its lifetime.
#[derive(Component)]
pub struct BeamVisual {
    pub lifetime: f32,
    pub max_lifetime: f32,
}

/// A projectile traveling from origin to target.
#[derive(Component)]
pub struct ProjectileVisual {
    pub origin: Vec3,
    pub target: Vec3,
    pub speed: f32,
    pub progress: f32,
    pub arc_height: f32,
}

/// A burst of multiple small beam segments (spray weapons like PacketBeam).
#[derive(Component)]
pub struct BurstSegment {
    pub lifetime: f32,
}

/// Shared material cache to avoid per-frame allocations.
#[derive(Resource, Default)]
pub struct BeamMaterialCache {
    entries: Vec<CachedMaterial>,
}

struct CachedMaterial {
    key: MaterialKey,
    handle: Handle<StandardMaterial>,
}

#[derive(Clone, Copy, PartialEq)]
struct MaterialKey {
    r: u8,
    g: u8,
    b: u8,
    additive: bool,
}

impl BeamMaterialCache {
    fn get_or_create(
        &mut self,
        color: LinearRgba,
        additive: bool,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let key = MaterialKey {
            r: (color.red.clamp(0.0, 1.0) * 15.0).round() as u8,
            g: (color.green.clamp(0.0, 1.0) * 15.0).round() as u8,
            b: (color.blue.clamp(0.0, 1.0) * 15.0).round() as u8,
            additive,
        };
        for entry in &self.entries {
            if entry.key == key {
                return entry.handle.clone();
            }
        }
        let alpha_mode = if additive {
            AlphaMode::Add
        } else {
            AlphaMode::Blend
        };
        let handle = materials.add(StandardMaterial {
            base_color: Color::LinearRgba(color),
            emissive: color * 8.0,
            unlit: true,
            alpha_mode,
            ..default()
        });
        self.entries.push(CachedMaterial {
            key,
            handle: handle.clone(),
        });
        handle
    }
}

// ── Color helper ────────────────────────────────────────────────────

/// TDF stores RGB either 0-255 or 0-1. Normalize to LinearRgba 0-1.
fn tdf_color(rgb: [f32; 3]) -> LinearRgba {
    let [r, g, b] = rgb;
    if r > 2.0 || g > 2.0 || b > 2.0 {
        LinearRgba::new(r / 255.0, g / 255.0, b / 255.0, 1.0)
    } else if r == 0.0 && g == 0.0 && b == 0.0 {
        LinearRgba::new(0.7, 0.7, 0.7, 1.0)
    } else {
        LinearRgba::new(r, g, b, 1.0)
    }
}

// ── Main spawn system ───────────────────────────────────────────────

pub fn spawn_weapon_visuals(
    mut pending: ResMut<PendingAttacks>,
    weapon_registry: Res<WeaponRegistry>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<BeamMaterialCache>,
) {
    for event in pending.events.drain(..) {
        let Some(weapon) = weapon_registry.get(event.weapon_name) else {
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
    }
}

// ── Beam (Line, MegaBeam, BugShot, DOS_Beam, VirusBeam, GaussCannon) ───

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

    // Duration from TDF fields.
    let duration = if weapon.beam_time > 0.0 {
        weapon.beam_time
    } else if weapon.duration > 0.0 {
        weapon.duration
    } else {
        0.15
    };

    let mat = cache.get_or_create(color, true, materials);
    let rotation = Quat::from_rotation_arc(Vec3::Z, dir.normalize());

    // The Byte's MegaBeam fires in bursts of 4 thick rectangles.
    // The Line weapon has corethickness=1 which gives a thinner core + thicker outer.
    // Most beam weapons: single cuboid from A to B.
    let core = weapon.core_thickness * 0.25;
    if core > 0.1 && thickness > 1.0 {
        // Two-layer beam: bright thin core + dimmer outer.
        let core_mat = cache.get_or_create(LinearRgba::WHITE, true, materials);
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

// ── Burst beam (PacketBeam — multiple small beams with spray) ───────

fn spawn_burst_beam(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BeamMaterialCache,
) {
    let color = tdf_color(weapon.rgb_color);
    let mat = cache.get_or_create(color, true, materials);

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

// ── Projectile (Geometric, BugCannon, end_game_logic_bomb) ──────────

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

// ── Melee flash (Wormbite) ──────────────────────────────────────────

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

// ── Tick system ─────────────────────────────────────────────────────

pub fn tick_weapon_fx(
    time: Res<Time>,
    mut beams: Query<(Entity, &mut BeamVisual, &mut Transform), Without<ProjectileVisual>>,
    mut bursts: Query<
        (Entity, &mut BurstSegment),
        (Without<BeamVisual>, Without<ProjectileVisual>),
    >,
    mut projectiles: Query<(Entity, &mut ProjectileVisual, &mut Transform)>,
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
}
