//! Command-fire abilities: NX Flag, Infection Gas, and (eventually)
//! SIGTERM airstrikes + Firewall reflector shields.
//!
//! These weapons have `commandfire=1` in their TDFs; the auto-fire path
//! in `combat_system` skips them, and they enter play only through
//! an explicit player order processed here.
//!
//! The shared mechanism is the `AreaDenialZone` entity: a volume that
//! deals DPS to units in radius until its TTL expires. Zone parameters
//! mirror upstream `LuaRules/Gadgets/areadenial.lua`:
//!
//! | Weapon    | Radius | DPS | TTL | Friendly-fire | Infects |
//! |-----------|--------|-----|-----|---------------|---------|
//! | nx        | 120    | 100 | 60s | yes           | no      |
//! | infection | 400    | 120 | 13s | no            | yes     |
//! | sigterm   | 350    | 2000| 3s  | yes           | no      |

use std::collections::HashMap;

use bevy::prelude::*;

use crate::units::assets::meshes::{S3OModelCache, load_s3o_mesh, unit_material};
use crate::units::combat::{Infected, weapon_infection_duration};
use crate::units::components::{Faction, Health, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::spatial::SpatialIndex;

/// Filename of the SIGTERM bomber's S3O model (`signal.fbi:ObjectName`).
const SIGNAL_MODEL: &str = "signal.s3o";
/// Filename of the SIGTERM bomb's S3O model
/// (`weapons/retroweapons.tdf:[SigTerm]:model`).
const SIGTERM_BOMB_MODEL: &str = "sigterm.s3o";

/// Lazily-loaded mesh + per-faction material handles for the two
/// SIGTERM visual stages. Mirrors `DeathParticleAssets` /
/// `BuildSparkleAssets`. The 90 s SIGTERM cooldown means cache
/// misses are rare anyway, but caching keeps the per-cast spawn
/// allocation-free and avoids leaking a fresh `StandardMaterial`
/// per cast.
#[derive(Resource, Default)]
pub struct SigTermAssets {
    signal_mesh: Option<Handle<Mesh>>,
    bomb_mesh: Option<Handle<Mesh>>,
    /// One material per faction (caster colour-tints the visual).
    signal_material: HashMap<Faction, Handle<StandardMaterial>>,
    bomb_material: HashMap<Faction, Handle<StandardMaterial>>,
}

impl SigTermAssets {
    fn signal(
        &mut self,
        faction: Faction,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
        model_cache: &mut S3OModelCache,
    ) -> (Handle<Mesh>, Handle<StandardMaterial>) {
        let mesh = self
            .signal_mesh
            .get_or_insert_with(|| {
                load_s3o_mesh(SIGNAL_MODEL, meshes, model_cache)
                    .unwrap_or_else(|| meshes.add(Cuboid::new(18.0, 6.0, 24.0)))
            })
            .clone();
        let material = self
            .signal_material
            .entry(faction)
            .or_insert_with(|| {
                unit_material(
                    UnitKind::Signal,
                    faction,
                    materials,
                    images,
                    model_cache,
                    SIGNAL_MODEL,
                )
            })
            .clone();
        (mesh, material)
    }

    fn bomb(
        &mut self,
        faction: Faction,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
        model_cache: &mut S3OModelCache,
    ) -> (Handle<Mesh>, Handle<StandardMaterial>) {
        let mesh = self
            .bomb_mesh
            .get_or_insert_with(|| {
                load_s3o_mesh(SIGTERM_BOMB_MODEL, meshes, model_cache)
                    .unwrap_or_else(|| meshes.add(Sphere::new(14.0)))
            })
            .clone();
        // Why: `unit_material` keys textures by faction, not by
        // UnitKind — Signal is the right faction lookup for both
        // stages of the strike.
        let material = self
            .bomb_material
            .entry(faction)
            .or_insert_with(|| {
                unit_material(
                    UnitKind::Signal,
                    faction,
                    materials,
                    images,
                    model_cache,
                    SIGTERM_BOMB_MODEL,
                )
            })
            .clone();
        (mesh, material)
    }
}

/// Firewall protection zone: all allies within `FIREWALL_RADIUS` at
/// cast time gain a `Protected` component for `FIREWALL_DURATION`.
pub const FIREWALL_RADIUS: f32 = 300.0;
pub const FIREWALL_DURATION: f32 = 20.0;
pub const FIREWALL_COOLDOWN: f32 = 90.0;
/// Fraction of incoming damage that a Protected unit *takes*. The
/// remaining `1.0 - DAMAGE_TAKEN_FRACTION` is reflected back to the
/// attacker.
pub const FIREWALL_DAMAGE_TAKEN: f32 = 0.5;

/// Byte `LaunchMines` ability (§3.5): suicide-cast that drops a fan of
/// Logic Bombs at the cursor. Values from upstream
/// `LuaRules/Gadgets/specialattack.lua`:
/// > "Launches several mines in a forward arc, at the cost of 6000
/// >  hitpoints. 10s reload."
pub const MINELAUNCHER_COOLDOWN: f32 = 10.0;
pub const MINELAUNCHER_HP_COST: f32 = 6000.0;
/// Below this remaining HP the Byte refuses to cast so a mis-click
/// can't instantly end it. Upstream lets the Byte suicide-cast; we
/// keep a small buffer for ergonomics.
pub const MINELAUNCHER_HP_FLOOR: f32 = 100.0;
/// Number of Logic Bombs dropped per cast. Matches the loop count in
/// upstream's `byte.bos LaunchMines` script (5 FIRE opcodes in series).
pub const MINELAUNCHER_FAN_COUNT: usize = 5;
/// Distance from the cast centre to the outermost mine in the fan.
/// The whole fan spans roughly `2 × FAN_RADIUS` across, sitting in
/// front of the Byte at the clicked position.
pub const MINELAUNCHER_FAN_RADIUS: f32 = 40.0;

/// Terminal `SIGTERM` airstrike: a [`SigTermSignal`] flies to the
/// target, then drops a [`SigTermBomb`] that gravity-falls and
/// detonates. Why: matches upstream `airstrike.lua`'s two-stage
/// strike. Neither stage carries a `UnitType`, which is what makes
/// them untargetable — same effect as upstream's
/// `Category=NUKE VTOL` + every ground unit's `NoChaseCategory=VTOL`.
pub const SIGTERM_COOLDOWN: f32 = 90.0;
/// Signal's cruise altitude. Matches signal.fbi `cruiseAlt=200`.
pub const SIGTERM_SIGNAL_ALTITUDE: f32 = 200.0;
/// Signal's ground speed. Matches signal.fbi `MaxVelocity=8` in elmos
/// per sim-frame @ 30 Hz → 240 elmos/s. Fast enough that the flight
/// reads as a jet blip, not a blimp.
pub const SIGTERM_SIGNAL_SPEED: f32 = 240.0;
/// Seconds of bomb free-fall from Signal's release altitude.
/// Matches upstream's `myGravity=0.3` — `sqrt(2 * 200 / (0.3 * 30²))`
/// works out to roughly 1.2 s from `cruiseAlt=200`.
pub const SIGTERM_FALL_DURATION: f32 = 1.2;
/// One-shot blast radius, matching upstream `SigTerm.areaofeffect=900`.
pub const SIGTERM_BLAST_RADIUS: f32 = 900.0;
/// Blast damage at the centre, pre armor-class / `damage_modifier`.
/// Matches `DAMAGE.default=10000` in `retroweapons.tdf`.
pub const SIGTERM_BLAST_DAMAGE: f32 = 10000.0;
/// Blast edge damage as a fraction of centre damage. Matches
/// `edgeeffectiveness=0.8`.
pub const SIGTERM_BLAST_EDGE: f32 = 0.8;
/// Area-denial tail: `weaponInfo[sigterm]` in upstream
/// `LuaRules/Gadgets/areadenial.lua`.
pub const SIGTERM_DENIAL_RADIUS: f32 = 350.0;
pub const SIGTERM_DENIAL_DPS: f32 = 2000.0;
pub const SIGTERM_DENIAL_TTL: f32 = 3.0;

/// SIGTERM stage 1: bomber flying from `start` to over `target`.
/// `tick_sigterm_signals` lerps along the cruise path, then spawns a
/// [`SigTermBomb`] on arrival.
#[derive(Component, Debug, Clone)]
pub struct SigTermSignal {
    pub target: Vec3,
    pub start: Vec3,
    /// Cruise-path length, divided by [`SIGTERM_SIGNAL_SPEED`] for flight time.
    pub total_distance: f32,
    pub elapsed: f32,
    pub owner_team: u8,
    pub owner_faction: Faction,
}

/// A Terminal-spawned SIGTERM bomb falling toward [`target`]. Ticked
/// by `tick_sigterm_bombs`: each frame lerps its transform toward the
/// ground hit; when `time_to_detonate` hits zero, delivers the blast +
/// denial zone and despawns the bomb entity.
#[derive(Component, Debug, Clone)]
pub struct SigTermBomb {
    pub target: Vec3,
    pub start: Vec3,
    pub time_to_detonate: f32,
    pub total_fall: f32,
    pub owner_team: u8,
    pub owner_faction: Faction,
}

/// Queued request to spawn a Logic Bomb, drained next frame by
/// [`spawning::spawn_queued_mines`]. Lives as a resource so the
/// command-fire path doesn't have to carry the 7-argument
/// `spawn_unit` parameter pack.
#[derive(Debug, Clone)]
pub struct MineSpawn {
    pub position: Vec3,
    pub faction: Faction,
    pub team: u8,
}

#[derive(Resource, Default)]
pub struct MineSpawnQueue(Vec<MineSpawn>);

impl MineSpawnQueue {
    pub fn push(&mut self, spawn: MineSpawn) {
        self.0.push(spawn);
    }

    pub fn drain(&mut self) -> std::vec::Drain<'_, MineSpawn> {
        self.0.drain(..)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// While present, the unit takes `FIREWALL_DAMAGE_TAKEN` of any
/// incoming damage and the rest is reflected to the attacker.
#[derive(Component, Debug, Clone)]
pub struct Protected {
    pub remaining: f32,
}

/// Per-caster cooldown in seconds before the command-fire ability is
/// available again. Matches upstream weapon `reloadtime` (30s for nx,
/// longer for Infection) so the ability cadence feels right without
/// reading TDF values at runtime.
#[derive(Component)]
pub struct CommandFireCooldown {
    pub remaining: f32,
}

/// Event: a selected unit should fire its command-fire ability at
/// `target`. The weapon and source-unit resolution happens when the
/// event is processed so the hotkey handler doesn't need to know which
/// slot (Weapon1 vs Weapon2) holds the ability.
#[derive(Message, Debug, Clone)]
pub struct CommandFireEvent {
    pub attacker: Entity,
    pub target: Vec3,
}

/// A persistent area-denial volume. Applies `dps` damage per second to
/// every unit in `radius` until `remaining` hits zero, then despawns.
///
/// `owner_team` / `owner_faction` are copied from the caster so friendly
/// filtering still works after the caster dies (upstream's gadget
/// reassigns ownership to a random homebase; we just cache it).
///
/// `infection_duration` is `Some(window_seconds)` for zones that tag
/// units with [`Infected`] on every DPS tick — matches upstream's
/// `areadenial.lua` re-applying the source weapon's `UnitDamaged`
/// event, which re-arms the infection window via `infection.lua`.
/// `None` means the zone does not infect at all.
#[derive(Component)]
pub struct AreaDenialZone {
    pub center: Vec3,
    pub radius: f32,
    pub dps: f32,
    pub remaining: f32,
    pub damage_friendly: bool,
    pub infection_duration: Option<f32>,
    pub owner_team: u8,
    pub owner_faction: Faction,
}

/// Drain queued `CommandFireEvent`s into persistent
/// `AreaDenialZone` entities. A unit identifies its command-fire
/// weapon by looking at its `UnitKind` — NX Flag for Pointer, Infection
/// for Obelisk. Units without a registered ability are ignored.
#[allow(clippy::too_many_arguments)]
pub fn process_command_fire(
    mut events: MessageReader<CommandFireEvent>,
    casters: Query<(
        &UnitType,
        &TeamId,
        &Faction,
        &GlobalTransform,
        Option<&CommandFireCooldown>,
    )>,
    protect_targets: Query<(Entity, &TeamId, &Faction, &GlobalTransform), With<Health>>,
    mut health_q: Query<&mut Health>,
    mut mine_spawns: ResMut<MineSpawnQueue>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut sigterm_assets: ResMut<SigTermAssets>,
    mut commands: Commands,
) {
    for event in events.read() {
        let Ok((unit, team, faction, gtf, cd)) = casters.get(event.attacker) else {
            continue;
        };
        if cd.is_some_and(|c| c.remaining > 0.0) {
            continue;
        }

        if unit.0 == UnitKind::Firewall {
            apply_firewall(
                event.target,
                team.0,
                *faction,
                &protect_targets,
                &mut commands,
            );
            commands.entity(event.attacker).insert(CommandFireCooldown {
                remaining: FIREWALL_COOLDOWN,
            });
            continue;
        }

        if unit.0 == UnitKind::Byte {
            let Ok(mut health) = health_q.get_mut(event.attacker) else {
                continue;
            };
            if !fire_minelauncher(
                event.target,
                gtf.translation(),
                &mut health,
                *faction,
                team.0,
                &mut mine_spawns,
            ) {
                continue;
            }
            commands.entity(event.attacker).insert(CommandFireCooldown {
                remaining: MINELAUNCHER_COOLDOWN,
            });
            continue;
        }

        if unit.0 == UnitKind::Terminal {
            // Signal aircraft spawns at the Terminal's altitude + a
            // climb-out offset so the takeoff reads instead of the
            // Signal punching sideways through the roof. Flies in a
            // straight line to cruise altitude directly over the
            // target, then releases a bomb. No `UnitType` on either
            // entity → not in the spatial index → not targetable.
            let caster_pos = gtf.translation();
            let start = Vec3::new(
                caster_pos.x,
                caster_pos.y + SIGTERM_SIGNAL_ALTITUDE,
                caster_pos.z,
            );
            let release = Vec3::new(
                event.target.x,
                event.target.y + SIGTERM_SIGNAL_ALTITUDE,
                event.target.z,
            );
            let distance = start.distance(release);

            let (signal_mesh, signal_material) = sigterm_assets.signal(
                *faction,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
            );
            let facing = if distance > 1e-3 {
                Transform::from_translation(start).looking_at(release, Vec3::Y)
            } else {
                Transform::from_translation(start)
            };
            commands.spawn((
                SigTermSignal {
                    target: event.target,
                    start,
                    total_distance: distance,
                    elapsed: 0.0,
                    owner_team: team.0,
                    owner_faction: *faction,
                },
                Mesh3d(signal_mesh),
                MeshMaterial3d(signal_material),
                facing,
                Visibility::default(),
            ));
            commands.entity(event.attacker).insert(CommandFireCooldown {
                remaining: SIGTERM_COOLDOWN,
            });
            continue;
        }

        let Some(ability) = ability_for(unit.0) else {
            continue;
        };
        commands.spawn(AreaDenialZone {
            center: event.target,
            radius: ability.radius,
            dps: ability.dps,
            remaining: ability.ttl,
            damage_friendly: ability.damage_friendly,
            infection_duration: ability.infection_weapon.and_then(weapon_infection_duration),
            owner_team: team.0,
            owner_faction: *faction,
        });
        commands.entity(event.attacker).insert(CommandFireCooldown {
            remaining: ability.cooldown,
        });
    }
}

/// Queue a fan of Logic Bombs at `target` and deduct the HP cost from
/// the casting Byte. Returns `false` (so the caller skips the cooldown
/// stamp) when the cast is refused — not enough HP, or caster and
/// target share a position so we can't derive a forward direction.
fn fire_minelauncher(
    target: Vec3,
    caster_pos: Vec3,
    caster_health: &mut Health,
    faction: Faction,
    team: u8,
    mine_spawns: &mut MineSpawnQueue,
) -> bool {
    if caster_health.current < MINELAUNCHER_HP_COST + MINELAUNCHER_HP_FLOOR {
        return false;
    }

    let to_target = target - caster_pos;
    let horizontal_sq = to_target.x * to_target.x + to_target.z * to_target.z;
    if horizontal_sq < 1.0 {
        // Cast on self — no forward direction to align the fan with.
        return false;
    }
    let horizontal = horizontal_sq.sqrt();
    let forward = Vec3::new(to_target.x / horizontal, 0.0, to_target.z / horizontal);
    // Right-hand perpendicular in XZ (Y is up, right-handed), so the
    // fan spreads symmetrically across the line of fire.
    let perp = Vec3::new(-forward.z, 0.0, forward.x);

    let half = (MINELAUNCHER_FAN_COUNT - 1) as f32 * 0.5;
    let step = MINELAUNCHER_FAN_RADIUS / half;
    for i in 0..MINELAUNCHER_FAN_COUNT {
        let offset = (i as f32 - half) * step;
        mine_spawns.push(MineSpawn {
            position: target + perp * offset,
            faction,
            team,
        });
    }

    caster_health.current -= MINELAUNCHER_HP_COST;
    true
}

fn apply_firewall(
    center: Vec3,
    caster_team: u8,
    caster_faction: Faction,
    targets: &Query<(Entity, &TeamId, &Faction, &GlobalTransform), With<Health>>,
    commands: &mut Commands,
) {
    let radius_sq = FIREWALL_RADIUS * FIREWALL_RADIUS;
    for (entity, team, faction, gtf) in targets.iter() {
        if !crate::units::components::is_friendly(team.0, *faction, caster_team, caster_faction) {
            continue;
        }
        if gtf.translation().distance_squared(center) > radius_sq {
            continue;
        }
        commands.entity(entity).insert(Protected {
            remaining: FIREWALL_DURATION,
        });
    }
}

pub fn tick_protection(
    time: Res<Time>,
    mut query: Query<(Entity, &mut Protected)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut protected) in &mut query {
        protected.remaining -= dt;
        if protected.remaining <= 0.0 {
            commands.entity(entity).remove::<Protected>();
        }
    }
}

/// Tick the per-caster `CommandFireCooldown` down to zero, then
/// remove the component so the caster becomes eligible again.
pub fn tick_command_fire_cooldown(
    time: Res<Time>,
    mut query: Query<(Entity, &mut CommandFireCooldown)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut cd) in &mut query {
        cd.remaining -= dt;
        if cd.remaining <= 0.0 {
            commands.entity(entity).remove::<CommandFireCooldown>();
        }
    }
}

/// Tick every active `AreaDenialZone`. Applies `dps*dt` raw HP
/// damage directly to each unit in radius (matching upstream's
/// `Spring.AddUnitDamage` in areadenial.lua, which bypasses the armor
/// multiplier table), optionally infects, and despawns expired zones.
#[allow(clippy::too_many_arguments)]
pub fn tick_area_denial(
    time: Res<Time>,
    mut zones: Query<(Entity, &mut AreaDenialZone)>,
    mut health_q: Query<&mut Health>,
    spatial: Res<SpatialIndex>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    // Reused across zones so repeated casts don't realloc.
    let mut hits: Vec<(Entity, bool)> = Vec::new();
    for (zone_entity, mut zone) in &mut zones {
        zone.remaining -= dt;
        if zone.remaining <= 0.0 {
            commands.entity(zone_entity).despawn();
            continue;
        }

        let radius_sq = zone.radius * zone.radius;
        let tick_damage = zone.dps * dt;

        hits.clear();
        spatial.query_radius(zone.center, zone.radius, |candidate| {
            let friendly = crate::units::components::is_friendly(
                candidate.team,
                candidate.faction,
                zone.owner_team,
                zone.owner_faction,
            );
            if friendly && !zone.damage_friendly {
                return;
            }
            if candidate.pos.distance_squared(zone.center) >= radius_sq {
                return;
            }
            hits.push((candidate.entity, friendly));
        });

        for (unit_entity, friendly) in hits.drain(..) {
            if let Ok(mut health) = health_q.get_mut(unit_entity) {
                health.current -= tick_damage;
            }
            if let Some(window) = zone.infection_duration
                && !friendly
            {
                commands.entity(unit_entity).insert(Infected {
                    timer: window,
                    attacker_faction: zone.owner_faction,
                    attacker_team: zone.owner_team,
                });
            }
        }
    }
}

/// Fly every in-flight Signal bomber. On arrival, drop a
/// [`SigTermBomb`] at the release point and despawn the Signal.
#[allow(clippy::too_many_arguments)]
pub fn tick_sigterm_signals(
    time: Res<Time>,
    mut signals: Query<(Entity, &mut SigTermSignal, &mut Transform)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut sigterm_assets: ResMut<SigTermAssets>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut signal, mut transform) in &mut signals {
        signal.elapsed += dt;
        let total_time = if signal.total_distance > 1e-3 {
            signal.total_distance / SIGTERM_SIGNAL_SPEED
        } else {
            0.0
        };
        let release = Vec3::new(signal.target.x, signal.start.y, signal.target.z);

        if total_time <= 0.0 || signal.elapsed >= total_time {
            // Arrived — drop the bomb from cruise altitude. Free-fall
            // duration is canonical, not recalculated per altitude.
            let (bomb_mesh, bomb_material) = sigterm_assets.bomb(
                signal.owner_faction,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
            );
            commands.spawn((
                SigTermBomb {
                    target: signal.target,
                    start: release,
                    time_to_detonate: SIGTERM_FALL_DURATION,
                    total_fall: SIGTERM_FALL_DURATION,
                    owner_team: signal.owner_team,
                    owner_faction: signal.owner_faction,
                },
                Mesh3d(bomb_mesh),
                MeshMaterial3d(bomb_material),
                Transform::from_translation(release),
                Visibility::default(),
            ));
            commands.entity(entity).despawn();
            continue;
        }

        let t = (signal.elapsed / total_time).clamp(0.0, 1.0);
        transform.translation = signal.start.lerp(release, t);
        if signal.total_distance > 1e-3 {
            transform.look_at(release, Vec3::Y);
        }
    }
}

/// Fall + detonate every in-flight SIGTERM bomb.
///
/// Each frame, lerps the bomb's transform from its spawn altitude
/// toward the clicked ground target. When `time_to_detonate` drops
/// below zero, delivers the blast (one pass over [`SpatialIndex`] at
/// [`SIGTERM_BLAST_RADIUS`] applying linearly-falloff damage),
/// leaves an [`AreaDenialZone`] tail behind, then despawns the bomb.
pub fn tick_sigterm_bombs(
    time: Res<Time>,
    mut bombs: Query<(Entity, &mut SigTermBomb, &mut Transform)>,
    mut health_q: Query<&mut Health>,
    spatial: Res<SpatialIndex>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut bomb, mut transform) in &mut bombs {
        bomb.time_to_detonate -= dt;

        if bomb.time_to_detonate > 0.0 {
            // Lerp start → target. `t=1` at detonation; the visual
            // reaches the ground exactly when the blast fires.
            let t = if bomb.total_fall > 0.0 {
                1.0 - (bomb.time_to_detonate / bomb.total_fall)
            } else {
                1.0
            };
            transform.translation = bomb.start.lerp(bomb.target, t.clamp(0.0, 1.0));
            continue;
        }

        // Detonate. One-shot damage pass across BLAST_RADIUS with
        // edge-falloff, then drop the denial tail.
        let radius_sq = SIGTERM_BLAST_RADIUS * SIGTERM_BLAST_RADIUS;
        spatial.query_radius(bomb.target, SIGTERM_BLAST_RADIUS, |candidate| {
            let d_sq = candidate.pos.distance_squared(bomb.target);
            if d_sq >= radius_sq {
                return;
            }
            if let Ok(mut health) = health_q.get_mut(candidate.entity) {
                let t = (d_sq.sqrt() / SIGTERM_BLAST_RADIUS).clamp(0.0, 1.0);
                let falloff = 1.0 - t * (1.0 - SIGTERM_BLAST_EDGE);
                health.current -= SIGTERM_BLAST_DAMAGE * falloff;
            }
        });
        commands.spawn(AreaDenialZone {
            center: bomb.target,
            radius: SIGTERM_DENIAL_RADIUS,
            dps: SIGTERM_DENIAL_DPS,
            remaining: SIGTERM_DENIAL_TTL,
            damage_friendly: true,
            infection_duration: None,
            owner_team: bomb.owner_team,
            owner_faction: bomb.owner_faction,
        });
        commands.entity(entity).despawn();
    }
}

/// Definition of a unit's command-fire ability. Values come from
/// upstream `LuaRules/Gadgets/areadenial.lua` (radius / dps / ttl /
/// friendly-fire) and the weapon's own `reloadtime` for cooldown.
///
/// `infection_weapon` names the source weapon whose `UnitDamaged`
/// event upstream's `infection.lua` watches — for Obelisk's gas that
/// is the `"Infection"` weapon (1 s window). `None` means the zone
/// doesn't infect.
struct Ability {
    radius: f32,
    dps: f32,
    /// Zone lifetime in seconds.
    ttl: f32,
    cooldown: f32,
    damage_friendly: bool,
    infection_weapon: Option<&'static str>,
}

fn ability_for(kind: UnitKind) -> Option<Ability> {
    match kind {
        UnitKind::Pointer => Some(Ability {
            radius: 120.0,
            dps: 100.0,
            ttl: 60.0,
            cooldown: 30.0,
            damage_friendly: true,
            infection_weapon: None,
        }),
        UnitKind::Obelisk => Some(Ability {
            radius: 400.0,
            dps: 120.0,
            ttl: 13.0,
            cooldown: 40.0,
            damage_friendly: false,
            infection_weapon: Some("Infection"),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nx_flag_ability_matches_upstream() {
        let a = ability_for(UnitKind::Pointer).unwrap();
        assert_eq!(a.radius, 120.0);
        assert_eq!(a.dps, 100.0);
        assert_eq!(a.ttl, 60.0);
        assert_eq!(a.cooldown, 30.0);
        assert!(a.damage_friendly);
        assert_eq!(a.infection_weapon, None);
    }

    #[test]
    fn infection_ability_matches_upstream() {
        let a = ability_for(UnitKind::Obelisk).unwrap();
        assert_eq!(a.radius, 400.0);
        assert_eq!(a.dps, 120.0);
        assert_eq!(a.ttl, 13.0);
        assert_eq!(a.cooldown, 40.0);
        assert!(!a.damage_friendly);
        // Upstream infection.lua: `infector[WeaponDefNames["infection"].id]
        // = 30` frames @ 30 fps → 1 s infection window per DPS tick.
        assert_eq!(a.infection_weapon, Some("Infection"));
        assert_eq!(
            weapon_infection_duration(a.infection_weapon.unwrap()),
            Some(1.0)
        );
    }

    #[test]
    fn units_without_abilities_return_none() {
        assert!(ability_for(UnitKind::Bit).is_none());
        assert!(ability_for(UnitKind::Kernel).is_none());
    }

    /// A healthy Byte enqueues exactly `MINELAUNCHER_FAN_COUNT` Logic
    /// Bombs, centred on the clicked target, and loses
    /// `MINELAUNCHER_HP_COST` HP in the process.
    #[test]
    fn minelauncher_queues_fan_and_deducts_hp() {
        let mut health = Health {
            current: 15_000.0,
            max: 15_000.0,
        };
        let mut queue = MineSpawnQueue::default();
        let target = Vec3::new(200.0, 0.0, 0.0);

        let fired = fire_minelauncher(
            target,
            Vec3::ZERO,
            &mut health,
            Faction::System,
            0,
            &mut queue,
        );
        assert!(fired);
        assert_eq!(queue.len(), MINELAUNCHER_FAN_COUNT);

        // All mines lie on the Z-axis line through `target` (perp to
        // +X forward), so X equals target.x and Z is symmetric.
        let mut zs: Vec<f32> = vec![];
        for spawn in queue.drain() {
            assert!((spawn.position.x - target.x).abs() < 1e-3);
            zs.push(spawn.position.z);
        }
        let sum: f32 = zs.iter().sum();
        assert!(sum.abs() < 1e-3, "fan is not centred on target: {zs:?}");
        assert!((health.current - (15_000.0 - MINELAUNCHER_HP_COST)).abs() < 1e-3);
    }

    /// Casting on your own position leaves no forward direction — the
    /// fan can't be oriented so the cast is refused.
    #[test]
    fn minelauncher_refuses_self_cast() {
        let mut health = Health {
            current: 15_000.0,
            max: 15_000.0,
        };
        let mut queue = MineSpawnQueue::default();

        let fired = fire_minelauncher(
            Vec3::ZERO,
            Vec3::ZERO,
            &mut health,
            Faction::System,
            0,
            &mut queue,
        );
        assert!(!fired);
        assert_eq!(queue.len(), 0);
        assert_eq!(health.current, 15_000.0);
    }

    /// A Byte without enough remaining HP to survive the cast refuses.
    /// Prevents mis-clicks from one-shotting your own Byte.
    #[test]
    fn minelauncher_refuses_when_low_hp() {
        let mut health = Health {
            current: MINELAUNCHER_HP_COST - 1.0,
            max: 15_000.0,
        };
        let mut queue = MineSpawnQueue::default();

        let fired = fire_minelauncher(
            Vec3::new(200.0, 0.0, 0.0),
            Vec3::ZERO,
            &mut health,
            Faction::System,
            0,
            &mut queue,
        );
        assert!(!fired);
        assert_eq!(queue.len(), 0);
    }

    /// SIGTERM values line up with upstream's `SigTerm` weapon +
    /// `areadenial.lua` — sanity-checks the constants as a block so
    /// future tuning changes are visible in the test.
    #[test]
    fn sigterm_constants_match_upstream() {
        assert_eq!(SIGTERM_BLAST_RADIUS, 900.0);
        assert_eq!(SIGTERM_BLAST_DAMAGE, 10_000.0);
        assert_eq!(SIGTERM_BLAST_EDGE, 0.8);
        assert_eq!(SIGTERM_DENIAL_RADIUS, 350.0);
        assert_eq!(SIGTERM_DENIAL_DPS, 2_000.0);
        assert_eq!(SIGTERM_DENIAL_TTL, 3.0);
    }

    /// A Signal flown to its release point hands off to a SigTermBomb
    /// at the current altitude. Two-stage airstrike reproduced: the
    /// Signal does the horizontal traversal, the bomb does the fall.
    #[test]
    fn sigterm_signal_drops_bomb_on_arrival() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Image>>()
            .init_resource::<S3OModelCache>()
            .init_resource::<SigTermAssets>();

        // Start 500 elmos east of the target at cruise altitude. At
        // SIGTERM_SIGNAL_SPEED = 240 elmos/s, flight takes ~2.08 s.
        let target = Vec3::ZERO;
        let start = Vec3::new(500.0, SIGTERM_SIGNAL_ALTITUDE, 0.0);
        let distance = start.distance(Vec3::new(0.0, SIGTERM_SIGNAL_ALTITUDE, 0.0));
        app.world_mut().spawn((
            SigTermSignal {
                target,
                start,
                total_distance: distance,
                elapsed: 0.0,
                owner_team: 0,
                owner_faction: Faction::System,
            },
            Transform::from_translation(start),
        ));

        // Advance past the full flight time. One tick of the system
        // after arrival spawns the bomb + despawns the Signal.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(2_500));
        app.world_mut()
            .run_system_once(tick_sigterm_signals)
            .unwrap();

        let signals_left = app
            .world_mut()
            .query::<&SigTermSignal>()
            .iter(app.world())
            .count();
        assert_eq!(signals_left, 0, "signal must despawn after dropping bomb");

        let bombs: Vec<SigTermBomb> = app
            .world_mut()
            .query::<&SigTermBomb>()
            .iter(app.world())
            .cloned()
            .collect();
        assert_eq!(bombs.len(), 1, "signal must spawn exactly one bomb");
        let bomb = &bombs[0];
        assert_eq!(bomb.target, target);
        assert!(
            (bomb.start.y - SIGTERM_SIGNAL_ALTITUDE).abs() < 1e-3,
            "bomb should fall from cruise altitude, got {}",
            bomb.start.y
        );
        assert!(
            (bomb.start.x - target.x).abs() < 1e-3 && (bomb.start.z - target.z).abs() < 1e-3,
            "bomb should release directly above the target, got {:?}",
            bomb.start
        );
        assert_eq!(bomb.owner_team, 0);
    }

    /// A bomb spawned at altitude reaches ground just as its timer
    /// hits zero. Verifies the lerp math doesn't overshoot or drift.
    #[test]
    fn sigterm_bomb_reaches_target_at_detonation() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>().init_resource::<SpatialIndex>();

        let target = Vec3::new(100.0, 0.0, 50.0);
        let start = target + Vec3::new(0.0, SIGTERM_SIGNAL_ALTITUDE, 0.0);
        let bomb = app
            .world_mut()
            .spawn((
                SigTermBomb {
                    target,
                    start,
                    time_to_detonate: SIGTERM_FALL_DURATION,
                    total_fall: SIGTERM_FALL_DURATION,
                    owner_team: 0,
                    owner_faction: Faction::System,
                },
                Transform::from_translation(start),
            ))
            .id();

        // Advance to roughly half the fall — bomb should be halfway.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(600));
        app.world_mut().run_system_once(tick_sigterm_bombs).unwrap();
        let pos = app.world().get::<Transform>(bomb).unwrap().translation;
        let half_y = (start.y + target.y) * 0.5;
        assert!(
            (pos.y - half_y).abs() < 5.0,
            "bomb at t=0.6s should be ~halfway: expected y≈{half_y}, got y={}",
            pos.y
        );
    }

    /// On detonation the bomb entity despawns and an `AreaDenialZone`
    /// tail appears at the impact site. The blast pass itself needs a
    /// populated `SpatialIndex` and `Health` entities to be visible;
    /// those are exercised in the playtest, not here.
    #[test]
    fn sigterm_bomb_spawns_denial_zone_on_detonation() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>().init_resource::<SpatialIndex>();

        let target = Vec3::new(10.0, 0.0, 20.0);
        app.world_mut().spawn((
            SigTermBomb {
                target,
                start: target + Vec3::new(0.0, SIGTERM_SIGNAL_ALTITUDE, 0.0),
                time_to_detonate: 0.0,
                total_fall: SIGTERM_FALL_DURATION,
                owner_team: 1,
                owner_faction: Faction::System,
            },
            Transform::from_translation(target),
        ));

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(20));
        app.world_mut().run_system_once(tick_sigterm_bombs).unwrap();

        // Bomb is gone, zone is present.
        let bombs_left = app
            .world_mut()
            .query::<&SigTermBomb>()
            .iter(app.world())
            .count();
        assert_eq!(bombs_left, 0, "bomb should have despawned after detonate");

        let mut zones_q = app.world_mut().query::<&AreaDenialZone>();
        let zones: Vec<_> = zones_q.iter(app.world()).collect();
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].radius, SIGTERM_DENIAL_RADIUS);
        assert_eq!(zones[0].dps, SIGTERM_DENIAL_DPS);
        assert_eq!(zones[0].remaining, SIGTERM_DENIAL_TTL);
        assert!(zones[0].damage_friendly);
        assert_eq!(zones[0].owner_team, 1);
    }
}
