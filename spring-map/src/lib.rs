pub mod map_types;
pub mod sd7_archive;
pub mod smf_parser;
pub mod smt_parser;

use std::path::Path;

use map_types::{GroundTexture, MapError, ParsedMap};
use sd7_archive::load_map_archive;
use smf_parser::parse_smf;
use smt_parser::{assemble_ground_texture, parse_smt_tiles};

/// Result of loading a Spring map from an archive.
pub struct SpringMap {
    pub parsed: ParsedMap,
    /// Ground texture assembled from SMT tiles, if available.
    pub ground_texture: Option<GroundTexture>,
    /// Raw SMF binary data (retained for callers that need it).
    pub smf_data: Vec<u8>,
}

/// Load a Spring map from a .sd7, .sdz, or raw .smf file.
///
/// This is the main entry point for the library. It handles archive
/// extraction, SMF parsing, SMT tile decoding, and texture assembly.
pub fn load_map(path: &Path) -> Result<SpringMap, MapError> {
    let extracted = load_map_archive(path)?;
    let parsed = parse_smf(&extracted.smf_data)?;

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

    Ok(SpringMap {
        parsed,
        ground_texture,
        smf_data: extracted.smf_data,
    })
}
