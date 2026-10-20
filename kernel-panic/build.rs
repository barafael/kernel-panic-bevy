//! Embed the map, unit-definition and music catalogs at compile time.
//!
//! The web build has no filesystem to scan, and the native one ships
//! the same fixed set of files, so `map_loading::pick_map` takes the
//! list of `.kpmap` files in `assets/maps/` from here on both (with each
//! map's start count and intended players, for the random picks), and
//! `units::content::defs` the text of every unit, weapon, explosion
//! and feature definition in `assets/defs/`. The music player (`audio::music`) gets
//! the same treatment for the `.ogg` tracks in `assets/music/` (web
//! only: native scans the folder, so a player can drop tracks in).

use std::path::{Path, PathBuf};

fn main() {
    catalog(
        "assets/maps",
        "kpmap",
        "MAP_CATALOG",
        "map_catalog.rs",
        |_| true,
    );
    map_meta();
    // Tracks still a Git LFS pointer stay out: the player would fetch
    // the pointer and `bevy_audio` panics on what it cannot decode.
    catalog(
        "assets/music",
        "ogg",
        "WEB_MUSIC_CATALOG",
        "web_music_catalog.rs",
        is_ogg,
    );

    let defs = [
        ("assets/defs/units", "fbi", "UNIT_FBIS"),
        ("assets/defs/weapons", "tdf", "WEAPON_TDFS"),
        ("assets/defs/explosions", "tdf", "EXPLOSION_TDFS"),
        ("assets/defs/features", "tdf", "FEATURE_TDFS"),
    ]
    .into_iter()
    .map(|(dir, ext, const_name)| text_catalog(dir, ext, const_name))
    .collect::<Vec<_>>()
    .join("");
    let moveinfo = format!(
        "pub const MOVEINFO_TDF: &str = include_str!({});\n",
        literal(&crate_path("assets/defs/moveinfo.tdf").display().to_string())
    );
    write("defs_catalog.rs", format!("{defs}{moveinfo}"));
}

/// Write `OUT_DIR/<out_file>` defining `const_name` as the sorted file
/// stems of every `*.<ext>` in `dir` (relative to the crate root) that
/// `keep` accepts.
fn catalog(dir: &str, ext: &str, const_name: &str, out_file: &str, keep: fn(&Path) -> bool) {
    let entries: Vec<String> = files(dir, ext)
        .iter()
        .filter(|path| keep(path))
        .map(|path| literal(&stem(path)))
        .collect();
    write(
        out_file,
        format!(
            "#[allow(dead_code)]\npub const {const_name}: &[&str] = &[{}];\n",
            entries.join(", ")
        ),
    );
}

/// `MAP_META`: per map, `(stem, start positions, (fewest, most)
/// players)`, the players from the description where it names them,
/// else two to the start count (four at most).
fn map_meta() {
    let entries: Vec<String> = files("assets/maps", "kpmap")
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path).expect("read map");
            let info = spring_map::baked::read_map_info(&bytes)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
                .unwrap_or_default();
            let starts = info.start_positions.len() as u32;
            let (low, high) = info.intended_players().unwrap_or((2, starts.min(4)));
            let high = high.min(starts).max(1);
            format!(
                "({}, {starts}, ({}, {high}))",
                literal(&stem(path)),
                low.min(high)
            )
        })
        .collect();
    write(
        "map_meta.rs",
        format!(
            "#[allow(dead_code)]\npub const MAP_META: &[(&str, u32, (u32, u32))] = &[{}];\n",
            entries.join(", ")
        ),
    );
}

/// `const_name`: every `*.<ext>` in `dir` as `(file name, contents)`,
/// the contents `include_str!`d so the text is part of the binary.
fn text_catalog(dir: &str, ext: &str, const_name: &str) -> String {
    let entries: Vec<String> = files(dir, ext)
        .iter()
        .map(|path| {
            format!(
                "({}, include_str!({}))",
                literal(&path.file_name().unwrap().to_string_lossy()),
                literal(&path.display().to_string())
            )
        })
        .collect();
    format!(
        "pub const {const_name}: &[(&str, &str)] = &[{}];\n",
        entries.join(", ")
    )
}

/// The `*.<ext>` files of `dir`, sorted by name.
fn files(dir: &str, ext: &str) -> Vec<PathBuf> {
    println!("cargo:rerun-if-changed={dir}");
    let mut files: Vec<PathBuf> = std::fs::read_dir(crate_path(dir))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some(ext))
        .collect();
    files.sort();
    files
}

/// The file starts with the Ogg capture pattern (not an LFS pointer).
fn is_ogg(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path).is_ok_and(|mut f| f.read_exact(&mut magic).is_ok())
        && &magic == b"OggS"
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

fn crate_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// A Rust string literal (escaped, so Windows paths survive).
fn literal(text: &str) -> String {
    format!("{text:?}")
}

fn write(out_file: &str, body: String) {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let generated = format!("// @generated by build.rs — do not edit.\n{body}");
    std::fs::write(Path::new(&out_dir).join(out_file), generated).expect("write catalog");
}
