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

use bevy::prelude::*;

use super::combat::{INFECTION_DURATION, Infected};
use super::components::{Faction, Health, TeamId, UnitType};
use super::definitions::UnitKind;
use super::spatial::SpatialIndex;

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

/// Terminal `SIGTERM` airstrike (§3.5). Upstream spawns an invisible
/// Signal bomber that flies to the target and drops a `sigterm.s3o`
/// AircraftBomb (AoE 900, damage 10000); on detonation the
/// `areadenial.lua` gadget lays a persistent poison zone at the impact
/// site. We skip the bomber entirely — the player never sees it —
/// and drop a stand-in projectile straight down from high altitude,
/// which detonates on a fixed timer.
pub const SIGTERM_COOLDOWN: f32 = 90.0;
/// Height above the target where the bomb appears when the cast fires.
/// Large enough that the descent reads as "the sky just dropped
/// something" without forcing an awkwardly long wait.
pub const SIGTERM_BOMB_ALTITUDE: f32 = 500.0;
/// Seconds from cast to detonation. Matches upstream's
/// `myGravity=0.3` fall time from a spawn altitude of roughly
/// 250 elmos — fast enough that the player can't walk a unit out of
/// the way but slow enough to feel air-strike-y.
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
#[derive(Component)]
pub struct AreaDenialZone {
    pub center: Vec3,
    pub radius: f32,
    pub dps: f32,
    pub remaining: f32,
    pub damage_friendly: bool,
    pub infects: bool,
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
            let start = event.target + Vec3::new(0.0, SIGTERM_BOMB_ALTITUDE, 0.0);
            // Visual stand-in for the missing sigterm.s3o model: a
            // bright red/orange sphere. Mesh + material cloned fresh
            // per-cast since casts are rare (≥90s cd per Terminal) and
            // this path isn't hot.
            let mesh = meshes.add(Sphere::new(14.0));
            let material = materials.add(StandardMaterial {
                base_color: Color::srgb(1.0, 0.3, 0.0),
                emissive: LinearRgba::new(1.0, 0.4, 0.1, 1.0) * 8.0,
                unlit: true,
                ..default()
            });
            commands.spawn((
                SigTermBomb {
                    target: event.target,
                    start,
                    time_to_detonate: SIGTERM_FALL_DURATION,
                    total_fall: SIGTERM_FALL_DURATION,
                    owner_team: team.0,
                    owner_faction: *faction,
                },
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::from_translation(start),
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
            infects: ability.infects,
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
        if !super::components::is_friendly(team.0, *faction, caster_team, caster_faction) {
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
            let friendly = super::components::is_friendly(
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
            if zone.infects && !friendly {
                commands.entity(unit_entity).insert(Infected {
                    timer: INFECTION_DURATION,
                    attacker_faction: zone.owner_faction,
                    attacker_team: zone.owner_team,
                });
            }
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
            infects: false,
            owner_team: bomb.owner_team,
            owner_faction: bomb.owner_faction,
        });
        commands.entity(entity).despawn();
    }
}

/// Definition of a unit's command-fire ability. Values come from
/// upstream `LuaRules/Gadgets/areadenial.lua` (radius / dps / ttl /
/// friendly-fire) and the weapon's own `reloadtime` for cooldown.
struct Ability {
    radius: f32,
    dps: f32,
    /// Zone lifetime in seconds.
    ttl: f32,
    cooldown: f32,
    damage_friendly: bool,
    infects: bool,
}

fn ability_for(kind: UnitKind) -> Option<Ability> {
    match kind {
        UnitKind::Pointer => Some(Ability {
            radius: 120.0,
            dps: 100.0,
            ttl: 60.0,
            cooldown: 30.0,
            damage_friendly: true,
            infects: false,
        }),
        UnitKind::Obelisk => Some(Ability {
            radius: 400.0,
            dps: 120.0,
            ttl: 13.0,
            cooldown: 40.0,
            damage_friendly: false,
            infects: true,
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
        assert!(!a.infects);
    }

    #[test]
    fn infection_ability_matches_upstream() {
        let a = ability_for(UnitKind::Obelisk).unwrap();
        assert_eq!(a.radius, 400.0);
        assert_eq!(a.dps, 120.0);
        assert_eq!(a.ttl, 13.0);
        assert_eq!(a.cooldown, 40.0);
        assert!(!a.damage_friendly);
        assert!(a.infects);
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

    /// A bomb spawned at altitude reaches ground just as its timer
    /// hits zero. Verifies the lerp math doesn't overshoot or drift.
    #[test]
    fn sigterm_bomb_reaches_target_at_detonation() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>().init_resource::<SpatialIndex>();

        let target = Vec3::new(100.0, 0.0, 50.0);
        let start = target + Vec3::new(0.0, SIGTERM_BOMB_ALTITUDE, 0.0);
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
                start: target + Vec3::new(0.0, SIGTERM_BOMB_ALTITUDE, 0.0),
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
