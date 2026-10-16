//! What the net tests share: the stand-in sim of the protocol-level
//! tests ([`Fake`]), the trail comparison ([`assert_agree`]) and the
//! environment knobs of the long runs ([`env`]).

use std::collections::BTreeMap;

use super::driver::{GameSpeed, TickRunner};
use super::session::{Desync, Session};
use super::state::Fnv;
use super::transport::PeerId;
use super::wire::Turn;
use super::{CommandFrame, NetId, SimCommand, Units};

/// `k` from the environment, `d` if unset or unparsable.
pub fn env<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

/// Every trail (peer, per-tick hashes; the host's first) equals the
/// host's over their common prefix, at least `min` ticks long; the
/// prefix length.
pub fn assert_agree<T: AsRef<[u64]>>(
    trails: impl IntoIterator<Item = (PeerId, T)>,
    min: usize,
) -> usize {
    let mut trails = trails.into_iter();
    let (_, host) = trails.next().expect("the host's trail");
    let host = host.as_ref();
    let mut shortest = host.len();
    for (id, t) in trails {
        let t = t.as_ref();
        shortest = shortest.min(t.len());
        let n = t.len().min(host.len());
        if let Some(k) = (0..n).find(|&k| t[k] != host[k]) {
            panic!("{id} diverges from the host at tick {}", k + 1);
        }
    }
    assert!(shortest >= min, "only {shortest} ticks in common");
    shortest
}

/// The order with command id `id`: a `Stop` of unit `id`.
pub fn order(id: u32) -> SimCommand {
    SimCommand::Stop {
        units: Units::of([NetId(id)]),
    }
}

/// The command id of an [`order`].
pub fn id_of(cmd: &SimCommand) -> Option<u32> {
    match cmd {
        SimCommand::Stop {
            units: Units::List(l),
        } => l.ids().first().map(|n| n.0),
        _ => None,
    }
}

/// A stand-in sim: its state folds in every applied turn, so any
/// difference in the turn stream shows in the trail. It records the
/// orders it applies and the leaves and returns; `Pause` pauses it.
#[derive(Default)]
pub struct Fake {
    pub player: Option<u8>,
    pub tick: u32,
    pub state: Fnv,
    pub trail: Vec<u64>,
    pub staged: Vec<SimCommand>,
    pub speed: GameSpeed,
    /// (local tick at issue, command id)
    pub issued: Vec<(u32, u32)>,
    /// command id → tick applied
    pub applied: BTreeMap<u32, u32>,
    /// Fold in one more byte after this tick (a forced desync).
    pub perturb_after: Option<u32>,
    /// Desync dumps written.
    pub dumps: u32,
    /// A desync dump panics: none is expected.
    pub strict: bool,
    /// (tick, player, returned) of every `Dropped` / `Returned`.
    pub leaves: Vec<(u32, u8, bool)>,
}

impl Fake {
    /// Stage [`order`]`(id)`, issued at the current tick.
    pub fn issue(&mut self, id: u32) {
        self.issued.push((self.tick, id));
        self.staged.push(order(id));
    }

    /// (tick, player) of every `Dropped` applied.
    pub fn dropped(&self) -> Vec<(u32, u8)> {
        self.leaves
            .iter()
            .filter(|(_, _, returned)| !returned)
            .map(|&(tick, player, _)| (tick, player))
            .collect()
    }
}

impl TickRunner for Fake {
    fn speed(&self) -> GameSpeed {
        self.speed
    }

    fn seal(&mut self, delay: u32) -> Vec<CommandFrame> {
        match self.player {
            Some(player) if !self.staged.is_empty() => vec![CommandFrame {
                tick: self.tick + delay,
                player,
                commands: std::mem::take(&mut self.staged),
            }],
            _ => {
                self.staged.clear();
                Vec::new()
            }
        }
    }

    fn run(&mut self, turn: &Turn) {
        assert_eq!(turn.tick, self.tick, "turns in order");
        self.state.bytes(&turn.tick.to_le_bytes());
        self.state
            .bytes(&postcard::to_allocvec(&turn.frames).unwrap());
        for (player, cmds) in &turn.frames {
            for c in cmds {
                if let Some(id) = id_of(c) {
                    assert!(
                        self.applied.insert(id, turn.tick).is_none(),
                        "command {id} applied twice"
                    );
                }
                match c {
                    SimCommand::Pause { on } => self.speed.paused = *on,
                    SimCommand::Dropped => self.leaves.push((turn.tick, *player, false)),
                    SimCommand::Returned => self.leaves.push((turn.tick, *player, true)),
                    _ => {}
                }
            }
        }
        self.tick += 1;
        if self.perturb_after == Some(self.tick) {
            self.state.bytes(&[1]);
        }
        self.trail.push(self.state.finish());
    }

    fn hash(&mut self) -> u64 {
        self.state.finish()
    }

    fn dump(&mut self, _session: &Session, _desync: &Desync) -> String {
        assert!(!self.strict, "no desync expected");
        self.dumps += 1;
        "fake".into()
    }
}
