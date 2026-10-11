//! The native entry points of the golden-replay matrix (`replay`
//! feature), taken over by `main` before any game setup:
//!
//! ```text
//! kernel-panic --replay <log.kpreplay>… [--trail-dir <dir>] [--ticks <n>]
//!              [--dump-at <t1,t2…> --dump-dir <dir>] [--ignore-compat]
//!              [--memory-assets] [--shuffle <every n ticks>]
//! kernel-panic --record-golden [<name>…] [--out <dir>]
//! ```
//!
//! `--replay` writes `<trail-dir>/<log stem>.trail` (default `trails/`)
//! per log; `--dump-at` also writes readable unit dumps
//! `<dump-dir>/<log stem>_<tick>.txt` for the bisect. `--record-golden`
//! records the golden logs ([`super::golden`]), all or the named ones,
//! into `--out` (default `kernel-panic/tests/golden`).

use std::path::{Path, PathBuf};

use super::golden;
use super::replay::{self, MapInput, PlayOptions, Replay};

/// Run the command line if it asks for a replay mode; `None` lets the
/// game start.
pub fn run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--replay") => replay_logs(&args[1..]),
        Some("--record-golden") => record_golden(&args[1..]),
        _ => return None,
    };
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    })
}

/// `--flag value` pairs and the positional rest.
fn split_args<'a>(
    args: &'a [String],
    flags: &[&str],
    switches: &[&str],
) -> Result<(Vec<(&'a str, &'a str)>, Vec<&'a str>, Vec<&'a str>), String> {
    let (mut values, mut set, mut rest) = (Vec::new(), Vec::new(), Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if flags.contains(&a.as_str()) {
            let v = it.next().ok_or(format!("{a} needs a value"))?;
            values.push((a.as_str(), v.as_str()));
        } else if switches.contains(&a.as_str()) {
            set.push(a.as_str());
        } else if a.starts_with("--") {
            return Err(format!("unknown option {a}"));
        } else {
            rest.push(a.as_str());
        }
    }
    Ok((values, set, rest))
}

fn value<'a>(values: &[(&str, &'a str)], flag: &str) -> Option<&'a str> {
    values
        .iter()
        .rev()
        .find(|(f, _)| *f == flag)
        .map(|(_, v)| *v)
}

fn replay_logs(args: &[String]) -> Result<(), String> {
    let (values, switches, logs) = split_args(
        args,
        &[
            "--trail-dir",
            "--ticks",
            "--dump-at",
            "--dump-dir",
            "--shuffle",
        ],
        &["--ignore-compat", "--memory-assets"],
    )?;
    if logs.is_empty() {
        return Err("--replay needs at least one .kpreplay".into());
    }
    let trail_dir = PathBuf::from(value(&values, "--trail-dir").unwrap_or("trails"));
    let options = PlayOptions {
        ticks: value(&values, "--ticks")
            .map(|t| t.parse().map_err(|e| format!("--ticks {t}: {e}")))
            .transpose()?,
        ignore_compat: switches.contains(&"--ignore-compat"),
        dump_at: value(&values, "--dump-at")
            .unwrap_or_default()
            .split(',')
            .filter_map(|t| t.trim().parse().ok())
            .collect(),
        shuffle_every: value(&values, "--shuffle")
            .map(|t| t.parse().map_err(|e| format!("--shuffle {t}: {e}")))
            .transpose()?
            .unwrap_or(0),
    };
    let dump_dir = value(&values, "--dump-dir").map(PathBuf::from);
    std::fs::create_dir_all(&trail_dir).map_err(|e| format!("{}: {e}", trail_dir.display()))?;
    for log in logs {
        let path = Path::new(log);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| log.to_string());
        let bytes = std::fs::read(path).map_err(|e| format!("{log}: {e}"))?;
        let replay = Replay::decode(&bytes).map_err(|e| format!("{log}: {e}"))?;
        let mut map = MapInput::checkout(&replay.setup.map).map_err(|e| e.to_string())?;
        if switches.contains(&"--memory-assets") {
            // An empty asset server, as on wasm (native reads the map
            // file itself): no sound, font or image loads.
            map.assets = super::AssetRoot::Memory(Default::default());
        }
        let played = bevy::tasks::block_on(replay::play(&replay, map, &options))
            .map_err(|e| format!("{log}: {e}"))?;
        let out = trail_dir.join(format!("{stem}.trail"));
        std::fs::write(&out, replay::trail_text(&played.hashes))
            .map_err(|e| format!("{}: {e}", out.display()))?;
        if let Some(dir) = &dump_dir {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            for (tick, lines) in &played.dumps {
                let f = dir.join(format!("{stem}_{tick}.txt"));
                std::fs::write(&f, lines.join("\n"))
                    .map_err(|e| format!("{}: {e}", f.display()))?;
            }
        }
        eprintln!("{}", replay::summary(&stem, &replay, &played));
        eprintln!("  trail: {}", out.display());
    }
    Ok(())
}

fn record_golden(args: &[String]) -> Result<(), String> {
    let (values, _, names) = split_args(args, &["--out"], &[])?;
    let out = value(&values, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::paths::from_project_root("kernel-panic/tests/golden"));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let all = golden::all();
    for name in &names {
        if !all.iter().any(|g| g.name == *name) {
            let known: Vec<&str> = all.iter().map(|g| g.name).collect();
            return Err(format!(
                "no golden log {name} (known: {})",
                known.join(", ")
            ));
        }
    }
    for g in all
        .iter()
        .filter(|g| names.is_empty() || names.contains(&g.name))
    {
        eprintln!(
            "recording {} ({} ticks on {})…",
            g.name, g.ticks, g.setup.map
        );
        let (replay, trail) = golden::record(g).map_err(|e| format!("{}: {e}", g.name))?;
        let file = out.join(format!("{}.kpreplay", g.name));
        let bytes = replay.encode();
        std::fs::write(&file, &bytes).map_err(|e| format!("{}: {e}", file.display()))?;
        let last = trail.lines().last().unwrap_or_default();
        eprintln!(
            "  {}: {} bytes, {} frames / {} commands; replay matches the recording; last tick: {last}",
            file.display(),
            bytes.len(),
            replay.frames.len(),
            replay.commands(),
        );
    }
    Ok(())
}
