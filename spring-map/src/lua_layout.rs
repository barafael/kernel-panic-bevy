//! The HexFarm layout as the gadget's unsynced half sees it.
//!
//! The synced half of `HexFarm8.lua` packs each hex tower / connecting
//! bridge into a single `SendToUnsynced("ReceiveHexFarmLayout", kind, n,
//! ...)` call (`SendHexFarmToUnsynced`, lines 1466-1493): corners,
//! heights, vent flag, visibility. This is that payload as a flat Rust
//! struct — produced by [`crate::hexfarm::HexFarm::layout`] — which the
//! renderer builds meshes from without knowing about the synced state.

/// One hex tower from the captured layout.
///
/// `corners` are the six corner positions on the top face (all at `y =
/// center.y`). `corner_bridges[k]` is the bridge ID at the side
/// starting at corner k+1 — non-zero means a bridge connects out of
/// that side, which the gadget renders with a different UV region.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HexBridge {
    pub hex1: i64,
    pub hex2: i64,
    pub corners: [[f32; 3]; 4],
    pub hidden: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HexFarmLayout {
    pub skin: Option<i64>,
    pub team_colored: bool,
    pub hexes: Vec<HexTower>,
    pub bridges: Vec<HexBridge>,
}
