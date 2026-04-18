//! Project-root resolution that works regardless of cwd.
//!
//! When `cargo run` kicks off the binary it usually sets cwd to the
//! workspace root, but users frequently run the built exe directly
//! from anywhere. Using `kernel-panic/assets/...` as a relative-path
//! probe silently fails in those cases and the game launches with no
//! maps, no units, no weapons.
//!
//! The fix: compute one `project_root` at startup and resolve every
//! asset path against it. We try three sources in order and return
//! the first that exists:
//!
//! 1. `CARGO_MANIFEST_DIR`'s workspace root — set at build time, so
//!    this is authoritative during `cargo run` and `cargo test`.
//! 2. Cwd — matches the legacy behaviour for anyone already running
//!    from the workspace root.
//! 3. The exe's directory, walking upward — handles the common case
//!    where someone ships `target/release/kernel-panic.exe` or copies
//!    the exe next to its assets.

use std::path::{Path, PathBuf};

/// Return the workspace root as resolved at startup.
///
/// The returned path is guaranteed to contain at least one of the
/// expected marker subdirectories (`kernel-panic/assets` or
/// `upstream/Kernel-Panic`); otherwise the caller's operation will
/// fail against the resolved path and the missing data is the real
/// issue, not the resolution.
pub fn project_root() -> PathBuf {
    // `env!("CARGO_MANIFEST_DIR")` expands to the directory containing
    // the *binary crate's* Cargo.toml, i.e. `<workspace>/kernel-panic`.
    // Its parent is the workspace root.
    let manifest_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf);
    if let Some(root) = manifest_root
        && looks_like_project_root(&root)
    {
        return root;
    }

    if let Ok(cwd) = std::env::current_dir()
        && looks_like_project_root(&cwd)
    {
        return cwd;
    }

    if let Ok(exe) = std::env::current_exe() {
        let mut walker = exe.parent();
        while let Some(dir) = walker {
            if looks_like_project_root(dir) {
                return dir.to_path_buf();
            }
            walker = dir.parent();
        }
    }

    // Nothing matched. Fall back to cwd so the caller still gets a
    // reasonable error message when it tries to read files that
    // don't exist, rather than a panic from unwrap on None.
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// `dir` looks like the workspace root when at least one of the
/// well-known asset subdirectories is present.
fn looks_like_project_root(dir: &Path) -> bool {
    dir.join("kernel-panic/assets").is_dir() || dir.join("upstream/Kernel-Panic").is_dir()
}

/// Join `relative` onto [`project_root`] and return the absolute path.
pub fn from_project_root(relative: &str) -> PathBuf {
    project_root().join(relative)
}
