//! The network experiments. All `#[ignore]`d (they load real maps and
//! run thousands of ticks); run one at a time, release for timings:
//!
//! ```text
//! cargo test --release -p kernel-panic headless::tests::determinism -- --ignored --nocapture
//! ```
//!
//! Knobs (environment): `KP_NET_MAP` (default `Speed_Balls_16_Way`),
//! `KP_NET_TEAMS` × `KP_NET_PER_TEAM` (default 2 × 4 = a 4v4),
//! `KP_NET_SEED`, `KP_NET_TICKS`, `KP_NET_EXEC=single|multi`,
//! `KP_NET_PACING=uneven` (second run of `determinism` renders uneven
//! frames: 0–3 ticks each), `KP_NET_TRACE=<file>` (per-tick hashes of
//! the first run, for cross-process diffs), `KP_NET_CSV=<file>`
//! (per-tick numbers of `bandwidth`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;

use super::encode::{self, NetIds, QState};
use super::*;

fn env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn scenario_from_env() -> Scenario {
    let map: String = env("KP_NET_MAP", "Speed_Balls_16_Way".to_string());
    let mut s = Scenario::ai_match(
        &map,
        env("KP_NET_TEAMS", 2),
        env("KP_NET_PER_TEAM", 4),
        env("KP_NET_SEED", 0x5eed_u64),
    );
    s.difficulty = env("KP_NET_DIFFICULTY", 2);
    s.executor = match env("KP_NET_EXEC", "multi".to_string()).as_str() {
        "single" => Executor::Single,
        _ => Executor::Multi,
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
        let n: usize = env("KP_NET_ID_SHIFT", 0);
        let ids: Vec<Entity> = (0..n).map(|_| h.world_mut().spawn_empty().id()).collect();
        for e in ids.into_iter().step_by(2) {
            h.world_mut().despawn(e);
        }
    }
    let mut i = 0;
    while h.tick < ticks {
        if let Some(pattern) = pattern {
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

/// Per-update size samples of one encoding at one rate.
#[derive(Default)]
struct Series(Vec<f64>);

impl Series {
    fn push(&mut self, n: usize) {
        self.0.push(n as f64);
    }
    fn summary(&self, hz: f64) -> String {
        let mut v = self.0.clone();
        let med = pct(&mut v, 0.5);
        let p95 = pct(&mut v, 0.95);
        let max = pct(&mut v, 1.0);
        format!(
            "median {:>7.0} B  p95 {:>7.0} B  max {:>7.0} B | {:>6.1} / {:>6.1} KB/s (median / p95 x {hz} Hz)",
            med,
            p95,
            max,
            med * hz / 1000.0,
            p95 * hz / 1000.0,
        )
    }
}

/// A long AI match: entity counts, snapshot sizes (full / delta, raw /
/// quantized / compressed, at 30, 15 and 10 Hz), AI command volume and
/// sim cost per tick.
#[test]
#[ignore = "runs a long match; --ignored, --release for timings"]
fn bandwidth() {
    let scenario = scenario_from_env();
    let ticks: u64 = env("KP_NET_TICKS", 30 * 60 * 8);
    let players = scenario.seats.len();
    let teams: HashSet<u8> = scenario.seats.iter().map(|s| s.team).collect();
    eprintln!(
        "bandwidth: {} seats ({} teams) on {}, {:?}, {} ticks ({:.1} game min)",
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
            "tick,sim_ms,frame_ms,units,projectiles,entities,full_raw,full_q,full_q_zstd,\
             d30_units,d30_units_zstd,d30_all,d30_all_zstd,d30_all_lz4,cmds,cmd_bytes,census"
        )
        .unwrap();
    }

    crate::netprobe::imp::start();
    let mut h = SimHarness::new(scenario);
    let mut ids = NetIds::default();
    let mut proj_ids = NetIds::default();
    // Baselines per update rate (every 1, 2, 3 ticks).
    let rates = [(1u64, 30.0), (2, 15.0), (3, 10.0)];
    let mut bases: Vec<QState> = vec![QState::default(); 3];
    let mut series: BTreeMap<String, Series> = BTreeMap::new();
    let mut sim_ms: Vec<f64> = Vec::new();
    let mut frame_ms: Vec<f64> = Vec::new();
    let mut peak: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut peak_units = (0usize, 0u64);
    let mut peak_entities = 0usize;
    // Commands.
    let mut cmd_total = 0usize;
    let mut cmd_bytes_total = 0usize;
    let mut cmd_by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut cmd_tick_max_team = (0usize, 0u64);
    let mut cmd_tick_max_bytes_team = 0usize;
    let mut cmd_ticks_nonempty = 0usize;
    let mut cmd_per_team: HashMap<u8, usize> = HashMap::new();
    let mut prev_fx: HashMap<&'static str, HashSet<Entity>> = HashMap::new();
    let mut fx_spawns: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut fx_spawn_tick_max = 0usize;
    let mut units_series: Vec<(u64, usize, usize, f64)> = Vec::new();

    while h.tick < ticks {
        h.step();
        for d in h.take_tick_times() {
            sim_ms.push(d.as_secs_f64() * 1000.0);
        }
        frame_ms.push(h.last_frame.as_secs_f64() * 1000.0);
        let tick = h.tick;
        let c = state::capture(h.world_mut());

        // --- census -------------------------------------------------
        for (cat, n) in &c.census {
            let p = peak.entry(cat).or_default();
            *p = (*p).max(*n);
        }
        if c.units.len() > peak_units.0 {
            peak_units = (c.units.len(), tick);
        }
        peak_entities = peak_entities.max(c.total_entities);
        // New visual-effect entities this tick = fire / explosion
        // events a client would need.
        let mut spawned_this_tick = 0;
        for cat in [
            "projectile",
            "beam",
            "laser_bolt",
            "arc",
            "impact",
            "ground_flash",
        ] {
            let now: HashSet<Entity> = c
                .category_entities
                .get(cat)
                .map(|v| v.iter().copied().collect())
                .unwrap_or_default();
            let prev = prev_fx.entry(cat).or_default();
            let new = now.difference(prev).count();
            *fx_spawns.entry(cat).or_default() += new;
            spawned_this_tick += new;
            *prev = now;
        }
        fx_spawn_tick_max = fx_spawn_tick_max.max(spawned_this_tick);

        // --- snapshots ----------------------------------------------
        let live: HashSet<Entity> = c.units.iter().map(|u| u.entity).collect();
        ids.forget_missing(&live);
        let live_p: HashSet<Entity> = c.projectiles.iter().map(|p| p.entity).collect();
        proj_ids.forget_missing(&live_p);
        let q = QState::from_capture(&c, &mut ids, &mut proj_ids);
        let full_raw = encode::full_raw(&c, true);
        let full_q = encode::full_quant(&q, true);
        let full_q_units = encode::full_quant(&q, false);
        let full_q_zstd = encode::zstd_len(&full_q);
        series
            .entry("full raw f32 (units+proj)".into())
            .or_default()
            .push(full_raw.len());
        series
            .entry("full quant (units+proj)".into())
            .or_default()
            .push(full_q.len());
        series
            .entry("full quant (units only)".into())
            .or_default()
            .push(full_q_units.len());
        series
            .entry("full quant+zstd (units+proj)".into())
            .or_default()
            .push(full_q_zstd);
        series
            .entry("full quant+lz4 (units+proj)".into())
            .or_default()
            .push(encode::lz4_len(&full_q));
        let mut d30 = (0, 0, 0, 0, 0);
        for (k, (every, hz)) in rates.iter().enumerate() {
            if tick % every != 0 {
                continue;
            }
            let du = encode::delta_quant(&bases[k], &q, false);
            let da = encode::delta_quant(&bases[k], &q, true);
            let (du_z, da_z, da_l) = (
                encode::zstd_len(&du),
                encode::zstd_len(&da),
                encode::lz4_len(&da),
            );
            series
                .entry(format!("{hz:>2} Hz delta quant (units)"))
                .or_default()
                .push(du.len());
            series
                .entry(format!("{hz:>2} Hz delta quant+zstd (units)"))
                .or_default()
                .push(du_z);
            series
                .entry(format!("{hz:>2} Hz delta quant (units+proj)"))
                .or_default()
                .push(da.len());
            series
                .entry(format!("{hz:>2} Hz delta quant+zstd (units+proj)"))
                .or_default()
                .push(da_z);
            series
                .entry(format!("{hz:>2} Hz delta quant+lz4 (units+proj)"))
                .or_default()
                .push(da_l);
            if k == 0 {
                d30 = (du.len(), du_z, da.len(), da_z, da_l);
            }
            bases[k] = q.clone();
        }

        // --- commands -----------------------------------------------
        let cmds = crate::netprobe::imp::drain();
        let mut per_team_tick: HashMap<u8, (usize, usize)> = HashMap::new();
        let mut bytes_tick = 0;
        for cmd in &cmds {
            let size = encode::command_size(cmd);
            let e = per_team_tick.entry(cmd.team).or_default();
            e.0 += 1;
            e.1 += size;
            bytes_tick += size;
            *cmd_by_kind.entry(encode::kind_name(cmd.kind)).or_default() += 1;
            *cmd_per_team.entry(cmd.team).or_default() += 1;
        }
        for (_, (n, b)) in &per_team_tick {
            if *n > cmd_tick_max_team.0 {
                cmd_tick_max_team = (*n, tick);
            }
            cmd_tick_max_bytes_team = cmd_tick_max_bytes_team.max(b + encode::COMMAND_HEADER);
        }
        if !cmds.is_empty() {
            cmd_ticks_nonempty += 1;
        }
        cmd_total += cmds.len();
        cmd_bytes_total += bytes_tick + per_team_tick.len() * encode::COMMAND_HEADER;

        if tick % 300 == 0 {
            let s = sim_ms.last().copied().unwrap_or(0.0);
            units_series.push((tick, c.units.len(), c.projectiles.len(), s));
            eprintln!(
                "  t={:>5} ({:>4.1} min) units {:>4} proj {:>4} entities {:>6} | full_q {:>6} B, d30 {:>6} B ({:>5} zstd) | sim {:>6.2} ms frame {:>6.2} ms",
                tick,
                tick as f64 / 1800.0,
                c.units.len(),
                c.projectiles.len(),
                c.total_entities,
                full_q.len(),
                d30.2,
                d30.3,
                s,
                h.last_frame.as_secs_f64() * 1000.0
            );
        }
        if let Some(f) = csv.as_mut() {
            let census: Vec<String> = c.census.iter().map(|(k, v)| format!("{k}={v}")).collect();
            writeln!(
                f,
                "{},{:.3},{:.3},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                tick,
                sim_ms.last().copied().unwrap_or(0.0),
                h.last_frame.as_secs_f64() * 1000.0,
                c.units.len(),
                c.projectiles.len(),
                c.total_entities,
                full_raw.len(),
                full_q.len(),
                full_q_zstd,
                d30.0,
                d30.1,
                d30.2,
                d30.3,
                d30.4,
                cmds.len(),
                bytes_tick,
                census.join(" ")
            )
            .unwrap();
        }
    }
    crate::netprobe::imp::stop();

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
    eprintln!("\n=== fx spawn events (fire / impact events a client needs) ===");
    for (cat, n) in &fx_spawns {
        eprintln!("  {cat:>15}: {n} total, {:.1}/s", *n as f64 / secs);
    }
    eprintln!("  per-tick max (all kinds): {fx_spawn_tick_max}");
    eprintln!("\n=== snapshot sizes per update ===");
    for (name, s) in &series {
        let hz = if name.starts_with("15") {
            15.0
        } else if name.starts_with("10") {
            10.0
        } else {
            30.0
        };
        eprintln!("  {name:<38} {}", s.summary(hz));
    }
    eprintln!("\n=== AI commands (lockstep input) ===");
    eprintln!(
        "total {cmd_total} over {secs:.0} s = {:.2}/s all players, {:.2}/s per team, {:.2}/s per seat",
        cmd_total as f64 / secs,
        cmd_total as f64 / secs / teams.len() as f64,
        cmd_total as f64 / secs / players as f64
    );
    eprintln!(
        "bytes {} = {:.1} B/s all players ({:.1} B/s per team); ticks with any command: {} of {}",
        cmd_bytes_total,
        cmd_bytes_total as f64 / secs,
        cmd_bytes_total as f64 / secs / teams.len() as f64,
        cmd_ticks_nonempty,
        ticks
    );
    eprintln!(
        "per-tick max for one team: {} commands (tick {}), {} B incl. header",
        cmd_tick_max_team.0, cmd_tick_max_team.1, cmd_tick_max_bytes_team
    );
    for (k, n) in &cmd_by_kind {
        eprintln!("  {k:>16}: {n}");
    }
    let mut per_team: Vec<_> = cmd_per_team.into_iter().collect();
    per_team.sort();
    eprintln!("  per team: {per_team:?}");
    eprintln!("\n=== sim cost (FixedFirst..FixedLast wall time) ===");
    let late_from = sim_ms.len().saturating_sub(30 * 120);
    let mut all = sim_ms.clone();
    let mut late = sim_ms[late_from..].to_vec();
    let mut frames = frame_ms.clone();
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
        pct(&mut frames, 0.5),
        pct(&mut frames, 0.95)
    );
    // Units over time (every 10 s).
    eprintln!("\n=== units over time (every 10 s): tick units proj sim_ms ===");
    for (t, u, p, s) in units_series {
        eprint!("{t}:{u}/{p}/{s:.1} ");
    }
    eprintln!();
}

/// Start-position counts of the shipped maps (which maps seat 8).
#[test]
#[ignore = "reads every map; --ignored"]
fn map_seats() {
    let dir = crate::paths::from_project_root("kernel-panic/assets/maps");
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        if p.extension().is_some_and(|e| e == "kpmap") {
            let bytes = std::fs::read(&p).unwrap();
            match spring_map::baked::read_baked_map(&bytes) {
                Ok(m) => eprintln!(
                    "{:<40} start positions {:?}",
                    p.file_name().unwrap().to_string_lossy(),
                    m.map_info.as_ref().map(|i| i.start_positions.len())
                ),
                Err(e) => eprintln!("{}: {e}", p.display()),
            }
        }
    }
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
        h.tick,
        c.units.len(),
        c.census,
        c.sim_hash
    );
    assert!(c.units.len() >= 8);
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

/// Component names on a unit root after a few seconds: which of them a
/// frame (not a tick) system inserts decides the unit's archetype moves.
#[test]
#[ignore = "boots the app; --ignored"]
fn unit_components() {
    let mut h = SimHarness::new(scenario_from_env());
    for _ in 0..600 {
        h.step();
    }
    let world = h.world_mut();
    let mut q = world.query_filtered::<Entity, With<crate::units::components::UnitType>>();
    let units: Vec<Entity> = q.iter(world).collect();
    let mut seen = HashSet::new();
    for e in units {
        let names: Vec<String> = world
            .inspect_entity(e)
            .unwrap()
            .map(|(_, info)| info.name().to_string())
            .collect();
        let kind = world
            .get::<crate::units::components::UnitType>(e)
            .unwrap()
            .0;
        if seen.insert(format!("{kind:?}")) {
            eprintln!("{kind:?}: {}", names.join(", "));
        }
    }
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
