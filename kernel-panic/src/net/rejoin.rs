//! Joining a running match (M3, `docs/multiplayer-plan.md`; write-up
//! `net-experiments/m3-turn-log.md`): a player who dropped (or whose
//! page reloaded) comes back to its seat, and anyone else who opens the
//! room watches.
//!
//! - **Who is who.** A connection's [`PeerId`] is new every time (a
//!   matchbox UUID per socket), so seats belong to a [`PlayerKey`]: a
//!   random number each game keeps for itself ([`PlayerKey::local`]: per
//!   browser tab in `sessionStorage`, so a reload keeps it; per process
//!   natively) and says in its lobby `Hello`. omdurman's finding
//!   (`omdurman-net/src/lib.rs`, `PlayerKey`): bind seats to stable
//!   keys, not to session ids. Spring binds them to the player's name
//!   (`CGameServer::BindConnection`, `Net/GameServer.cpp:2914`).
//! - **The door.** The host's session answers a newcomer's `Hello` while
//!   the match runs ([`super::session::Session`]'s `knock`): the key of
//!   a seat gets that seat back (the old connection leaves the roster,
//!   and is told, should it still be open: Spring's "terminating
//!   existing connection"); any other key watches (Spring lets anyone
//!   join a running game as a spectator by default, `AllowSpectatorJoin`,
//!   `Net/GameServer.cpp:73`, `:2957-2966`). Every guest hears of
//!   it ([`super::wire::Msg::Joined`]). The newcomer gets [`Running`]:
//!   the `Start` it missed with the roster as it is now.
//! - **Not a knock: back in the room** (`net-experiments/m3-rooms.md`).
//!   A player that left with "Back to room" keeps its connection and
//!   says hello to the room; the match's host may still be playing. A
//!   hello from a connection the match already has (its roster), or
//!   under the key of a player that went back to the room
//!   ([`Door::in_room`]), is the room's business: the host's session
//!   ignores it, and the room counts the host as away until it is back
//!   too. Only a connection new to the match, under any other key, is let
//!   in (a seat's key back on its seat, the rest watching).
//! - **Catching up.** The newcomer starts the match like everyone else
//!   (map, seed, seats from the `Start`) with a session that holds no
//!   turn yet ([`super::session::Session::late`]); the host's whole turn
//!   log comes as gap fills in ≤ 16 KiB chunks from tick 0, and the
//!   driver runs the sim as fast as the machine can, drawing a frame
//!   now and then ([`super::driver::Pacer::fast_forward`]), until it is
//!   live. Spring's way (the server resends its whole packet cache,
//!   `GameServer.cpp:3046-3047`, and the client simulates it).
//! - **The seat's fate.** A disconnect is dropped only after
//!   [`super::session::Timeouts::rejoin`] (30 s): a player back by then
//!   finds everything as it was, nothing sequenced. After the drop
//!   (the user's leaving rules acted: its units idle for an ally to
//!   take, or fused with its side), the host sequences
//!   [`SimCommand::Returned`](super::SimCommand::Returned) after the
//!   `Dropped`, and every peer decides alike: back in play if its side
//!   is still in it and its units still its own, else it watches.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::lobby::StartMsg;
use super::transport::PeerId;

/// A player's identity across connections: what a seat belongs to.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct PlayerKey(pub u64);

impl std::fmt::Display for PlayerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl PlayerKey {
    /// A fresh random key (never 0: 0 is "none").
    pub fn random() -> Self {
        Self(crate::game_setup::random_seed() | 1)
    }

    /// This game's key. Web: kept in the tab's `sessionStorage`, so a
    /// reload (and only this tab) keeps it. Native: `KP_PLAYER_KEY`
    /// (hex) if set, else one per process — a player whose connection
    /// broke rejoins from the same game; a restarted game watches.
    pub fn local() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            web_key().unwrap_or_else(Self::random)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            static KEY: std::sync::OnceLock<PlayerKey> = std::sync::OnceLock::new();
            *KEY.get_or_init(|| {
                std::env::var("KP_PLAYER_KEY")
                    .ok()
                    .and_then(|k| u64::from_str_radix(k.trim(), 16).ok())
                    .filter(|k| *k != 0)
                    .map_or_else(Self::random, PlayerKey)
            })
        }
    }
}

/// Web: the tab's key from `sessionStorage`, made and stored on first
/// use.
#[cfg(target_arch = "wasm32")]
fn web_key() -> Option<PlayerKey> {
    const STORAGE_KEY: &str = "kernel-panic.player-key";
    let storage = web_sys::window()?.session_storage().ok()??;
    if let Some(k) = storage
        .get_item(STORAGE_KEY)
        .ok()
        .flatten()
        .and_then(|k| u64::from_str_radix(&k, 16).ok())
        .filter(|k| *k != 0)
    {
        return Some(PlayerKey(k));
    }
    let key = PlayerKey::random();
    let _ = storage.set_item(STORAGE_KEY, &key.to_string());
    Some(key)
}

/// What the host needs to let late joiners into its running match: the
/// `Start` everyone got, and who played with which key and name. The
/// lobby hands it over with the match ([`super::lobby::MatchStart`]);
/// the session keeps it up to date as players come back.
#[derive(Clone, Debug)]
pub struct Door {
    pub start: StartMsg,
    /// The key of every peer of the roster.
    pub keys: BTreeMap<PeerId, PlayerKey>,
    /// Their names, for the newcomers' player lists.
    pub names: BTreeMap<PeerId, String>,
    /// Keys of players that went back to the room ([`super::wire::Msg::Bye`]):
    /// a hello under one of them is the room's, not a knock.
    pub in_room: BTreeSet<PlayerKey>,
}

impl Door {
    /// Where a newcomer with `key` goes: the seat (`None`: watching) and
    /// the connection it replaces. A key that played a seat gets it back,
    /// one that watched watches again (replacing its old connection); any
    /// other watches. The host's own key never moves the host.
    pub fn seat_for(
        &self,
        roster: &BTreeMap<PeerId, Option<u8>>,
        key: PlayerKey,
        host: PeerId,
    ) -> (Option<u8>, Option<PeerId>) {
        if key == PlayerKey::default() {
            return (None, None);
        }
        roster
            .iter()
            .find(|(p, _)| **p != host && self.keys.get(*p) == Some(&key))
            .map_or((None, None), |(p, seat)| (*seat, Some(*p)))
    }

    /// `peer` is no longer part of the match (replaced).
    pub fn forget(&mut self, peer: PeerId) {
        self.keys.remove(&peer);
        self.names.remove(&peer);
    }
}

/// The host's answer to a peer that introduced itself in the room of a
/// running match: join it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    /// The match's `Start`, its roster as it is now (the newcomer in it,
    /// on its seat or watching).
    pub start: StartMsg,
    /// The match's host (the lowest id when the match started; a
    /// newcomer's id may be lower).
    pub host: PeerId,
    /// Peers of the roster that already left.
    pub gone: Vec<PeerId>,
    /// Everyone's name.
    pub names: Vec<(PeerId, String)>,
    /// Turns the host has cut so far (how far there is to catch up).
    pub newest: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seats_follow_keys_not_connections() {
        let host = PeerId(1);
        let roster: BTreeMap<PeerId, Option<u8>> =
            [(host, Some(0)), (PeerId(5), Some(1)), (PeerId(9), None)].into();
        let door = Door {
            start: StartMsg {
                match_id: 1,
                setup: crate::net::lobby::encode_setup(&Default::default()),
                roster: roster.iter().map(|(p, s)| (*p, *s)).collect(),
                compat: crate::net::compat::Compat::of(crate::net::compat::Build::current(), 0),
            },
            keys: [
                (host, PlayerKey(10)),
                (PeerId(5), PlayerKey(11)),
                (PeerId(9), PlayerKey(12)),
            ]
            .into(),
            names: BTreeMap::new(),
            in_room: BTreeSet::new(),
        };
        assert_eq!(
            door.seat_for(&roster, PlayerKey(11), host),
            (Some(1), Some(PeerId(5))),
            "the seat's key gets it back"
        );
        assert_eq!(
            door.seat_for(&roster, PlayerKey(12), host),
            (None, Some(PeerId(9))),
            "a spectator watches again"
        );
        assert_eq!(door.seat_for(&roster, PlayerKey(77), host), (None, None));
        assert_eq!(
            door.seat_for(&roster, PlayerKey(10), host),
            (None, None),
            "nobody takes the host's place"
        );
        assert_eq!(door.seat_for(&roster, PlayerKey(0), host), (None, None));
    }
}
