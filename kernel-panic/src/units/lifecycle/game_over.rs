use std::collections::HashMap;

use bevy::prelude::*;

use crate::game_setup::{GameOverDismissed, GameSetup};
use crate::units::combat::Dying;
use crate::units::components::{Health, TeamId, UnitType};
use crate::units::player::LocalTeam;

/// Current game state. Systems in gameplay sets only run in `Playing`.
/// The menu system watches for transitions into Victory/Defeat and opens
/// the game-over panel (see `ui::menu`).
#[derive(States, Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameState {
    #[default]
    Playing,
    Victory,
    Defeat,
}

/// Seconds between elimination checks. Upstream `game_over.lua` checks
/// once a second (`f % 30 == 25`), never on the first frame.
const CHECK_INTERVAL: f32 = 1.0;

/// Accumulator for [`check_game_over`], seeded so the first census lands
/// on sim frame 25 and then every 30 (upstream's `f % 30 == 25`) — and
/// not on the same tick as the other 1 Hz passes (AI brain at phase 0,
/// Flow speed recount at phase 15).
pub struct CheckTimer(f32);

impl Default for CheckTimer {
    fn default() -> Self {
        Self(CHECK_INTERVAL - 25.0 / 30.0)
    }
}

/// One team's standing for the elimination rule.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TeamCensus {
    /// Live (non-dying) units of any kind.
    units: u32,
    /// Live factories — homebases + MiniFacs, nanoframes included (the
    /// upstream census is `Spring.GetTeamUnitsByDefs`, which counts
    /// units still being built).
    factories: u32,
}

/// Teams that still field units but own no factory: upstream gamemode
/// 1 ("Kill all factories", the default) `KillTeam`s them.
fn eliminated(census: &HashMap<u8, TeamCensus>) -> impl Iterator<Item = u8> + '_ {
    census
        .iter()
        .filter(|(_, c)| c.units > 0 && c.factories == 0)
        .map(|(team, _)| *team)
}

/// The local player's result, if the match is decided: **Defeat** once
/// the local team owns no factory, **Victory** once no other team does
/// (every opponent has been — or is being — killed). `None` while the
/// match is live, and for a census without any factory at all (test /
/// sandbox maps, which never flip state).
fn outcome(census: &HashMap<u8, TeamCensus>, local: u8) -> Option<GameState> {
    if census.values().all(|c| c.factories == 0) {
        return None;
    }
    let alive = |team: &u8| census.get(team).is_some_and(|c| c.factories > 0);
    if !alive(&local) {
        Some(GameState::Defeat)
    } else if !census.keys().any(|t| *t != local && alive(t)) {
        Some(GameState::Victory)
    } else {
        None
    }
}

/// Upstream `game_over.lua`, gamemode 1: once a second, every team left
/// without a factory (homebase or MiniFac) is out — all its remaining
/// units are destroyed (`Spring.KillTeam`). Here that zeroes their HP so
/// `death_system` runs the regular death path (Killed() animation,
/// `ExplodeAs`), the same route self-destruct takes.
///
/// The local player's **Defeat** / **Victory** follows from it (see
/// [`outcome`]). Maps with no factories at all (test / sandbox variants)
/// never kill or flip state; neither does showcase mode. The menu's
/// all-AI demo eliminates but never flips state. Once the player dismisses the game-over panel ("Keep on playing") the
/// state stops re-triggering, but eliminations continue. Teams without
/// a seat in [`GameSetup::players`] (map-event neutrals) are never
/// counted or killed.
#[allow(clippy::too_many_arguments)]
pub fn check_game_over(
    time: Res<Time>,
    mut since_check: Local<CheckTimer>,
    local: Res<LocalTeam>,
    dismissed: Res<GameOverDismissed>,
    setup: Res<GameSetup>,
    mut units: Query<(&TeamId, &UnitType, &mut Health), Without<Dying>>,
    mut next_state: ResMut<NextState<GameState>>,
) {
    // Showcase: no win/lose — run forever. The menu demo still
    // eliminates beaten seats (so its battles resolve) but never shows
    // a result; its director restarts the match instead.
    if setup.showcase.is_some() {
        return;
    }
    since_check.0 += time.delta_secs();
    if since_check.0 < CHECK_INTERVAL {
        return;
    }
    since_check.0 = 0.0;

    // Only seated teams can be eliminated — upstream exempts the Gaia
    // team the same way. Neutral parties such as the map-event eruption
    // spawns (`map_events::ERUPTION_TEAM`) own no factory by design.
    let seated = |team: u8| setup.players.iter().any(|p| p.team == team);
    let mut census: HashMap<u8, TeamCensus> = HashMap::new();
    for (team, unit, _) in &units {
        if !seated(team.0) {
            continue;
        }
        let entry = census.entry(team.0).or_default();
        entry.units += 1;
        if unit.0.is_factory() {
            entry.factories += 1;
        }
    }
    // Sandbox guard: without any factory there is no game to lose.
    if census.values().all(|c| c.factories == 0) {
        return;
    }

    let doomed: Vec<u8> = eliminated(&census).collect();
    if !doomed.is_empty() {
        for (team, _, mut health) in &mut units {
            if doomed.contains(&team.0) && health.current > 0.0 {
                health.current = 0.0;
            }
        }
    }

    if dismissed.0 || setup.demo {
        return;
    }
    if let Some(state) = outcome(&census, local.0) {
        next_state.set(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::content::definitions::UnitKind;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::state::app::StatesPlugin;

    fn census(entries: &[(u8, u32, u32)]) -> HashMap<u8, TeamCensus> {
        entries
            .iter()
            .map(|&(team, units, factories)| (team, TeamCensus { units, factories }))
            .collect()
    }

    #[test]
    fn team_without_factory_is_eliminated() {
        let c = census(&[(0, 5, 1), (1, 3, 0), (2, 0, 0)]);
        let mut out: Vec<u8> = eliminated(&c).collect();
        out.sort_unstable();
        // Team 2 has nothing left to kill.
        assert_eq!(out, vec![1]);
    }

    #[test]
    fn outcome_follows_factories() {
        assert_eq!(outcome(&census(&[(0, 4, 1), (1, 4, 1)]), 0), None);
        assert_eq!(
            outcome(&census(&[(0, 4, 0), (1, 4, 1)]), 0),
            Some(GameState::Defeat)
        );
        assert_eq!(
            outcome(&census(&[(0, 4, 2), (1, 9, 0)]), 0),
            Some(GameState::Victory)
        );
        // Every opponent must be out, not just one of them.
        assert_eq!(outcome(&census(&[(0, 1, 1), (1, 1, 0), (2, 1, 1)]), 0), None);
        // No factories anywhere: sandbox, never decided.
        assert_eq!(outcome(&census(&[(0, 4, 0), (1, 4, 0)]), 0), None);
    }

    fn world() -> App {
        let mut app = App::new();
        app.add_plugins(StatesPlugin)
            .init_state::<GameState>()
            .init_resource::<Time>()
            .init_resource::<GameOverDismissed>()
            .insert_resource(GameSetup::default())
            .insert_resource(LocalTeam(0));
        app
    }

    fn spawn(app: &mut App, kind: UnitKind, team: u8) -> Entity {
        app.world_mut()
            .spawn((UnitType(kind), TeamId(team), Health::full(100.0)))
            .id()
    }

    fn run_check(app: &mut App) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(1_100));
        app.world_mut().run_system_once(check_game_over).unwrap();
    }

    fn pending_state(app: &App) -> Option<GameState> {
        match app.world().resource::<NextState<GameState>>() {
            NextState::Pending(s) => Some(*s),
            _ => None,
        }
    }

    /// An enemy that lost its last factory has every remaining unit
    /// zeroed (KillTeam), and the local player wins.
    #[test]
    fn factoryless_enemy_is_killed_and_local_wins() {
        let mut app = world();
        spawn(&mut app, UnitKind::Kernel, 0);
        let own_bit = spawn(&mut app, UnitKind::Bit, 0);
        let enemy_bug = spawn(&mut app, UnitKind::Bug, 1);
        let enemy_obelisk = spawn(&mut app, UnitKind::Obelisk, 1);
        run_check(&mut app);

        let hp = |app: &App, e| app.world().get::<Health>(e).unwrap().current;
        assert_eq!(hp(&app, enemy_bug), 0.0);
        assert_eq!(hp(&app, enemy_obelisk), 0.0);
        assert_eq!(hp(&app, own_bit), 100.0);
        assert_eq!(pending_state(&app), Some(GameState::Victory));
    }

    /// A MiniFac alone keeps a team alive — the homebase is not special
    /// in gamemode 1.
    #[test]
    fn minifac_keeps_team_alive() {
        let mut app = world();
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Window, 1);
        let bug = spawn(&mut app, UnitKind::Bug, 1);
        run_check(&mut app);
        assert_eq!(app.world().get::<Health>(bug).unwrap().current, 100.0);
        assert_eq!(pending_state(&app), None);
    }

    /// Losing the local team's factories kills its army and is Defeat.
    #[test]
    fn local_without_factory_is_defeat() {
        let mut app = world();
        let own_bit = spawn(&mut app, UnitKind::Bit, 0);
        spawn(&mut app, UnitKind::Hole, 1);
        run_check(&mut app);
        assert_eq!(app.world().get::<Health>(own_bit).unwrap().current, 0.0);
        assert_eq!(pending_state(&app), Some(GameState::Defeat));
    }

    /// No factory anywhere (sandbox / demo maps): nothing dies, no
    /// state flip.
    #[test]
    fn sandbox_without_factories_is_left_alone() {
        let mut app = world();
        let bit = spawn(&mut app, UnitKind::Bit, 0);
        spawn(&mut app, UnitKind::Bug, 1);
        run_check(&mut app);
        assert_eq!(app.world().get::<Health>(bit).unwrap().current, 100.0);
        assert_eq!(pending_state(&app), None);
    }

    /// Units on a team without a seat (map-event eruption neutrals)
    /// own no factory by design and must survive the census.
    #[test]
    fn unseated_neutral_team_is_not_eliminated() {
        let mut app = world();
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Hole, 1);
        let wall = spawn(&mut app, UnitKind::BadBlock, 99);
        run_check(&mut app);
        assert_eq!(app.world().get::<Health>(wall).unwrap().current, 100.0);
        assert_eq!(pending_state(&app), None);
    }

    /// The check waits for its one-second cadence.
    #[test]
    fn check_runs_once_a_second() {
        let mut app = world();
        spawn(&mut app, UnitKind::Kernel, 0);
        let bug = spawn(&mut app, UnitKind::Bug, 1);
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(500));
        app.world_mut().run_system_once(check_game_over).unwrap();
        assert_eq!(app.world().get::<Health>(bug).unwrap().current, 100.0);
    }

    /// After "Keep on playing" the state no longer flips, but a
    /// factoryless team is still wiped.
    #[test]
    fn dismissed_keeps_eliminating_without_state_flip() {
        let mut app = world();
        app.world_mut().resource_mut::<GameOverDismissed>().0 = true;
        spawn(&mut app, UnitKind::Kernel, 0);
        let bug = spawn(&mut app, UnitKind::Bug, 1);
        run_check(&mut app);
        assert_eq!(app.world().get::<Health>(bug).unwrap().current, 0.0);
        assert_eq!(pending_state(&app), None);
    }
}
