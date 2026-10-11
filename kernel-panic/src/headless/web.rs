//! The wasm32 entry point of the golden-replay matrix (`replay`
//! feature): the module exports two functions instead of starting the
//! game, and the runner (`.github/scripts/kp-replay.mjs`, Node or a
//! headless browser page) hands in the files, since wasm has no
//! filesystem:
//!
//! - `kp_replay_map(log)`: the map stem the log plays on, so the runner
//!   knows which `<map>.kpmap` to read;
//! - `kp_replay(log, map, ticks, dump_at)`: play `log` on the baked map
//!   bytes for `ticks` ticks (0: the recording's length); resolves to
//!   `[trail, summary, dumps]`, `dumps` being the readable unit state at
//!   each tick of `dump_at` (for the bisect), `== tick <n>` headed.
//!
//! The same harness as native ([`super::replay::play`]), the map served
//! from an in-memory asset source.

use wasm_bindgen::prelude::*;

use super::replay::{self, MapInput, PlayOptions, Replay};

fn console_error(message: &str) {
    let global = js_sys::global();
    let error = js_sys::Reflect::get(&global, &"console".into())
        .and_then(|console| js_sys::Reflect::get(&console, &"error".into()).map(|f| (console, f)));
    if let Ok((console, error)) = error
        && let Ok(error) = error.dyn_into::<js_sys::Function>()
    {
        let _ = error.call1(&console, &message.into());
    }
}

/// Panics reach the runner's console instead of a bare `unreachable`.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| console_error(&format!("panic: {info}"))));
    });
}

/// The map stem `log` plays on.
#[wasm_bindgen]
pub fn kp_replay_map(log: &[u8]) -> Result<String, JsError> {
    install_panic_hook();
    Ok(Replay::decode(log)?.setup.map)
}

/// Play `log` on `map` (the baked `.kpmap`) for `ticks` ticks (0: all
/// the recording's); resolves to `[trail, summary, dumps]`.
#[wasm_bindgen]
pub async fn kp_replay(
    log: Vec<u8>,
    map: Vec<u8>,
    ticks: u32,
    dump_at: Vec<u32>,
) -> Result<js_sys::Array, JsError> {
    install_panic_hook();
    let replay = Replay::decode(&log)?;
    let input = MapInput::in_memory(&replay.setup.map, map);
    let options = PlayOptions {
        ticks: (ticks > 0).then_some(ticks),
        dump_at: dump_at.into_iter().map(|t| t as usize).collect(),
        ..Default::default()
    };
    let played = replay::play(&replay, input, &options).await?;
    let out = js_sys::Array::new();
    out.push(&replay::trail_text(&played.hashes).into());
    out.push(&replay::summary(&replay.setup.map, &replay, &played).into());
    let mut dumps = String::new();
    for (tick, lines) in &played.dumps {
        dumps.push_str(&format!("== tick {tick}\n"));
        for line in lines {
            dumps.push_str(line);
            dumps.push('\n');
        }
    }
    out.push(&dumps.into());
    Ok(out)
}
