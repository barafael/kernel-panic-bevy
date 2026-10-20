//! The game's maps: the `.kpmap` reader ([`baked`]) and the terrain code
//! that runs on what it reads (Hex Farm's per-match layout, datavent
//! facings, the aircraft smooth mesh).
//!
//! Every shipped map is a committed `.kpmap` (`kernel-panic/assets/maps`),
//! converted once from the original Spring archives; the converter and
//! the Spring `.sd7` / `.smf` / `.smt` / `.smd` readers it was built on
//! are in the git history (see README.md).

pub mod baked;
pub mod cells;
pub mod datavent;
pub mod hexfarm;
#[cfg(feature = "write")]
pub mod kpgen;
pub mod lua_layout;
pub mod lua_skin;
pub mod map_info;
pub mod map_types;
pub mod ring;
pub mod shifting;
pub mod smooth_mesh;

use lua_skin::SkinAtlas;
use map_info::MapInfo;
use map_types::{GroundTexture, ParsedMap, TerrainTypes};

/// The game's shipped maps (`kernel-panic/assets/maps`), located from
/// this crate's manifest — `cargo test` runs with the crate directory as
/// cwd, so workspace-relative paths never resolve.
#[cfg(test)]
pub(crate) fn shipped_maps_dir() -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../kernel-panic/assets/maps");
    assert!(dir.is_dir(), "shipped maps not found at {}", dir.display());
    dir
}

/// Hex Farm's Lua-drawn world: the skin atlas its towers and bridges
/// are textured with. The layout itself is rolled per match by
/// [`hexfarm::HexFarm::generate`], as the gadget does at `Initialize()`.
///
/// Its presence also means the map is drawn Lua-only: Hex Farm's ground
/// is hidden by `voidGround` (mapinfo + KP's `hotfixes.lua` ~l.207), so
/// there is no `ground_texture` for it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LuaCompositing {
    pub atlas: SkinAtlas,
}

/// A loaded map.
pub struct SpringMap {
    pub parsed: ParsedMap,
    pub ground_texture: Option<GroundTexture>,
    pub map_info: Option<MapInfo>,
    /// What [`ParsedMap::typemap`] indexes: per-type move speeds.
    pub terrain_types: TerrainTypes,
    pub lua_compositing: Option<LuaCompositing>,
    /// Every geovent with the facing a factory on it takes
    /// ([`datavent::datavent_facings`]).
    pub datavents: Vec<map_types::Datavent>,
}
