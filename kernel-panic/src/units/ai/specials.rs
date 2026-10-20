//! Special abilities the AI fires: SIGTERM, Infection, NX Flag, packet
//! dispatch and Bug ↔ Exploit deploys (upstream `KPAI.lua` slow update).

use bevy::prelude::Vec3;

use crate::units::spatial::flat_dist_sq;

/// Cluster radius for "crowded" (upstream `GetUnitsInCylinder(..,300)`).
pub const CROWD_RADIUS: f32 = 300.0;
/// A spot is worth an area ability (the Terminal's SIGTERM, the
/// Pointers' NX Flags) once this many enemies stand within
/// [`CROWD_RADIUS`]. Upstream waits for more than 15 units of any team
/// (`SelectForceFor`: `if es > 15`, team filter commented out), which a
/// small match rarely gathers: its Terminals sat out whole games.
pub const CROWD_MIN_ENEMIES: usize = 6;
/// … and no more than one of our own units there per this many
/// enemies: SIGTERM and NX hurt friends too.
pub const CROWD_ENEMIES_PER_FRIEND: usize = 2;
/// Minimum spacing between one team's SIGTERMs (`lastNuke < f - 15*30`).
pub const SIGTERM_INTERVAL: f32 = 15.0;
/// NX Flag weapon range (`retroweapons.tdf [nx] range=1400`).
pub const NX_RANGE: f32 = 1400.0;
/// Obelisk targets the nearest enemy within this range
/// (`GetUnitNearestEnemy(u,1500)`).
pub const OBELISK_RANGE: f32 = 1500.0;

/// Counter-dispatch: a teleporter with an attacker this close
/// (`dispatchRange = 300`) …
pub const COUNTER_DISPATCH_RANGE: f32 = 300.0;
/// … and at least this many buffered packets (`teamBufferSize >= 3`)
/// dispatches straight at it,
pub const COUNTER_DISPATCH_MIN_BUFFER: u32 = 3;
/// at most once per `dispatchCoolDown = 5` seconds per teleporter.
pub const DISPATCH_COOLDOWN: f32 = 5.0;
/// Fair KPAI additionally requires `Lack.spams > 5`.
pub const COUNTER_DISPATCH_MIN_LACK: i32 = 5;

/// Bugs deploy into Exploits when the nearest enemy is in this band
/// (`GetUnitNearestEnemy(u,1000)` and separation `> 600`) — far enough
/// that the distance-scaled Exploit shot pays off. Upstream also
/// refuses while the Bug stands within `IsItOccupied`'s ±48 box of any
/// building (any team) — a stationary Exploit must not clog a
/// building plot.
pub const DEPLOY_MIN: f32 = 600.0;
pub const DEPLOY_MAX: f32 = 1000.0;
/// `IsItOccupied` clearance: the ±48 XZ box half-extent.
pub const DEPLOY_CLEAR_HALF: f32 = 48.0;
/// Exploits pack up when nothing is within this range …
pub const UNDEPLOY_MAX: f32 = 1100.0;
/// … or an enemy has closed inside this range (too close to benefit).
pub const UNDEPLOY_MIN: f32 = 500.0;

/// Firewall (`CMD_FIREWALL`): the reflector drops over the spot where
/// allies were last hit, once the fight there is worth shielding —
/// ≥3 enemies within 500 and ≥4 allies within 300 (upstream: 7, which a
/// small match's skirmishes seldom reach), none of them already
/// `Protected`. Upstream expires `lastAllyDamage` with
/// `t > GetGameSeconds() + 6`, a comparison a past timestamp can never
/// pass; we implement the evident intent (damage fresher than 6 s).
pub const FIREWALL_FRESH: f32 = 6.0;
pub const FIREWALL_ENEMY_MIN: usize = 3;
pub const FIREWALL_ALLY_MIN: usize = 4;
pub const FIREWALL_ENEMY_RADIUS: f32 = 500.0;
pub const FIREWALL_ALLY_RADIUS: f32 = 300.0;

/// The `CMD_FIREWALL` gate. `damage_age` is seconds since the recorded
/// ally damage; freshness is part of the pure function so it stays
/// testable.
pub fn firewall_should_cast(
    damage_age: f32,
    enemies_near: usize,
    allies_near: usize,
    protected_near: bool,
) -> bool {
    damage_age <= FIREWALL_FRESH
        && enemies_near >= FIREWALL_ENEMY_MIN
        && allies_near >= FIREWALL_ALLY_MIN
        && !protected_near
}

/// Crowded-cluster pick for area abilities: the enemy position with the
/// most enemies within `radius`, provided there are at least
/// [`CROWD_MIN_ENEMIES`] and at most one of our units per
/// [`CROWD_ENEMIES_PER_FRIEND`] enemies there. Returns the center and
/// its enemy count.
///
/// Upstream fires at the chosen mission spot once more than 15 units of
/// any team stand there (`SelectForceFor`), friends included and with
/// no care for them; that gate is too high for a small match, and
/// counting our own army as the crowd aimed SIGTERM at it. Candidate
/// centers are strided down to ~64 so the O(n²) count stays cheap at
/// 1 Hz on large armies.
pub fn crowded_cluster(enemies: &[Vec3], friends: &[Vec3], radius: f32) -> Option<(Vec3, usize)> {
    let r_sq = radius * radius;
    let within =
        |c: Vec3, pts: &[Vec3]| pts.iter().filter(|p| flat_dist_sq(**p, c) <= r_sq).count();
    let stride = (enemies.len() / 64).max(1);
    enemies
        .iter()
        .step_by(stride)
        .filter_map(|&c| {
            let n = within(c, enemies);
            let clear = within(c, friends) * CROWD_ENEMIES_PER_FRIEND <= n;
            (n >= CROWD_MIN_ENEMIES && clear).then_some((c, n))
        })
        .max_by_key(|&(_, n)| n)
}

/// Where a ready Terminal drops SIGTERM when no crowd has gathered: on
/// the enemy minifac or special (`targets`) with the most enemies round
/// it, clear of our own units as [`crowded_cluster`] wants. A Terminal
/// that waits for a crowd may wait all game; its recharge is 96 s, so
/// hitting production in between is the better use.
pub fn sigterm_fallback(targets: &[Vec3], enemies: &[Vec3], friends: &[Vec3]) -> Option<Vec3> {
    let r_sq = CROWD_RADIUS * CROWD_RADIUS;
    let within =
        |c: Vec3, pts: &[Vec3]| pts.iter().filter(|p| flat_dist_sq(**p, c) <= r_sq).count();
    targets
        .iter()
        .filter_map(|&c| {
            let n = within(c, enemies).max(1);
            (within(c, friends) * CROWD_ENEMIES_PER_FRIEND <= n).then_some((c, n))
        })
        // The first of equals, in the caller's (deterministic) order.
        .fold(None, |best: Option<(Vec3, usize)>, (c, n)| match best {
            Some((_, m)) if m >= n => best,
            _ => Some((c, n)),
        })
        .map(|(c, _)| c)
}

/// Should a teleporter under attack counter-dispatch? (`UnitDamaged`
/// teleporter branch of `KPAI_Fair.lua`.) `since_last` is `None` for a
/// teleporter that never dispatched.
pub fn should_counter_dispatch(buffer: u32, lack_spams: i32, since_last: Option<f32>) -> bool {
    buffer >= COUNTER_DISPATCH_MIN_BUFFER
        && lack_spams > COUNTER_DISPATCH_MIN_LACK
        && since_last.is_none_or(|s| s > DISPATCH_COOLDOWN)
}

/// Idle Bug: deploy iff it stands clear of buildings and the nearest
/// enemy distance is in (600, 1000].
pub fn bug_should_deploy(nearest_enemy: Option<f32>, on_building: bool) -> bool {
    !on_building && nearest_enemy.is_some_and(|d| d > DEPLOY_MIN && d <= DEPLOY_MAX)
}

/// Exploit: undeploy when nothing is within 1100 or the nearest enemy
/// is already inside 500.
pub fn exploit_should_undeploy(nearest_enemy: Option<f32>) -> bool {
    nearest_enemy.is_none_or(|d| d > UNDEPLOY_MAX || d < UNDEPLOY_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(center: Vec3, n: usize) -> Vec<Vec3> {
        (0..n)
            .map(|i| center + Vec3::new((i % 5) as f32 * 20.0, 0.0, (i / 5) as f32 * 20.0))
            .collect()
    }

    #[test]
    fn crowded_cluster_finds_densest_blob() {
        let mut enemies = blob(Vec3::new(2000.0, 0.0, 0.0), 20);
        enemies.extend(blob(Vec3::new(-2000.0, 0.0, 0.0), 10));
        let (c, n) = crowded_cluster(&enemies, &[], CROWD_RADIUS).unwrap();
        assert!(c.x > 1000.0);
        assert_eq!(n, 20);
    }

    /// Six enemies make a crowd, five don't.
    #[test]
    fn crowded_cluster_needs_six_enemies() {
        assert!(crowded_cluster(&blob(Vec3::ZERO, 5), &[], CROWD_RADIUS).is_none());
        assert!(crowded_cluster(&blob(Vec3::ZERO, 6), &[], CROWD_RADIUS).is_some());
    }

    /// Why: upstream counted our own attackers as the crowd (16 units,
    /// 10 of them ours) and SIGTERMed its own army. Only enemies make
    /// the crowd, and a spot with more than one friend per two enemies
    /// is spared.
    #[test]
    fn crowded_cluster_spares_our_own_units() {
        let enemies = blob(Vec3::ZERO, 6);
        let friends = blob(Vec3::new(10.0, 0.0, 10.0), 10);
        assert!(crowded_cluster(&enemies, &friends, CROWD_RADIUS).is_none());
        let (_, n) = crowded_cluster(&enemies, &friends[..3], CROWD_RADIUS).expect("6 enemies");
        assert_eq!(n, 6);
        // No enemy, no spot to fire at.
        assert!(crowded_cluster(&[], &blob(Vec3::ZERO, 20), CROWD_RADIUS).is_none());
    }

    /// No crowd: SIGTERM the enemy building with the most enemies round
    /// it, but not one our army stands at.
    #[test]
    fn sigterm_falls_back_on_enemy_production() {
        let busy = Vec3::new(1000.0, 0.0, 0.0);
        let quiet = Vec3::new(-1000.0, 0.0, 0.0);
        let enemies = blob(busy, 3);
        assert_eq!(sigterm_fallback(&[quiet, busy], &enemies, &[]), Some(busy));
        let friends = blob(busy, 4);
        assert_eq!(
            sigterm_fallback(&[quiet, busy], &enemies, &friends),
            Some(quiet)
        );
        assert_eq!(sigterm_fallback(&[], &enemies, &[]), None);
    }

    #[test]
    fn counter_dispatch_rules() {
        assert!(should_counter_dispatch(3, 6, None));
        assert!(!should_counter_dispatch(2, 6, None));
        assert!(!should_counter_dispatch(3, 5, None));
        assert!(!should_counter_dispatch(3, 6, Some(4.0)));
        assert!(should_counter_dispatch(3, 6, Some(5.5)));
    }

    /// Upstream `CMD_FIREWALL`: needs a big enough brawl on fresh
    /// damage, and refuses while any ally in the zone is already
    /// shielded.
    #[test]
    fn firewall_needs_a_fresh_worthwhile_fight() {
        assert!(firewall_should_cast(1.0, 3, 4, false));
        assert!(!firewall_should_cast(6.1, 3, 4, false));
        assert!(!firewall_should_cast(1.0, 2, 4, false));
        assert!(!firewall_should_cast(1.0, 3, 3, false));
        assert!(!firewall_should_cast(1.0, 3, 4, true));
    }

    #[test]
    fn deploy_hysteresis_bands_do_not_overlap() {
        assert!(!bug_should_deploy(None, false));
        assert!(!bug_should_deploy(Some(600.0), false));
        assert!(bug_should_deploy(Some(800.0), false));
        assert!(!bug_should_deploy(Some(1001.0), false));
        // A Bug on a building plot never bombard-deploys, whatever the
        // range (upstream `IsItOccupied` gate).
        assert!(!bug_should_deploy(Some(800.0), true));
        assert!(exploit_should_undeploy(None));
        assert!(exploit_should_undeploy(Some(1200.0)));
        assert!(exploit_should_undeploy(Some(400.0)));
        assert!(!exploit_should_undeploy(Some(800.0)));
        // Anything a Bug deploys at, an Exploit keeps holding.
        for d in [601.0, 800.0, 1000.0] {
            assert!(bug_should_deploy(Some(d), false) && !exploit_should_undeploy(Some(d)));
        }
    }
}
