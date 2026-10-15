//! Golden replays (`.kpreplay`, `docs/multiplayer-plan.md` M0.4).
//!
//! A replay is what a lockstep match is made of: the match setup (map,
//! seats, difficulty, seed), a compatibility stamp (sim version, unit
//! bundle and map content hashes) and the command log, every human
//! player's [`CommandFrame`]s by tick. The in-sim AI is part of the
//! simulation, so an all-AI replay carries no frames at all.
//!
//! - **Recording** ([`Replay::record`]): from the [`CommandBus`] log of
//!   applied frames, keeping the frames of the human seats (the AI's
//!   are re-derived by the AI itself on replay).
//! - **Playing** ([`play`]): the recorded setup through the headless
//!   harness, each frame delivered to the bus ([`CommandBus::deliver`],
//!   the lockstep transport's entry) right before the tick it is due
//!   on, applied by the same `apply_sim_commands` as everything else.
//!   Every tick's state hash goes into the trail ([`trail_text`]).
//!
//! The file: [`MAGIC`], the format version as u16 LE, then the postcard
//! encoding of [`Replay`]. A replay refuses to play against a different
//! sim version, unit bundle or map ([`Compat`]); its trail would be
//! meaningless.
//!
//! The harness runs `FixedUpdate` on the single-threaded executor (the
//! only one wasm has, and the one lockstep pins).

use std::path::PathBuf;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::state::Fnv;
use super::{AssetRoot, Executor, HashLog, Scenario, SimHarness, TickHash};
use crate::game_setup::PlayerSpec;
use crate::net::{CommandBus, CommandFrame, MatchRules};
use crate::units::components::Faction;

/// File magic of a `.kpreplay`.
pub const MAGIC: &[u8; 8] = b"kpreplay";
/// Layout of the file itself; bumped whenever [`Replay`] changes shape
/// or meaning. 2 (M1): seat `i` is owner `i` and [`Seat::ally_team`] its
/// ally team, where 1 gave every seat of an ally team one shared owner.
/// 3 (M2): the setup carries the host's [`MatchRules`].
pub const FORMAT_VERSION: u16 = 3;
/// Bumped by hand whenever the simulation changes on purpose in a way
/// that changes outcomes (then the golden logs are re-recorded; see
/// `net-experiments/m0-golden-replay.md`). The unit bundle and map
/// hashes catch data changes on their own.
///
/// 2 (M1, A): per-seat owners and ally teams, shared victory and
/// defeat, the sim no longer stops for the local outcome, `DetFloat`
/// max / min / clamp.
/// 3 (M1, B on top): commands are validated when they apply
/// (`net-experiments/m1-commands.md`): attacks need a weapon that can
/// hit the target, points must lie on the map (the AI's scattered
/// pushes near an edge lose those units' orders, as in Spring).
/// 4 (2026-10-15, the user's leaving rules): a leaver's units wait for
/// an ally's `Take` (an AI ally takes them itself) or, with the last
/// active owner of the ally team gone, blow up within 4 s; unit caps
/// never change.
/// 5 (M2): a `Take` hands over the leaver's packet buffer with its
/// units; a peer dropping out leaves play ([`SimCommand::Dropped`]);
/// pause, speed and cheats follow the setup's [`MatchRules`].
///
/// [`SimCommand::Dropped`]: crate::net::SimCommand::Dropped
pub const SIM_VERSION: u32 = 5;

/// One seat of the match: seat `i` is owner `i` (`TeamId(i)`,
/// `units::teams`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seat {
    /// Index into [`Faction::ALL`].
    pub faction: u8,
    /// Ally team (Spring's allyteam).
    pub ally_team: u8,
    pub ai: bool,
}

/// Everything that starts a match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchSetup {
    /// Map file stem (`assets/maps/<map>.kpmap`).
    pub map: String,
    pub seats: Vec<Seat>,
    /// AI difficulty, 1 Easy … 4 Extreme.
    pub difficulty: u8,
    /// The match seed (AI dice, procedural map content).
    pub seed: u64,
    /// No win / lose check.
    pub sandbox: bool,
    /// The host's rules (pause, speed range, cheats).
    pub rules: MatchRules,
}

impl MatchSetup {
    /// The game's form of this setup.
    pub fn game_setup(&self) -> crate::game_setup::GameSetup {
        let s = self.scenario(None);
        crate::game_setup::GameSetup {
            map: s.map,
            players: s.seats,
            difficulty: s.difficulty,
            demo: false,
            showcase: None,
            sandbox: s.sandbox,
            seed: s.seed,
            rules: s.rules,
        }
    }

    /// The harness scenario for this setup, loading `map_file`.
    pub fn scenario(&self, map_file: Option<PathBuf>) -> Scenario {
        Scenario {
            map: self.map.clone(),
            seats: self
                .seats
                .iter()
                .map(|s| PlayerSpec {
                    faction: Faction::ALL[s.faction as usize % Faction::ALL.len()],
                    ally_team: s.ally_team,
                    ai: s.ai,
                })
                .collect(),
            seed: self.seed,
            executor: Executor::Single,
            difficulty: self.difficulty,
            sandbox: self.sandbox,
            rules: self.rules,
            map_file,
        }
    }
}

/// What a replay needs to be meaningful: the same sim code (as far as
/// [`SIM_VERSION`] tells), the same unit data, the same map bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compat {
    pub sim_version: u32,
    /// FNV-1a of the embedded `assets/units.kpu`.
    pub units_hash: u64,
    /// FNV-1a of the baked `.kpmap`.
    pub map_hash: u64,
}

impl Compat {
    /// This build's stamp for these map bytes.
    pub fn current(map_bytes: &[u8]) -> Self {
        Self {
            sim_version: SIM_VERSION,
            units_hash: fnv(crate::units::content::bundle::bundle_bytes()),
            map_hash: fnv(map_bytes),
        }
    }

    /// The one-number form (what a lobby would compare).
    pub fn hash(&self) -> u64 {
        let mut h = Fnv::default();
        h.bytes(&self.sim_version.to_le_bytes());
        h.bytes(&self.units_hash.to_le_bytes());
        h.bytes(&self.map_hash.to_le_bytes());
        h.finish()
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = Fnv::default();
    h.bytes(bytes);
    h.finish()
}

/// A recorded match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replay {
    pub setup: MatchSetup,
    pub compat: Compat,
    /// Ticks the recording ran: the default replay length.
    pub ticks: u32,
    /// The human seats' frames, by tick then player.
    pub frames: Vec<CommandFrame>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("not a .kpreplay file")]
    Magic,
    #[error("replay format version {0}, this build reads {FORMAT_VERSION}")]
    Version(u16),
    #[error("corrupt replay: {0}")]
    Decode(#[from] postcard::Error),
    #[error(
        "replay recorded with sim version {}, unit bundle {:016x}, map {:016x}; \
         this build has sim version {}, unit bundle {:016x}, map {:016x} — re-record it",
        recorded.sim_version, recorded.units_hash, recorded.map_hash,
        current.sim_version, current.units_hash, current.map_hash
    )]
    Compat { recorded: Compat, current: Compat },
    #[error("command frame for tick {frame} found at tick {at}: the log is out of order")]
    Order { frame: u32, at: u32 },
    #[error("map {0}: {1}")]
    Map(String, String),
    #[error("{0}")]
    Check(String),
}

impl Replay {
    /// A replay of the match `setup` played on `map_bytes` for `ticks`
    /// ticks, from the frames the [`CommandBus`] applied (its `log`):
    /// every frame of an owner without AI, i.e. a human player's.
    pub fn record(
        setup: MatchSetup,
        map_bytes: &[u8],
        ticks: u32,
        applied: impl IntoIterator<Item = CommandFrame>,
    ) -> Self {
        let ai = crate::units::ai::AiSeats::from_setup(&setup.game_setup());
        let mut frames: Vec<CommandFrame> = applied
            .into_iter()
            .filter(|f| !ai.contains(f.player) && !f.commands.is_empty())
            .collect();
        frames.sort_by_key(|f| (f.tick, f.player));
        Self {
            setup,
            compat: Compat::current(map_bytes),
            ticks,
            frames,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend(FORMAT_VERSION.to_le_bytes());
        postcard::to_extend(self, out).expect("replays always serialize")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ReplayError> {
        let rest = bytes.strip_prefix(MAGIC).ok_or(ReplayError::Magic)?;
        let (version, body) = rest.split_at_checked(2).ok_or(ReplayError::Magic)?;
        let version = u16::from_le_bytes([version[0], version[1]]);
        if version != FORMAT_VERSION {
            return Err(ReplayError::Version(version));
        }
        Ok(postcard::from_bytes(body)?)
    }

    /// Commands in the log (all frames).
    pub fn commands(&self) -> usize {
        self.frames.iter().map(|f| f.commands.len()).sum()
    }
}

/// The map a replay plays on: its bytes (for the [`Compat`] check), the
/// asset root that serves them and the catalog entry that names them.
pub struct MapInput {
    pub bytes: Vec<u8>,
    pub assets: AssetRoot,
    pub file: PathBuf,
}

impl MapInput {
    /// `assets/maps/<map>.kpmap` of this checkout.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn checkout(map: &str) -> Result<Self, ReplayError> {
        let file = crate::paths::from_project_root("kernel-panic/assets/maps")
            .join(format!("{map}.kpmap"));
        let bytes = std::fs::read(&file).map_err(|e| {
            ReplayError::Map(
                map.to_string(),
                format!(
                    "{}: {e} (bake it: cargo run --release -p spring-map --bin bake-map -- \
                     kernel-panic/assets/maps/{map}.sd7)",
                    file.display()
                ),
            )
        })?;
        Ok(Self {
            bytes,
            assets: AssetRoot::checkout(),
            file,
        })
    }

    /// The map served from memory as `maps/<map>.kpmap` (wasm).
    pub fn in_memory(map: &str, bytes: Vec<u8>) -> Self {
        let file = PathBuf::from(format!("maps/{map}.kpmap"));
        let dir = bevy::asset::io::memory::Dir::default();
        dir.insert_asset(&file, bytes.clone());
        Self {
            bytes,
            assets: AssetRoot::Memory(dir),
            file,
        }
    }
}

/// Knobs of [`play`].
#[derive(Clone, Debug, Default)]
pub struct PlayOptions {
    /// Ticks to run (default: the recording's length).
    pub ticks: Option<u32>,
    /// Play even if the [`Compat`] stamp differs (for bisecting).
    pub ignore_compat: bool,
    /// 1-based ticks to also dump a readable per-unit state at.
    pub dump_at: Vec<usize>,
    /// Every this many ticks (0: never), shuffle the ECS storage order
    /// of every sim entity ([`shuffle_storage`]): a native stand-in for
    /// another machine's archetype history.
    pub shuffle_every: u32,
}

/// What a replay produced.
pub struct Played {
    /// The state hash after every tick, tick 1 first.
    pub hashes: Vec<TickHash>,
    /// Readable unit dumps at [`PlayOptions::dump_at`].
    pub dumps: Vec<(usize, Vec<String>)>,
    /// Wall time inside `FixedUpdate` (the sim), summed over all ticks.
    pub sim_secs: f64,
    /// Wall time of the whole frames (the sim plus `Update`'s UI).
    pub frame_secs: f64,
    /// Wall time to boot the app and load the map.
    pub load_secs: f64,
}

/// Play `replay` on `map`.
pub async fn play(
    replay: &Replay,
    map: MapInput,
    options: &PlayOptions,
) -> Result<Played, ReplayError> {
    let current = Compat::current(&map.bytes);
    if current != replay.compat && !options.ignore_compat {
        return Err(ReplayError::Compat {
            recorded: replay.compat,
            current,
        });
    }
    let ticks = options.ticks.unwrap_or(replay.ticks) as u64;
    let t0 = bevy::platform::time::Instant::now();
    let mut h = SimHarness::start(replay.setup.scenario(Some(map.file)), map.assets).await;
    let load_secs = t0.elapsed().as_secs_f64();
    {
        let mut log = h.world_mut().resource_mut::<HashLog>();
        log.enabled = true;
        log.dump_at = options.dump_at.clone();
    }
    let mut frames = replay.frames.iter().peekable();
    let mut shuffle_rng = 0x5eed_u32;
    let (mut sim_secs, mut frame_secs) = (0.0, 0.0);
    while h.tick < ticks {
        let mut bus = h.world_mut().resource_mut::<CommandBus>();
        let now = bus.tick;
        while let Some(frame) = frames.next_if(|f| f.tick <= now) {
            if frame.tick < now {
                return Err(ReplayError::Order {
                    frame: frame.tick,
                    at: now,
                });
            }
            bus.deliver(frame.clone());
        }
        if options.shuffle_every > 0 && h.tick.is_multiple_of(options.shuffle_every as u64) {
            shuffle_storage(h.world_mut(), &mut shuffle_rng);
        }
        h.step();
        sim_secs += h
            .take_tick_times()
            .iter()
            .map(|d| d.as_secs_f64())
            .sum::<f64>();
        frame_secs += h.last_frame.as_secs_f64();
        // Browsers kill scripts that never return to the event loop.
        #[cfg(target_arch = "wasm32")]
        if h.tick.is_multiple_of(256) {
            super::pause().await;
        }
    }
    let mut log = h.world_mut().resource_mut::<HashLog>();
    Ok(Played {
        hashes: std::mem::take(&mut log.ticks),
        dumps: std::mem::take(&mut log.dumps),
        sim_secs,
        frame_secs,
        load_secs,
    })
}

/// Scratch marker [`shuffle_storage`] moves entities through.
#[derive(Component)]
struct Shuffled;

/// Marker [`shuffle_storage`] leaves on a random half of the entities,
/// splitting each archetype in two.
#[derive(Component)]
struct Tagged;

/// Change the ECS storage order of the sim entities (units and
/// projectiles), as a machine with a different spawn / archetype history
/// would hold them, without touching the sim state:
///
/// - a random half gets [`Tagged`] (and the rest loses it): every
///   archetype splits, and the halves are queried in a new order;
/// - every entity then moves to a scratch archetype and back in a
///   shuffled order, which leaves it at a new row of its table.
///
/// Component values, their change ticks and their hooks are untouched
/// (only the markers' own are involved), so a sim that never depends on
/// query order produces the very same hash trail. That is the
/// query-order fuzz test (`--shuffle`, `determinism.yml`).
pub fn shuffle_storage(world: &mut World, rng: &mut u32) {
    let mut q = world.query_filtered::<Entity, Or<(
        With<crate::net::NetId>,
        With<crate::rendering::interpolation::SimPose>,
    )>>();
    let mut entities: Vec<Entity> = q.iter(world).collect();
    let shuffle = |v: &mut Vec<Entity>, rng: &mut u32| {
        for i in (1..v.len()).rev() {
            let j = crate::rng::xorshift32(rng) as usize % (i + 1);
            v.swap(i, j);
        }
    };
    shuffle(&mut entities, rng);
    for &e in &entities {
        let mut entity = world.entity_mut(e);
        if crate::rng::xorshift32(rng) & 1 == 0 {
            entity.insert(Tagged);
        } else {
            entity.remove::<Tagged>();
        }
        entity.insert(Shuffled);
    }
    shuffle(&mut entities, rng);
    for &e in &entities {
        world.entity_mut(e).remove::<Shuffled>();
    }
}

/// The trail: one line per tick, `<tick> <sim hash>` then the part
/// hashes (positions, health, orders, projectiles), the unit and
/// projectile counts and the NaN count, LF-terminated. Hashes are
/// 16-digit lower-case hex.
pub fn trail_text(hashes: &[TickHash]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(hashes.len() * 100);
    for (i, h) in hashes.iter().enumerate() {
        let _ = writeln!(
            out,
            "{} {:016x} {:016x} {:016x} {:016x} {:016x} {} {} {}",
            i + 1,
            h.sim,
            h.parts[0],
            h.parts[1],
            h.parts[2],
            h.parts[3],
            h.units,
            h.projectiles,
            h.nans
        );
    }
    out
}

/// The first line where two trails differ (1-based), if any.
pub fn first_difference(a: &str, b: &str) -> Option<usize> {
    let (mut la, mut lb) = (a.lines(), b.lines());
    let mut n = 0;
    loop {
        n += 1;
        match (la.next(), lb.next()) {
            (None, None) => return None,
            (x, y) if x != y => return Some(n),
            _ => {}
        }
    }
}

/// Log a summary of a finished replay (CLI and wasm alike).
pub fn summary(name: &str, replay: &Replay, played: &Played) -> String {
    let n = played.hashes.len().max(1) as f64;
    let last = played.hashes.last();
    let nans: u64 = played.hashes.iter().map(|h| h.nans as u64).sum();
    format!(
        "{name}: {} seats on {}, seed {:#x}, {} frames / {} commands; {} ticks, \
         final units {} projectiles {} hash {:016x}; NaNs {nans}; \
         load {:.1} s, sim {:.3} ms/tick, frame {:.3} ms/tick",
        replay.setup.seats.len(),
        replay.setup.map,
        replay.setup.seed,
        replay.frames.len(),
        replay.commands(),
        played.hashes.len(),
        last.map_or(0, |h| h.units),
        last.map_or(0, |h| h.projectiles),
        last.map_or(0, |h| h.sim),
        played.load_secs,
        played.sim_secs * 1000.0 / n,
        played.frame_secs * 1000.0 / n,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{SimCommand, Units};

    fn sample() -> Replay {
        let setup = MatchSetup {
            map: "Marble_Madness_Map".into(),
            seats: vec![
                Seat {
                    faction: 0,
                    ally_team: 0,
                    ai: false,
                },
                Seat {
                    faction: 1,
                    ally_team: 1,
                    ai: true,
                },
            ],
            difficulty: 2,
            seed: 7,
            sandbox: false,
            rules: MatchRules::default(),
        };
        let frame = |tick, player| CommandFrame {
            tick,
            player,
            commands: vec![SimCommand::Stop {
                units: Units::of([crate::net::NetId(3)]),
            }],
        };
        // The AI's frame (owner 1) is dropped, so is the empty one.
        let applied = vec![
            frame(9, 0),
            frame(4, 1),
            frame(4, 0),
            CommandFrame {
                tick: 5,
                player: 0,
                commands: vec![],
            },
        ];
        Replay::record(setup, b"map bytes", 100, applied)
    }

    #[test]
    fn record_keeps_human_frames_in_tick_order() {
        let r = sample();
        let ticks: Vec<(u32, u8)> = r.frames.iter().map(|f| (f.tick, f.player)).collect();
        assert_eq!(ticks, vec![(4, 0), (9, 0)]);
        assert_eq!(r.compat, Compat::current(b"map bytes"));
    }

    #[test]
    fn encode_round_trips_and_checks_the_header() {
        let r = sample();
        let bytes = r.encode();
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(Replay::decode(&bytes).unwrap(), r);
        let mut wrong = bytes.clone();
        wrong[8] = 99;
        assert!(matches!(
            Replay::decode(&wrong),
            Err(ReplayError::Version(99))
        ));
        assert!(matches!(
            Replay::decode(b"nonsense"),
            Err(ReplayError::Magic)
        ));
    }

    #[test]
    fn trail_lines_and_first_difference() {
        let h = TickHash {
            sim: 1,
            fx: 2,
            parts: [3, 4, 5, 6],
            units: 7,
            projectiles: 8,
            nans: 0,
        };
        let t = trail_text(&[h, h]);
        assert_eq!(
            t.lines().nth(1).unwrap(),
            "2 0000000000000001 0000000000000003 0000000000000004 0000000000000005 \
             0000000000000006 7 8 0"
        );
        assert_eq!(first_difference(&t, &t), None);
        let mut other = h;
        other.parts[2] = 9;
        assert_eq!(first_difference(&t, &trail_text(&[h, other])), Some(2));
        assert_eq!(first_difference(&t, &trail_text(&[h])), Some(2));
    }

    /// The query-order fuzz on the golden logs: every replay again with
    /// the ECS storage order reshuffled every tick ([`shuffle_storage`])
    /// must leave the same trail. Needs the baked maps; `KP_FUZZ_TICKS`
    /// (default 3000) per log, `KP_FUZZ_LOGS=a,b` to pick logs. CI runs
    /// the full-length version (`determinism.yml`, `--shuffle 1`).
    ///
    /// ```text
    /// cargo test --release -p kernel-panic headless::replay::tests::query_order_fuzz -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "replays the golden logs on the baked maps; --ignored"]
    fn query_order_fuzz() {
        let ticks: u32 = std::env::var("KP_FUZZ_TICKS")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(3000);
        let only = std::env::var("KP_FUZZ_LOGS").ok();
        let dir = crate::paths::from_project_root("kernel-panic/tests/golden");
        let mut logs: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "kpreplay"))
            .filter(|p| {
                only.as_deref().is_none_or(|o| {
                    let stem = p.file_stem().unwrap().to_string_lossy();
                    o.split(',').any(|n| n == stem)
                })
            })
            .collect();
        logs.sort();
        assert!(!logs.is_empty(), "no golden logs in {}", dir.display());
        for log in logs {
            let replay = Replay::decode(&std::fs::read(&log).unwrap()).unwrap();
            let run = |shuffle_every| {
                let options = PlayOptions {
                    ticks: Some(ticks.min(replay.ticks)),
                    shuffle_every,
                    ..Default::default()
                };
                let map = MapInput::checkout(&replay.setup.map).unwrap();
                trail_text(
                    &bevy::tasks::block_on(play(&replay, map, &options))
                        .unwrap()
                        .hashes,
                )
            };
            let (plain, shuffled) = (run(0), run(1));
            let name = log.file_stem().unwrap().to_string_lossy().into_owned();
            if let Some(line) = first_difference(&plain, &shuffled) {
                panic!(
                    "{name}: the sim depends on ECS query order from tick {line}:\n  plain    {}\n  shuffled {}",
                    plain.lines().nth(line - 1).unwrap_or("<end>"),
                    shuffled.lines().nth(line - 1).unwrap_or("<end>"),
                );
            }
            eprintln!(
                "{name}: {} ticks, same trail with the storage shuffled",
                plain.lines().count()
            );
        }
    }
}
