//! Weapon visual effects — beams, projectiles, and impact flashes.
//!
//! The combat system pushes [`AttackEvent`]s into a [`PendingAttacks`] buffer.
//! Two systems consume them each frame:
//!
//! - [`spawn_beam_visuals`] — short-lived line entities for beam/laser weapons
//! - [`spawn_projectile_visuals`] — traveling entities for ballistic weapons
//!
//! A third system [`tick_weapon_fx`] fades/moves/despawns the visuals over time.

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

// ── Beam effect ─────────────────────────────────────────────────────

/// Marker for a beam visual entity.
#[derive(Component)]
pub struct BeamVisual {
    pub lifetime: f32,
    pub max_lifetime: f32,
}

/// Shared material cache so we don't create one per beam per frame.
#[derive(Resource, Default)]
pub struct BeamMaterialCache {
    materials: Vec<(LinearRgba, Handle<StandardMaterial>)>,
}

impl BeamMaterialCache {
    fn get_or_create(
        &mut self,
        color: LinearRgba,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        // Reuse a material with similar color (quantized to avoid explosion).
        let quantized = LinearRgba::new(
            (color.red * 4.0).round() / 4.0,
            (color.green * 4.0).round() / 4.0,
            (color.blue * 4.0).round() / 4.0,
            1.0,
        );
        for (cached_color, handle) in &self.materials {
            if (*cached_color - quantized).length_squared() < 0.01 {
                return handle.clone();
            }
        }
        let handle = materials.add(StandardMaterial {
            base_color: Color::LinearRgba(quantized),
            emissive: quantized * 8.0,
            unlit: true,
            alpha_mode: AlphaMode::Add,
            ..default()
        });
        self.materials.push((quantized, handle.clone()));
        handle
    }
}

// ── Projectile effect ───────────────────────────────────────────────

/// A projectile traveling from origin to target.
#[derive(Component)]
pub struct ProjectileVisual {
    pub origin: Vec3,
    pub target: Vec3,
    pub speed: f32,
    pub progress: f32,
    pub gravity: f32,
    pub arc_height: f32,
}

/// Shared mesh + material for projectiles.
#[derive(Resource)]
pub struct ProjectileAssets {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
}

// ── Systems ─────────────────────────────────────────────────────────

/// Spawn beam visuals from pending attacks.
pub fn spawn_beam_visuals(
    mut pending: ResMut<PendingAttacks>,
    weapon_registry: Res<WeaponRegistry>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut beam_cache: ResMut<BeamMaterialCache>,
) {
    for event in pending.events.drain(..) {
        let Some(weapon) = weapon_registry.get(event.weapon_name) else {
            continue;
        };

        // Determine if this is a beam or a projectile.
        let is_beam = weapon.beam_weapon
            || weapon.beam_laser
            || weapon.weapon_type == "BeamLaser"
            || weapon.weapon_type == "Melee";
        let is_projectile = weapon.ballistic
            || !weapon.model.is_empty()
            || weapon.weapon_type == "MissileLauncher"
            || weapon.weapon_type == "StarburstLauncher"
            || weapon.weapon_type == "Cannon"
            || weapon.weapon_type == "LaserCannon"
            || weapon.weapon_type == "AircraftBomb";

        if is_beam {
            spawn_beam(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut beam_cache,
            );
        } else if is_projectile {
            spawn_projectile(&event, weapon, &mut commands, &mut meshes, &mut materials);
        } else {
            // Default: treat as beam.
            spawn_beam(
                &event,
                weapon,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut beam_cache,
            );
        }
    }
}

fn spawn_beam(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    beam_cache: &mut BeamMaterialCache,
) {
    let dir = event.target_pos - event.attacker_pos;
    let length = dir.length();
    if length < 0.1 {
        return;
    }
    let midpoint = (event.attacker_pos + event.target_pos) / 2.0;

    // Weapon color — TDF stores RGB 0-255 or 0-1 scale.
    let [r, g, b] = weapon.rgb_color;
    let color = if r > 2.0 || g > 2.0 || b > 2.0 {
        // 0-255 scale
        LinearRgba::new(r / 255.0, g / 255.0, b / 255.0, 1.0)
    } else {
        // 0-1 scale
        LinearRgba::new(r, g, b, 1.0)
    };

    let thickness = (weapon.thickness * 0.3).max(0.5).min(4.0);

    let beam_material = beam_cache.get_or_create(color, materials);

    // Beam duration from TDF, with fallback.
    let duration = if weapon.duration > 0.0 {
        weapon.duration
    } else if weapon.beam_time > 0.0 {
        weapon.beam_time
    } else {
        0.15
    };

    let mesh = meshes.add(Cuboid::new(thickness, thickness, length));

    let rotation = Quat::from_rotation_arc(Vec3::Z, dir.normalize());

    commands.spawn((
        BeamVisual {
            lifetime: duration,
            max_lifetime: duration,
        },
        Mesh3d(mesh),
        MeshMaterial3d(beam_material),
        Transform::from_translation(midpoint).with_rotation(rotation),
    ));
}

fn spawn_projectile(
    event: &AttackEvent,
    weapon: &spring_tdf::WeaponDef,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let [r, g, b] = weapon.rgb_color;
    let color = if r > 2.0 || g > 2.0 || b > 2.0 {
        LinearRgba::new(r / 255.0, g / 255.0, b / 255.0, 1.0)
    } else if r == 0.0 && g == 0.0 && b == 0.0 {
        // No color specified — use a neutral bright color.
        LinearRgba::new(0.8, 0.8, 0.4, 1.0)
    } else {
        LinearRgba::new(r, g, b, 1.0)
    };

    let speed = if weapon.weapon_velocity > 0.0 {
        weapon.weapon_velocity
    } else {
        400.0
    };

    let arc_height = weapon.trajectory_height * 0.5;
    let gravity = weapon.my_gravity;

    let proj_size = (weapon.size * 0.5).max(2.0).min(8.0);
    let mesh = meshes.add(Sphere::new(proj_size));
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
            gravity,
            arc_height,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(event.attacker_pos),
    ));
}

/// Tick beam lifetimes (fade out) and projectile movement, despawning when done.
pub fn tick_weapon_fx(
    time: Res<Time>,
    mut beams: Query<(Entity, &mut BeamVisual, &mut Transform)>,
    mut projectiles: Query<(Entity, &mut ProjectileVisual, &mut Transform), Without<BeamVisual>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();

    // Beams: count down lifetime, scale down Y to fade.
    for (entity, mut beam, mut transform) in &mut beams {
        beam.lifetime -= dt;
        if beam.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let fade = beam.lifetime / beam.max_lifetime;
        let current_scale = transform.scale;
        transform.scale = Vec3::new(
            current_scale.x * fade.sqrt(),
            current_scale.y * fade.sqrt(),
            current_scale.z,
        );
    }

    // Projectiles: move along path, despawn on arrival.
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

        // Arc: parabolic offset peaking at t=0.5.
        if proj.arc_height > 0.0 || proj.gravity > 0.0 {
            let arc = 4.0 * t * (1.0 - t);
            let height_offset = proj.arc_height * total_dist * arc;
            // Gravity pulls downward over time.
            let gravity_offset = 0.5 * proj.gravity * total_dist * t * t;
            pos.y += height_offset - gravity_offset;
        }

        transform.translation = pos;
    }
}
