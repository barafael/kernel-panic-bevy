use std::io::{Cursor, Read};
use std::path::Path;

use crate::map_types::ArchiveError;

/// Extracted map data from an archive.
#[derive(Debug)]
pub struct ExtractedMap {
    pub smf_data: Vec<u8>,
    pub smf_name: String,
    pub smt_data: Option<Vec<u8>>,
    /// Raw text of the .smd metadata file (if found).
    pub smd_text: Option<String>,
    /// Lua files found in the archive: `(path, content)` pairs.
    /// Includes gadgets, mapinfo.lua, featureplacer scripts, etc.
    pub lua_files: Vec<(String, String)>,
}

/// Load map data from a .sd7, .sdz, or raw .smf file.
pub fn load_map_archive(path: &Path) -> Result<ExtractedMap, ArchiveError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "sd7" => extract_from_7z(path),
        "sdz" => extract_from_zip(path),
        "smf" => {
            let data = std::fs::read(path)?;
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let smt_path = path.with_extension("smt");
            let smt_data = std::fs::read(&smt_path)
                .inspect_err(|e| {
                    eprintln!(
                        "Note: companion .smt not found at {}: {e}",
                        smt_path.display()
                    );
                })
                .ok();
            let smd_path = path.with_extension("smd");
            let smd_text = std::fs::read_to_string(&smd_path).ok();
            Ok(ExtractedMap {
                smf_data: data,
                smf_name: name,
                smt_data,
                smd_text,
                lua_files: vec![],
            })
        }
        other => Err(ArchiveError::UnsupportedFormat(other.to_string())),
    }
}

fn extract_from_7z(path: &Path) -> Result<ExtractedMap, ArchiveError> {
    let file_data = std::fs::read(path)?;
    let cursor = Cursor::new(file_data);
    let mut archive =
        sevenz_rust::SevenZReader::new(cursor, path.metadata()?.len(), Default::default())
            .map_err(|e| ArchiveError::SevenZ(e.to_string()))?;

    let mut smf_data: Option<Vec<u8>> = None;
    let mut smt_data: Option<Vec<u8>> = None;
    let mut smd_text: Option<String> = None;
    let mut lua_files: Vec<(String, String)> = Vec::new();
    let mut smf_name = String::new();

    archive
        .for_each_entries(|entry, reader| {
            let name = entry.name().to_string();
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".smf") && smf_data.is_none() {
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf)?;
                smf_name = name;
                smf_data = Some(buf);
            } else if lower.ends_with(".smt") && smt_data.is_none() {
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf)?;
                smt_data = Some(buf);
            } else if lower.ends_with(".smd") && smd_text.is_none() {
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf)?;
                smd_text = Some(String::from_utf8_lossy(&buf).into_owned());
            } else if lower.ends_with(".lua") {
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf)?;
                lua_files.push((name, String::from_utf8_lossy(&buf).into_owned()));
            }
            Ok(true) // always continue — Lua files can be anywhere
        })
        .map_err(|e| ArchiveError::SevenZ(e.to_string()))?;

    match smf_data {
        Some(data) => Ok(ExtractedMap {
            smf_data: data,
            smf_name,
            smt_data,
            smd_text,
            lua_files,
        }),
        None => Err(ArchiveError::NoSmfFound),
    }
}

fn extract_from_zip(path: &Path) -> Result<ExtractedMap, ArchiveError> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    let mut smf_data: Option<Vec<u8>> = None;
    let mut smt_data: Option<Vec<u8>> = None;
    let mut smd_text: Option<String> = None;
    let mut lua_files: Vec<(String, String)> = Vec::new();
    let mut smf_name = String::new();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".smf") && smf_data.is_none() {
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            smf_name = name;
            smf_data = Some(buf);
        } else if lower.ends_with(".smt") && smt_data.is_none() {
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            smt_data = Some(buf);
        } else if lower.ends_with(".smd") && smd_text.is_none() {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            smd_text = Some(String::from_utf8_lossy(&buf).into_owned());
        } else if lower.ends_with(".lua") {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            lua_files.push((name, String::from_utf8_lossy(&buf).into_owned()));
        }
    }

    match smf_data {
        Some(data) => Ok(ExtractedMap {
            smf_data: data,
            smf_name,
            smt_data,
            smd_text,
            lua_files,
        }),
        None => Err(ArchiveError::NoSmfFound),
    }
}

/// Extract only the SMT data from an archive.
pub fn load_smt_from_archive(path: &Path) -> Result<Vec<u8>, ArchiveError> {
    let extracted = load_map_archive(path)?;
    extracted.smt_data.ok_or(ArchiveError::NoSmtFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn maps_dir() -> PathBuf {
        let candidates = [
            PathBuf::from("kernel-panic/assets/maps"),
            PathBuf::from("assets/maps"),
        ];
        candidates
            .into_iter()
            .find(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from("assets/maps"))
    }

    #[test]
    fn extract_sd7_marble_madness() {
        let path = maps_dir().join("Marble_Madness_Map.sd7");
        if !path.exists() {
            eprintln!("Skipping: {path:?} not found");
            return;
        }
        let extracted = load_map_archive(&path).expect("should extract .sd7");
        assert!(extracted.smf_name.ends_with(".smf"));
        assert_eq!(&extracted.smf_data[..16], b"spring map file\0");
    }

    #[test]
    fn extract_sdz_speed_balls() {
        let path = maps_dir().join("Speed_Balls_16_Way.sdz");
        if !path.exists() {
            eprintln!("Skipping: {path:?} not found");
            return;
        }
        let extracted = load_map_archive(&path).expect("should extract .sdz");
        assert!(extracted.smf_name.ends_with(".smf"));
        assert_eq!(&extracted.smf_data[..16], b"spring map file\0");
    }

    #[test]
    fn extract_all_maps() {
        let dir = maps_dir();
        if !dir.exists() {
            eprintln!("Skipping: maps directory not found");
            return;
        }
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext == "sd7" || ext == "sdz" {
                let extracted = load_map_archive(&path)
                    .unwrap_or_else(|e| panic!("failed to extract {}: {e}", path.display()));
                assert_eq!(
                    &extracted.smf_data[..16],
                    b"spring map file\0",
                    "bad magic in {}",
                    path.display()
                );
                count += 1;
            }
        }
        eprintln!("Successfully extracted {count} map archives");
        assert!(count > 0, "expected at least one map archive");
    }
}
