//! Shared helpers for loading TDF-format files from upstream directories.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use spring_tdf::{ParseError, Tdf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TdfLoadError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(#[from] ParseError),
}

/// Find an upstream Kernel-Panic subdirectory by leaf name (e.g. "units", "weapons").
pub fn find_upstream_dir(leaf: &str) -> Option<PathBuf> {
    let resolved = crate::paths::from_project_root(&format!("upstream/Kernel-Panic/{leaf}"));
    resolved.is_dir().then_some(resolved)
}

/// Parse a single TDF-format file from disk.
pub fn load_tdf_file(path: &Path) -> Result<Tdf, TdfLoadError> {
    let text = std::fs::read_to_string(path)?;
    let tdf = Tdf::parse(&text)?;
    Ok(tdf)
}

/// Load and parse all files with the given extension from a directory.
/// Returns a vec of `(filename, parsed_tdf)` pairs. Logs warnings for failures.
pub fn load_all_tdf_files(dir: &Path, extension: &str) -> Vec<(String, Tdf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        warn!("Failed to read directory: {}", dir.display());
        return Vec::new();
    };

    let mut results = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(error) => {
                warn!("Failed to read directory entry: {error}");
                continue;
            }
        };
        let path = entry.path();
        if path.extension().is_some_and(|e| e == extension) {
            match load_tdf_file(&path) {
                Ok(tdf) => {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    results.push((name, tdf));
                }
                Err(error) => {
                    warn!("Failed to load {}: {error}", path.display());
                }
            }
        }
    }
    results
}
