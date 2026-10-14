use std::collections::BTreeMap;

use bevy::prelude::*;

use crate::game_setup::{ActiveMatch, GameOverDismissed, GameSetup, SPECTATOR_TEAM};
use crate::sim::SimTick;
use crate::units::combat::Dying;
use crate::units::components::{Health, TeamId, UnitType};
use crate::units::lifecycle::bookkeeping::TeamUnitCounts;
use crate::units::player::LocalTeam;
use crate::units::teams::{Alliances, TeamFates};

/// The local player's result panel — presentation only. Nothing in the
/// simulation reads it: the sim runs on for every peer whatever its
/// player's fate (lockstep), and a defeated player watches. Set by
/// [`announce_outcome`] from the sim's [`TeamFates`]; the menu opens the
/// game-over panel on Victory / Defeat (`ui::menu`), and "Keep on
/// playing" puts it back to `Playing`.
#[derive(States, Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameState {
    #[default]
    Playing,
    Victory,
    Defeat,
}

/// Upstream `game_over.lua` checks once a second on sim frame
/// `f % 30 == 25` (never on the first frame), off the other 1 Hz passes
/// (AI brain at phase 0, Flow speed recount at phase 15).
const CHECK_PERIOD: u64 = 30;
const CHECK_PHASE: u64 = 25;

/// Is `tick` a census frame (`f % 30 == 25`)?
fn census_tick(tick: SimTick) -> bool {
    tick.frame() % CHECK_PERIOD == CHECK_PHASE
}

/// The match result once no two living owners are enemies
/// (`game_over.lua`: no pair `not AreTeamsAllied` left among the
/// remaining teams → `GameOver(WinningAllies)`): the ally teams of the
/// living owners, sorted. Empty when nobody is left (a draw). `None`
/// while enemies remain.
fn decided(alliances: &Alliances, fates: &TeamFates) -> Option<Vec<u8>> {
    let living: Vec<u8> = alliances.owners().filter(|o| !fates.is_dead(*o)).collect();
    let enemies_left = living
        .iter()
        .any(|a| living.iter().any(|b| !alliances.friendly(*a, *b)));
    if enemies_left {
        return None;
    }
    let mut winners: Vec<u8> = living
        .iter()
        .filter_map(|o| alliances.ally_team(*o))
        .collect();
    winners.sort_unstable();
    winners.dedup();
    Some(winners)
}

/// Upstream `game_over.lua`, gamemode 1 ("Kill all factories", KP's
/// default; census `:76`, end check `:80-104`, kills `:106-113`), on
/// the sim tick:
///
/// - Every census frame (once a second), each living seated owner
///   without a factory — homebase or MiniFac, nanoframes included
///   (`Spring.GetTeamUnitsByDefs` counts units still being built) — is
///   killed (`Spring.KillTeam` → `CTeam::Died`, [`TeamFates::kill_team`]),
///   in seat order, the one after each killed owner left for the next
///   census as upstream's list walk does.
/// - At the census after a death (`CheckForGameOver`), the match ends
///   once no two living owners are enemies: the living owners' ally
///   teams win ([`decided`]). An ally team loses when all its owners
///   are dead: shared victory and defeat.
/// - A dead owner's units are destroyed — HP to zero, so `death_system`
///   runs the regular death path (Killed() animation, `ExplodeAs`), the
///   same route self-destruct takes — on the tick it dies (a resign
///   from the command apply at the head of the tick included) and again
///   at every census for whatever it still spawns. Its unit cap goes to
///   its living allies ([`TeamUnitCounts::team_died`]).
///
/// The simulation never stops for an outcome: the local player's
/// Victory / Defeat is presentation ([`announce_outcome`]). Owners
/// without a seat in the running match ([`ActiveMatch`]; the map-event
/// neutrals) are never counted or killed, as upstream exempts Gaia.
/// Showcase and sandbox matches have no census; a match where no seat
/// has a factory and nobody has died (test maps) is never judged. The
/// menu's pending [`GameSetup`](crate::game_setup::GameSetup) is not
/// consulted: the old world keeps running while the next map loads and
/// is judged against its own seats.
#[allow(clippy::too_many_arguments)]
pub fn check_game_over(
    tick: Res<SimTick>,
    active: Option<Res<ActiveMatch>>,
    alliances: Res<Alliances>,
    mut fates: ResMut<TeamFates>,
    mut unit_counts: ResMut<TeamUnitCounts>,
    mut units: Query<(&TeamId, &UnitType, &mut Health), Without<Dying>>,
) {
    // No match in the world yet: nothing to judge.
    let Some(ActiveMatch(setup)) = active.as_deref() else {
        return;
    };
    let census = census_tick(*tick) && judged(setup);
    if census {
        if fates.take_check_end()
            && let Some(winners) = decided(&alliances, &fates)
        {
            fates.decide(winners);
        }
        let mut factories: BTreeMap<u8, u32> = alliances
            .owners()
            .filter(|o| !fates.is_dead(*o))
            .map(|o| (o, 0))
            .collect();
        for (team, unit, _) in &units {
            if unit.0.is_factory()
                && let Some(n) = factories.get_mut(&team.0)
            {
                *n += 1;
            }
        }
        // Test / sandbox maps without a single factory: no game to
        // lose — unless someone already lost it.
        let real = factories.values().any(|n| *n > 0) || fates.dead().next().is_some();
        if real {
            // Upstream removes the dead team from `RemainingTeamList`
            // while `ipairs` walks it (`:107-110`), so the team after a
            // killed one is not looked at until the next census.
            let mut skip = false;
            for (&owner, &n) in &factories {
                if std::mem::take(&mut skip) {
                    continue;
                }
                if n == 0 {
                    fates.kill_team(owner);
                    skip = true;
                }
            }
        }
    }

    // One `CTeam::Died` after the other, as the kills came: an ally
    // killed later in the same pass was still alive for this one.
    let doomed = fates.take_doomed();
    for &owner in &doomed {
        let living: Vec<u8> = alliances
            .allies_of(owner)
            .filter(|o| *o != owner && (!fates.is_dead(*o) || (*o > owner && doomed.contains(o))))
            .collect();
        unit_counts.team_died(owner, &living);
    }
    if doomed.is_empty() && !(census && fates.dead().next().is_some()) {
        return;
    }
    for (team, _, mut health) in &mut units {
        if fates.is_dead(team.0) && health.current > 0.0 {
            health.current = 0.0;
        }
    }
}

/// Does this match have a win / lose census at all? Showcase and
/// sandbox (the bot's single-seat match) run forever. The menu's demo
/// is judged (its battles resolve) but never announced.
fn judged(setup: &GameSetup) -> bool {
    setup.showcase.is_none() && !setup.sandbox
}

/// The local player's result, if any: **Defeat** once the local owner
/// is dead (it watches from then on, as Spring's `CTeam::Died` demotes
/// the team's players to spectators), and at the match's end
/// **Victory** or **Defeat** by whether the local ally team is among
/// the winners (`CEndGameBox`: "Your ally-team won!"). A dead player
/// whose allies go on to win sees Victory then. `None` while the local
/// player is alive in an undecided match, for a spectator, and in
/// matches without a result screen (demo, showcase, sandbox).
pub fn local_outcome(
    setup: &GameSetup,
    local: u8,
    alliances: &Alliances,
    fates: &TeamFates,
) -> Option<GameState> {
    if setup.demo || !judged(setup) || local == SPECTATOR_TEAM {
        return None;
    }
    let ally = alliances.ally_team(local)?;
    match fates.winners() {
        Some(winners) if winners.contains(&ally) => Some(GameState::Victory),
        Some(_) => Some(GameState::Defeat),
        None => fates.is_dead(local).then_some(GameState::Defeat),
    }
}

/// The outcome last shown to the local player (match-scoped), so a
/// dismissed panel ("Keep on playing / watching") stays closed until
/// the outcome changes.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AnnouncedOutcome(pub Option<GameState>);

/// `Update`: show the local player's result ([`local_outcome`]) when
/// it changes. UI only — the sim reads none of this. A new outcome
/// re-arms a dismissed panel (a fallen player's allies winning
/// later turns its "You lost!" into "You won!").
pub fn announce_outcome(
    active: Option<Res<ActiveMatch>>,
    local: Res<LocalTeam>,
    alliances: Res<Alliances>,
    fates: Res<TeamFates>,
    mut announced: ResMut<AnnouncedOutcome>,
    mut dismissed: ResMut<GameOverDismissed>,
    mut next_state: ResMut<NextState<GameState>>,
) {
    let Some(ActiveMatch(setup)) = active.as_deref() else {
        return;
    };
    let outcome = local_outcome(setup, local.0, &alliances, &fates);
    if outcome == announced.0 {
        return;
    }
    announced.0 = outcome;
    if let Some(state) = outcome {
        dismissed.0 = false;
        next_state.set(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_setup::PlayerSpec;
    use crate::units::components::Faction;
    use crate::units::content::definitions::UnitKind;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::state::app::StatesPlugin;

    fn seats(allies: &[u8]) -> GameSetup {
        GameSetup {
            players: allies
                .iter()
                .enumerate()
                .map(|(i, &ally_team)| PlayerSpec {
                    faction: Faction::ALL[i % 3],
                    ally_team,
                    ai: i != 0,
                })
                .collect(),
            ..GameSetup::default()
        }
    }

    fn world(setup: GameSetup) -> App {
        let mut app = App::new();
        app.add_plugins(StatesPlugin)
            .init_state::<GameState>()
            .init_resource::<GameOverDismissed>()
            .init_resource::<TeamFates>()
            .init_resource::<AnnouncedOutcome>()
            .init_resource::<TeamUnitCounts>()
            .insert_resource(Alliances::from_setup(&setup))
            .insert_resource(ActiveMatch(setup))
            .insert_resource(SimTick(0))
            .insert_resource(LocalTeam(0));
        app
    }

    fn spawn(app: &mut App, kind: UnitKind, team: u8) -> Entity {
        app.world_mut()
            .spawn((UnitType(kind), TeamId(team), Health::full(100.0)))
            .id()
    }

    /// Run the check on the next census frame (`f % 30 == 25`).
    fn run_census(app: &mut App) {
        let mut tick = app.world_mut().resource_mut::<SimTick>();
        tick.0 = (tick.0 / CHECK_PERIOD + 1) * CHECK_PERIOD + CHECK_PHASE;
        app.world_mut().run_system_once(check_game_over).unwrap();
        app.world_mut().run_system_once(announce_outcome).unwrap();
    }

    fn hp(app: &App, e: Entity) -> f32 {
        app.world().get::<Health>(e).unwrap().current
    }

    fn fates(app: &App) -> &TeamFates {
        app.world().resource::<TeamFates>()
    }

    fn pending_state(app: &App) -> Option<GameState> {
        match app.world().resource::<NextState<GameState>>() {
            NextState::Pending(s) => Some(*s),
            _ => None,
        }
    }

    /// An enemy that lost its last factory has every remaining unit
    /// zeroed (KillTeam); the match is decided at the next census, and
    /// the local player wins.
    #[test]
    fn factoryless_enemy_is_killed_and_local_wins() {
        let mut app = world(seats(&[0, 1]));
        spawn(&mut app, UnitKind::Kernel, 0);
        let own_bit = spawn(&mut app, UnitKind::Bit, 0);
        let enemy_bug = spawn(&mut app, UnitKind::Bug, 1);
        let enemy_obelisk = spawn(&mut app, UnitKind::Obelisk, 1);
        run_census(&mut app);

        assert_eq!(hp(&app, enemy_bug), 0.0);
        assert_eq!(hp(&app, enemy_obelisk), 0.0);
        assert_eq!(hp(&app, own_bit), 100.0);
        assert!(fates(&app).is_dead(1));
        // Upstream declares the end one census after the kill.
        assert_eq!(fates(&app).winners(), None);
        assert_eq!(pending_state(&app), None);
        run_census(&mut app);
        assert_eq!(fates(&app).winners(), Some(&[0][..]));
        assert_eq!(pending_state(&app), Some(GameState::Victory));
    }

    /// Two factoryless owners in a row: upstream's list walk skips the
    /// one after a kill, so the second dies a census later.
    #[test]
    fn adjacent_deaths_take_two_censuses() {
        let mut app = world(seats(&[0, 1, 2]));
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Bug, 1);
        spawn(&mut app, UnitKind::Bug, 2);
        run_census(&mut app);
        assert!(fates(&app).is_dead(1) && !fates(&app).is_dead(2));
        run_census(&mut app);
        assert!(fates(&app).is_dead(2));
    }

    /// A MiniFac alone keeps an owner alive — the homebase is not
    /// special in gamemode 1.
    #[test]
    fn minifac_keeps_team_alive() {
        let mut app = world(seats(&[0, 1]));
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Window, 1);
        let bug = spawn(&mut app, UnitKind::Bug, 1);
        run_census(&mut app);
        assert_eq!(hp(&app, bug), 100.0);
        assert_eq!(pending_state(&app), None);
    }

    /// Losing the local owner's factories kills its army and shows
    /// Defeat at once; the simulation is not involved.
    #[test]
    fn local_without_factory_is_defeat() {
        let mut app = world(seats(&[0, 1]));
        let own_bit = spawn(&mut app, UnitKind::Bit, 0);
        spawn(&mut app, UnitKind::Hole, 1);
        run_census(&mut app);
        assert_eq!(hp(&app, own_bit), 0.0);
        assert_eq!(pending_state(&app), Some(GameState::Defeat));
    }

    /// 2v2: one owner of a side falling doesn't decide anything; its
    /// ally fights on. When the second falls, the whole ally team has
    /// lost: shared defeat, and the other side's owners share the win.
    #[test]
    fn ally_team_loses_when_all_its_owners_are_dead() {
        let mut app = world(seats(&[0, 0, 1, 1]));
        // The local player (owner 0) has no factory: dead, watching.
        let own = spawn(&mut app, UnitKind::Bit, 0);
        let kernel = spawn(&mut app, UnitKind::Kernel, 1);
        spawn(&mut app, UnitKind::Hole, 2);
        let enemy_ally = spawn(&mut app, UnitKind::Carrier, 3);
        run_census(&mut app);
        assert_eq!(hp(&app, own), 0.0);
        assert_eq!(pending_state(&app), Some(GameState::Defeat));
        run_census(&mut app);
        assert_eq!(fates(&app).winners(), None, "owner 1 still fights");
        assert_eq!(hp(&app, enemy_ally), 100.0);

        // Owner 1 falls too: ally team 0 is out, ally team 1 wins.
        app.world_mut().despawn(kernel);
        run_census(&mut app);
        assert!(fates(&app).is_dead(1));
        run_census(&mut app);
        assert_eq!(fates(&app).winners(), Some(&[1][..]));
        assert!(!fates(&app).is_dead(3), "allied owners win together");
    }

    /// The dead player's allies winning later turns its Defeat into a
    /// Victory, even after the panel was dismissed.
    #[test]
    fn fallen_player_shares_its_allies_victory() {
        let mut app = world(seats(&[0, 0, 1]));
        spawn(&mut app, UnitKind::Bit, 0);
        spawn(&mut app, UnitKind::Kernel, 1);
        spawn(&mut app, UnitKind::Bug, 2);
        run_census(&mut app);
        assert!(fates(&app).is_dead(0) && fates(&app).is_dead(2));
        assert_eq!(pending_state(&app), Some(GameState::Defeat));
        app.world_mut().resource_mut::<GameOverDismissed>().0 = true;
        run_census(&mut app);
        assert_eq!(fates(&app).winners(), Some(&[0][..]));
        assert_eq!(pending_state(&app), Some(GameState::Victory));
        assert!(!app.world().resource::<GameOverDismissed>().0);
    }

    /// A resign (`kill_team` from the command apply) destroys the
    /// owner's units on the same tick, off the census cadence, and
    /// passes its unit cap to its living ally.
    #[test]
    fn killed_team_dies_on_the_tick() {
        let mut app = world(seats(&[0, 0, 1]));
        spawn(&mut app, UnitKind::Kernel, 0);
        let k1 = spawn(&mut app, UnitKind::Kernel, 1);
        spawn(&mut app, UnitKind::Hole, 2);
        app.world_mut().resource_mut::<TeamFates>().kill_team(1);
        app.world_mut().resource_mut::<SimTick>().0 = 3;
        app.world_mut().run_system_once(check_game_over).unwrap();
        assert_eq!(hp(&app, k1), 0.0);
        let counts = app.world().resource::<TeamUnitCounts>();
        assert_eq!(counts.limit(1), 0);
        assert_eq!(
            counts.limit(0),
            2 * crate::units::lifecycle::bookkeeping::MAX_UNITS_PER_TEAM
        );
        assert_eq!(
            counts.limit(2),
            crate::units::lifecycle::bookkeeping::MAX_UNITS_PER_TEAM
        );
    }

    /// Two allies dying in one pass hand their caps on one after the
    /// other (upstream 3 × 1000 → 2 × 1500 → 1 × 3000).
    #[test]
    fn caps_pass_on_death_by_death() {
        use crate::units::lifecycle::bookkeeping::MAX_UNITS_PER_TEAM as MAX;
        let mut app = world(seats(&[0, 0, 0, 1]));
        for owner in 0..4 {
            spawn(&mut app, UnitKind::Kernel, owner);
        }
        let mut fates = app.world_mut().resource_mut::<TeamFates>();
        fates.kill_team(1);
        fates.kill_team(0);
        app.world_mut().run_system_once(check_game_over).unwrap();
        let counts = app.world().resource::<TeamUnitCounts>();
        assert_eq!((counts.limit(0), counts.limit(1)), (0, 0));
        assert_eq!(counts.limit(2), 3 * MAX);
        assert_eq!(counts.limit(3), MAX);
    }

    /// No factory anywhere (sandbox / demo maps): nothing dies, no
    /// state flip.
    #[test]
    fn sandbox_without_factories_is_left_alone() {
        let mut app = world(seats(&[0, 1]));
        let bit = spawn(&mut app, UnitKind::Bit, 0);
        spawn(&mut app, UnitKind::Bug, 1);
        run_census(&mut app);
        assert_eq!(hp(&app, bit), 100.0);
        assert_eq!(pending_state(&app), None);
    }

    /// Units of an owner without a seat (map-event eruption neutrals)
    /// own no factory by design and must survive the census.
    #[test]
    fn unseated_neutral_team_is_not_eliminated() {
        let mut app = world(seats(&[0, 1]));
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Hole, 1);
        let wall = spawn(&mut app, UnitKind::BadBlock, 99);
        run_census(&mut app);
        assert_eq!(hp(&app, wall), 100.0);
        assert_eq!(pending_state(&app), None);
    }

    /// The census waits for its frame (`f % 30 == 25`).
    #[test]
    fn census_runs_on_frame_25() {
        let mut app = world(seats(&[0, 1]));
        spawn(&mut app, UnitKind::Kernel, 0);
        let bug = spawn(&mut app, UnitKind::Bug, 1);
        for frame in [1, 24, 26, 30, 54] {
            app.world_mut().resource_mut::<SimTick>().0 = frame;
            app.world_mut().run_system_once(check_game_over).unwrap();
            assert_eq!(hp(&app, bug), 100.0, "frame {frame}");
        }
        app.world_mut().resource_mut::<SimTick>().0 = 55;
        app.world_mut().run_system_once(check_game_over).unwrap();
        assert_eq!(hp(&app, bug), 0.0);
    }

    /// After "Keep on playing" the panel stays closed, but a
    /// factoryless owner is still wiped.
    #[test]
    fn dismissed_keeps_eliminating_without_state_flip() {
        let mut app = world(seats(&[0, 1, 2]));
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Hole, 1);
        let bug = spawn(&mut app, UnitKind::Bug, 2);
        app.world_mut().resource_mut::<GameOverDismissed>().0 = true;
        run_census(&mut app);
        assert_eq!(hp(&app, bug), 0.0);
        assert_eq!(pending_state(&app), None);
    }

    /// The census judges the match in the world, not the one the menu
    /// just asked for: while a Restart / demo reroll loads, `GameSetup`
    /// already names the next match (here a skirmish whose human seat
    /// owns nothing in this world) but the running demo is spectated
    /// and never announced.
    #[test]
    fn pending_setup_does_not_judge_the_running_match() {
        let demo = GameSetup {
            demo: true,
            ..seats(&[0, 1])
        };
        let mut app = world(demo);
        app.insert_resource(GameSetup::default())
            .insert_resource(LocalTeam(SPECTATOR_TEAM));
        spawn(&mut app, UnitKind::Kernel, 1);
        spawn(&mut app, UnitKind::Hole, 0);
        run_census(&mut app);
        assert_eq!(pending_state(&app), None);
    }

    /// The demo still eliminates beaten seats so its battles resolve.
    #[test]
    fn demo_eliminates_without_announcing() {
        let demo = GameSetup {
            demo: true,
            ..seats(&[0, 1])
        };
        let mut app = world(demo);
        app.insert_resource(LocalTeam(SPECTATOR_TEAM));
        spawn(&mut app, UnitKind::Kernel, 0);
        let bug = spawn(&mut app, UnitKind::Bug, 1);
        run_census(&mut app);
        run_census(&mut app);
        assert_eq!(hp(&app, bug), 0.0);
        assert_eq!(fates(&app).winners(), Some(&[0][..]));
        assert_eq!(pending_state(&app), None);
    }

    /// Before the first match there is nothing to judge.
    #[test]
    fn no_active_match_no_check() {
        let mut app = world(seats(&[0, 1]));
        app.world_mut().remove_resource::<ActiveMatch>();
        let bit = spawn(&mut app, UnitKind::Bit, 0);
        spawn(&mut app, UnitKind::Hole, 1);
        run_census(&mut app);
        assert_eq!(hp(&app, bit), 100.0);
        assert_eq!(pending_state(&app), None);
    }
}
