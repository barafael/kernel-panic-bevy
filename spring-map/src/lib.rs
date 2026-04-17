pub mod lua_heightmap;
pub mod map_types;
pub mod sd7_archive;
pub mod smd_parser;
pub mod smf_parser;
pub mod smt_parser;

use std::path::Path;

use map_types::{GroundTexture, MapError, ParsedMap};
use sd7_archive::load_map_archive;
use smd_parser::MapInfo;
use smf_parser::parse_smf;
use smt_parser::{assemble_ground_texture, parse_smt_tiles};

/// Result of loading a Spring map from an archive.
pub struct SpringMap {
    pub parsed: ParsedMap,
    pub ground_texture: Option<GroundTexture>,
    pub map_info: Option<MapInfo>,
    pub smf_data: Vec<u8>,
}

/// Load a Spring map from a .sd7, .sdz, or raw .smf file.
///
/// Handles archive extraction, SMF parsing, Lua heightmap gadget execution,
/// SMT tile decoding, .smd metadata parsing, and texture assembly.
pub fn load_map(path: &Path) -> Result<SpringMap, MapError> {
    let extracted = load_map_archive(path)?;
    let mut parsed = parse_smf(&extracted.smf_data)?;

    // Execute any Lua heightmap gadgets (e.g., Palladium's platform system).
    let gadget_count =
        lua_heightmap::apply_lua_heightmap_gadgets(&mut parsed, &extracted.lua_files);
    if gadget_count > 0 {
        eprintln!("Applied {gadget_count} Lua heightmap gadget(s)");
    }

    let ground_texture = match &extracted.smt_data {
        Some(smt_data) => {
            let tiles = parse_smt_tiles(smt_data)?;
            Some(assemble_ground_texture(
                &extracted.smf_data,
                &parsed,
                &tiles,
            )?)
        }
        None => None,
    };

    let map_info = extracted.smd_text.as_deref().map(smd_parser::parse_smd);

    Ok(SpringMap {
        parsed,
        ground_texture,
        map_info,
        smf_data: extracted.smf_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn maps_dir() -> Option<PathBuf> {
        // Try relative paths from both workspace root and crate root,
        // plus an absolute path derived from CARGO_MANIFEST_DIR.
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().unwrap_or(&manifest_dir);

        [
            workspace_root.join("kernel-panic/assets/maps"),
            PathBuf::from("kernel-panic/assets/maps"),
            PathBuf::from("assets/maps"),
        ]
        .into_iter()
        .find(|p| p.is_dir())
    }

    /// Load every .sd7/.sdz map through the full pipeline and verify
    /// the output is sane: heightmap has data, features parsed, texture
    /// assembled, .smd metadata present.
    #[test]
    fn load_all_maps_end_to_end() {
        let Some(dir) = maps_dir() else {
            eprintln!("Skipping: maps directory not found");
            return;
        };

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
