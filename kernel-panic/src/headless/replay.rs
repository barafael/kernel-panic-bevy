//! Golden replays (`.kpreplay`, `docs/multiplayer-plan.md` M0.4).
//!
//! A replay is what a lockstep match is made of: the match setup (map,
//! seats, difficulty, seed), a compatibility stamp (sim version, unit
//! bundle and map content hashes) and the command log, every human
//! player's `CommandFrame`s by tick. The in-sim AI is part of the
//! simulation, so an all-AI replay carries no frames at all.
//!
//! - **Recording** ([`Replay::record`]): from the `CommandBus` log of
//!   applied frames, keeping the frames of the human seats (the AI's
//!   are re-derived by the AI itself on replay). The game records every
//!   match from its turns instead (`net::replay_log`).
//! - **Playing** ([`play`]): the recorded setup through the headless
//!   harness, the log played back as turns by a replay session
//!   ([`NetSession::replay`], as the game's replay viewer plays a file),
//!   one per tick, applied by the same `apply_sim_commands` as
//!   everything else. Every tick's state hash goes into the trail
//!   ([`trail_text`]).
//!
//! The file: `MAGIC`, the format version as u16 LE, then the postcard
//! encoding of [`Replay`]. A replay refuses to play against a different
//! sim version, unit bundle or map ([`Compat`]); its trail would be
//! meaningless.
//!
//! The harness runs `FixedUpdate` on the single-threaded executor (the
//! only one wasm has, and the one lockstep pins).

use std::path::PathBuf;

use bevy::prelude::*;

use super::{AssetRoot, Executor, HashLog, Scenario, SimHarness, TickHash};
use crate::game_setup::PlayerSpec;
use crate::net::driver::NetSession;

/// The sim version and the compatibility stamp live with the lobby,
/// which compares them before a networked match ([`crate::net::compat`]).
pub use crate::net::compat::Compat;
/// The file format lives with the game, which records every match
/// (`net::replay_log`); the golden logs are such files.
pub use crate::net::replay_log::{MatchSetup, Replay, ReplayError, Seat};

impl MatchSetup {
    /// The harness scenario for this setup, loading `map_file`.
    pub fn scenario(&self, map_file: Option<PathBuf>) -> Scenario {
        Scenario {
            map: self.map.clone(),
            seats: self.seats.iter().map(PlayerSpec::from).collect(),
            seed: self.seed,
            executor: Executor::Single,
            difficulty: self.difficulty,
            sandbox: self.sandbox,
            rules: self.rules,
            map_file,
        }
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
        let bytes = std::fs::read(&file)
            .map_err(|e| ReplayError::Map(map.to_string(), format!("{}: {e}", file.display())))?;
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
    if let Some(w) = replay.frames.windows(2).find(|w| w[1].tick < w[0].tick) {
        return Err(ReplayError::Order {
            frame: w[1].tick,
            at: w[0].tick,
        });
    }
    // The log as turns, one per tick (the clock below runs one tick a
    // frame: no catch-up).
    let net = NetSession::replay(&replay.frames, ticks as u32);
    h.world_mut().insert_resource(net);
    let mut shuffle_rng = 0x5eed_u32;
    let (mut sim_secs, mut frame_secs) = (0.0, 0.0);
    while h.tick < ticks {
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
    use crate::net::replay_log::MAGIC;
    use crate::net::{CommandFrame, MatchRules, SimCommand, Units};

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
    /// must leave the same trail. `KP_FUZZ_TICKS`
    /// (default 3000) per log, `KP_FUZZ_LOGS=a,b` to pick logs. CI runs
    /// the full-length version (`determinism.yml`, `--shuffle 1`).
    ///
    /// ```text
    /// cargo test --release -p kernel-panic headless::replay::tests::query_order_fuzz -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "replays the golden logs; --ignored"]
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
