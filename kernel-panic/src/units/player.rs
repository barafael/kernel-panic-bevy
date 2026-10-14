//! The local (human) player.
//!
//! One owner (seat) on the map is driven by the human sitting at this
//! machine; AI seats are driven by the AI (`super::ai`), other humans'
//! seats by their peers. This module carries that identity so the input
//! pipeline (selection, orders, command panel), the result panel and the
//! local perspective (fog, cloak fades) have a single source of truth
//! for "who am I?". No sim state depends on it (only sounds and
//! visibility the fixed tick drives read it).

use bevy::prelude::*;

/// The owner the human player controls (`TeamId`, Spring's
/// `gu->myTeam`): the index of the
/// [`GameSetup`](crate::game_setup::GameSetup)'s human seat, set at match
/// start, or [`SPECTATOR_TEAM`](crate::game_setup::SPECTATOR_TEAM) when
/// every seat is AI (the menu's attract demo). Its side is
/// `Alliances::ally_team(local)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Resource)]
pub struct LocalTeam(pub u8);
