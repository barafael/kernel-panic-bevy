//! Parser for compiled COB animation script files.
//!
//! The COB format is a bytecode derived from Total Annihilation's unit
//! animation system. Files contain a header, script/piece name tables,
//! bytecode, and optionally sound name tables (TA:K version 6).

use std::io::Cursor;

use binrw::{BinRead, binread};
use thiserror::Error;

use crate::opcodes::Opcode;
use crate::script_names::{CallinSlot, resolve_callin, total_callin_slots};

/// A parsed COB file ready for execution by the VM.
#[derive(Debug, Clone)]
pub struct CobFile {
    /// Source identifier — usually the script's basename ("bit",
    /// "kernel", ...). Mirrors upstream `CCobFile::name`. Used in error
    /// messages and for logging only; not consulted during execution.
    pub name: String,
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
    /// For each script index, `Some(name)` if the script is a Lua-only
    /// reference (either declared with the `lua_` prefix, or whose
    /// first opcode is [`Opcode::SignatureLua`]). The string is the
    /// Lua callback name minus the `lua_` prefix. Mirrors upstream
    /// `CCobFile::luaScripts`. Always the same length as
    /// `script_names`; non-Lua entries are an empty string.
    pub lua_scripts: Vec<String>,
    /// Callin lookup: for each `CallinSlot::callin_index()` returns the
    /// script function id (`Some(idx)`) if the script defines that
    /// well-known entry point, else `None`. Mirrors upstream
    /// `CCobFile::scriptIndex`.
    callin_index: Vec<Option<usize>>,
}

/// Raw COB file header — 11 little-endian i32 fields, plus two more on
/// version 6 (TA:K). All offsets are byte offsets into the file.
#[binread]
#[derive(Debug, Clone)]
#[br(little)]
struct CobHeader {
    #[br(temp)]
    version: i32,
    num_scripts: i32,
    num_pieces: i32,
    total_script_len: i32,
    num_static_vars: i32,
    #[br(temp)]
    _unknown_2: i32,
    offset_script_code_index: i32,
    offset_script_names: i32,
    offset_piece_names: i32,
    offset_script_code: i32,
    #[br(temp)]
    _unknown_3: i32,
    #[br(if(version == 6), little)]
    taksound: Option<TakSoundHeader>,
}

#[derive(Debug, Clone, BinRead)]
#[br(little)]
struct TakSoundHeader {
    offset_sound_names: i32,
    num_sounds: i32,
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

impl From<binrw::Error> for CobParseError {
    fn from(err: binrw::Error) -> Self {
        match err {
            binrw::Error::Io(io) => CobParseError::Io(io),
            other => CobParseError::Io(std::io::Error::other(other.to_string())),
        }
    }
}

/// Parse a COB file from raw bytes. The resulting file's
/// [`CobFile::name`] is left empty — use [`parse_cob_named`] when you
/// know the script's source name (typically the basename without
/// extension), so error messages and logs identify it.
pub fn parse_cob(data: &[u8]) -> Result<CobFile, CobParseError> {
    parse_cob_named(data, String::new())
}

/// Same as [`parse_cob`] but stamps the file with its source name so
/// later logging / error messages can identify it.
pub fn parse_cob_named(data: &[u8], name: String) -> Result<CobFile, CobParseError> {
    if data.len() < 44 {
        return Err(CobParseError::HeaderTruncated(data.len()));
    }

    let mut cursor = Cursor::new(data);
    let header = CobHeader::read(&mut cursor)?;

    let num_scripts = usize::try_from(header.num_scripts)
        .map_err(|_| CobParseError::HeaderTruncated(data.len()))?;
    let num_pieces = usize::try_from(header.num_pieces)
        .map_err(|_| CobParseError::HeaderTruncated(data.len()))?;
    let total_script_len = usize::try_from(header.total_script_len)
        .map_err(|_| CobParseError::HeaderTruncated(data.len()))?;
    let num_static_vars = usize::try_from(header.num_static_vars)
        .map_err(|_| CobParseError::HeaderTruncated(data.len()))?;
    let offset_script_code_index = usize::try_from(header.offset_script_code_index)
        .map_err(|_| CobParseError::HeaderTruncated(data.len()))?;
    let offset_script_names = usize::try_from(header.offset_script_names)
        .map_err(|_| CobParseError::HeaderTruncated(data.len()))?;
    let offset_piece_names = usize::try_from(header.offset_piece_names)
        .map_err(|_| CobParseError::HeaderTruncated(data.len()))?;
    let offset_script_code = usize::try_from(header.offset_script_code)
        .map_err(|_| CobParseError::HeaderTruncated(data.len()))?;
    let (offset_sound_names, num_sounds) = match header.taksound {
        Some(s) => (
            usize::try_from(s.offset_sound_names)
                .map_err(|_| CobParseError::HeaderTruncated(data.len()))?,
            usize::try_from(s.num_sounds)
                .map_err(|_| CobParseError::HeaderTruncated(data.len()))?,
        ),
        None => (0, 0),
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

    // Read bytecode — a flat i32 array from offset_script_code to EOF.
    let code_bytes = data.get(offset_script_code..).unwrap_or(&[]);
    let code: Vec<i32> = code_bytes
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    // Read sound names (version 6 only)
    let mut sound_names = Vec::with_capacity(num_sounds);
    for i in 0..num_sounds {
        let name_offset = read_i32_at(data, offset_sound_names + i * 4)? as usize;
        sound_names.push(read_null_string(data, name_offset)?);
    }

    // Detect Lua scripts. Either: name starts with `lua_` (engine
    // synthesises a callback under that name), or the first bytecode
    // word is the SIGNATURE_LUA sentinel (Recoil-style Lua reference).
    // See upstream CobFile.cpp:139-145.
    let signature_lua = Opcode::SignatureLua as i32;
    let lua_scripts: Vec<String> = script_names
        .iter()
        .enumerate()
        .map(|(i, fn_name)| {
            if let Some(stripped) = fn_name.strip_prefix("lua_") {
                stripped.to_string()
            } else if code.get(script_offsets[i]).copied() == Some(signature_lua) {
                fn_name.clone()
            } else {
                String::new()
            }
        })
        .collect();

    // Build the COBFN_* → script-id table. Multiple `script_names` may
    // legitimately resolve to the same callin slot (legacy aliases),
    // and we want the *last* match to win — matches upstream's
    // unordered-map iteration which the engine relies on for the
    // weapon-numbered name (e.g. `AimWeapon1`) to override the legacy
    // `AimPrimary` alias. Iterating in script-id order with overwrite
    // is good enough since both writes land in the same slot.
    let mut callin_index = vec![None; total_callin_slots()];
    for (id, fn_name) in script_names.iter().enumerate() {
        if let Some(slot) = resolve_callin(fn_name) {
            callin_index[slot.callin_index()] = Some(id);
        }
    }

    Ok(CobFile {
        name,
        script_names,
        script_offsets,
        script_lengths,
        piece_names,
        code,
        num_static_vars,
        sound_names,
        lua_scripts,
        callin_index,
    })
}

impl CobFile {
    /// Find a script function by name, returning its index.
    pub fn function_id(&self, name: &str) -> Option<usize> {
        self.script_names.iter().position(|n| n == name)
    }

    /// Resolve a known callin slot ([`CallinSlot::Plain`] or
    /// [`CallinSlot::Weapon`]) to the script function id that
    /// implements it on this file, or `None` if the script omits
    /// the callin. Mirrors upstream's `cobFile->scriptIndex[COBFN_*]`
    /// O(1) lookup; preferred over [`CobFile::function_id`] when the
    /// callin is a well-known one because it skips the string compare.
    pub fn function_id_for_callin(&self, slot: CallinSlot) -> Option<usize> {
        self.callin_index
            .get(slot.callin_index())
            .copied()
            .flatten()
    }

    /// True if the script defines [`CallinSlot::Plain`] /
    /// [`CallinSlot::Weapon`] entry — equivalent to
    /// `function_id_for_callin(slot).is_some()`. Mirrors upstream's
    /// `CCobInstance::HasFunction`.
    pub fn has_callin(&self, slot: CallinSlot) -> bool {
        self.function_id_for_callin(slot).is_some()
    }

    /// True if the given script id is a Lua-only reference (defined
    /// via the `lua_` prefix or [`Opcode::SignatureLua`]). Lua
    /// references must not be executed as bytecode — the engine
    /// dispatches them to a Lua callback instead. Without a Lua VM
    /// the VM still detects them and avoids running garbage.
    pub fn is_lua_script(&self, function_id: usize) -> bool {
        self.lua_scripts
            .get(function_id)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    /// Hand-build a [`CobFile`] without going through the binary
    /// parser. Intended for unit tests that want to exercise the VM on
    /// synthetic bytecode (single-function fixtures, opcode probes,
    /// etc.) without round-tripping through `.cob` headers.
    ///
    /// Populates `lua_scripts` and `callin_index` consistently with
    /// the binary parser so callin lookups work identically.
    #[doc(hidden)]
    pub fn from_test_parts(
        name: impl Into<String>,
        script_names: Vec<String>,
        script_offsets: Vec<usize>,
        script_lengths: Vec<usize>,
        piece_names: Vec<String>,
        code: Vec<i32>,
        num_static_vars: usize,
        sound_names: Vec<String>,
    ) -> Self {
        let signature_lua = Opcode::SignatureLua as i32;
        let lua_scripts: Vec<String> = script_names
            .iter()
            .enumerate()
            .map(|(i, fn_name)| {
                if let Some(stripped) = fn_name.strip_prefix("lua_") {
                    stripped.to_string()
                } else if code.get(script_offsets[i]).copied() == Some(signature_lua) {
                    fn_name.clone()
                } else {
                    String::new()
                }
            })
            .collect();
        let mut callin_index = vec![None; total_callin_slots()];
        for (id, fn_name) in script_names.iter().enumerate() {
            if let Some(slot) = resolve_callin(fn_name) {
                callin_index[slot.callin_index()] = Some(id);
            }
        }
        Self {
            name: name.into(),
            script_names,
            script_offsets,
            script_lengths,
            piece_names,
            code,
            num_static_vars,
            sound_names,
            lua_scripts,
            callin_index,
        }
    }
}

fn read_i32_at(data: &[u8], offset: usize) -> Result<i32, CobParseError> {
    let slice = data
        .get(offset..offset + 4)
        .ok_or(CobParseError::StringOutOfBounds(offset))?;
    Ok(i32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_null_string(data: &[u8], offset: usize) -> Result<String, CobParseError> {
    let slice = data
        .get(offset..)
        .ok_or(CobParseError::StringOutOfBounds(offset))?;
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
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
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
