use std::collections::BTreeMap;

use bevy::prelude::*;

use crate::game_setup::{ActiveMatch, GameOverDismissed, GameSetup, SPECTATOR_TEAM};
use crate::net::NetId;
use crate::sim::{RngStream, SimRng, SimTick};
use crate::units::combat::Dying;
use crate::units::components::{Health, TeamId, UnitType};
use crate::units::lifecycle::production::Producer;
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

/// The longest fuse, in sim frames: once an ally team's last active
/// owner has left, each of its units blows up at a random frame within
/// the next 4 s (the user's rule, 2026-10-15).
pub const FUSE_FRAMES: u32 = 120;

/// A unit of an ally team that is out: it blows up (HP to zero, the
/// regular death path with its `ExplodeAs` blast) on sim frame `at`
/// ([`detonate_fuses`]).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
#[component(storage = "SparseSet")]
pub struct LeaveFuse {
    pub at: u64,
}

/// The fuses' dice: 0..=[`FUSE_FRAMES`] per unit, in `NetId` order.
pub struct LeaveFuseRng;

impl RngStream for LeaveFuseRng {
    const SALT: u64 = 0x1ea7_ef05;
}

/// A fuse burnt down: the unit dies.
pub fn detonate_fuses(
    tick: Res<SimTick>,
    mut fused: Query<(&LeaveFuse, &mut Health), Without<Dying>>,
) {
    for (fuse, mut health) in &mut fused {
        if tick.frame() >= fuse.at && health.current > 0.0 {
            health.current = 0.0;
        }
    }
}

/// Is `owner` part of an ally team that is out — seated, and no owner
/// of its ally team still active?
fn ally_team_out(alliances: &Alliances, fates: &TeamFates, owner: u8) -> bool {
    alliances.is_seated(owner) && alliances.allies_of(owner).all(|o| fates.is_dead(o))
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
/// the sim tick, with the user's rules for what leaving does
/// (2026-10-15, `docs/multiplayer-plan.md`):
///
/// - Every census frame (once a second), each active seated owner
///   without a factory — homebase or MiniFac, nanoframes included
///   (`Spring.GetTeamUnitsByDefs` counts units still being built) — is
///   taken out ([`TeamFates::leave`], `Spring.KillTeam`), in seat order,
///   the one after each removed owner left for the next census as
///   upstream's list walk does. A resign (the command apply) leaves
///   through the same function.
/// - **An owner leaves while an ally is still active:** its units stay,
///   idle (orders and factory queues cancelled), still its own and
///   friendly to the team, for an ally to take (`SimCommand::Take`).
///   Upstream destroys a dead team's units; KP's original would have
///   kept a resigned team's units fighting leaderless.
/// - **The ally team's last active owner leaves** (resign, taken out, a
///   1v1): every remaining unit of the ally team — the last leaver's and
///   whatever earlier leavers' nobody took — gets a [`LeaveFuse`] of
///   0..=[`FUSE_FRAMES`] frames from its own [`SimRng`] stream, in
///   `NetId` order, and blows up with its regular death blast. Anything
///   of that ally team that appears later is fused at the next census.
/// - At the census after a leave (`CheckForGameOver`), the match ends
///   once no two active owners are enemies ([`decided`]) **and** the
///   losers' units are gone, so the end comes after the explosions. An
///   ally team loses when all its owners have left: shared victory and
///   defeat.
/// - Unit caps never change (no `UpdateTeamUnitLimitsOnDeath`, see
///   `TeamUnitCounts::limit`).
///
/// The simulation never stops for an outcome: the local player's
/// Victory / Defeat is presentation ([`announce_outcome`]). Owners
/// without a seat in the running match ([`ActiveMatch`]; the map-event
/// neutrals) are never counted or removed, as upstream exempts Gaia.
/// Showcase and sandbox matches have no census (a resign still blows
/// the resigner up); a match where no seat has a factory and nobody has
/// left (test maps) is never judged. The menu's pending
/// [`GameSetup`](crate::game_setup::GameSetup) is not consulted: the
/// old world keeps running while the next map loads and is judged
/// against its own seats.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn check_game_over(
    tick: Res<SimTick>,
    active: Option<Res<ActiveMatch>>,
    alliances: Res<Alliances>,
    mut fates: ResMut<TeamFates>,
    mut rng: ResMut<SimRng<LeaveFuseRng>>,
    units: Query<(Entity, &NetId, &TeamId, &UnitType, Has<LeaveFuse>), Without<Dying>>,
    mut producers: Query<&mut Producer>,
    mut commands: Commands,
) {
    // No match in the world yet: nothing to judge.
    let Some(ActiveMatch(setup)) = active.as_deref() else {
        return;
    };
    let census = census_tick(*tick) && judged(setup);
    if census {
        if fates.take_check_end() {
            // The losers' fuses burn first: the end waits for the last
            // explosion.
            let blasting = units
                .iter()
                .any(|(_, _, team, ..)| ally_team_out(&alliances, &fates, team.0));
            if blasting {
                fates.check_end_again();
            } else if let Some(winners) = decided(&alliances, &fates) {
                fates.decide(winners);
            }
        }
        let mut factories: BTreeMap<u8, u32> = alliances
            .owners()
            .filter(|o| !fates.is_dead(*o))
            .map(|o| (o, 0))
            .collect();
        for (_, _, team, unit, _) in &units {
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
            // removed one is not looked at until the next census.
            let mut skip = false;
            for (&owner, &n) in &factories {
                if std::mem::take(&mut skip) {
                    continue;
                }
                if n == 0 {
                    fates.leave(owner);
                    skip = true;
                }
            }
        }
    }

    let left = fates.take_doomed();
    for &owner in &left {
        fates.note_left(owner, tick.frame());
    }
    if left.is_empty() && !census {
        return;
    }
    // In `NetId` order: the fuses draw dice per unit.
    let mut mine: Vec<_> = units.iter().collect();
    mine.sort_unstable_by_key(|(_, id, ..)| **id);
    for (entity, _, team, _, fused) in mine {
        let owner = team.0;
        if ally_team_out(&alliances, &fates, owner) {
            if !fused {
                let delay = crate::rng::xorshift32(&mut rng) % (FUSE_FRAMES + 1);
                commands.entity(entity).try_insert(LeaveFuse {
                    at: tick.frame() + u64::from(delay),
                });
                // Nothing new comes off the pads meanwhile.
                if let Ok(mut producer) = producers.get_mut(entity) {
                    producer.cancel_all();
                }
            }
        } else if left.contains(&owner) {
            // An ally plays on: the leaver's units stand idle for it.
            crate::interaction::clear_orders(&mut commands.entity(entity));
            if let Ok(mut producer) = producers.get_mut(entity) {
                producer.cancel_all();
            }
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
    use crate::units::lifecycle::bookkeeping::{MAX_UNITS_PER_TEAM, TeamUnitCounts};
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
            .init_resource::<SimRng<LeaveFuseRng>>()
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

    /// The fuse of a unit, if it has one.
    fn fuse(app: &App, e: Entity) -> Option<u64> {
        app.world().get::<LeaveFuse>(e).map(|f| f.at)
    }

    /// Every fuse burns down (the units die), as the sim would within
    /// 4 s.
    fn blow_up(app: &mut App) {
        let w = app.world_mut();
        let fused: Vec<Entity> = w
            .query_filtered::<Entity, With<LeaveFuse>>()
            .iter(w)
            .collect();
        for e in fused {
            w.entity_mut(e).insert(Dying { timer: 1.0 });
        }
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

    /// An enemy that lost its last factory, alone on its side, is out:
    /// its units are fused to blow up within 4 s, and the match is
    /// decided at the first census after the last one is gone.
    #[test]
    fn factoryless_enemy_blows_up_and_local_wins() {
        let mut app = world(seats(&[0, 1]));
        spawn(&mut app, UnitKind::Kernel, 0);
        let own_bit = spawn(&mut app, UnitKind::Bit, 0);
        let enemy_bug = spawn(&mut app, UnitKind::Bug, 1);
        let enemy_obelisk = spawn(&mut app, UnitKind::Obelisk, 1);
        run_census(&mut app);
        let now = app.world().resource::<SimTick>().frame();
        for e in [enemy_bug, enemy_obelisk] {
            let at = fuse(&app, e).expect("fused");
            assert!((now..=now + u64::from(FUSE_FRAMES)).contains(&at));
        }
        assert_eq!(fuse(&app, own_bit), None);
        assert!(fates(&app).is_dead(1));
        // Not before the losers' units are gone.
        run_census(&mut app);
        assert_eq!(fates(&app).winners(), None);
        assert_eq!(pending_state(&app), None);
        blow_up(&mut app);
        run_census(&mut app);
        assert_eq!(fates(&app).winners(), Some(&[0][..]));
        assert_eq!(pending_state(&app), Some(GameState::Victory));
    }

    /// A fuse goes off on its frame, not before.
    #[test]
    fn fuses_go_off_on_their_frame() {
        let mut app = world(seats(&[0, 1]));
        let bit = spawn(&mut app, UnitKind::Bit, 1);
        app.world_mut().entity_mut(bit).insert(LeaveFuse { at: 40 });
        for (frame, hp) in [(39, 100.0), (40, 0.0)] {
            app.world_mut().resource_mut::<SimTick>().0 = frame;
            app.world_mut().run_system_once(detonate_fuses).unwrap();
            assert_eq!(app.world().get::<Health>(bit).unwrap().current, hp);
        }
    }

    /// Two factoryless owners in a row: upstream's list walk skips the
    /// one after a removal, so the second leaves a census later.
    #[test]
    fn adjacent_departures_take_two_censuses() {
        let mut app = world(seats(&[0, 1, 2]));
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Bug, 1);
        spawn(&mut app, UnitKind::Bug, 2);
        run_census(&mut app);
        assert!(fates(&app).is_dead(1) && !fates(&app).is_dead(2));
        run_census(&mut app);
        assert!(fates(&app).is_dead(2));
    }

    /// A MiniFac alone keeps an owner in play — the homebase is not
    /// special in gamemode 1.
    #[test]
    fn minifac_keeps_team_alive() {
        let mut app = world(seats(&[0, 1]));
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Window, 1);
        let bug = spawn(&mut app, UnitKind::Bug, 1);
        run_census(&mut app);
        assert_eq!(fuse(&app, bug), None);
        assert_eq!(pending_state(&app), None);
    }

    /// Losing the local owner's factories in a duel fuses its army and
    /// shows Defeat at once; the simulation is not involved.
    #[test]
    fn local_without_factory_is_defeat() {
        let mut app = world(seats(&[0, 1]));
        let own_bit = spawn(&mut app, UnitKind::Bit, 0);
        spawn(&mut app, UnitKind::Hole, 1);
        run_census(&mut app);
        assert!(fuse(&app, own_bit).is_some());
        assert_eq!(pending_state(&app), Some(GameState::Defeat));
    }

    /// 2v2: one owner of a side out doesn't decide anything — its units
    /// stay, idle, for its ally (no fuse) and its ally fights on. When
    /// the second leaves, the whole ally team is out: everything left of
    /// it, the first leaver's untaken units included, is fused; shared
    /// defeat, the other side's owners share the win.
    #[test]
    fn ally_team_loses_when_all_its_owners_have_left() {
        let mut app = world(seats(&[0, 0, 1, 1]));
        // The local player (owner 0) has no factory: out, watching.
        let own = spawn(&mut app, UnitKind::Bit, 0);
        let kernel = spawn(&mut app, UnitKind::Kernel, 1);
        spawn(&mut app, UnitKind::Hole, 2);
        let enemy_ally = spawn(&mut app, UnitKind::Carrier, 3);
        run_census(&mut app);
        assert!(fates(&app).is_dead(0));
        assert_eq!(fuse(&app, own), None, "left for owner 1 to take");
        assert_eq!(pending_state(&app), Some(GameState::Defeat));
        run_census(&mut app);
        assert_eq!(fates(&app).winners(), None, "owner 1 still fights");
        assert_eq!(fuse(&app, enemy_ally), None);

        // Owner 1 falls too: ally team 0 is out, ally team 1 wins.
        app.world_mut().despawn(kernel);
        run_census(&mut app);
        assert!(fates(&app).is_dead(1));
        assert!(fuse(&app, own).is_some(), "untaken units blow up too");
        blow_up(&mut app);
        run_census(&mut app);
        assert_eq!(fates(&app).winners(), Some(&[1][..]));
        assert!(!fates(&app).is_dead(3), "allied owners win together");
    }

    /// The fallen player's allies winning later turns its Defeat into a
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
        blow_up(&mut app);
        run_census(&mut app);
        assert_eq!(fates(&app).winners(), Some(&[0][..]));
        assert_eq!(pending_state(&app), Some(GameState::Victory));
        assert!(!app.world().resource::<GameOverDismissed>().0);
    }

    /// A resign (`leave` from the command apply) with an ally still in
    /// play, off the census cadence: the units stay, idle, unfused; no
    /// unit cap changes (the user's rule; upstream would share the cap
    /// out, `UpdateTeamUnitLimitsOnDeath`).
    #[test]
    fn leaving_with_an_ally_keeps_units_and_caps() {
        let mut app = world(seats(&[0, 0, 1]));
        spawn(&mut app, UnitKind::Kernel, 0);
        let k1 = spawn(&mut app, UnitKind::Kernel, 1);
        spawn(&mut app, UnitKind::Hole, 2);
        app.world_mut().resource_mut::<TeamFates>().leave(1);
        app.world_mut().resource_mut::<SimTick>().0 = 3;
        app.world_mut().run_system_once(check_game_over).unwrap();
        assert_eq!(fuse(&app, k1), None);
        assert_eq!(app.world().get::<Health>(k1).unwrap().current, 100.0);
        assert_eq!(fates(&app).left_at(1), Some(3));
        let counts = app.world().resource::<TeamUnitCounts>();
        for owner in 0..3 {
            assert_eq!(counts.limit(owner), MAX_UNITS_PER_TEAM);
        }
    }

    /// The last active owner of an ally team resigns: every unit of the
    /// team gets a fuse within 4 s, drawn per unit — not all the same.
    #[test]
    fn last_leaver_fuses_the_whole_ally_team() {
        let mut app = world(seats(&[0, 0, 1]));
        let mut ours = Vec::new();
        for owner in [0, 1] {
            ours.push(spawn(&mut app, UnitKind::Kernel, owner));
            for _ in 0..10 {
                ours.push(spawn(&mut app, UnitKind::Bit, owner));
            }
        }
        let theirs = spawn(&mut app, UnitKind::Hole, 2);
        let mut fates = app.world_mut().resource_mut::<TeamFates>();
        fates.leave(1);
        fates.leave(0);
        app.world_mut().resource_mut::<SimTick>().0 = 7;
        app.world_mut().run_system_once(check_game_over).unwrap();
        let ats: Vec<u64> = ours
            .iter()
            .map(|e| fuse(&app, *e).expect("fused"))
            .collect();
        assert!(
            ats.iter()
                .all(|at| (7..=7 + u64::from(FUSE_FRAMES)).contains(at))
        );
        assert!(ats.iter().any(|at| *at != ats[0]), "random fuses");
        assert_eq!(fuse(&app, theirs), None);
    }

    /// No factory anywhere (sandbox / demo maps): nobody leaves, no
    /// state flip.
    #[test]
    fn sandbox_without_factories_is_left_alone() {
        let mut app = world(seats(&[0, 1]));
        let bit = spawn(&mut app, UnitKind::Bit, 0);
        spawn(&mut app, UnitKind::Bug, 1);
        run_census(&mut app);
        assert_eq!(fuse(&app, bit), None);
        assert_eq!(pending_state(&app), None);
    }

    /// Units of an owner without a seat (map-event neutrals)
    /// own no factory by design and must survive the census.
    #[test]
    fn unseated_neutral_team_is_not_eliminated() {
        let mut app = world(seats(&[0, 1]));
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Hole, 1);
        let wall = spawn(&mut app, UnitKind::BadBlock, 99);
        run_census(&mut app);
        assert_eq!(fuse(&app, wall), None);
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
            assert_eq!(fuse(&app, bug), None, "frame {frame}");
        }
        app.world_mut().resource_mut::<SimTick>().0 = 55;
        app.world_mut().run_system_once(check_game_over).unwrap();
        assert!(fuse(&app, bug).is_some());
    }

    /// After "Keep on playing" the panel stays closed, but a
    /// factoryless owner is still taken out.
    #[test]
    fn dismissed_keeps_eliminating_without_state_flip() {
        let mut app = world(seats(&[0, 1, 2]));
        spawn(&mut app, UnitKind::Kernel, 0);
        spawn(&mut app, UnitKind::Hole, 1);
        let bug = spawn(&mut app, UnitKind::Bug, 2);
        app.world_mut().resource_mut::<GameOverDismissed>().0 = true;
        run_census(&mut app);
        assert!(fuse(&app, bug).is_some());
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
        assert!(fuse(&app, bug).is_some());
        blow_up(&mut app);
        run_census(&mut app);
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
        assert_eq!(fuse(&app, bit), None);
        assert_eq!(pending_state(&app), None);
    }
}
