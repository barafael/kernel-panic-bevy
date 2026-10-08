//! Army decisions: where to push, and the fight-order the push uses.

use bevy::prelude::*;

use crate::interaction::movement::QueuedCommand;
use crate::rng::{next_f32, xorshift32};
use crate::units::content::definitions::UnitKind;

/// Upstream `minifacLimit`: an enemy team with fewer small buildings
/// than this is weak enough that a big army goes straight for its
/// homebase (`AttackHomeBase`).
pub const MINIFAC_LIMIT: usize = 4;

/// Upstream triggers `AttackHomeBase` once `forceSize + buffer > 50`.
pub const HOMEBASE_RUSH_FORCE: u32 = 50;

/// Units the AI moves as army. Upstream sends `spam`, `heavy` and
/// `arty` on missions; the Connection is the Network heavy (a mobile
/// teleporter — pushing it forward also moves the packet dispatch
/// point toward the front).
pub fn is_army(kind: UnitKind) -> bool {
    kind.is_combat_unit() || kind == UnitKind::Connection
}

/// An enemy building the AI might push at.
#[derive(Debug, Clone, Copy)]
pub struct EnemyStructure {
    pub team: u8,
    pub pos: Vec3,
    pub homebase: bool,
}

/// Pick the push destination, following upstream's priorities:
///
/// 1. `AttackHomeBase`: once `force + buffer > 50`, rush the nearest
///    homebase of an enemy team owning fewer than [`MINIFAC_LIMIT`]
///    small buildings.
/// 2. Otherwise the nearest enemy small building — upstream's spot
///    weighting (`GetWeightFor`) prefers `STATE_ENEMY` datavents, i.e.
///    enemy minifacs, as mission targets.
/// 3. Fall back to the nearest enemy homebase.
pub fn pick_attack_target(
    from: Vec3,
    enemies: &[EnemyStructure],
    force_plus_buffer: u32,
) -> Option<Vec3> {
    let nearest = |filter: &dyn Fn(&EnemyStructure) -> bool| {
        enemies
            .iter()
            .filter(|e| filter(e))
            .min_by(|a, b| {
                from.distance_squared(a.pos)
                    .total_cmp(&from.distance_squared(b.pos))
            })
            .map(|e| e.pos)
    };
    let small_count = |team: u8| {
        enemies
            .iter()
            .filter(|e| e.team == team && !e.homebase)
            .count()
    };
    if force_plus_buffer > HOMEBASE_RUSH_FORCE
        && let Some(pos) = nearest(&|e| e.homebase && small_count(e.team) < MINIFAC_LIMIT)
    {
        return Some(pos);
    }
    nearest(&|e| !e.homebase).or_else(|| nearest(&|e| e.homebase))
}

/// Upstream's mission loop runs `1 + (forceSize + bufferSize) * 0.02`
/// spot selections per slow update (`for _=1,1+...*.02`), so the army
/// pushes on more fronts as it grows.
pub fn mission_count(force_plus_buffer: u32) -> usize {
    1 + (force_plus_buffer as f32 * 0.02) as usize
}

/// The push destinations for one AI tick: `missions` targets. The
/// first comes from [`pick_attack_target`] (keeping the big-army rush
/// rule and the fallbacks); the rest are spread over the remaining
/// enemy structures with upstream `GetWeightFor`'s enemy-spot weight
/// `(1 + 1/dist) * (1 + forceSize)`, sampling `max(n/3, 2)`
/// candidates per mission the way upstream samples spots. Never
/// repeats a target within a tick; returns fewer if the map runs out
/// of enemy structures.
pub fn pick_attack_targets(
    from: Vec3,
    enemies: &[EnemyStructure],
    force_plus_buffer: u32,
    missions: usize,
    rng: &mut u32,
) -> Vec<Vec3> {
    let mut targets = Vec::with_capacity(missions);
    let Some(first) = pick_attack_target(from, enemies, force_plus_buffer) else {
        return targets;
    };
    targets.push(first);
    let weight = |e: &EnemyStructure| {
        (1.0 + 1.0 / from.distance(e.pos).max(1.0)) * (1.0 + force_plus_buffer as f32)
    };
    while targets.len() < missions {
        let remaining: Vec<&EnemyStructure> = enemies
            .iter()
            .filter(|e| !targets.contains(&e.pos))
            .collect();
        if remaining.is_empty() {
            break;
        }
        let samples = (remaining.len() / 3).max(2).min(remaining.len());
        let pick = (0..samples)
            .map(|_| xorshift32(rng) as usize % remaining.len())
            .map(|i| remaining[i])
            .max_by(|a, b| weight(a).total_cmp(&weight(b)));
        if let Some(e) = pick {
            targets.push(e.pos);
        } else {
            break;
        }
    }
    targets
}

/// Upstream `DispatchSpam` scatters each unit's destination on a ring
/// (`amp = 40 + random(90)`) so a group doesn't converge on one point.
pub fn scatter(target: Vec3, rng: &mut u32) -> Vec3 {
    let angle = next_f32(rng) * std::f32::consts::TAU;
    let amp = 40.0 + next_f32(rng) * 90.0;
    target + Vec3::new(angle.cos() * amp, 0.0, angle.sin() * amp)
}

/// Upstream's idle-constructor spread order (`KPAI_Fair.lua`
/// `DispatchCon`'s no-budget branch): `phase = pi*2*random()`,
/// `amp = 128 + 128*random()` around the homebase — far enough to
/// clear the exit, close enough to come back for the next vent.
pub fn spread_around(center: Vec3, rng: &mut u32) -> Vec3 {
    let angle = next_f32(rng) * std::f32::consts::TAU;
    let amp = 128.0 + next_f32(rng) * 128.0;
    center + Vec3::new(angle.cos() * amp, 0.0, angle.sin() * amp)
}

/// Issue a fight order (Spring `CMD.FIGHT`): walk to `target`, but stop
/// and engage anything hostile in weapon range on the way. Goes through
/// the player's replace path so the movement system treats AI and player
/// orders identically.
pub fn attack_move(entity: Entity, target: Vec3, commands: &mut Commands) {
    crate::interaction::replace_order(
        &mut commands.entity(entity),
        QueuedCommand::AttackMove(target),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(team: u8, x: f32, homebase: bool) -> EnemyStructure {
        EnemyStructure {
            team,
            pos: Vec3::new(x, 0.0, 0.0),
            homebase,
        }
    }

    #[test]
    fn small_army_hits_nearest_minifac_first() {
        let enemies = [s(2, 1000.0, true), s(2, 600.0, false), s(2, 300.0, false)];
        assert_eq!(
            pick_attack_target(Vec3::ZERO, &enemies, 10),
            Some(Vec3::new(300.0, 0.0, 0.0))
        );
    }

    #[test]
    fn falls_back_to_homebase_without_minifacs() {
        let enemies = [s(2, 1000.0, true), s(3, 700.0, true)];
        assert_eq!(
            pick_attack_target(Vec3::ZERO, &enemies, 0),
            Some(Vec3::new(700.0, 0.0, 0.0))
        );
        assert_eq!(pick_attack_target(Vec3::ZERO, &[], 99), None);
    }

    /// A big army rushes the homebase of a team with < 4 minifacs, but
    /// not one that is still well expanded.
    #[test]
    fn big_army_rushes_weak_homebase() {
        let weak = [s(2, 1000.0, true), s(2, 300.0, false)];
        assert_eq!(
            pick_attack_target(Vec3::ZERO, &weak, 51),
            Some(Vec3::new(1000.0, 0.0, 0.0))
        );
        let strong = [
            s(2, 1000.0, true),
            s(2, 300.0, false),
            s(2, 400.0, false),
            s(2, 500.0, false),
            s(2, 600.0, false),
        ];
        assert_eq!(
            pick_attack_target(Vec3::ZERO, &strong, 51),
            Some(Vec3::new(300.0, 0.0, 0.0))
        );
    }

    /// `1 + (force + buffer) * 0.02` missions — Lua's fractional for
    /// bound floors.
    #[test]
    fn mission_count_scales_with_force() {
        assert_eq!(mission_count(0), 1);
        assert_eq!(mission_count(49), 1);
        assert_eq!(mission_count(50), 2);
        assert_eq!(mission_count(150), 4);
        assert_eq!(mission_count(1000), 21);
    }

    /// Multi-prong: first target obeys the single-target rules, extra
    /// targets are distinct enemy structures, and the set never
    /// repeats a position.
    #[test]
    fn multi_prong_targets_are_distinct_and_from_the_enemy_set() {
        let enemies = [
            s(2, 300.0, false),
            s(2, 600.0, false),
            s(2, 900.0, false),
            s(3, 1200.0, false),
            s(2, 1000.0, true),
        ];
        let mut rng = 0x5EED_5EED;
        let targets = pick_attack_targets(Vec3::ZERO, &enemies, 10, 3, &mut rng);
        assert_eq!(targets.len(), 3);
        // The nearest minifac leads; the rest are sampled from the set.
        assert_eq!(targets[0], Vec3::new(300.0, 0.0, 0.0));
        for (i, t) in targets.iter().enumerate() {
            assert!(
                enemies.iter().any(|e| e.pos == *t),
                "target {i} not an enemy structure"
            );
            assert!(
                targets[..i].iter().all(|u| *u != *t),
                "target {i} repeats an earlier one"
            );
        }
        // Asking for everything returns every structure exactly once.
        let all = pick_attack_targets(Vec3::ZERO, &enemies, 10, 5, &mut rng);
        assert_eq!(all.len(), 5);
        // One mission degrades to the classic single target.
        let one = pick_attack_targets(Vec3::ZERO, &enemies, 10, 1, &mut rng);
        assert_eq!(one, vec![Vec3::new(300.0, 0.0, 0.0)]);
        assert!(pick_attack_targets(Vec3::ZERO, &[], 10, 3, &mut rng).is_empty());
    }

    #[test]
    fn scatter_stays_on_upstream_ring() {
        let mut rng = 0xC0FF_EE01;
        for _ in 0..100 {
            let p = scatter(Vec3::ZERO, &mut rng);
            let r = p.length();
            assert!((40.0..=130.0).contains(&r), "radius {r}");
        }
    }

    #[test]
    fn spread_around_stays_on_the_constructor_ring() {
        let mut rng = 0xD15_EA5E;
        for _ in 0..100 {
            let p = spread_around(Vec3::new(10.0, 5.0, 20.0), &mut rng);
            assert!((p.x - 10.0).hypot(p.z - 20.0) >= 127.9);
            assert!((p.x - 10.0).hypot(p.z - 20.0) <= 256.1);
            // The ring is planar; the center's height carries over.
            assert_eq!(p.y, 5.0);
        }
    }

    #[test]
    fn army_includes_connection_not_constructors() {
        assert!(is_army(UnitKind::Bit));
        assert!(is_army(UnitKind::Connection));
        assert!(!is_army(UnitKind::Assembler));
        assert!(!is_army(UnitKind::Socket));
    }
}
