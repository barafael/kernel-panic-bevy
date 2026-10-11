pub mod baked;
pub mod datavent;
pub mod gadget_bake;
pub mod hexfarm;
// Source-archive extraction is native-only: `sevenz-rust` assumes
// native std fs and doesn't build for wasm32 (plan §8.1). Web loads
// baked `.kpmap` files through `baked::read_baked_map` instead. The
// Lua gadget *runner* is gone entirely — its deterministic outputs for
// the two Lua-driven maps are captured in `gadget_bake`. The HexFarm
// layout/skin types are plain data (they round-trip through `.kpmap`
// v4), so they build everywhere.
pub mod lua_layout;
pub mod lua_skin;
pub mod map_types;
#[cfg(not(target_arch = "wasm32"))]
pub mod sd7_archive;
pub mod smd_parser;
pub mod smf_parser;
pub mod smooth_mesh;
pub mod smt_parser;

use std::path::Path;

use lua_skin::SkinAtlas;

/// The game's shipped maps (`kernel-panic/assets/maps`, tracked in the
/// repository), located from this crate's manifest — `cargo test` runs
/// with the crate directory as cwd, so workspace-relative paths never
/// resolve. Missing maps fail the test instead of silently skipping it.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn shipped_maps_dir() -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../kernel-panic/assets/maps");
    assert!(dir.is_dir(), "shipped maps not found at {}", dir.display());
    dir
}

/// One shipped map archive by file name (see [`shipped_maps_dir`]).
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn shipped_map(file_name: &str) -> std::path::PathBuf {
    let path = shipped_maps_dir().join(file_name);
    assert!(path.is_file(), "shipped map not found: {}", path.display());
    path
}
use map_types::{GroundTexture, MapError, ParsedMap, TerrainTypes};
#[cfg(not(target_arch = "wasm32"))]
use sd7_archive::load_map_archive;
use smd_parser::MapInfo;
#[cfg(not(target_arch = "wasm32"))]
use smf_parser::parse_smf;
#[cfg(not(target_arch = "wasm32"))]
use smt_parser::{assemble_ground_texture, parse_smt_tiles};

/// Hex Farm's Lua-drawn world: the skin atlas its towers and bridges
/// are textured with. The layout itself is rolled per match by
/// [`hexfarm::HexFarm::generate`], as the gadget does at `Initialize()`.
///
/// Its presence also means the map is drawn Lua-only: Hex Farm's SMT is
/// fully transparent and hidden by `voidGround` (mapinfo + KP's
/// `hotfixes.lua` ~l.207), so there is no `ground_texture` for it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LuaCompositing {
    pub atlas: SkinAtlas,
}

/// Result of loading a Spring map from an archive.
pub struct SpringMap {
    pub parsed: ParsedMap,
    pub ground_texture: Option<GroundTexture>,
    pub map_info: Option<MapInfo>,
    /// What [`ParsedMap::typemap`] indexes: per-type move speeds.
    pub terrain_types: TerrainTypes,
    pub smf_data: Vec<u8>,
    pub lua_compositing: Option<LuaCompositing>,
    /// Every geovent with the facing a factory on it takes
    /// ([`datavent::datavent_facings`]).
    pub datavents: Vec<map_types::Datavent>,
}

/// Load a Spring map from a .sd7, .sdz, or raw .smf file.
///
/// Handles archive extraction, SMF parsing, Lua heightmap gadget execution,
/// SMT tile decoding, .smd metadata parsing, and texture assembly.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_map(path: &Path) -> Result<SpringMap, MapError> {
    let extracted = load_map_archive(path)?;
    let mut parsed = parse_smf(&extracted.smf_data)?;

    // Apply the pre-captured Lua gadget outputs where the mlua runner
    // used to execute them (see `gadget_bake` for the rationale).
    let baked_gadgets = gadget_bake::lookup(&extracted.lua_files);
    if let Some(baked) = baked_gadgets {
        baked.apply_heights(&mut parsed)?;
        eprintln!(
            "Applied baked Lua gadget terrain ({} unsynced message(s))",
            baked.unsynced_messages.len(),
        );
    }

    // Hex Farm: the gadget is ported (`hexfarm`) and rolls the layout
    // at match start; the archive only contributes its skin atlas.
    let lua_compositing = gadget_bake::is_hex_farm(&extracted.lua_files)
        .then(|| lua_skin::decode_skin_atlas(lua_skin::KERNEL_PANIC_SKIN, &extracted.bitmaps))
        .flatten()
        .map(|atlas| LuaCompositing { atlas });

    // A Lua-composited map (Hex Farm) ships a 100% transparent SMT that
    // the engine hides via `voidGround`: everything visible is the
    // gadget's towers and bridges over a black void. Don't decode or
    // carry the (invisible) ground texture at all — the renderer treats
    // `lua_compositing.is_some()` as voidGround.
    let ground_texture = match (&extracted.smt_data, &lua_compositing) {
        (Some(smt_data), None) => {
            let tiles = parse_smt_tiles(smt_data)?;
            Some(assemble_ground_texture(
                &extracted.smf_data,
                &parsed,
                &tiles,
            )?)
        }
        _ => None,
    };

    let map_info = extracted
        .smd_text
        .as_deref()
        .map(smd_parser::parse_smd)
        .or_else(|| {
            // Modern maps (e.g. Hex Farm) ship mapinfo.lua instead of .smd
            // — pre-parsed and checked in with the gadget capture.
            let info = baked_gadgets?.map_info.clone()?;
            Some(info)
        });
    // Hex Farm's captured mapinfo.lua defines no terrain types: neutral.
    let terrain_types = extracted
        .smd_text
        .as_deref()
        .map(smd_parser::parse_terrain_types)
        .unwrap_or_default();
    let datavents = datavent::datavent_facings(&parsed, &terrain_types);

    Ok(SpringMap {
        parsed,
        ground_texture,
        map_info,
        terrain_types,
        smf_data: extracted.smf_data,
        lua_compositing,
        datavents,
    })
}

/// Web stub: `.sd7`/`.sdz` extraction needs sevenz/zip-from-file and the
/// Lua gadgets need mlua — none of which exist on wasm. Bake maps to
/// `.kpmap` (see `baked`) and load those instead.
#[cfg(target_arch = "wasm32")]
pub fn load_map(_path: &Path) -> Result<SpringMap, MapError> {
    Err(MapError::SourceArchivesUnsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Load every .sd7/.sdz map through the full pipeline and verify
    /// the output is sane: heightmap has data, features parsed, texture
    /// assembled, .smd metadata present.
    #[test]
    fn load_all_maps_end_to_end() {
        let dir = shipped_maps_dir();

        let mut count = 0;
        let mut failures: Vec<String> = Vec::new();

        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext != "sd7" && ext != "sdz" {
                continue;
            }

            let name = path.file_stem().unwrap_or_default().to_string_lossy();

            match load_map(&path) {
                Ok(spring_map) => {
                    let p = &spring_map.parsed;

                    // Heightmap should have the right number of samples.
                    assert_eq!(
                        p.heights.len(),
                        p.header.heightmap_len(),
                        "{name}: heightmap length mismatch"
                    );

                    // Metalmap should have the right size.
                    assert_eq!(
                        p.metalmap.len(),
                        p.header.metalmap_width() * p.header.metalmap_height(),
                        "{name}: metalmap length mismatch"
                    );

                    // Feature section may be empty for minimalist maps (e.g. the
                    // Showcase plain) — just sanity-check that if it *is* populated,
                    // each entry has finite coordinates. Parsing no-features is a
                    // valid outcome, not a bug.
                    for f in &p.features {
                        assert!(
                            f.x.is_finite() && f.y.is_finite() && f.z.is_finite(),
                            "{name}: feature has non-finite coordinates"
                        );
                    }

                    // Ground texture should be present and correctly sized.
                    if let Some(ground) = &spring_map.ground_texture {
                        assert_eq!(
                            ground.pixels.len(),
                            ground.width * ground.height * 4,
                            "{name}: ground texture pixel count mismatch"
                        );
                        assert!(ground.width > 0 && ground.height > 0);
                    }

                    // .smd metadata should be present (all KP maps have it).
                    let map_info = spring_map
                        .map_info
                        .as_ref()
                        .unwrap_or_else(|| panic!("{name}: missing .smd metadata"));

                    // Should have at least 2 start positions.
                    assert!(
                        map_info.start_positions.len() >= 2,
                        "{name}: expected at least 2 start positions, got {}",
                        map_info.start_positions.len()
                    );

                    // Start positions should be within map bounds.
                    let world_w = p.header.world_width();
                    let world_d = p.header.world_depth();
                    for sp in &map_info.start_positions {
                        assert!(
                            sp.x >= 0.0 && sp.x <= world_w && sp.z >= 0.0 && sp.z <= world_d,
                            "{name}: start position team {} at ({}, {}) is out of bounds ({}x{})",
                            sp.team,
                            sp.x,
                            sp.z,
                            world_w,
                            world_d
                        );
                    }

                    eprintln!(
                        "  OK: {name} — {}x{}, {} features, {} starts, texture {}",
                        p.header.map_x,
                        p.header.map_y,
                        p.features.len(),
                        map_info.start_positions.len(),
                        spring_map
                            .ground_texture
                            .as_ref()
                            .map(|g| format!("{}x{}", g.width, g.height))
                            .unwrap_or_else(|| "none".into()),
                    );

                    // Typemap present and sized; report the ground the
                    // terrain types actually slow down or speed up.
                    assert_eq!(
                        p.typemap.len(),
                        p.header.typemap_width() * p.header.typemap_height(),
                        "{name}: typemap length mismatch"
                    );
                    let mut by_speed: Vec<(f32, usize)> = Vec::new();
                    for &t in &p.typemap {
                        let kbot = spring_map.terrain_types.get(t).kbot;
                        match by_speed.iter_mut().find(|(s, _)| *s == kbot) {
                            Some((_, n)) => *n += 1,
                            None => by_speed.push((kbot, 1)),
                        }
                    }
                    eprintln!(
                        "      gravity {}, kbot speed cells {by_speed:?}",
                        map_info.gravity
                    );

                    count += 1;
                }
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
                }
            }
        }

        if !failures.is_empty() {
            panic!(
                "{} map(s) failed to load:\n  {}",
                failures.len(),
                failures.join("\n  ")
            );
        }

        eprintln!("All {count} maps loaded successfully");
        assert!(count >= 13, "expected at least 13 KP maps, got {count}");
    }
}
