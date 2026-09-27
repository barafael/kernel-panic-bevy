//! Army decisions: where to push, and the fight-order the push uses.

use bevy::prelude::*;

use crate::interaction::movement::QueuedCommand;
use crate::rng::next_f32;
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

/// Upstream `DispatchSpam` scatters each unit's destination on a ring
/// (`amp = 40 + random(90)`) so a group doesn't converge on one point.
pub fn scatter(target: Vec3, rng: &mut u32) -> Vec3 {
    let angle = next_f32(rng) * std::f32::consts::TAU;
    let amp = 40.0 + next_f32(rng) * 90.0;
    target + Vec3::new(angle.cos() * amp, 0.0, angle.sin() * amp)
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
    fn army_includes_connection_not_constructors() {
        assert!(is_army(UnitKind::Bit));
        assert!(is_army(UnitKind::Connection));
        assert!(!is_army(UnitKind::Assembler));
        assert!(!is_army(UnitKind::Socket));
    }
}
