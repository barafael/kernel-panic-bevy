//! Joining a running match (M3, `docs/multiplayer-plan.md`; write-up
//! `net-experiments/m3-turn-log.md`): a player who dropped (or whose
//! page reloaded) comes back to its seat, and anyone else who opens the
//! room watches.
//!
//! - **Who is who.** A connection's [`PeerId`] is new every time (a
//!   matchbox UUID per socket), so seats belong to a [`PlayerKey`]: the
//!   public half of a key pair each game keeps for itself
//!   ([`PlayerSecret::local`]: per browser tab in `sessionStorage`, so a
//!   reload keeps it; per process natively). omdurman's finding
//!   (`omdurman-net/src/lib.rs`, `PlayerKey`): bind seats to stable
//!   keys, not to session ids. Spring binds them to the player's name
//!   (`CGameServer::BindConnection`, `Net/GameServer.cpp:2914`).
//! - **Proving it.** The lobby `Hello` goes to everyone, so the key in it
//!   is public; what claims the seat is the [`SeatProof`] beside it, a
//!   signature over the sender's own connection id. The signaling server
//!   assigns those ids and the transport says who sent what, so a `Hello`
//!   copied onto another connection proves nothing
//!   ([`super::lobby::Hello::player_key`] gives the "none" key, which watches). It takes no round trip and no
//!   secret on the wire, and any peer can check it: a host elected later
//!   needs nothing handed over.
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

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::lobby::StartMsg;
use super::transport::PeerId;

/// A player's identity across connections, what a seat belongs to: an
/// Ed25519 public key. Public: every lobby `Hello` shows it. All zeros
/// is "none" (nobody's seat).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct PlayerKey(pub [u8; 32]);

impl std::fmt::Display for PlayerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl PlayerKey {
    /// Did this key's owner sign `connection` as its own?
    pub fn proven(&self, connection: PeerId, proof: &SeatProof) -> bool {
        // `verify_strict`: no weak keys, whose signatures fit any message.
        *self != Self::default()
            && VerifyingKey::from_bytes(&self.0).is_ok_and(|key| {
                key.verify_strict(&proof_message(connection), &proof.signature())
                    .is_ok()
            })
    }
}

/// An Ed25519 signature over one connection id ([`PlayerSecret::proof`]).
/// Two halves: serde has no 64-byte arrays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatProof(pub [u8; 32], pub [u8; 32]);

impl SeatProof {
    fn signature(&self) -> Signature {
        let mut bytes = [0; 64];
        bytes[..32].copy_from_slice(&self.0);
        bytes[32..].copy_from_slice(&self.1);
        Signature::from_bytes(&bytes)
    }
}

/// What a [`SeatProof`] signs: the connection, under a tag of its own so
/// the signature means nothing anywhere else.
fn proof_message(connection: PeerId) -> [u8; 43] {
    const TAG: &[u8; 27] = b"kernel-panic seat proof v1\0";
    let mut message = [0; 43];
    message[..27].copy_from_slice(TAG);
    message[27..].copy_from_slice(&connection.0.to_le_bytes());
    message
}

/// The private half of a [`PlayerKey`]. It never leaves this game.
#[derive(Clone)]
pub struct PlayerSecret(SigningKey);

impl std::fmt::Debug for PlayerSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PlayerSecret({})", self.key())
    }
}

impl PlayerSecret {
    /// A fresh key from the system's secure random source (not the
    /// game's: [`crate::game_setup::random_seed`] is guessable).
    pub fn random() -> Self {
        Self(SigningKey::from_bytes(&secure_seed()))
    }

    /// The key derived from a small number, for tests and `KP_PLAYER_KEY`.
    /// Anyone can guess such a key: never a player's by default.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_small_seed(seed: u64) -> Self {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&seed.to_le_bytes());
        Self(SigningKey::from_bytes(&bytes))
    }

    /// This game's key. Web: kept in the tab's `sessionStorage`, so a
    /// reload (and only this tab) keeps it. Native: `KP_PLAYER_KEY` (hex,
    /// a dev tool: see [`Self::from_small_seed`]) if set, else one per
    /// process — a player whose connection broke rejoins from the same
    /// game; a restarted game watches.
    pub fn local() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            web_secret().unwrap_or_else(Self::random)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            static SECRET: std::sync::OnceLock<PlayerSecret> = std::sync::OnceLock::new();
            SECRET
                .get_or_init(|| {
                    std::env::var("KP_PLAYER_KEY")
                        .ok()
                        .and_then(|k| u64::from_str_radix(k.trim(), 16).ok())
                        .filter(|k| *k != 0)
                        .map_or_else(Self::random, Self::from_small_seed)
                })
                .clone()
        }
    }

    /// The public half: who this is.
    pub fn key(&self) -> PlayerKey {
        PlayerKey(self.0.verifying_key().to_bytes())
    }

    /// `connection` is ours.
    pub fn proof(&self, connection: PeerId) -> SeatProof {
        let bytes = self.0.sign(&proof_message(connection)).to_bytes();
        let mut proof = SeatProof::default();
        proof.0.copy_from_slice(&bytes[..32]);
        proof.1.copy_from_slice(&bytes[32..]);
        proof
    }
}

/// 32 bytes from the operating system. Without them there is no safe key
/// to make, and no way to play on: a guessable key would give the seat
/// to anyone.
#[cfg(not(target_arch = "wasm32"))]
fn secure_seed() -> [u8; 32] {
    let mut seed = [0; 32];
    getrandom::fill(&mut seed).expect("the system's random source failed");
    seed
}

/// 32 bytes from the browser's `crypto.getRandomValues` (see the native
/// [`secure_seed`] on failing).
#[cfg(target_arch = "wasm32")]
fn secure_seed() -> [u8; 32] {
    let mut seed = [0; 32];
    web_sys::window()
        .and_then(|w| w.crypto().ok())
        .and_then(|c| c.get_random_values_with_u8_array(&mut seed).ok())
        .expect("the browser's random source failed");
    seed
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Web: the tab's secret from `sessionStorage`, made and stored on first
/// use. Anything else stored there (an older build's key) is replaced.
#[cfg(target_arch = "wasm32")]
fn web_secret() -> Option<PlayerSecret> {
    const STORAGE_KEY: &str = "kernel-panic.player-key";
    let storage = web_sys::window()?.session_storage().ok()??;
    if let Some(stored) = storage.get_item(STORAGE_KEY).ok().flatten()
        && let Some(seed) = unhex32(&stored)
    {
        return Some(PlayerSecret(SigningKey::from_bytes(&seed)));
    }
    let secret = PlayerSecret::random();
    let _ = storage.set_item(STORAGE_KEY, &hex(secret.0.as_bytes()));
    Some(secret)
}

#[cfg(target_arch = "wasm32")]
fn unhex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
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

    fn key(seed: u64) -> PlayerKey {
        PlayerSecret::from_small_seed(seed).key()
    }

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
            keys: [(host, key(10)), (PeerId(5), key(11)), (PeerId(9), key(12))].into(),
            names: BTreeMap::new(),
            in_room: BTreeSet::new(),
        };
        assert_eq!(
            door.seat_for(&roster, key(11), host),
            (Some(1), Some(PeerId(5))),
            "the seat's key gets it back"
        );
        assert_eq!(
            door.seat_for(&roster, key(12), host),
            (None, Some(PeerId(9))),
            "a spectator watches again"
        );
        assert_eq!(door.seat_for(&roster, key(77), host), (None, None));
        assert_eq!(
            door.seat_for(&roster, key(10), host),
            (None, None),
            "nobody takes the host's place"
        );
        assert_eq!(
            door.seat_for(&roster, PlayerKey::default(), host),
            (None, None)
        );
    }

    /// A proof names one key and one connection: a copy on any other
    /// connection, or under any other key, is worth nothing.
    #[test]
    fn a_proof_is_good_for_its_own_connection_only() {
        let secret = PlayerSecret::from_small_seed(1);
        let me = PeerId(5);
        let proof = secret.proof(me);
        assert!(secret.key().proven(me, &proof));
        assert!(
            !secret.key().proven(PeerId(6), &proof),
            "copied onto another connection"
        );
        assert!(!key(2).proven(me, &proof), "under another key");
        let mut bent = proof;
        bent.0[0] ^= 1;
        assert!(!secret.key().proven(me, &bent), "altered");
        assert!(
            !PlayerKey::default().proven(me, &SeatProof::default()),
            "the \"none\" key"
        );
        // A weak public key (the identity point) signs anything.
        let mut weak = [0; 32];
        weak[0] = 1;
        let mut anything = SeatProof::default();
        anything.0[0] = 1;
        assert!(!PlayerKey(weak).proven(me, &anything), "a weak key");
    }

    /// Fresh keys come from the secure source, and differ.
    #[test]
    fn fresh_keys_differ() {
        assert_ne!(PlayerSecret::random().key(), PlayerSecret::random().key());
    }
}
