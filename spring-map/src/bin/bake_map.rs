//! Bake a Spring `.sd7` / `.sdz` to the `.kpmap` runtime format.
//!
//! ```text
//! cargo run -p spring-map --bin bake_map -- INPUT [OUTPUT]
//! ```
//!
//! If `OUTPUT` is omitted, writes alongside the input with a `.kpmap`
//! extension. Bakes do all the slow work once (7z extract → SMF parse
//! → Lua heightmap gadgets → SMT tile decode → texture assembly → SMD
//! parse) so the runtime can `read_baked_map` from a single mmap'd
//! buffer with no archive / Lua / image dependencies. Foundational
//! step for §8.1 (WASM web build) — we don't deploy yet, but the
//! gameplay binary already prefers the baked form when present.

use std::path::PathBuf;
use std::process::ExitCode;

use spring_map::baked::write_baked_map;
use spring_map::load_map;

#[derive(Debug, thiserror::Error)]
enum BakeError {
    #[error("usage: bake_map INPUT.sd7 [OUTPUT.kpmap]")]
    Usage,
    #[error("input not found: {0}")]
    InputMissing(PathBuf),
    #[error("failed to load source map: {0}")]
    Load(#[from] spring_map::map_types::MapError),
    #[error("failed to encode baked map: {0}")]
    Encode(#[from] spring_map::baked::BakedMapError),
    #[error("I/O error writing {path}: {error}")]
    Write {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bake_map: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), BakeError> {
    let mut args = std::env::args().skip(1);
    let input = args.next().map(PathBuf::from).ok_or(BakeError::Usage)?;
    let output = args.next().map(PathBuf::from).unwrap_or_else(|| {
        let mut o = input.clone();
        o.set_extension("kpmap");
        o
    });
    if !input.is_file() {
        return Err(BakeError::InputMissing(input));
    }

    eprintln!("Loading {}", input.display());
    let map = load_map(&input)?;

    eprintln!(
        "  {}x{}, {} features, {} starts, texture {}",
        map.parsed.header.map_x,
        map.parsed.header.map_y,
        map.parsed.features.len(),
        map.map_info
            .as_ref()
            .map(|m| m.start_positions.len())
            .unwrap_or(0),
        map.ground_texture
            .as_ref()
            .map(|g| format!("{}x{}", g.width, g.height))
            .unwrap_or_else(|| "none".into()),
    );

    let bytes = write_baked_map(&map)?;
    std::fs::write(&output, &bytes).map_err(|error| BakeError::Write {
        path: output.clone(),
        error,
    })?;
    eprintln!("Wrote {} ({} bytes)", output.display(), bytes.len());
    Ok(())
}
