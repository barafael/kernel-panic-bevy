//! Pre-captured Lua gadget outputs, applied as data.
//!
//! Two of the shipped maps drive their terrain/layout from Lua gadgets
//! run by the engine at load time:
//!
//! * **Palladium** — `PalladiumHeight.lua` carves the floating platforms
//!   into the heightmap during `Initialize()`.
//! * **Hex Farm 8** — `HexFarm8.lua` rolls a new random layout every
//!   game, so a capture can't stand in for it: the gadget itself is
//!   ported ([`crate::hexfarm`]) and runs per match. Only its parsed
//!   `mapinfo.lua` (modern maps ship that instead of `.smd`) is kept
//!   here, with an empty heightmap / message list.
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
    /// No-op for a capture without terrain (Hex Farm's is generated per
    /// match). Captures are matched by gadget file name, so a different
    /// build of the map (another size) can reach here: that is a load
    /// error, not a panic on the loader thread.
    pub fn apply_heights(
        &self,
        parsed: &mut crate::map_types::ParsedMap,
    ) -> Result<(), crate::map_types::MapError> {
        if self.heights.is_empty() {
            return Ok(());
        }
        if parsed.heights.len() != self.heights.len() {
            return Err(crate::map_types::MapError::GadgetCaptureMismatch {
                expected: self.heights.len(),
                actual: parsed.heights.len(),
            });
        }
        parsed.heights.copy_from_slice(&self.heights);
        Ok(())
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

/// Is this archive Hex Farm (its gadget, however the archive is named)?
pub fn is_hex_farm(lua_files: &[LuaFile]) -> bool {
    gadget_matches(lua_files, "hexfarm8.lua")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_types::{MapError, ParsedMap, SmfHeader};

    /// A capture applied to a map of another size is a load error the
    /// caller can report, not a panic on the loader thread.
    #[test]
    fn mismatched_capture_is_an_error() {
        let header = SmfHeader::new_flat(4, 4, 0.0, 1.0);
        let mut parsed = ParsedMap {
            heights: vec![0.0; header.heightmap_len()],
            header,
            features: Vec::new(),
            metalmap: Vec::new(),
        };
        assert!(matches!(
            palladium().apply_heights(&mut parsed),
            Err(MapError::GadgetCaptureMismatch { .. })
        ));
        assert!(parsed.heights.iter().all(|&h| h == 0.0), "left untouched");
        // Hex Farm's capture carries no terrain: always a no-op.
        assert!(hexfarm8().apply_heights(&mut parsed).is_ok());
    }
}
