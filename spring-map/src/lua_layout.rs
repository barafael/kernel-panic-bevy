//! Reconstruct a HexFarm layout from captured `SendToUnsynced(...)` calls.
//!
//! The synced half of `HexFarm8.lua` packs each hex tower / connecting
//! bridge into a single `SendToUnsynced("ReceiveHexFarmLayout", kind, n,
//! ...)` call (see lines 1471–1493 of the gadget). We capture those
//! messages, walk them, and produce a flat Rust struct the renderer
//! can build meshes from without knowing about Lua at all.

use crate::lua_heightmap::LuaGadgetResult;
use crate::map_types::{UnsyncedArg, UnsyncedMessage};

/// One hex tower from the captured layout.
///
/// `corners` are the six corner positions on the top face (all at `y =
/// center.y`). `corner_bridges[k]` is the bridge ID at the side
/// starting at corner k+1 — non-zero means a bridge connects out of
/// that side, which the gadget renders with a different UV region.
#[derive(Debug, Clone)]
pub struct HexTower {
    pub center: [f32; 3],
    pub g: i64,
    pub corners: [[f32; 3]; 6],
    pub corner_bridges: [i64; 6],
    pub hidden: bool,
}

/// One bridge connecting two hex towers. Four corners, top face only —
/// the gadget extrudes the sides downwards by `VisualBridgeThickness`
/// at draw time.
#[derive(Debug, Clone)]
pub struct HexBridge {
    pub hex1: i64,
    pub hex2: i64,
    pub corners: [[f32; 3]; 4],
    pub hidden: bool,
}

#[derive(Debug, Clone, Default)]
pub struct HexFarmLayout {
    pub skin: Option<i64>,
    pub team_colored: bool,
    pub hexes: Vec<HexTower>,
    pub bridges: Vec<HexBridge>,
}

impl HexFarmLayout {
    /// Walk every captured `SendToUnsynced(...)` call and assemble the
    /// layout. Returns `None` if no messages mention HexFarm.
    pub fn from_gadget_results(gadget_results: &[LuaGadgetResult]) -> Option<Self> {
        let mut layout = Self::default();
        let mut seen_any = false;

        for result in gadget_results {
            for msg in &result.unsynced_messages {
                if !is_hex_farm_message(msg) {
                    continue;
                }
                seen_any = true;
                match msg.get(1).and_then(UnsyncedArg::as_str) {
                    Some(kind) if kind.eq_ignore_ascii_case("hex") => {
                        if let Some(hex) = parse_hex(msg) {
                            layout.hexes.push(hex);
                        }
                    }
                    Some(kind) if kind.eq_ignore_ascii_case("rect") => {
                        if let Some(rect) = parse_rect(msg) {
                            layout.bridges.push(rect);
                        }
                    }
                    Some(kind) if kind.eq_ignore_ascii_case("Skin") => {
                        layout.skin = msg.get(2).and_then(UnsyncedArg::as_i64);
                    }
                    Some(kind) if kind.eq_ignore_ascii_case("TeamColoredMapTexture") => {
                        layout.team_colored = match msg.get(2) {
                            Some(UnsyncedArg::Bool(b)) => *b,
                            Some(UnsyncedArg::Integer(i)) => *i != 0,
                            Some(UnsyncedArg::Number(n)) => *n != 0.0,
                            _ => false,
                        };
                    }
                    _ => {}
                }
            }
        }

        if seen_any { Some(layout) } else { None }
    }
}

fn is_hex_farm_message(msg: &UnsyncedMessage) -> bool {
    msg.first()
        .and_then(UnsyncedArg::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("ReceiveHexFarmLayout"))
}

/// Hex message layout (mirrors `SendHexFarmToUnsynced` lines 1471–1483):
/// `[0]="ReceiveHexFarmLayout", [1]="hex", [2]=n, [3..6]=x,y,z,g,
///  [7..24]=c1.x..c6.z, [25..30]=cb1..cb6, [31]=hidden`.
fn parse_hex(msg: &UnsyncedMessage) -> Option<HexTower> {
    let arg = |i: usize| -> Option<&UnsyncedArg> { msg.get(i) };
    let f = |i: usize| arg(i).and_then(num_to_f32);
    let n = |i: usize| arg(i).and_then(UnsyncedArg::as_i64);

    let cx = f(3)?;
    let cy = f(4)?;
    let cz = f(5)?;
    let g = n(6)?;

    let mut corners = [[0.0; 3]; 6];
    for k in 0..6 {
        let base = 7 + k * 3;
        corners[k] = [f(base)?, f(base + 1)?, f(base + 2)?];
    }
    let mut corner_bridges = [0i64; 6];
    for k in 0..6 {
        corner_bridges[k] = n(25 + k).unwrap_or(0);
    }
    let hidden = n(31).unwrap_or(0) != 0;

    Some(HexTower {
        center: [cx, cy, cz],
        g,
        corners,
        corner_bridges,
        hidden,
    })
}

/// Rect (bridge) message layout: `[0]="ReceiveHexFarmLayout",
/// [1]="rect", [2]=n, [3..4]=hex1,hex2, [5..16]=c1.x..c4.z, [17]=hidden`.
fn parse_rect(msg: &UnsyncedMessage) -> Option<HexBridge> {
    let arg = |i: usize| -> Option<&UnsyncedArg> { msg.get(i) };
    let f = |i: usize| arg(i).and_then(num_to_f32);
    let n = |i: usize| arg(i).and_then(UnsyncedArg::as_i64);

    let hex1 = n(3)?;
    let hex2 = n(4)?;
    let mut corners = [[0.0; 3]; 4];
    for k in 0..4 {
        let base = 5 + k * 3;
        corners[k] = [f(base)?, f(base + 1)?, f(base + 2)?];
    }
    let hidden = n(17).unwrap_or(0) != 0;

    Some(HexBridge {
        hex1,
        hex2,
        corners,
        hidden,
    })
}

fn num_to_f32(arg: &UnsyncedArg) -> Option<f32> {
    match arg {
        UnsyncedArg::Number(n) => Some(*n as f32),
        UnsyncedArg::Integer(i) => Some(*i as f32),
        _ => None,
    }
}
