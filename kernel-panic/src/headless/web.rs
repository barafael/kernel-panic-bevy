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
//!   each tick of `dump_at` (for the bisect), `== tick <n>` headed;
//! - `kp_net_peer(signaling, ice, room, map_name, map, humans, ai, seed,
//!   secs)`: play one peer of a networked match over WebRTC
//!   (`net::matchbox`; `ice` in `KP_ICE` syntax, empty: the default
//!   servers) for `secs` seconds once the room is full, the browser side of the
//!   browser↔native test (`.github/scripts/kp-netpeer.mjs`,
//!   `net::matchbox_tests::browser_peer`); resolves to the trail,
//!   `<tick> <hash>` per line;
//! - `kp_net_probe(signaling, ice, room, secs)`: the browser end of the
//!   transport probe (`net::matchbox_tests::browser_probe`): once one
//!   other peer is in the room, a 64-byte packet numbered from 0 every
//!   33 ms on the unreliable channel for `secs` seconds.
//!
//! The same harness as native ([`super::replay::play`],
//! [`super::netplay`]), the map served from an in-memory asset source.

use wasm_bindgen::prelude::*;

use super::replay::{self, MapInput, PlayOptions, Replay};
use super::{SimHarness, netplay};

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

/// Resolve after `ms` milliseconds (`setTimeout`).
async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let global = js_sys::global();
        let set_timeout = js_sys::Reflect::get(&global, &"setTimeout".into())
            .ok()
            .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
            .expect("no global setTimeout");
        let _ = set_timeout.call2(&global, &resolve, &ms.into());
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// The ICE servers of `spec` ([`IceServer::parse_list`] syntax); empty:
/// none given (matchbox's Google STUN in a browser).
///
/// [`IceServer::parse_list`]: crate::net::matchbox::IceServer::parse_list
fn ice_servers(spec: &str) -> Result<Vec<crate::net::matchbox::IceServer>, JsError> {
    if spec.trim().is_empty() {
        return Ok(Vec::new());
    }
    crate::net::matchbox::IceServer::parse_list(spec).map_err(|e| JsError::new(&e))
}

/// One peer of the networked match [`netplay::scenario`]`(map_name,
/// humans, ai, seed)` in room `room` of the signaling server
/// `signaling`, with the ICE servers `ice` (see [`ice_servers`]), played
/// for `secs` seconds of wall time after everyone is in; resolves to its
/// trail.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub async fn kp_net_peer(
    signaling: String,
    ice: String,
    room: String,
    map_name: String,
    map: Vec<u8>,
    humans: u8,
    ai: u8,
    seed: u32,
    secs: f64,
) -> Result<String, JsError> {
    use crate::net::matchbox::{MatchboxTransport, NetConfig, RoomId};
    use crate::net::transport::Transport;
    use bevy::platform::time::Instant;
    use std::time::Duration;

    install_panic_hook();
    let room: RoomId = room.parse()?;
    let input = MapInput::in_memory(&map_name, map);
    let mut scenario = netplay::scenario(&map_name, humans, ai, seed as u64);
    scenario.map_file = Some(input.file.clone());
    let h = SimHarness::start(scenario, input.assets).await;
    let cfg = NetConfig {
        signaling,
        ice: ice_servers(&ice)?,
    };
    let mut t = MatchboxTransport::open(&cfg.room_url(&room), &cfg);
    let mut rv = netplay::Rendezvous::default();
    let t0 = Instant::now();
    let ids = loop {
        if let Some(ids) = rv.poll(&mut t, humans as usize - 1) {
            break ids;
        }
        if let Some(why) = t.failure() {
            return Err(JsError::new(&why));
        }
        if t0.elapsed() > Duration::from_secs(120) {
            return Err(JsError::new("the room never filled"));
        }
        sleep_ms(10).await;
    };
    let me = t.local_id().expect("an id once connected");
    console_error(&format!(
        "browser peer {me}: room of {} in {:?}",
        ids.len(),
        t0.elapsed()
    ));
    let mut peer = netplay::NetPeer::join(h, me, &ids, humans, seed as u64, Box::new(t));
    let start = Instant::now();
    let mut last = start;
    while start.elapsed().as_secs_f64() < secs {
        let now = Instant::now();
        let dt = (now - last).min(Duration::from_millis(100));
        last = now;
        peer.frame(dt);
        if peer.session(|s| s.ended().is_some()) {
            break;
        }
        // WebRTC and the timers run on the event loop.
        sleep_ms(4).await;
    }
    Ok(netplay::trail_text(&peer.trail()))
}

/// The browser end of `net::matchbox_tests::browser_probe`; resolves to
/// how many packets it sent.
#[wasm_bindgen]
pub async fn kp_net_probe(
    signaling: String,
    ice: String,
    room: String,
    secs: f64,
) -> Result<String, JsError> {
    use crate::net::matchbox::{MatchboxTransport, NetConfig, RoomId};
    use crate::net::transport::{Channel, Transport};
    use bevy::platform::time::Instant;
    use std::time::Duration;

    install_panic_hook();
    let room: RoomId = room.parse()?;
    let cfg = NetConfig {
        signaling,
        ice: ice_servers(&ice)?,
    };
    let mut t = MatchboxTransport::open(&cfg.room_url(&room), &cfg);
    let mut rv = netplay::Rendezvous::default();
    let t0 = Instant::now();
    let ids = loop {
        if let Some(ids) = rv.poll(&mut t, 1) {
            break ids;
        }
        if t0.elapsed() > Duration::from_secs(120) {
            return Err(JsError::new("nobody came"));
        }
        sleep_ms(10).await;
    };
    let me = t.local_id().expect("an id once connected");
    let peer = *ids.iter().find(|&&p| p != me).expect("the other peer");
    let start = Instant::now();
    let mut seq = 0u64;
    while start.elapsed().as_secs_f64() < secs {
        if start.elapsed() >= Duration::from_millis(33 * seq) {
            let mut m = vec![0u8; 64];
            m[..8].copy_from_slice(&seq.to_le_bytes());
            t.send(peer, Channel::Unreliable, m);
            seq += 1;
        }
        t.poll_peers();
        t.receive(Channel::Unreliable);
        sleep_ms(2).await;
    }
    Ok(format!("{seq} packets sent\n"))
}
