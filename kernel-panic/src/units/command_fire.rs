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

/// System: drain queued `CommandFireEvent`s into persistent
/// `AreaDenialZone` entities. A unit identifies its command-fire
/// weapon by looking at its `UnitKind` — NX Flag for Pointer, Infection
/// for Obelisk. Units without a registered ability are ignored.
pub fn process_command_fire(
    mut events: MessageReader<CommandFireEvent>,
    casters: Query<(&UnitType, &TeamId, &Faction, Option<&CommandFireCooldown>)>,
    mut commands: Commands,
) {
    for event in events.read() {
        let Ok((unit, team, faction, cd)) = casters.get(event.attacker) else {
            continue;
        };
        if cd.is_some_and(|c| c.remaining > 0.0) {
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

/// System: tick the per-caster `CommandFireCooldown` down to zero, then
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

/// System: tick every active `AreaDenialZone`. Applies `dps*dt` raw HP
/// damage directly to each unit in radius (matching upstream's
/// `Spring.AddUnitDamage` in areadenial.lua, which bypasses the armor
/// multiplier table), optionally infects, and despawns expired zones.
#[allow(clippy::too_many_arguments)]
pub fn tick_area_denial(
    time: Res<Time>,
    mut zones: Query<(Entity, &mut AreaDenialZone)>,
    mut units: Query<(
        Entity,
        &UnitType,
        &Faction,
        &TeamId,
        &GlobalTransform,
        &mut Health,
    )>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (zone_entity, mut zone) in &mut zones {
        zone.remaining -= dt;
        if zone.remaining <= 0.0 {
            commands.entity(zone_entity).despawn();
            continue;
        }

        let radius_sq = zone.radius * zone.radius;
        let tick_damage = zone.dps * dt;

        for (unit_entity, _unit, faction, team, gtf, mut health) in &mut units {
            let is_friendly = team.0 == zone.owner_team || *faction == zone.owner_faction;
            if is_friendly && !zone.damage_friendly {
                continue;
            }
            if gtf.translation().distance_squared(zone.center) >= radius_sq {
                continue;
            }

            health.current -= tick_damage;

            if zone.infects && !is_friendly {
                commands.entity(unit_entity).insert(Infected {
                    timer: INFECTION_DURATION,
                    attacker_faction: zone.owner_faction,
                    attacker_team: zone.owner_team,
                });
            }
        }
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
            cooldown: 30.0,
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
        assert_eq!(a.cooldown, 30.0);
        assert!(!a.damage_friendly);
        assert!(a.infects);
    }

    #[test]
    fn units_without_abilities_return_none() {
        assert!(ability_for(UnitKind::Bit).is_none());
        assert!(ability_for(UnitKind::Kernel).is_none());
    }
}
