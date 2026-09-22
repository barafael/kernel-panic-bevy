//! Pre-captured Lua gadget outputs, applied as data.
//!
//! Two of the shipped maps drive their terrain/layout from Lua gadgets
//! run by the engine at load time:
//!
//! * **Palladium** — `PalladiumHeight.lua` carves the floating platforms
//!   into the heightmap during `Initialize()`.
//! * **Hex Farm 8** — `HexFarm8.lua` carves the hex-pit terrain *and*
//!   reports the tower/bridge layout + skin via `SendToUnsynced`, plus a
//!   `mapinfo.lua` (modern maps ship that instead of `.smd`).
//!
//! Running real Lua (mlua, a vendored C build) to re-derive deterministic
//! outputs on every load was the last interpreter in the map pipeline.
//! This module replaces it the same way `spring-cob` was retired: the
//! gadgets' observable outputs were captured once (final heightmap,
//! `SendToUnsynced` message stream, parsed mapinfo — see the
//! `bake_lua` tool history) and are checked in as postcard+deflate blobs
//! under `src/data/`. Applying them is a memcpy and a message walk; the
//! layout/skin compositing downstream is unchanged pure Rust.
//!
//! Raw `.sd7` maps *without* a captured gadget load exactly as before;
//! a third-party Lua-gadget map needs re-capturing (bake with the old
//! pipeline once, drop in a new blob).

use std::io::Read;
use std::sync::OnceLock;

use crate::map_types::{LuaFile, UnsyncedMessage};
use crate::smd_parser::MapInfo;

/// The captured outputs of one map's gadget run.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BakedGadgets {
    /// Final heightmap after every `Spring.SetHeightMap` call.
    heights: Vec<f32>,
    /// Every `SendToUnsynced(...)` the gadgets issued, in order.
    pub unsynced_messages: Vec<UnsyncedMessage>,
    /// Parsed `mapinfo.lua`, for maps without `.smd`.
    pub map_info: Option<MapInfo>,
}

impl BakedGadgets {
    /// Overwrite `parsed.heights` with the captured post-gadget terrain.
    pub fn apply_heights(&self, parsed: &mut crate::map_types::ParsedMap) {
        assert_eq!(
            parsed.heights.len(),
            self.heights.len(),
            "baked heightmap size mismatch — archive and capture disagree"
        );
        parsed.heights.copy_from_slice(&self.heights);
    }
}

/// Decode a checked-in blob (postcard body, deflated). Runs once per
/// process, on first use — map loading is a one-shot startup path.
fn decode<T: serde::de::DeserializeOwned>(bytes: &'static [u8]) -> T {
    let mut decoder = flate2::read::DeflateDecoder::new(bytes);
    let mut body = Vec::new();
    decoder.read_to_end(&mut body).expect("baked blob inflates");
    postcard::from_bytes(&body).expect("baked blob deserializes")
}

fn palladium() -> &'static BakedGadgets {
    static ONCE: OnceLock<BakedGadgets> = OnceLock::new();
    ONCE.get_or_init(|| decode(include_bytes!("data/palladium.luabake").as_slice()))
}

fn hexfarm8() -> &'static BakedGadgets {
    static ONCE: OnceLock<BakedGadgets> = OnceLock::new();
    ONCE.get_or_init(|| decode(include_bytes!("data/hexfarm8.luabake").as_slice()))
}

/// Match a captured map by its gadget file, so renamed/archived copies
/// still resolve.
fn gadget_matches(lua_files: &[LuaFile], gadget: &str) -> bool {
    lua_files
        .iter()
        .any(|f| f.path.to_ascii_lowercase().ends_with(gadget))
}

/// Find the captured gadget outputs for a map archive, if it is one of
/// the Lua-driven maps. `None` = load without terrain edits, exactly
/// like an archive whose gadgets don't exist.
pub fn lookup(lua_files: &[LuaFile]) -> Option<&'static BakedGadgets> {
    if gadget_matches(lua_files, "palladiumheight.lua") {
        Some(palladium())
    } else if gadget_matches(lua_files, "hexfarm8.lua") {
        Some(hexfarm8())
    } else {
        None
    }
}
