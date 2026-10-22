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

/// What the AI builds and uses: each team's buildings by kind every two
/// game minutes, then the build orders and ability commands it issued.
#[test]
#[ignore = "runs a long match; --ignored --release"]
fn ai_census() {
    use crate::net::SimCommand;
    use crate::units::components::{TeamId, UnitType};
    use crate::units::content::unit_registry::UnitRegistry;
    let scenario = scenario_from_env();
    let ticks: u64 = env("KP_NET_TICKS", 30 * 60 * 12);
    eprintln!(
        "ai_census on {}: {} seats",
        scenario.map,
        scenario.seats.len()
    );
    let mut h = SimHarness::new(scenario);
    h.world_mut().resource_mut::<CommandBus>().log = Some(Vec::new());
    let mut builds: BTreeMap<(u8, String), usize> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    while h.tick < ticks {
        h.step();
        let frames = h
            .world_mut()
            .resource_mut::<CommandBus>()
            .log
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let kind_of: HashMap<crate::net::NetId, String> = {
            let world = h.world_mut();
            let mut q = world.query::<(&crate::net::NetId, &UnitType)>();
            q.iter(world)
                .map(|(id, k)| (*id, format!("{:?}", k.0)))
                .collect()
        };
        for frame in &frames {
            for cmd in &frame.commands {
                if let SimCommand::Cast { units, .. } = cmd {
                    for id in units.list().map(|l| l.ids().to_vec()).unwrap_or_default() {
                        let k = kind_of.get(&id).cloned().unwrap_or_default();
                        *kinds.entry(format!("Cast by {k}")).or_default() += 1;
                    }
                }
                if let SimCommand::Build { kind, .. } = cmd {
                    *builds
                        .entry((frame.player, format!("{kind:?}")))
                        .or_default() += 1;
                }
                let name = match cmd {
                    SimCommand::Order { order, .. } => format!(
                        "Order::{}",
                        format!("{order:?}")
                            .split([' ', '{', '('])
                            .next()
                            .unwrap_or_default()
                    ),
                    other => command_name(other),
                };
                *kinds.entry(name).or_default() += 1;
            }
        }
        if h.tick.is_multiple_of(30 * 120) {
            let world = h.world_mut();
            let mut q = world.query::<(&UnitType, &TeamId)>();
            let all: Vec<_> = q.iter(world).map(|(k, t)| (k.0, t.0)).collect();
            let registry = world.resource::<UnitRegistry>();
            let mut per: BTreeMap<u8, BTreeMap<String, usize>> = BTreeMap::new();
            for (kind, team) in all {
                if registry.is_building(kind) || kind.is_constructor() {
                    *per.entry(team)
                        .or_default()
                        .entry(format!("{kind:?}"))
                        .or_default() += 1;
                }
            }
            eprintln!("t={:>4.1} min", h.tick as f64 / 1800.0);
            for (team, m) in per {
                eprintln!("  team {team}: {m:?}");
            }
        }
    }
    eprintln!("build orders (team, kind): {builds:?}");
    eprintln!("commands: {kinds:?}");
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
    let (pointer, target, nudge, away, far, runner, runner_to) = {
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
        // Phase 5's runner: out of the Geometric's 1400 reach from
        // `away` and wide of the first Byte's 512 MegaBeam, walking
        // toward the Pointer's side while it closes in.
        let p2 = at(world, kernel + toward * 1750.0 + side * 1100.0);
        let runner_to = at(world, kernel + toward * 1050.0 + side * 1100.0);
        let mut state: SystemState<SpawnContext> = SystemState::new(world);
        let mut ctx = state.get_mut(world).expect("spawn context");
        let pointer = spawn_unit(UnitKind::Pointer, Faction::System, 0, p0, &mut ctx);
        let target = spawn_unit(UnitKind::Byte, Faction::System, 1, p1, &mut ctx);
        let runner = spawn_unit(UnitKind::Byte, Faction::System, 1, p2, &mut ctx);
        state.apply(world);
        (pointer, target, nudge, away, far, runner, runner_to)
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
    // Phase 5: an attack order on a target out of range that is itself
    // walking away: the Pointer rolls after it folded (the chase keeps
    // running while it is closed), stops once the runner is in reach,
    // opens and fires — `CMobileCAI::ExecuteObjectAttack` with the
    // script's `isOpen` gating only the shot.
    {
        let world = h.world_mut();
        {
            let mut commands = world.commands();
            let mut ec = commands.entity(runner);
            crate::interaction::replace_order(&mut ec, QueuedCommand::Move(runner_to));
            let mut ec = commands.entity(pointer);
            crate::interaction::clear_orders(&mut ec).insert(AttackTargetOrder { target: runner });
        }
        world.flush();
    }
    let chase_from = timeline.len();
    run(&mut h, 1200, &mut timeline);
    let shots_chase = timeline[chase_from..].iter().filter(|t| t.fired).count();
    let runner_dist = {
        let world = h.world_mut();
        let mut q = world.query::<&Transform>();
        let p = q.get(world, pointer).expect("pointer").translation;
        let r = q.get(world, runner).expect("runner").translation;
        p.distance(r)
    };

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
    assert!(
        shots_chase >= 1,
        "no shot at the runner it had to chase (ends {runner_dist:.0} elmos off):\n{}",
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

/// End to end: a move order given to the Kernel is carried out by the
/// Bit it builds next — it walks out of the factory's front, then to
/// the rally point.
#[test]
fn factory_orders_reach_built_units() {
    use crate::interaction::movement::{CommandQueue, MoveTarget, QueuedCommand};
    use crate::net::{CommandFrame, FactoryOp, NetId, NetPos, Order, SimCommand, Units};
    use crate::units::components::{TeamId, UnitType};
    use crate::units::content::definitions::UnitKind;
    use crate::units::lifecycle::spawning::Emerging;

    let mut scenario = Scenario::ai_match("Central_Hub", 2, 1, 7);
    scenario.seats[0].ai = false;
    scenario.sandbox = true;
    let mut h = SimHarness::new(scenario);
    h.step();

    let world = h.world_mut();
    let homes: Vec<(Entity, NetId, u8, Vec3)> = world
        .query_filtered::<(Entity, &NetId, &TeamId, &Transform), With<Homebase>>()
        .iter(world)
        .map(|(e, id, t, tf)| (e, *id, t.0, tf.translation))
        .collect();
    let (_, kernel, _, home) = *homes.iter().find(|h| h.2 == 0).unwrap();
    let enemy = homes.iter().find(|h| h.2 != 0).unwrap().3;
    let rally = NetPos::quantize(home + (enemy - home).normalize() * 300.0).to_vec3();
    let mut bus = world.resource_mut::<CommandBus>();
    let tick = bus.tick;
    bus.deliver(CommandFrame {
        tick,
        player: 0,
        commands: vec![
            SimCommand::Order {
                units: Units::of([kernel]),
                order: Order::Move(NetPos::quantize(rally)),
                queue: false,
            },
            SimCommand::Factory {
                factories: Units::of([kernel]),
                kind: UnitKind::Bit,
                op: FactoryOp::Add(1),
            },
        ],
    });

    let bits = |world: &mut World| -> Vec<(Entity, Vec3, bool)> {
        world
            .query::<(Entity, &UnitType, &TeamId, &Transform, Has<Emerging>)>()
            .iter(world)
            .filter(|(_, ut, t, ..)| ut.0 == UnitKind::Bit && t.0 == 0)
            .map(|(e, _, _, tf, emerging)| (e, tf.translation, emerging))
            .collect()
    };
    // Built and out of the pad.
    let mut bit = None;
    for _ in 0..2400 {
        h.step();
        if let Some(&(e, _, false)) = bits(h.world_mut()).first() {
            bit = Some(e);
            break;
        }
    }
    let bit = bit.expect("the Kernel builds the Bit");
    let world = h.world_mut();
    let queue: Vec<QueuedCommand> = world
        .get::<CommandQueue>(bit)
        .map_or(Vec::new(), |q| q.commands.clone());
    assert!(
        world.get::<MoveTarget>(bit).is_some(),
        "it walks out of the factory first"
    );
    assert_eq!(
        queue,
        vec![QueuedCommand::Move(rally)],
        "then to the rally point"
    );

    // And gets there.
    let mut closest = f32::MAX;
    for _ in 0..900 {
        h.step();
        let world = h.world_mut();
        let pos = world.get::<Transform>(bit).unwrap().translation;
        closest = closest.det_min(Vec2::new(pos.x - rally.x, pos.z - rally.z).length());
    }
    assert!(
        closest < 40.0,
        "the Bit came within {closest} elmos of the rally point"
    );
}

/// End to end: Launch Mines on a folded Byte. It unfolds, then fires a
/// MineLauncher bolt from each of its five tubes; each explodes on the
/// ground ahead and leaves a Logic Bomb there — five mines in a forward
/// arc — and the Byte is 6000 HP lighter and reloading.
#[test]
fn byte_launches_a_forward_arc_of_logic_bombs() {
    use crate::net::{CommandFrame, NetId, NetPos, SimCommand, Units};
    use crate::terrain::heightmap::Heightmap;
    use crate::units::components::{Faction, Health, TeamId, UnitType};
    use crate::units::content::definitions::UnitKind;
    use crate::units::lifecycle::spawning::{SpawnContext, spawn_unit};
    use crate::units::mechanics::command_fire::CommandFireCooldown;
    use bevy::ecs::system::SystemState;

    let mut scenario = Scenario::ai_match("Speed_Balls_16_Way", 2, 1, 7);
    for seat in &mut scenario.seats {
        seat.ai = false;
    }
    scenario.sandbox = true;
    let mut h = SimHarness::new(scenario);

    let byte = {
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
        let at = world
            .resource::<Heightmap>()
            .place(kernel.x + toward.x * 300.0, kernel.z + toward.z * 300.0);
        let mut state: SystemState<SpawnContext> = SystemState::new(world);
        let mut ctx = state.get_mut(world).expect("spawn context");
        let byte = spawn_unit(UnitKind::Byte, Faction::System, 0, at, &mut ctx);
        state.apply(world);
        byte
    };
    // Past the 6 s `ARMORED` after its build.
    for _ in 0..7 * 30 {
        h.step();
    }
    let (hp_before, byte_id) = {
        let world = h.world_mut();
        let hp = world.get::<Health>(byte).unwrap().current;
        (hp, *world.get::<NetId>(byte).unwrap())
    };
    {
        let world = h.world_mut();
        let mut bus = world.resource_mut::<CommandBus>();
        let tick = bus.tick;
        bus.deliver(CommandFrame {
            tick,
            player: 0,
            commands: vec![SimCommand::Cast {
                units: Units::of([byte_id]),
                target: NetPos::quantize(Vec3::ZERO),
                auto: false,
            }],
        });
    }
    let mut mines = Vec::new();
    let mut frames = 0;
    while mines.len() < 5 && frames < 6 * 30 {
        h.step();
        frames += 1;
        let world = h.world_mut();
        mines = world
            .query::<(&UnitType, &TeamId, &Transform)>()
            .iter(world)
            .filter(|(u, t, _)| u.0 == UnitKind::LogicBomb && t.0 == 0)
            .map(|(_, _, tf)| tf.translation)
            .collect::<Vec<_>>();
    }
    let world = h.world_mut();
    let tf = *world.get::<Transform>(byte).unwrap();
    let fwd = tf.forward().as_vec3().with_y(0.0).normalize();
    // Signed angle off the Byte's heading and distance of each mine.
    let fan: Vec<(f32, f32)> = mines
        .iter()
        .map(|m| {
            let to = (*m - tf.translation).with_y(0.0);
            let angle = fwd.angle_between(to.normalize()).to_degrees() * fwd.cross(to).y.signum();
            (angle, to.length())
        })
        .collect();
    eprintln!("after {frames} frames: {fan:?}");
    assert_eq!(mines.len(), 5, "five Logic Bombs: {fan:?}");
    // Unfolding (`sleep 500` checks) and ~2 s of flight at 200 elmos/s.
    assert!(frames > 45, "fired before the Byte had unfolded: {frames}");
    for &(angle, dist) in &fan {
        assert!(angle.abs() < 25.0, "not in front of the Byte: {fan:?}");
        assert!((250.0..500.0).contains(&dist), "not a short throw: {fan:?}");
    }
    let (lo, hi) = fan
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &(a, _)| {
            (lo.det_min(a), hi.det_max(a))
        });
    assert!(hi - lo > 25.0, "not fanned out: {fan:?}");
    let hp_after = world.get::<Health>(byte).unwrap().current;
    assert!(
        (hp_before - hp_after - 6000.0).abs() < 1.0,
        "{hp_before} -> {hp_after}"
    );
    assert!(
        world.get::<CommandFireCooldown>(byte).is_some(),
        "reloading"
    );
}

/// End to end on Dual Core: the map's 184 Bad Blocks stand where the
/// map places them, block a LIGHT mover's squares but not a HEAVY's
/// (which crushes them), are never picked as a target, and take the
/// splash of a blast next to them like a unit.
#[test]
fn dual_core_places_its_bad_blocks() {
    use crate::interaction::movement::NavGridSet;
    use crate::sim::SQUARE_SIZE;
    use crate::units::combat::{AimTarget, TargetCache};
    use crate::units::components::{Faction, Health};
    use crate::units::content::definitions::UnitKind;
    use crate::units::features::Feature;
    use crate::units::lifecycle::spawning::{SpawnContext, spawn_unit};
    use bevy::ecs::system::SystemState;

    let mut scenario = Scenario::ai_match("Dual_Core", 2, 1, 7);
    for seat in &mut scenario.seats {
        seat.ai = false;
    }
    scenario.sandbox = true;
    let mut h = SimHarness::new(scenario);
    h.step();
    h.step();

    let world = h.world_mut();
    let mut blocks: Vec<(Entity, Vec3)> = world
        .query::<(Entity, &Feature, &Transform)>()
        .iter(world)
        .map(|(e, f, tf)| {
            assert_eq!(f.def.name, "badblock");
            (e, tf.translation)
        })
        .collect();
    assert_eq!(blocks.len(), 184);
    // The map's first one, (372, 372).
    blocks.sort_by(|a, b| (a.1.z, a.1.x).partial_cmp(&(b.1.z, b.1.x)).unwrap());
    let (block, at) = blocks[0];
    assert_eq!(at.xz(), Vec2::new(372.0, 372.0));
    let nav = world.resource::<NavGridSet>();
    let (sx, sz) = ((at.x / SQUARE_SIZE) as i32, (at.z / SQUARE_SIZE) as i32);
    assert!(nav.structures.square_blocked(sx, sz, false), "blocks LIGHT");
    assert!(
        !nav.structures.square_blocked(sx, sz, true),
        "HEAVY crushes it"
    );

    // A Bit beside it sees no target in it.
    let bit = {
        let mut state: SystemState<SpawnContext> = SystemState::new(world);
        let mut ctx = state.get_mut(world).expect("spawn context");
        let bit = spawn_unit(
            UnitKind::Bit,
            Faction::System,
            0,
            at + Vec3::new(-60.0, 0.0, 0.0),
            &mut ctx,
        );
        state.apply(world);
        bit
    };
    for _ in 0..90 {
        h.step();
    }
    let world = h.world_mut();
    assert!(world.get::<AimTarget>(bit).is_none(), "aims at a feature");
    assert!(
        world
            .get::<TargetCache>(bit)
            .is_none_or(|c| c.target.is_none()),
        "targets a feature"
    );
    assert_eq!(world.get::<Health>(block).unwrap().current, 400.0);

    // A SIGTERM bomb going off 50 elmos away: its splash reaches it
    // (`DoExplosionDamage` hits features like units).
    {
        assert!(
            world
                .resource::<crate::units::content::weapons::WeaponRegistry>()
                .is_present(crate::units::content::weapons::WeaponKind::SigTerm),
            "SigTerm in the TDFs"
        );
        let sigterm = crate::units::content::weapons::WeaponKind::SigTerm;
        world
            .resource_mut::<crate::units::combat::DamageQueue>()
            .push(crate::units::combat::PendingDamage {
                target: None,
                attacker: bit,
                weapon: sigterm,
                impact_pos: at + Vec3::new(0.0, 0.0, 50.0),
                attacker_distance: 0.0,
            });
    }
    h.step();
    let world = h.world_mut();
    let hp = world.get::<Health>(block).map(|h| h.current);
    eprintln!("bad block after the blast: {hp:?}");
    assert!(hp.is_none_or(|hp| hp < 400.0), "untouched by the blast");
    // Whatever the blast took to 0 HP is gone the same tick, not left
    // standing at 0.
    assert!(
        world
            .query::<(&Feature, &Health)>()
            .iter(world)
            .all(|(_, h)| h.current > 0.0)
    );
}

/// Heap Fragmentation's opening allocation rises in the sim: within a
/// dozen ticks every block of the first state stands its full height
/// over the map's ground and the movers' nav grids have closed over it,
/// while the free lane segments stay open.
#[test]
#[ignore = "loads a real map; --ignored"]
fn shifting_terrain_rises_in_the_sim() {
    use crate::terrain::heightmap::Heightmap;
    use spring_map::shifting::{Shifting, heap};
    let mut h = SimHarness::new(Scenario::ai_match(heap::MAP, 2, 1, 7));
    let ground = h.world_mut().resource::<Heightmap>().heights().to_vec();
    for _ in 0..12 {
        h.step();
    }
    let rules = Shifting::for_map(heap::MAP).unwrap();
    let world = h.world_mut();
    let hm = world.resource::<Heightmap>();
    let nav = world.resource::<crate::interaction::movement::NavGridSet>();
    let (hw, _) = hm.grid_size();
    let speeds = &nav.buckets[0].speed_map;
    let mut raised = 0;
    for (k, &up) in rules.states[0].iter().enumerate() {
        let (x, z) = rules.blocks[k].rect.centre();
        let i = (z as usize / 8) * hw + x as usize / 8;
        let cell = (z as u32 / 8) * speeds.width + x as u32 / 8;
        if up {
            raised += 1;
            assert_eq!(
                hm.heights()[i],
                ground[i] + heap::RISE,
                "block {k} is not up"
            );
            // Its edge: the wall the movers can't climb.
            let edge = rules.blocks[k].rect;
            let (ex, ez) = (
                edge.x0 as u32 * 4,
                (edge.z0 as u32 * 4 + edge.z1 as u32 * 4) / 2,
            );
            assert_eq!(
                speeds.speeds[(ez * speeds.width + ex) as usize],
                0.0,
                "block {k} wall is open"
            );
        } else {
            assert_eq!(hm.heights()[i], ground[i], "block {k} moved");
            assert!(
                speeds.speeds[cell as usize] > 0.0,
                "free block {k} is closed"
            );
        }
    }
    assert!(raised > 0);
}

/// The post-game statistics follow an AI match: a sample a second for
/// every seat, the AI's orders as actions, production growing as the
/// factories turn out units, the minifacs the constructors raise
/// marked, and none of it in the sync hash's way.
#[test]
fn match_statistics_follow_the_match() {
    use crate::stats::{MatchStats, SAMPLE_TICKS};

    let mut h = SimHarness::new(Scenario::ai_match("Marble_Madness_Map", 2, 1, 11));
    let ticks = 30 * 150;
    while h.tick < ticks {
        h.step();
    }
    let stats = h.world_mut().resource::<MatchStats>();
    assert_eq!(stats.seats.len(), 2);
    // The first tick's sample, then one every SAMPLE_TICKS.
    assert_eq!(
        stats.ticks.len(),
        1 + ticks as usize / SAMPLE_TICKS as usize
    );
    for (i, seat) in stats.seats.iter().enumerate() {
        assert_eq!(seat.units.len(), stats.ticks.len(), "seat {i}");
        assert!(seat.units[0] >= 1.0, "seat {i} starts with its homebase");
        let last = stats.ticks.len() - 1;
        assert!(
            seat.production[last] > 0.0,
            "seat {i} produced something: {:?}",
            seat.production
        );
        assert!(
            seat.production.windows(2).all(|w| w[0] <= w[1]),
            "seat {i}: production only grows"
        );
        assert!(seat.army_value[last] > 0.0, "seat {i} has an army");
        assert!(
            seat.apm.iter().any(|&a| a > 0.0),
            "seat {i}: the AI's orders count as actions"
        );
    }
    assert!(
        stats.events.iter().any(|e| e.built),
        "a minifac went up: {:?}",
        stats.events
    );
}

/// End to end: a constructor given a build, then two Shift-queued ones
/// (three separate placements, as the player clicks them) walks to and
/// builds all three, in order.
#[test]
fn shift_queued_builds_are_built_in_order() {
    use crate::net::{CommandFrame, FactoryOp, NetId, PosList, SimCommand, Units};
    use crate::terrain::geovent::GeoventSmoker;
    use crate::units::components::{TeamId, UnitType};
    use crate::units::content::definitions::UnitKind;
    use crate::units::lifecycle::construction::buildings_for;
    use crate::units::lifecycle::spawning::Emerging;

    // Two human seats: nobody attacks the constructor on its way.
    let mut scenario = Scenario::ai_match("Central_Hub", 2, 1, 7);
    for seat in &mut scenario.seats {
        seat.ai = false;
    }
    scenario.sandbox = true;
    let mut h = SimHarness::new(scenario);
    h.step();

    let world = h.world_mut();
    let homes: Vec<(NetId, u8, Vec3)> = world
        .query_filtered::<(&NetId, &TeamId, &Transform), With<Homebase>>()
        .iter(world)
        .map(|(id, t, tf)| (*id, t.0, tf.translation))
        .collect();
    let (kernel, _, home) = *homes.iter().find(|h| h.1 == 0).unwrap();
    let enemy = homes.iter().find(|h| h.1 != 0).unwrap().2;
    let mut bus = world.resource_mut::<CommandBus>();
    let tick = bus.tick;
    bus.deliver(CommandFrame {
        tick,
        player: 0,
        commands: vec![SimCommand::Factory {
            factories: Units::of([kernel]),
            kind: UnitKind::Assembler,
            op: FactoryOp::Add(1),
        }],
    });

    let mut builder = None;
    for _ in 0..2400 {
        h.step();
        let world = h.world_mut();
        builder = world
            .query::<(&NetId, &UnitType, &TeamId, Has<Emerging>)>()
            .iter(world)
            .find(|(_, ut, t, emerging)| ut.0 == UnitKind::Assembler && t.0 == 0 && !emerging)
            .map(|(id, ..)| *id);
        if builder.is_some() {
            break;
        }
    }
    let builder = builder.expect("the Kernel builds an Assembler");
    let kind = buildings_for(UnitKind::Assembler)[0];

    // A long build walk: the three vents nearest the middle of the map
    // (half way to the enemy), well away from home.
    let middle = (home + enemy) * 0.5;
    let world = h.world_mut();
    let mut vents: Vec<Vec3> = world
        .query::<&GeoventSmoker>()
        .iter(world)
        .map(|v| v.pos)
        .collect();
    vents.sort_by(|a, b| {
        a.distance_squared(middle)
            .total_cmp(&b.distance_squared(middle))
    });
    let sites: Vec<Vec3> = vents.into_iter().take(3).collect();
    assert!(sites.iter().all(|s| s.xz().distance(home.xz()) > 1000.0));
    let order = |h: &mut SimHarness, site: Vec3, queue: bool| {
        let mut bus = h.world_mut().resource_mut::<CommandBus>();
        let tick = bus.tick;
        bus.deliver(CommandFrame {
            tick,
            player: 0,
            commands: vec![SimCommand::Build {
                builders: Units::of([builder]),
                kind,
                sites: PosList::quantize([site]),
                queue,
            }],
        });
        h.step();
    };
    let building = |h: &mut SimHarness| {
        use crate::units::lifecycle::construction::Constructing;
        let world = h.world_mut();
        world
            .query_filtered::<&NetId, With<Constructing>>()
            .iter(world)
            .any(|id| *id == builder)
    };
    // The first build; the second queued on the walk there; the third
    // once the first is going up.
    order(&mut h, sites[0], false);
    order(&mut h, sites[1], true);
    for _ in 0..3000 {
        if building(&mut h) {
            break;
        }
        h.step();
    }
    assert!(building(&mut h), "the first site is reached");
    order(&mut h, sites[2], true);

    // The order the sites get a building of `kind`.
    let mut started: Vec<usize> = Vec::new();
    for _ in 0..9000 {
        h.step();
        let world = h.world_mut();
        let at: Vec<Vec3> = world
            .query::<(&UnitType, &TeamId, &Transform)>()
            .iter(world)
            .filter(|(ut, t, _)| ut.0 == kind && t.0 == 0)
            .map(|(_, _, tf)| tf.translation)
            .collect();
        for (i, site) in sites.iter().enumerate() {
            if !started.contains(&i) && at.iter().any(|p| p.xz().distance(site.xz()) < 24.0) {
                started.push(i);
            }
        }
        if started.len() == sites.len() {
            break;
        }
    }
    assert_eq!(
        started,
        vec![0, 1, 2],
        "every queued site is built, in order"
    );
}
