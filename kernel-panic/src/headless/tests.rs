//! The determinism experiments and sim measurements. All `#[ignore]`d
//! (they load real maps and run thousands of ticks); run one at a
//! time, release for timings:
//!
//! ```text
//! cargo test --release -p kernel-panic headless::tests::determinism -- --ignored --nocapture
//! ```
//!
//! Knobs (environment): `KP_NET_MAP` (default `Speed_Balls_16_Way`),
//! `KP_NET_TEAMS` × `KP_NET_PER_TEAM` (default 2 × 4 = a 4v4),
//! `KP_NET_SEED`, `KP_NET_TICKS`, `KP_NET_DIFFICULTY`,
//! `KP_NET_EXEC=single|multi`, `KP_NET_PACING=uneven|zeros|multi|even`
//! (pacing of the second `determinism` run), `KP_NET_ID_SHIFT=<n>`
//! (shift the second run's entity ids), `KP_NET_LOCAL=<team>` (the
//! second run watches as that team: its `LocalTeam`, the seats stay all
//! AI), `KP_NET_SELECT=1` (the second run's player keeps selecting
//! units), `KP_NET_RUNS=1` (first run
//! only), `KP_NET_TRACE=<file>` (per-tick hashes of the first run, for
//! cross-process diffs), `KP_NET_DUMP=<ticks>` + `KP_NET_DUMP_DIR`
//! (readable unit dumps), `KP_NET_CSV=<file>` (per-tick numbers of
//! `match_stats`), `KP_NET_WARMUP` / `KP_NET_WARMUP_MAP` /
//! `KP_NET_RESET_FIXED` (`rematch`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;

use super::*;
use crate::net::CommandBus;
use crate::net::test_support::env;
use crate::sim::DetFloat;

fn scenario_from_env() -> Scenario {
    let map: String = env("KP_NET_MAP", "Speed_Balls_16_Way".to_string());
    let mut s = Scenario::ai_match(
        &map,
        env("KP_NET_TEAMS", 2),
        env("KP_NET_PER_TEAM", 4),
        env("KP_NET_SEED", 0x5eed_u64),
    );
    s.difficulty = env("KP_NET_DIFFICULTY", 2);
    s.executor = match env("KP_NET_EXEC", "single".to_string()).as_str() {
        "multi" => Executor::Multi,
        _ => Executor::Single,
    };
    s
}

/// Ticks per frame of the uneven pacing (a jittery renderer: dropped
/// frames, catch-up bursts up to the three-tick cap).
const UNEVEN: [u32; 12] = [1, 0, 2, 1, 3, 0, 0, 1, 2, 1, 0, 1];

/// Frame patterns for `KP_NET_PACING`: `uneven` (above), `zeros` (one
/// tick per frame with tickless frames between: only `Update` runs
/// extra), `multi` (two or three ticks per frame, no tickless frames:
/// only the per-frame work between ticks is missing).
fn pacing_pattern(name: &str) -> Option<&'static [u32]> {
    match name {
        "uneven" => Some(&UNEVEN),
        "zeros" => Some(&[1, 0, 0, 1, 0]),
        "multi" => Some(&[2, 3, 2, 1]),
        // Same pacing as the first run (for `KP_NET_ID_SHIFT`).
        "even" => Some(&[1]),
        _ => None,
    }
}

/// Run `ticks` ticks, logging every tick's hash.
fn run_hashes(scenario: Scenario, ticks: u64, pattern: Option<&[u32]>) -> Vec<TickHash> {
    let mut h = SimHarness::new(scenario);
    h.world_mut().resource_mut::<HashLog>().enabled = true;
    // `KP_NET_DUMP=510,511`: readable unit dumps at those ticks, to
    // `KP_NET_DUMP_DIR/<run>_<tick>.txt`.
    let dump_at: Vec<usize> = env("KP_NET_DUMP", String::new())
        .split(',')
        .filter_map(|t| t.trim().parse().ok())
        .collect();
    h.world_mut().resource_mut::<HashLog>().dump_at = dump_at;
    // `KP_NET_ID_SHIFT=n` (paced run only): spawn n empty entities, and
    // despawn every other one, before the first tick — every later
    // spawn gets a different `Entity` (index and free-list reuse) while
    // nothing else changes. Divergence then means the sim depends on
    // entity ids.
    if pattern.is_some() {
        // `KP_NET_LOCAL=t`: this peer watches as team `t` (fog, cloak
        // fades, sounds follow it); the sim must not notice.
        if let Ok(t) = std::env::var("KP_NET_LOCAL") {
            let t: u8 = t.parse().expect("KP_NET_LOCAL: a team number");
            h.world_mut()
                .insert_resource(crate::units::player::LocalTeam(t));
        }
        let n: usize = env("KP_NET_ID_SHIFT", 0);
        let ids: Vec<Entity> = (0..n).map(|_| h.world_mut().spawn_empty().id()).collect();
        for e in ids.into_iter().step_by(2) {
            h.world_mut().despawn(e);
        }
    }
    // `KP_NET_SELECT=1` (paced run): this peer's player keeps selecting
    // a different seventh of the units every frame — UI markers on sim
    // entities, which move them between ECS tables.
    let select = pattern.is_some() && env("KP_NET_SELECT", 0u8) == 1;
    let mut i = 0;
    while h.tick < ticks {
        if let Some(pattern) = pattern {
            if select {
                reselect(h.world_mut(), i);
            }
            let want = pattern[i % pattern.len()].min((ticks - h.tick) as u32);
            h.frame(want);
            i += 1;
        } else {
            h.step();
        }
    }
    if let Ok(dir) = std::env::var("KP_NET_DUMP_DIR") {
        let tag = if pattern.is_some() { "paced" } else { "even" };
        for (t, lines) in std::mem::take(&mut h.world_mut().resource_mut::<HashLog>().dumps) {
            std::fs::write(format!("{dir}/{tag}_{t}.txt"), lines.join("\n")).unwrap();
        }
    }
    std::mem::take(&mut h.world_mut().resource_mut::<HashLog>().ticks)
}

/// Select the units whose `NetId` is `frame` mod 7, deselect the rest.
fn reselect(world: &mut World, frame: usize) {
    use crate::interaction::selection::Selected;
    use crate::net::NetId;
    let units: Vec<(Entity, u32, bool)> = world
        .query::<(Entity, &NetId, Has<Selected>)>()
        .iter(world)
        .map(|(e, id, sel)| (e, id.0, sel))
        .collect();
    for (e, id, selected) in units {
        let want = id as usize % 7 == frame % 7;
        if want && !selected {
            world.entity_mut(e).insert(Selected);
        } else if !want && selected {
            world.entity_mut(e).remove::<Selected>();
        }
    }
}

fn first_divergence(a: &[TickHash], b: &[TickHash], f: impl Fn(&TickHash) -> u64) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| f(x) != f(y))
}

/// Same seeded scenario twice in this process (second one optionally
/// with uneven frame pacing); prints the first divergent tick of each
/// hash. With `KP_NET_TRACE` the first run's hashes go to a file, so
/// two processes can be compared with `diff`.
#[test]
#[ignore = "runs two full matches; --ignored"]
fn determinism() {
    let scenario = scenario_from_env();
    let ticks: u64 = env("KP_NET_TICKS", 3000);
    let pacing = env("KP_NET_PACING", String::new());
    let pattern = pacing_pattern(&pacing);
    eprintln!(
        "determinism: {} seats on {}, {:?}, {} ticks, second run {}",
        scenario.seats.len(),
        scenario.map,
        scenario.executor,
        ticks,
        if pattern.is_some() {
            pacing.as_str()
        } else {
            "same pacing"
        }
    );
    let a = run_hashes(scenario.clone(), ticks, None);
    if let Ok(path) = std::env::var("KP_NET_TRACE") {
        let mut f = std::fs::File::create(&path).unwrap();
        for (t, x) in a.iter().enumerate() {
            writeln!(
                f,
                "{} {:016x} {:016x} {:016x} {:016x} {:016x} {} {}",
                t + 1,
                x.sim,
                x.parts[0],
                x.parts[1],
                x.parts[2],
                x.fx,
                x.units,
                x.projectiles
            )
            .unwrap();
        }
        eprintln!("trace written to {path}");
    }
    if env("KP_NET_RUNS", 2) < 2 {
        return;
    }
    let b = run_hashes(scenario, ticks, pattern);
    let report = |name: &str, d: Option<usize>| match d {
        Some(t) => eprintln!(
            "  {name:>9}: first divergence at tick {} (units {} vs {})",
            t + 1,
            a[t].units,
            b[t].units
        ),
        None => eprintln!(
            "  {name:>9}: identical for all {} ticks",
            a.len().min(b.len())
        ),
    };
    report("sim", first_divergence(&a, &b, |h| h.sim));
    report("positions", first_divergence(&a, &b, |h| h.parts[0]));
    report("health", first_divergence(&a, &b, |h| h.parts[1]));
    report("orders", first_divergence(&a, &b, |h| h.parts[2]));
    report("fx", first_divergence(&a, &b, |h| h.fx));
    let last = a.len().min(b.len()) - 1;
    eprintln!(
        "  final units {} vs {}, projectiles {} vs {}",
        a[last].units, b[last].units, a[last].projectiles, b[last].projectiles
    );
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let i = ((v.len() - 1) as f64 * p).round() as usize;
    v[i]
}

/// The variant name of a command (`Order`, `PerUnit`, `Factory`, …).
fn command_name(cmd: &crate::net::SimCommand) -> String {
    let dbg = format!("{cmd:?}");
    dbg.split([' ', '{', '('])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A long AI match: entity counts, the AI's command volume (the frames
/// the command bus applies, sized as postcard `CommandFrame`s — what a
/// host-run AI would send) and the sim cost per tick.
#[test]
#[ignore = "runs a long match; --ignored, --release for timings"]
fn match_stats() {
    let scenario = scenario_from_env();
    let ticks: u64 = env("KP_NET_TICKS", 30 * 60 * 8);
    let players = scenario.seats.len();
    let teams: HashSet<u8> = scenario.seats.iter().map(|s| s.ally_team).collect();
    eprintln!(
        "match_stats: {} seats ({} teams) on {}, {:?}, {} ticks ({:.1} game min)",
        players,
        teams.len(),
        scenario.map,
        scenario.executor,
        ticks,
        ticks as f64 / 30.0 / 60.0
    );
    let mut csv = std::env::var("KP_NET_CSV")
        .ok()
        .map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));
    if let Some(f) = csv.as_mut() {
        writeln!(
            f,
            "tick,sim_ms,frame_ms,units,projectiles,entities,cmds,frame_bytes,census"
        )
        .unwrap();
    }

    let mut h = SimHarness::new(scenario);
    // The bus is match-scoped: log from the match's own bus.
    h.world_mut().resource_mut::<CommandBus>().log = Some(Vec::new());
    let mut sim_ms: Vec<f64> = Vec::new();
    let mut frame_ms: Vec<f64> = Vec::new();
    let mut peak: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut peak_units = (0usize, 0u64);
    let mut peak_entities = 0usize;
    let mut cmd_total = 0usize;
    let mut bytes_total = 0usize;
    let mut cmd_by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut frame_max = (0usize, 0usize, 0u64);
    let mut ticks_nonempty = 0usize;
    let mut cmd_per_team: BTreeMap<u8, usize> = BTreeMap::new();
    let mut units_series: Vec<(u64, usize, usize, f64)> = Vec::new();

    while h.tick < ticks {
        h.step();
        for d in h.take_tick_times() {
            sim_ms.push(d.as_secs_f64() * 1000.0);
        }
        frame_ms.push(h.last_frame.as_secs_f64() * 1000.0);
        let tick = h.tick;
        let c = state::capture(h.world_mut());
        for (cat, n) in &c.census {
            let p = peak.entry(cat).or_default();
            *p = (*p).max(*n);
        }
        if c.units > peak_units.0 {
            peak_units = (c.units, tick);
        }
        peak_entities = peak_entities.max(c.total_entities);

        // --- commands: the frames applied this tick -------------------
        let frames = h
            .world_mut()
            .resource_mut::<CommandBus>()
            .log
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let (mut cmds_tick, mut bytes_tick) = (0, 0);
        for frame in &frames {
            let bytes = frame.encode().len();
            cmds_tick += frame.commands.len();
            bytes_tick += bytes;
            if frame.commands.len() > frame_max.0 {
                frame_max = (frame.commands.len(), bytes, tick);
            }
            *cmd_per_team.entry(frame.player).or_default() += frame.commands.len();
            for cmd in &frame.commands {
                *cmd_by_kind.entry(command_name(cmd)).or_default() += 1;
            }
        }
        if cmds_tick > 0 {
            ticks_nonempty += 1;
        }
        cmd_total += cmds_tick;
        bytes_total += bytes_tick;

        if tick.is_multiple_of(300) {
            let s = sim_ms.last().copied().unwrap_or(0.0);
            units_series.push((tick, c.units, c.projectiles, s));
            eprintln!(
                "  t={:>5} ({:>4.1} min) units {:>4} proj {:>4} entities {:>6} | sim {:>6.2} ms frame {:>6.2} ms",
                tick,
                tick as f64 / 1800.0,
                c.units,
                c.projectiles,
                c.total_entities,
                s,
                h.last_frame.as_secs_f64() * 1000.0
            );
        }
        if let Some(f) = csv.as_mut() {
            let census: Vec<String> = c.census.iter().map(|(k, v)| format!("{k}={v}")).collect();
            writeln!(
                f,
                "{},{:.3},{:.3},{},{},{},{},{},{}",
                tick,
                sim_ms.last().copied().unwrap_or(0.0),
                h.last_frame.as_secs_f64() * 1000.0,
                c.units,
                c.projectiles,
                c.total_entities,
                cmds_tick,
                bytes_tick,
                census.join(" ")
            )
            .unwrap();
        }
    }

    // --- report -------------------------------------------------------
    let secs = ticks as f64 / 30.0;
    eprintln!("\n=== entity peaks ===");
    eprintln!(
        "units peak {} at tick {} ({:.1} min)",
        peak_units.0,
        peak_units.1,
        peak_units.1 as f64 / 1800.0
    );
    eprintln!("entities peak {peak_entities}");
    for (cat, n) in &peak {
        eprintln!("  {cat:>15}: peak {n}");
    }
    eprintln!("\n=== AI commands (applied CommandFrames, postcard) ===");
    eprintln!(
        "total {cmd_total} over {secs:.0} s = {:.2}/s all players, {:.2}/s per team, {:.2}/s per seat",
        cmd_total as f64 / secs,
        cmd_total as f64 / secs / teams.len() as f64,
        cmd_total as f64 / secs / players as f64
    );
    eprintln!(
        "frame bytes {} = {:.1} B/s all players ({:.1} B/s per team); ticks with any command: {} of {}",
        bytes_total,
        bytes_total as f64 / secs,
        bytes_total as f64 / secs / teams.len() as f64,
        ticks_nonempty,
        ticks
    );
    eprintln!(
        "largest frame: {} commands, {} B (tick {})",
        frame_max.0, frame_max.1, frame_max.2
    );
    for (k, n) in &cmd_by_kind {
        eprintln!("  {k:>16}: {n}");
    }
    eprintln!("  per team: {cmd_per_team:?}");
    eprintln!("\n=== sim cost (FixedFirst..FixedLast wall time) ===");
    let late_from = sim_ms.len().saturating_sub(30 * 120);
    let mut all = sim_ms.clone();
    let mut late = sim_ms[late_from..].to_vec();
    eprintln!(
        "all ticks: median {:.2} ms p95 {:.2} ms p99 {:.2} ms max {:.2} ms",
        pct(&mut all, 0.5),
        pct(&mut all, 0.95),
        pct(&mut all, 0.99),
        pct(&mut all, 1.0)
    );
    eprintln!(
        "last 2 min: median {:.2} ms p95 {:.2} ms p99 {:.2} ms max {:.2} ms",
        pct(&mut late, 0.5),
        pct(&mut late, 0.95),
        pct(&mut late, 0.99),
        pct(&mut late, 1.0)
    );
    eprintln!(
        "whole headless frame: median {:.2} ms p95 {:.2} ms",
        pct(&mut frame_ms, 0.5),
        pct(&mut frame_ms, 0.95)
    );
    eprintln!("\n=== units over time (every 10 s): tick units proj sim_ms ===");
    for (t, u, p, s) in units_series {
        eprint!("{t}:{u}/{p}/{s:.1} ");
    }
    eprintln!();
}

/// Boots a 4v4 and runs ten seconds: the harness works headless.
#[test]
#[ignore = "loads a real map; --ignored"]
fn smoke() {
    let mut h = SimHarness::new(scenario_from_env());
    for _ in 0..300 {
        h.step();
    }
    let c = state::capture(h.world_mut());
    eprintln!(
        "tick {} units {} census {:?} hash {:016x}",
        h.tick, c.units, c.census, c.sim_hash
    );
    assert!(c.units >= 8);
}

/// The game's systems outside the fixed sim (they run once per render
/// frame, not per tick): candidates for frame-rate-dependent sim state.
#[test]
#[ignore = "boots the app; --ignored"]
fn frame_systems() {
    let mut h = SimHarness::new(scenario_from_env());
    h.step();
    let world = h.world_mut();
    let schedules = world.resource::<Schedules>();
    for (label, schedule) in schedules.iter() {
        let name = format!("{label:?}");
        let Ok(systems) = schedule.systems() else {
            continue;
        };
        let ours: Vec<String> = systems
            .map(|(_, s)| s.name().to_string())
            .filter(|n| n.starts_with("kernel_panic"))
            .collect();
        if !ours.is_empty() {
            eprintln!("{name}: {}", ours.join(", "));
        }
    }
}

/// A rematch in the same app versus a fresh app: the same seeded
/// scenario, but the rematch runs after `KP_NET_WARMUP` ticks (default
/// 900) of another match (`KP_NET_RESET_FIXED=1` also restarts the
/// fixed clock at the rematch). Divergence = sim state that outlives a
/// match (system `Local`s, the app-relative fixed clock), which lockstep
/// peers with different histories (the menu's attract demo, earlier
/// games) would not share.
#[test]
#[ignore = "runs three matches; --ignored"]
fn rematch() {
    let scenario = scenario_from_env();
    let ticks: u64 = env("KP_NET_TICKS", 3000);
    let warmup: u64 = env("KP_NET_WARMUP", 900);
    let reset = env("KP_NET_RESET_FIXED", 0u8) == 1;
    let a = run_hashes(scenario.clone(), ticks, None);
    let mut warm = scenario.clone();
    warm.seed ^= 0xdead_beef;
    // `KP_NET_WARMUP_MAP`: the earlier match on another map (another
    // archetype history, like the menu's attract demo would leave).
    if let Ok(map) = std::env::var("KP_NET_WARMUP_MAP") {
        warm.map = map;
    }
    let mut h = SimHarness::new(warm);
    for _ in 0..warmup {
        h.step();
    }
    h.rematch(scenario, reset);
    h.world_mut().resource_mut::<HashLog>().enabled = true;
    while h.tick < ticks {
        h.step();
    }
    let b = std::mem::take(&mut h.world_mut().resource_mut::<HashLog>().ticks);
    eprintln!("rematch after {warmup} ticks of another match, fixed clock reset: {reset}");
    for (name, d) in [
        ("sim", first_divergence(&a, &b, |h| h.sim)),
        ("positions", first_divergence(&a, &b, |h| h.parts[0])),
        ("health", first_divergence(&a, &b, |h| h.parts[1])),
        ("orders", first_divergence(&a, &b, |h| h.parts[2])),
    ] {
        match d {
            Some(t) => eprintln!("  {name:>9}: first divergence at tick {}", t + 1),
            None => eprintln!(
                "  {name:>9}: identical for all {} ticks",
                a.len().min(b.len())
            ),
        }
    }
}

/// How an all-AI alliance match ended ([`play_to_the_end`]).
struct Ending {
    /// Tick each owner died on.
    deaths: BTreeMap<u8, u64>,
    decided_at: u64,
    winners: Vec<u8>,
    /// The local player's (owner 0's) result panel at the end.
    shown: crate::units::lifecycle::game_over::GameState,
    hashes: Vec<TickHash>,
}

/// Play `scenario` until the match is decided (at most `max` ticks),
/// watching as owner 0 (its result is UI only), then `after` more
/// ticks. Asserts the rules on the way: shared defeat, a leaver's units
/// taken by an AI ally or blown up within 4 s with its ally team, caps
/// never changing, the end at the first census after the last
/// explosion, the sim never stopping.
fn play_to_the_end(scenario: Scenario, max: u64, after: u64) -> Ending {
    use crate::units::lifecycle::bookkeeping::{MAX_UNITS_PER_TEAM, TeamUnitCounts};
    use crate::units::lifecycle::game_over::GameState;
    use crate::units::teams::{Alliances, TeamFates};

    let mut h = SimHarness::new(scenario);
    h.world_mut().resource_mut::<HashLog>().enabled = true;
    h.world_mut()
        .insert_resource(crate::units::player::LocalTeam(0));
    let alliances = h.world_mut().resource::<Alliances>().clone();
    let mut deaths: BTreeMap<u8, u64> = BTreeMap::new();
    let mut decided: Option<(u64, Vec<u8>)> = None;
    let mut zero_seen: BTreeMap<Entity, u64> = BTreeMap::new();
    while decided.is_none() || h.tick < decided.as_ref().unwrap().0 + after {
        h.step();
        assert!(h.tick <= max + after, "undecided after {max} ticks");
        let tick = h.tick;
        let world = h.world_mut();
        assert_eq!(
            world.resource::<crate::sim::SimTick>().0,
            tick,
            "the sim runs every tick, decided or not"
        );
        let fates = world.resource::<TeamFates>().clone();
        for owner in fates.dead() {
            deaths.entry(owner).or_insert(tick);
        }
        if decided.is_none()
            && let Some(winners) = fates.winners()
        {
            decided = Some((tick, winners.to_vec()));
        }
        // Nothing stands at 0 HP: `death_system` takes every unit the
        // tick after its HP drops (the game-over sweep included).
        {
            use crate::units::components::{Health, TeamId, UnitType};
            let mut q = world.query_filtered::<(Entity, &TeamId, &UnitType, &Health), Without<crate::units::combat::Dying>>();
            for (e, team, kind, hp) in q.iter(world) {
                if hp.current <= 0.0 {
                    let first = *zero_seen.entry(e).or_insert(tick);
                    assert!(
                        tick - first < 3,
                        "owner {}'s {:?} at {} HP since tick {first}, now {tick}",
                        team.0,
                        kind.0,
                        hp.current
                    );
                }
            }
        }
        // Caps never change (the user's rule, 2026-10-15).
        let counts = world.resource::<TeamUnitCounts>();
        for owner in alliances.owners() {
            assert_eq!(counts.limit(owner), MAX_UNITS_PER_TEAM);
        }
        // A leaver keeps no units for long: an AI ally takes them
        // within a second or so, or they blow up within 4 s with the
        // ally team (plus the death animation).
        if tick.is_multiple_of(30) {
            use crate::units::components::{Health, TeamId, UnitType};
            let mut q = world.query_filtered::<(
                &TeamId,
                &UnitType,
                Option<&Health>,
                Has<crate::units::components::Faction>,
                Has<GlobalTransform>,
                Has<crate::net::NetId>,
                Has<crate::units::lifecycle::spawning::BuildDriven>,
            ), Without<crate::units::combat::Dying>>();
            let alive: Vec<(u8, String)> = q
                .iter(world)
                .map(|(t, k, h, f, g, n, b)| {
                    (
                        t.0,
                        format!(
                            "{:?} hp {:?} f{f} g{g} n{n} b{b}",
                            k.0,
                            h.map(|h| h.current)
                        ),
                    )
                })
                .collect();
            for (&owner, &died) in &deaths {
                if tick > died + 60 + u64::from(crate::units::lifecycle::game_over::FUSE_FRAMES) {
                    let left: Vec<&String> = alive
                        .iter()
                        .filter(|(o, _)| *o == owner)
                        .map(|(_, k)| k)
                        .collect();
                    assert!(
                        left.is_empty(),
                        "dead owner {owner} (died {died}) still fields {left:?} at tick {tick}"
                    );
                }
            }
        }
    }
    let (decided_at, winners) = decided.unwrap();
    // Shared defeat: one ally team wins; every owner of every other one
    // is dead; the match ends at the census after the last death.
    assert_eq!(winners.len(), 1, "one ally team wins: {winners:?}");
    for owner in alliances.owners() {
        if alliances.ally_team(owner) != Some(winners[0]) {
            assert!(
                deaths.get(&owner).is_some_and(|t| *t < decided_at),
                "owner {owner} fell with its side"
            );
        }
    }
    assert!(
        alliances
            .owners()
            .any(|o| alliances.ally_team(o) == Some(winners[0]) && !deaths.contains_key(&o)),
        "a winner is alive"
    );
    let last_death = *deaths.values().max().unwrap();
    // The first census after the losers' last explosion: at least one
    // census, at most the longest fuse plus a census later.
    assert!(
        decided_at >= last_death + 30
            && decided_at
                <= last_death
                    + 30
                    + 30 * (1 + u64::from(crate::units::lifecycle::game_over::FUSE_FRAMES) / 30),
        "decided at {decided_at}, last owner out at {last_death}"
    );
    let shown = *h.world_mut().resource::<State<GameState>>().get();
    let owner0_won = alliances.ally_team(0) == Some(winners[0]);
    assert_eq!(
        shown,
        if owner0_won {
            GameState::Victory
        } else {
            GameState::Defeat
        }
    );
    let hashes = std::mem::take(&mut h.world_mut().resource_mut::<HashLog>().ticks);
    // The world still changes after the first death and after the end.
    let first_death = *deaths.values().min().unwrap() as usize;
    let moved = |from: usize, to: usize| {
        hashes[from.min(hashes.len() - 1)].sim != hashes[to.min(hashes.len() - 1)].sim
    };
    assert!(
        moved(first_death, last_death as usize),
        "the sim went on after the first death"
    );
    assert!(
        moved(decided_at as usize, (decided_at + after) as usize - 1),
        "and after the end"
    );
    Ending {
        deaths,
        decided_at,
        winners,
        shown,
        hashes,
    }
}

fn report_ending(name: &str, e: &Ending) {
    eprintln!(
        "{name}: decided at tick {} ({:.1} game min), ally team {:?} wins; deaths {:?}; owner 0 sees {:?}",
        e.decided_at,
        e.decided_at as f64 / 1800.0,
        e.winners,
        e.deaths,
        e.shown,
    );
}

/// A 2v2 of AI owners (two ally teams, factions mixed) played to its
/// end: shared defeat, the dead's units gone, the sim running on after
/// each death and after the end, the local result UI only. Then the
/// same match again, ending on the same tick with the same trail.
/// `KP_NET_MAP` / `KP_NET_SEED` / `KP_NET_TICKS` (the cap) vary it.
#[test]
#[ignore = "plays a 2v2 to its end; --ignored --release"]
fn shared_defeat_2v2() {
    let mut scenario = Scenario::ai_match(
        &env("KP_NET_MAP", "Dual_Core".to_string()),
        2,
        2,
        env("KP_NET_SEED", 0x2_0202_u64),
    );
    scenario.difficulty = env("KP_NET_DIFFICULTY", 3);
    let max = env("KP_NET_TICKS", 30 * 60 * 40);
    let a = play_to_the_end(scenario.clone(), max, 300);
    report_ending("2v2", &a);
    let b = play_to_the_end(scenario, max, 300);
    assert_eq!(a.decided_at, b.decided_at);
    assert_eq!(a.deaths, b.deaths);
    assert_eq!(first_divergence(&a.hashes, &b.hashes, |h| h.sim), None);
}

/// The same for a 4v4 (eight AI owners in two ally teams).
#[test]
#[ignore = "plays a 4v4 to its end; --ignored --release"]
fn shared_defeat_4v4() {
    let mut scenario = Scenario::ai_match(
        &env("KP_NET_MAP", "Speed_Balls_16_Way".to_string()),
        2,
        4,
        env("KP_NET_SEED", 0x5eed_u64),
    );
    scenario.difficulty = env("KP_NET_DIFFICULTY", 2);
    let a = play_to_the_end(scenario, env("KP_NET_TICKS", 30 * 60 * 60), 300);
    report_ending("4v4", &a);
}

/// System pairs in `FixedUpdate` with conflicting data access and no
/// ordering between them: their run order is up to the executor, so
/// the single- and multi-threaded executors (and in principle two
/// multi-threaded runs) may order them differently.
#[test]
#[ignore = "boots the app; --ignored"]
fn fixed_ambiguities() {
    let mut app = build_app(Executor::Multi);
    let pairs: Vec<(String, String, Vec<String>)> =
        app.world_mut()
            .resource_scope(|world, mut schedules: Mut<Schedules>| {
                let schedule = schedules.get_mut(FixedUpdate).unwrap();
                schedule.initialize(world).unwrap();
                let names: HashMap<_, String> = schedule
                    .systems()
                    .unwrap()
                    .map(|(key, sys)| (key, sys.name().to_string()))
                    .collect();
                let name = |k| names.get(k).cloned().unwrap_or_default();
                schedule
                    .graph()
                    .conflicting_systems()
                    .iter()
                    .map(|(a, b, c)| {
                        let comps = c
                            .iter()
                            .map(|id| {
                                world
                                    .components()
                                    .get_name(*id)
                                    .map(|n| n.to_string())
                                    .unwrap_or_default()
                            })
                            .collect();
                        (name(a), name(b), comps)
                    })
                    .collect()
            });
    let ours = pairs
        .iter()
        .filter(|(a, b, _)| a.starts_with("kernel_panic") && b.starts_with("kernel_panic"))
        .count();
    eprintln!(
        "FixedUpdate: {} ambiguous pairs ({} between two game systems)",
        pairs.len(),
        ours
    );
    let mut by_component: BTreeMap<String, usize> = BTreeMap::new();
    for (a, b, names) in &pairs {
        for n in names {
            *by_component.entry(n.clone()).or_default() += 1;
        }
        if names.is_empty() {
            *by_component.entry("<world>".into()).or_default() += 1;
        }
        eprintln!("  {a}  <->  {b}  on [{}]", names.join(", "));
    }
    let mut top: Vec<_> = by_component.into_iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    eprintln!("most contended: {:?}", &top[..top.len().min(15)]);
}

/// One tick of a Pointer's fold state next to what it did.
#[derive(Debug, Clone, Copy)]
struct PointerTick {
    tick: u64,
    speed: f32,
    is_open: bool,
    state: crate::units::combat::DeployState,
    /// `gun` y offset (`[20]` when out) and `left` plate x (`[10]` when
    /// open; the rig mirrors the s3o x axis, so −10 here).
    gun_y: f32,
    plate_x: f32,
    fired: bool,
}

/// pointer.bos: `AimWeapon1` returns 1 only `if (isOpen)`, and `Open()`
/// sets `isOpen` after the gun is all the way out, so a Pointer never
/// fires while stowed, folding or unfolding — whatever it was ordered
/// to do meanwhile. Its movement is never held (the script's
/// `set MAX_SPEED` is commented out): it rolls off while still folding.
///
/// A Pointer and an enemy Byte 600 elmos apart (in the Pointer's 1400
/// range and 768 sight, out of the Byte's 512; the Byte's 15000 HP
/// outlast the test): the Pointer opens from its spawn and shoots; a
/// move order folds it mid-roll; arriving reopens it; a one-elmo nudge
/// (speed above 0.01 for a frame or two) swaps the Close thread for
/// Open with `isOpen` still set. Every shot in the timeline must come
/// from a fully open Pointer.
#[test]
fn pointer_only_fires_fully_open() {
    use crate::interaction::ground_move::GroundMover;
    use crate::interaction::movement::QueuedCommand;
    use crate::terrain::heightmap::Heightmap;
    use crate::units::assets::animation::UnitAnimator;
    use crate::units::combat::{AttackCooldown, AttackTargetOrder, DeployState, Deployable};
    use crate::units::components::{Faction, Homebase, TeamId};
    use crate::units::content::definitions::UnitKind;
    use crate::units::lifecycle::spawning::{SpawnContext, spawn_unit};
    use bevy::ecs::system::SystemState;

    let mut scenario = Scenario::ai_match("Speed_Balls_16_Way", 2, 1, 7);
    for seat in &mut scenario.seats {
        seat.ai = false;
    }
    scenario.sandbox = true;
    let mut h = SimHarness::new(scenario);

    // Next to team 0's Kernel, on its side of the map centre so the
    // ground between is the start area's.
    let (pointer, target, nudge, away, far) = {
        let world = h.world_mut();
        let kernel = world
            .query_filtered::<(&Transform, &TeamId), With<Homebase>>()
            .iter(world)
            .find(|(_, team)| team.0 == 0)
            .map(|(tf, _)| tf.translation)
            .expect("team 0 homebase");
        let (w, d) = world.resource::<Heightmap>().world_size();
        let toward = (Vec3::new(w * 0.5, 0.0, d * 0.5) - kernel)
            .with_y(0.0)
            .normalize_or(Vec3::Z);
        let side = Vec3::new(-toward.z, 0.0, toward.x);
        let at = |world: &World, p: Vec3| world.resource::<Heightmap>().place(p.x, p.z);
        let p0 = at(world, kernel + toward * 200.0);
        let p1 = at(world, kernel + toward * 800.0);
        let away = at(world, kernel + toward * 200.0 + side * 250.0);
        let nudge = at(world, kernel + toward * 200.0 + side * 251.0);
        let far = at(world, kernel + toward * 1100.0 + side * 250.0);
        let mut state: SystemState<SpawnContext> = SystemState::new(world);
        let mut ctx = state.get_mut(world).expect("spawn context");
        let pointer = spawn_unit(UnitKind::Pointer, Faction::System, 0, p0, &mut ctx);
        let target = spawn_unit(UnitKind::Byte, Faction::System, 1, p1, &mut ctx);
        state.apply(world);
        (pointer, target, nudge, away, far)
    };

    let mut timeline: Vec<PointerTick> = Vec::new();
    let mut last_ready: Option<u64> = None;
    let record = |h: &mut SimHarness, last_ready: &mut Option<u64>| {
        let tick = h.tick;
        let world = h.world_mut();
        let mut q = world.query::<(
            &GroundMover,
            &UnitAnimator,
            &Deployable,
            Option<&AttackCooldown>,
        )>();
        let (mover, animator, deployable, cooldown) = q.get(world, pointer).expect("pointer");
        let rig = &animator.rig;
        let gun = rig.piece("gun").expect("gun piece");
        let left = rig.piece("left").expect("left piece");
        let ready = cooldown.map(|c| c.ready_frame);
        let fired = ready.is_some() && ready != *last_ready;
        *last_ready = ready;
        PointerTick {
            tick,
            speed: mover.current_speed,
            is_open: animator.driver.is_open().unwrap_or(false),
            state: deployable.state,
            gun_y: rig.piece_translations[gun][1],
            plate_x: rig.piece_translations[left][0],
            fired,
        }
    };
    let mut run = |h: &mut SimHarness, ticks: u32, timeline: &mut Vec<PointerTick>| {
        for _ in 0..ticks {
            h.step();
            timeline.push(record(h, &mut last_ready));
        }
    };
    let order_move = |h: &mut SimHarness, to: Vec3| {
        let world = h.world_mut();
        {
            let mut commands = world.commands();
            let mut ec = commands.entity(pointer);
            crate::interaction::replace_order(&mut ec, QueuedCommand::Move(to));
        }
        world.flush();
    };

    // Phase 1: spawned complete, it opens at once and engages the Bug.
    {
        let world = h.world_mut();
        {
            let mut commands = world.commands();
            let mut ec = commands.entity(pointer);
            crate::interaction::clear_orders(&mut ec).insert(AttackTargetOrder { target });
        }
        world.flush();
    }
    run(&mut h, 240, &mut timeline);
    let shots_idle = timeline.iter().filter(|t| t.fired).count();
    // Phase 2: a move order while open: it rolls off folding, stops,
    // reopens, engages again.
    order_move(&mut h, away);
    run(&mut h, 420, &mut timeline);
    let moved = timeline.iter().map(|t| t.speed).fold(0.0, f32::det_max);
    // Phase 3: a one-elmo nudge (speed crosses 0.01 and drops back
    // within the Close delay).
    order_move(&mut h, nudge);
    run(&mut h, 300, &mut timeline);
    // Phase 4: an attack-move past the Byte: it rolls, stops to engage
    // (`AttackMoveActive` halts the mover for a target in range),
    // reopens and fires.
    {
        let world = h.world_mut();
        {
            let mut commands = world.commands();
            let mut ec = commands.entity(pointer);
            crate::interaction::replace_order(&mut ec, QueuedCommand::AttackMove(far));
        }
        world.flush();
    }
    run(&mut h, 420, &mut timeline);
    let shots_total = timeline.iter().filter(|t| t.fired).count();

    let dump = || {
        timeline
            .iter()
            .filter(|t| t.fired || t.speed > 0.01 || t.state != DeployState::Open)
            .map(|t| format!("{t:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // The transitions and shots, for the record (`--nocapture`).
    let mut last: Option<(bool, DeployState)> = None;
    for t in &timeline {
        let now = (t.is_open, t.state);
        if t.fired || last != Some(now) {
            eprintln!("{t:?}");
        }
        last = Some(now);
    }
    assert!(shots_idle >= 1, "no shot while idle and open:\n{}", dump());
    assert!(moved > 0.5, "the Pointer never rolled:\n{}", dump());
    assert!(
        shots_total > shots_idle,
        "no shot after moving:\n{}",
        dump()
    );
    for t in &timeline {
        if !t.fired {
            continue;
        }
        assert!(
            t.is_open
                && t.state == DeployState::Open
                && (t.gun_y - 20.0).abs() < 1e-3
                && (t.plate_x.abs() - 10.0).abs() < 1e-3,
            "shot from a Pointer that is not fully open: {t:?}\n{}",
            dump()
        );
    }
}
