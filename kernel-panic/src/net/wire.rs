//! The lockstep wire format: every message is an [`Envelope`] (format
//! version, match id, one [`Msg`]) encoded with postcard, never empty
//! and never over [`MAX_MESSAGE`] bytes.
//!
//! Which channel carries what (`net-experiments/m1-runtime.md` §2):
//!
//! | message | direction | channel |
//! |---|---|---|
//! | [`Msg::Turns`] — the newest turns the guest lacks, ack of its batches, clock stamps | host → guest | unreliable, ~30/s |
//! | [`Msg::Input`] — its unacknowledged command batches, turn ack, local tick, stamps | guest → host | unreliable, ~30/s |
//! | [`Msg::Resend`] / [`Msg::Fill`] — gap fills | guest ↔ host | reliable |
//! | [`Msg::Hash`] — state hash every [`super::session::HASH_INTERVAL`] frames | guest → host | reliable |
//! | [`Msg::Desync`] — mismatch found, where to stop | host → all | reliable |
//! | [`Msg::Left`] — a guest left (to the guest itself: it was dropped) | host → all | reliable |
//! | [`Msg::Pings`] — every guest's round trip, for the player list | host → all | unreliable, 1/s |
//! | [`Msg::Bye`] — out of the match, back to the room (the connection stays) | guest → host, host → all | reliable |
//! | [`Msg::Chat`] — a chat line (the host checks the sender and relays it) | guest → host → all | reliable |
//! | [`Msg::Away`] — the sender's browser tab is hidden (or shown again) | guest → host → all | reliable |
//!
//! Both unreliable streams repeat what the other side has not
//! acknowledged, so a lost packet costs one packet interval, not a
//! retransmission timeout.

use serde::{Deserialize, Serialize};

use super::SimCommand;
use super::transport::{MAX_MESSAGE, PeerId};

/// Bumped whenever a message changes shape. Peers with another version
/// ignore each other's messages (the M2 handshake refuses them first).
/// 2 (M2): [`Msg::Pings`]. 3 (M3): [`Msg::Bye`], [`Msg::Chat`],
/// [`Msg::Away`].
pub const WIRE_VERSION: u16 = 3;

/// One player's commands of one turn, in issue order.
pub type PlayerCommands = (u8, Vec<SimCommand>);

/// One tick's worth of everybody's commands, as the host sequenced
/// them: the only thing any peer (the host included) applies.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub tick: u32,
    /// The input delay in force from this tick on: what local commands
    /// sealed after it are scheduled ahead by. Not sim state.
    pub delay: u8,
    /// Per player (ascending), that player's commands in issue order.
    pub frames: Vec<PlayerCommands>,
}

/// Consecutive turns from `first` (the ticks are implied).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRun {
    pub first: u32,
    /// Per turn: its delay and frames.
    pub turns: Vec<(u8, Vec<PlayerCommands>)>,
}

impl TurnRun {
    pub fn of(turns: &[Turn]) -> Self {
        Self {
            first: turns.first().map_or(0, |t| t.tick),
            turns: turns.iter().map(|t| (t.delay, t.frames.clone())).collect(),
        }
    }

    pub fn into_turns(self) -> impl Iterator<Item = Turn> {
        let first = self.first;
        self.turns
            .into_iter()
            .enumerate()
            .map(move |(i, (delay, frames))| Turn {
                tick: first + i as u32,
                delay,
                frames,
            })
    }
}

/// "I received your stamp `stamp` and held it `held_ms` before
/// answering": the round trip, measured on the stamping side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Echo {
    pub stamp: u32,
    pub held_ms: u16,
}

/// A guest's sealed commands for one tick, numbered per guest so the
/// host applies each exactly once and in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub seq: u32,
    /// The tick the guest scheduled it for (its tick + input delay).
    pub target: u32,
    pub commands: Vec<SimCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnPacket {
    /// The next batch seq the host expects from this guest: everything
    /// below it is sequenced.
    pub cmd_ack: u32,
    /// The newest turn cut.
    pub newest: Option<u32>,
    /// Host clock, ms.
    pub stamp: u32,
    pub echo: Option<Echo>,
    pub run: TurnRun,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputPacket {
    /// Every turn before this one is in hand.
    pub next_turn: u32,
    /// The tick this guest sealed its newest batch at (its sim clock).
    pub tick: u32,
    /// Guest clock, ms.
    pub stamp: u32,
    pub echo: Option<Echo>,
    pub batches: Vec<Batch>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Msg {
    Turns(TurnPacket),
    Input(InputPacket),
    /// Please resend the turns from `from` on.
    Resend {
        from: u32,
    },
    Fill(TurnRun),
    /// My state hash after sim frame `frame`.
    Hash {
        frame: u32,
        hash: u64,
    },
    /// The state hashes of `peers` differed from the host's at `frame`:
    /// every peer stops after `stop_at` ticks and writes a dump.
    Desync {
        frame: u32,
        stop_at: u32,
        peers: Vec<PeerId>,
    },
    /// `peer` left the match. Sent to `peer` itself when the host
    /// dropped it (timed out).
    Left {
        peer: PeerId,
    },
    // M2. New variants go at the end.
    /// The host's round trip to every peer of the roster, in roster
    /// order, ms (the host: 0; `u16::MAX`: gone, or not measured yet).
    Pings {
        ms: Vec<u16>,
    },
    // M3.
    /// The sender left the match but not the room ("Back to room"): its
    /// connection stays for the lobby, so this is its leave. From the
    /// host it ends the match, as the host leaving does.
    Bye,
    /// A chat line ([`super::chat`]): `from` is the sender, which the
    /// host checks before it relays the line to everyone.
    Chat {
        from: PeerId,
        text: String,
    },
    /// `peer`'s browser tab is hidden (`away`) or shown again; the host
    /// checks the sender and relays it (the player list shows it).
    Away {
        peer: PeerId,
        away: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u16,
    /// The match this belongs to: a stale packet from an earlier match
    /// in the same room is dropped.
    pub match_id: u64,
    pub msg: Msg,
}

/// Encode `msg`. `None` if the result would be empty or larger than
/// [`MAX_MESSAGE`] (the caller sends less).
pub fn encode(match_id: u64, msg: Msg) -> Option<Vec<u8>> {
    let env = Envelope {
        version: WIRE_VERSION,
        match_id,
        msg,
    };
    match postcard::to_allocvec(&env) {
        Ok(v) if !v.is_empty() && v.len() <= MAX_MESSAGE => Some(v),
        Ok(v) => {
            bevy::log::debug!("{} byte message not sent", v.len());
            None
        }
        Err(e) => {
            bevy::log::error!("encode failed: {e}");
            None
        }
    }
}

/// Decode a message of this match and version.
pub fn decode(match_id: u64, bytes: &[u8]) -> Option<Msg> {
    let env: Envelope = postcard::from_bytes(bytes).ok()?;
    (env.version == WIRE_VERSION && env.match_id == match_id).then_some(env.msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetId, Order, Units};

    #[test]
    fn a_turn_run_round_trips() {
        let turns: Vec<Turn> = (40..45)
            .map(|tick| Turn {
                tick,
                delay: 3,
                frames: if tick == 42 {
                    vec![(
                        1,
                        vec![SimCommand::Order {
                            units: Units::of([NetId(7), NetId(9)]),
                            order: Order::AttackUnit(NetId(3)),
                            queue: false,
                        }],
                    )]
                } else {
                    Vec::new()
                },
            })
            .collect();
        let msg = Msg::Turns(TurnPacket {
            cmd_ack: 3,
            newest: Some(44),
            stamp: 1234,
            echo: Some(Echo {
                stamp: 99,
                held_ms: 4,
            }),
            run: TurnRun::of(&turns),
        });
        let bytes = encode(5, msg.clone()).unwrap();
        // Five turns, one carrying an order: a few dozen bytes.
        assert!(bytes.len() < 48, "{} bytes", bytes.len());
        let Some(Msg::Turns(p)) = decode(5, &bytes) else {
            panic!("wrong message");
        };
        assert_eq!(p.run.into_turns().collect::<Vec<_>>(), turns);
        assert_eq!(decode(6, &bytes), None, "another match");
        assert_eq!(decode(5, &[0xff, 0xff, 0xff]), None);
    }
}
