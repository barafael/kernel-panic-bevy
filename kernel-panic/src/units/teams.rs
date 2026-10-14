//! Owners and ally teams: upstream's `CTeamHandler` / `AllyTeam`
//! (`rts/Sim/Misc/TeamHandler.cpp`, `AllyTeam.h`, `GlobalSynced`).
//!
//! Spring keeps two levels, and so does the port:
//!
//! - **Owner** — Spring's *team* (`CTeam`), one per seat: seat `i` of the
//!   [`GameSetup`] is owner `i`, and its units carry [`TeamId`]`(i)`. An
//!   owner holds the units, the unit cap (`MaxUnits`, per team), the
//!   Network packet buffer, its AI brain and the right to command its
//!   units.
//! - **Ally team** — Spring's *allyteam* ([`PlayerSpec::ally_team`]):
//!   decides friend or foe ([`Alliances::friendly`]), shares cloak
//!   detection and the local player's sight, and wins or loses together
//!   ([`TeamFates`], `units::lifecycle::game_over`).
//!
//! A Duel has one seat per side, so owner and ally team coincide and
//! single player plays as before. Outgunned's AI seats are one owner
//! each in one ally team, as upstream's launcher seated them
//! (`GenerateGame.lua`: `TEAM<p>` with `AllyTeam=0|1`), so each runs
//! its own AI and they fight side by side. Mirror matches still fight:
//! the sides are different ally teams.
//!
//! Alliances are fixed for the match: Spring's `FixedAllies` (an engine
//! option, on by default; `EngineOptions.lua`). KP's launcher wrote
//! `fixedallies=0`, which only allowed the `/ally` chat command; the
//! port has no such command.
//!
//! Units of an owner without a seat (the map-event eruption neutrals,
//! `map_events::ERUPTION_TEAM`) are friendly to that owner alone, as
//! they were before: Spring's Gaia allyteam.
//!
//! [`GameSetup`]: crate::game_setup::GameSetup
//! [`PlayerSpec::ally_team`]: crate::game_setup::PlayerSpec::ally_team
//! [`TeamId`]: crate::units::components::TeamId

use std::collections::BTreeSet;

use bevy::prelude::*;

use crate::game_setup::GameSetup;

/// Key that makes an unseated owner an ally team of its own: never equal
/// to a seated owner's ally team (`u8`), or to another unseated owner's.
const UNSEATED: u16 = 0x100;

/// Which owners are allied: the match's seats and their ally teams
/// (`AllyTeam::allies`, from the setup). Set from the [`GameSetup`] at
/// every match swap (`begin_match`), never from who is watching. The
/// default (no seats) makes every owner an ally team of its own, which
/// is what a bare test world and the menu before the first match see.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct Alliances {
    /// Per owner: its seat's ally team, or `UNSEATED + owner`.
    key: [u16; 256],
    /// `(owner, ally team)` of every seat, in seat order.
    seats: Vec<(u8, u8)>,
}

impl Default for Alliances {
    fn default() -> Self {
        Self {
            key: std::array::from_fn(|owner| UNSEATED + owner as u16),
            seats: Vec::new(),
        }
    }
}

impl Alliances {
    /// The seats of `setup`: seat `i` is owner `i` in ally team
    /// `players[i].ally_team`.
    pub fn from_setup(setup: &GameSetup) -> Self {
        Self::from_seats(
            setup
                .players
                .iter()
                .enumerate()
                .map(|(i, p)| (i as u8, p.ally_team)),
        )
    }

    /// From `(owner, ally team)` pairs (tests, the replay's seats).
    pub fn from_seats(seats: impl IntoIterator<Item = (u8, u8)>) -> Self {
        let mut out = Self::default();
        for (owner, ally) in seats {
            out.key[owner as usize] = u16::from(ally);
            out.seats.push((owner, ally));
        }
        out
    }

    /// Are `a` and `b` on the same side (`CTeamHandler::Ally`, with an
    /// owner always its own ally)? Everything that tells friend from
    /// foe asks this: targeting, splash and line-of-fire checks, cloak
    /// detection, the AI's enemy lists, command validation.
    #[inline]
    pub fn friendly(&self, a: u8, b: u8) -> bool {
        self.key[a as usize] == self.key[b as usize]
    }

    /// The ally team of a seated owner (`CTeamHandler::AllyTeam`).
    pub fn ally_team(&self, owner: u8) -> Option<u8> {
        self.seats
            .iter()
            .find(|(o, _)| *o == owner)
            .map(|(_, ally)| *ally)
    }

    /// Does `owner` have a seat in the match?
    pub fn is_seated(&self, owner: u8) -> bool {
        self.seats.iter().any(|(o, _)| *o == owner)
    }

    /// The seated owners, in seat order.
    pub fn owners(&self) -> impl Iterator<Item = u8> + '_ {
        self.seats.iter().map(|(o, _)| *o)
    }

    /// The seated owners allied with `owner` (itself included), in seat
    /// order.
    pub fn allies_of(&self, owner: u8) -> impl Iterator<Item = u8> + '_ {
        self.owners().filter(move |o| self.friendly(*o, owner))
    }

    /// Owner bitmask (bit `o` = owner `o`, owners below 64) of `owner`
    /// and its allies: what one detector reveals to its whole ally team
    /// (`cloak::DetectedBy`). An unseated owner sees for itself.
    pub fn vision_mask(&self, owner: u8) -> u64 {
        let bit = |o: u8| 1u64.checked_shl(u32::from(o)).unwrap_or(0);
        if !self.is_seated(owner) {
            return bit(owner);
        }
        self.allies_of(owner).fold(0, |mask, o| mask | bit(o))
    }
}

/// What has become of the match's owners: Spring's `CTeam::isDead`, and
/// the result once the match is decided (`CGame::GameEnd`'s winning
/// ally teams). Sim state, match-scoped: every peer kills the same
/// owners on the same tick, whoever is watching.
///
/// An owner dies through [`TeamFates::kill_team`] — the game-over
/// census when it loses its last factory (`game_over.lua`'s
/// `Spring.KillTeam`), or a resign — and stays dead. Its units are
/// destroyed (`game_over::check_game_over`), its unit cap is shared out
/// to its living allies (`CTeamHandler::UpdateTeamUnitLimitsOnDeath`),
/// and a human owner's player watches from then on. The sim keeps
/// running for everyone.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamFates {
    dead: BTreeSet<u8>,
    /// Killed since the last pass of `check_game_over`: their units
    /// still have to be destroyed.
    doomed: BTreeSet<u8>,
    /// Upstream's `CheckForGameOver`: an owner died, so the next census
    /// looks for a pair of enemies still alive.
    check_end: bool,
    /// The winning ally teams once the match is decided.
    winners: Option<Vec<u8>>,
}

impl TeamFates {
    /// `Spring.KillTeam` → `CTeam::Died`: `owner` is dead from now on.
    /// Its remaining units are destroyed by the next pass of
    /// `check_game_over` (this tick's, when called from the command
    /// apply at the head of the tick), and the match checks for its end
    /// at the next census. `false` when it already was dead.
    pub fn kill_team(&mut self, owner: u8) -> bool {
        if !self.dead.insert(owner) {
            return false;
        }
        self.doomed.insert(owner);
        self.check_end = true;
        true
    }

    pub fn is_dead(&self, owner: u8) -> bool {
        self.dead.contains(&owner)
    }

    /// The dead owners, sorted.
    pub fn dead(&self) -> impl Iterator<Item = u8> + '_ {
        self.dead.iter().copied()
    }

    /// The winning ally teams, once the match is decided (`GameOver`).
    pub fn winners(&self) -> Option<&[u8]> {
        self.winners.as_deref()
    }

    /// Owners killed since the last game-over pass, cleared.
    pub(crate) fn take_doomed(&mut self) -> BTreeSet<u8> {
        std::mem::take(&mut self.doomed)
    }

    /// Upstream's `CheckForGameOver` flag, cleared.
    pub(crate) fn take_check_end(&mut self) -> bool {
        std::mem::take(&mut self.check_end)
    }

    pub(crate) fn decide(&mut self, winners: Vec<u8>) {
        self.winners.get_or_insert(winners);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_setup::PlayerSpec;
    use crate::units::components::Faction;

    fn setup(allies: &[u8]) -> GameSetup {
        GameSetup {
            players: allies
                .iter()
                .map(|&ally_team| PlayerSpec {
                    faction: Faction::System,
                    ally_team,
                    ai: true,
                })
                .collect(),
            ..GameSetup::default()
        }
    }

    /// A 2v2: owners 0, 1 on ally team 0, owners 2, 3 on ally team 1.
    #[test]
    fn seats_on_one_side_are_friendly() {
        let a = Alliances::from_setup(&setup(&[0, 0, 1, 1]));
        assert!(a.friendly(0, 1));
        assert!(a.friendly(2, 3));
        assert!(!a.friendly(1, 2));
        assert!(a.friendly(3, 3));
        assert_eq!(a.ally_team(3), Some(1));
        assert_eq!(a.allies_of(1).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(a.vision_mask(2), 0b1100);
    }

    /// One seat per side (a Duel): owner and ally team coincide, and a
    /// mirror match's two System seats still fight.
    #[test]
    fn duel_sides_are_enemies() {
        let a = Alliances::from_setup(&setup(&[0, 1]));
        assert!(!a.friendly(0, 1));
        assert!(a.friendly(0, 0));
    }

    /// An owner without a seat (map-event neutrals) is its own side, even
    /// when its number equals a seated ally team's.
    #[test]
    fn unseated_owner_is_alone() {
        let a = Alliances::from_setup(&setup(&[0, 1, 1]));
        assert!(!a.friendly(200, 0));
        assert!(!a.friendly(200, 1));
        assert!(!a.friendly(200, 201));
        assert!(a.friendly(200, 200));
        assert_eq!(a.vision_mask(40), 1 << 40);
        // No seats at all: every owner on its own (bare test worlds).
        let none = Alliances::default();
        assert!(!none.friendly(0, 1));
        assert!(none.friendly(1, 1));
    }

    #[test]
    fn a_team_dies_once() {
        let mut fates = TeamFates::default();
        assert!(fates.kill_team(2));
        assert!(!fates.kill_team(2));
        assert!(fates.is_dead(2));
        assert_eq!(fates.take_doomed().into_iter().collect::<Vec<_>>(), vec![2]);
        assert!(fates.take_doomed().is_empty());
        assert!(fates.take_check_end());
        assert!(!fates.take_check_end());
        fates.decide(vec![0]);
        fates.decide(vec![1]);
        assert_eq!(fates.winners(), Some(&[0][..]));
    }
}
