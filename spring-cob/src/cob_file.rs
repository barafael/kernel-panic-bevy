//! Parser for compiled COB animation script files.
//!
//! The COB format is a bytecode derived from Total Annihilation's unit
//! animation system. Files contain a header, script/piece name tables,
//! bytecode, and optionally sound name tables (TA:K version 6).

use std::io::{Cursor, Seek, SeekFrom};

use byteorder::{LittleEndian, ReadBytesExt};
use thiserror::Error;

/// A parsed COB file ready for execution by the VM.
#[derive(Debug, Clone)]
pub struct CobFile {
    /// Script function names (e.g. "Create", "AimWeapon1").
    pub script_names: Vec<String>,
    /// Byte offsets into `code` where each script starts (in 32-bit words).
    pub script_offsets: Vec<usize>,
    /// Length of each script in 32-bit words.
    pub script_lengths: Vec<usize>,
    /// Piece names (e.g. "base", "turret", "barrel").
    pub piece_names: Vec<String>,
    /// The bytecode — a flat array of 32-bit integers (opcodes + operands).
    pub code: Vec<i32>,
    /// Number of static (global) variables.
    pub num_static_vars: usize,
    /// Sound names (only present in version 6 / TA:K files).
    pub sound_names: Vec<String>,
}

#[derive(Debug, Error)]
pub enum CobParseError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("COB file too small for header (need 44 bytes, got {0})")]
    HeaderTruncated(usize),
    #[error("COB file has no scripts")]
    NoScripts,
    #[error("string offset {0} out of bounds")]
    StringOutOfBounds(usize),
}

/// Parse a COB file from raw bytes.
pub fn parse_cob(data: &[u8]) -> Result<CobFile, CobParseError> {
    if data.len() < 44 {
        return Err(CobParseError::HeaderTruncated(data.len()));
    }

    let mut cursor = Cursor::new(data);

    let version = cursor.read_i32::<LittleEndian>()?;
    let num_scripts = cursor.read_i32::<LittleEndian>()? as usize;
    let num_pieces = cursor.read_i32::<LittleEndian>()? as usize;
    let total_script_len = cursor.read_i32::<LittleEndian>()? as usize;
    let num_static_vars = cursor.read_i32::<LittleEndian>()? as usize;
    let _unknown_2 = cursor.read_i32::<LittleEndian>()?;
    let offset_script_code_index = cursor.read_i32::<LittleEndian>()? as usize;
    let offset_script_names = cursor.read_i32::<LittleEndian>()? as usize;
    let offset_piece_names = cursor.read_i32::<LittleEndian>()? as usize;
    let offset_script_code = cursor.read_i32::<LittleEndian>()? as usize;
    let _unknown_3 = cursor.read_i32::<LittleEndian>()?;

    // TA:K extension (version 6)
    let (offset_sound_names, num_sounds) = if version == 6 {
        let sn = cursor.read_i32::<LittleEndian>()? as usize;
        let ns = cursor.read_i32::<LittleEndian>()? as usize;
        (sn, ns)
    } else {
        (0, 0)
    };

    if num_scripts == 0 {
        return Err(CobParseError::NoScripts);
    }

    // Sanity-check counts against file size to avoid allocating absurd Vecs.
    let max_entries = data.len() / 4;
    if num_scripts > max_entries || num_pieces > max_entries || num_sounds > max_entries {
        return Err(CobParseError::HeaderTruncated(data.len()));
    }

    // Read script names
    let mut script_names = Vec::with_capacity(num_scripts);
    for i in 0..num_scripts {
        let name_offset = read_i32_at(data, offset_script_names + i * 4)? as usize;
        script_names.push(read_null_string(data, name_offset)?);
    }

    // Read script code offsets (these are word offsets into the code array)
    let mut script_offsets = Vec::with_capacity(num_scripts);
    for i in 0..num_scripts {
        let code_offset = read_i32_at(data, offset_script_code_index + i * 4)? as usize;
        script_offsets.push(code_offset);
    }

    // Compute script lengths
    let mut script_lengths = Vec::with_capacity(num_scripts);
    for i in 0..num_scripts - 1 {
        script_lengths.push(script_offsets[i + 1].saturating_sub(script_offsets[i]));
    }
    script_lengths.push(total_script_len.saturating_sub(script_offsets[num_scripts - 1]));

    // Read piece names
    let mut piece_names = Vec::with_capacity(num_pieces);
    for i in 0..num_pieces {
        let name_offset = read_i32_at(data, offset_piece_names + i * 4)? as usize;
        let name = read_null_string(data, name_offset)?;
        piece_names.push(name.to_ascii_lowercase());
    }

    // Read bytecode
    let code_bytes = data.len().saturating_sub(offset_script_code);
    let code_words = code_bytes / 4;
    let mut code = Vec::with_capacity(code_words);
    let mut code_cursor = Cursor::new(data);
    code_cursor.seek(SeekFrom::Start(offset_script_code as u64))?;
    for _ in 0..code_words {
        code.push(code_cursor.read_i32::<LittleEndian>()?);
    }

    // Read sound names (version 6 only)
    let mut sound_names = Vec::with_capacity(num_sounds);
    for i in 0..num_sounds {
        let name_offset = read_i32_at(data, offset_sound_names + i * 4)? as usize;
        sound_names.push(read_null_string(data, name_offset)?);
    }

    Ok(CobFile {
        script_names,
        script_offsets,
        script_lengths,
        piece_names,
        code,
        num_static_vars,
        sound_names,
    })
}

impl CobFile {
    /// Find a script function by name, returning its index.
    pub fn function_id(&self, name: &str) -> Option<usize> {
        self.script_names.iter().position(|n| n == name)
    }
}

fn read_i32_at(data: &[u8], offset: usize) -> Result<i32, CobParseError> {
    if offset + 4 > data.len() {
        return Err(CobParseError::StringOutOfBounds(offset));
    }
    let mut cursor = Cursor::new(&data[offset..]);
    Ok(cursor.read_i32::<LittleEndian>()?)
}

fn read_null_string(data: &[u8], offset: usize) -> Result<String, CobParseError> {
    if offset >= data.len() {
        return Err(CobParseError::StringOutOfBounds(offset));
    }
    let slice = &data[offset..];
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    Ok(String::from_utf8_lossy(&slice[..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scripts_dir() -> Option<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().unwrap_or(&manifest_dir);
        [
            workspace_root.join("upstream/Kernel-Panic/scripts"),
            PathBuf::from("upstream/Kernel-Panic/scripts"),
        ]
        .into_iter()
        .find(|p| p.is_dir())
    }

    #[test]
    fn parse_bit_cob() {
        let Some(dir) = scripts_dir() else {
            eprintln!("Skipping: scripts directory not found");
            return;
        };
        let data = std::fs::read(dir.join("bit.cob")).unwrap();
        let cob = parse_cob(&data).unwrap();

        assert!(!cob.script_names.is_empty());
        assert!(!cob.piece_names.is_empty());
        assert!(!cob.code.is_empty());

        // Bit has these pieces: base, body, shell, gunbase, gunpoint
        assert_eq!(cob.piece_names.len(), 5);
        assert!(cob.piece_names.contains(&"base".to_string()));

        // Should have Create, Killed, StartMoving, etc.
        assert!(cob.function_id("Create").is_some());
        assert!(cob.function_id("Killed").is_some());

        eprintln!(
            "bit.cob: {} scripts, {} pieces, {} code words, {} static vars",
            cob.script_names.len(),
            cob.piece_names.len(),
            cob.code.len(),
            cob.num_static_vars,
        );
        eprintln!("  scripts: {:?}", cob.script_names);
        eprintln!("  pieces: {:?}", cob.piece_names);
    }

    #[test]
    fn parse_kernel_cob() {
        let Some(dir) = scripts_dir() else {
            eprintln!("Skipping: scripts directory not found");
            return;
        };
        let data = std::fs::read(dir.join("kernel.cob")).unwrap();
        let cob = parse_cob(&data).unwrap();

        // Kernel has many pieces and scripts
        assert!(cob.piece_names.len() >= 20);
        assert!(cob.script_names.len() >= 10);

        assert!(cob.function_id("Create").is_some());
        assert!(cob.function_id("Activate").is_some());
        assert!(cob.function_id("Deactivate").is_some());

        eprintln!(
            "kernel.cob: {} scripts, {} pieces, {} code words",
            cob.script_names.len(),
            cob.piece_names.len(),
            cob.code.len(),
        );
    }

    #[test]
    fn parse_all_cob_files() {
        let Some(dir) = scripts_dir() else {
            eprintln!("Skipping: scripts directory not found");
            return;
        };

        let mut count = 0;
        let mut failures: Vec<String> = Vec::new();

        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("cob") {
                continue;
            }
            let name = path.file_stem().unwrap_or_default().to_string_lossy();
            let data = std::fs::read(&path).unwrap();

            match parse_cob(&data) {
                Ok(cob) => {
                    assert!(!cob.script_names.is_empty(), "{name}: no scripts");
                    assert!(!cob.code.is_empty(), "{name}: no code");
                    eprintln!(
                        "  OK: {name} — {} scripts, {} pieces, {} words",
                        cob.script_names.len(),
                        cob.piece_names.len(),
                        cob.code.len(),
                    );
                    count += 1;
                }
                Err(err) => {
                    failures.push(format!("{name}: {err}"));
                }
            }
        }

        if !failures.is_empty() {
            panic!(
                "{} COB file(s) failed:\n  {}",
                failures.len(),
                failures.join("\n  ")
            );
        }

        eprintln!("All {count} COB files parsed successfully");
        assert!(count > 0);
    }
}
