//! Packages SMF, SMT, and SMD files into an SDZ (zip) archive.

use std::io::Write;
use std::path::Path;

use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use crate::MapGenError;

/// Package the generated map files into an SDZ archive.
///
/// The archive will contain:
/// - `maps/{map_name}.smf`
/// - `maps/{map_name}.smt`
/// - `maps/{map_name}.smd`
pub fn package_sdz(
    output_path: &Path,
    map_name: &str,
    smf_data: &[u8],
    smt_data: &[u8],
    smd_text: &str,
) -> Result<(), MapGenError> {
    let file = std::fs::File::create(output_path)?;
    let mut zip = zip::ZipWriter::new(file);

    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    zip.start_file(format!("maps/{map_name}.smf"), options)?;
    zip.write_all(smf_data)?;

    zip.start_file(format!("maps/{map_name}.smt"), options)?;
    zip.write_all(smt_data)?;

    let text_options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file(format!("maps/{map_name}.smd"), text_options)?;
    zip.write_all(smd_text.as_bytes())?;

    zip.finish()?;
    Ok(())
}

/// Package into an in-memory SDZ (zip) buffer instead of writing to disk.
#[cfg_attr(not(test), allow(dead_code))]
pub fn package_sdz_to_memory(
    map_name: &str,
    smf_data: &[u8],
    smt_data: &[u8],
    smd_text: &str,
) -> Result<Vec<u8>, MapGenError> {
    let cursor = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(cursor);

    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    zip.start_file(format!("maps/{map_name}.smf"), options)?;
    zip.write_all(smf_data)?;

    zip.start_file(format!("maps/{map_name}.smt"), options)?;
    zip.write_all(smt_data)?;

    let text_options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file(format!("maps/{map_name}.smd"), text_options)?;
    zip.write_all(smd_text.as_bytes())?;

    let cursor = zip.finish()?;
    Ok(cursor.into_inner())
}
