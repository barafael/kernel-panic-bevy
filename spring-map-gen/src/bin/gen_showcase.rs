//! Generate a flat showcase map for visually inspecting every mobile unit.
//!
//! The map is a large featureless plain with 10 start positions spread
//! across the full area — a 4×3 grid (one slot left empty) whose cells
//! are several thousand elmos wide, far beyond any unit's sight range
//! (max 768 elmos). The kernel-panic game maps these positions to a
//! showcase-mode spawn that places one of each mobile UnitKind instead
//! of the usual homebases.
//!
//! Run: `cargo run -p spring-map-gen --bin gen-showcase`
//! Output: `kernel-panic/assets/maps/Showcase.sdz` (zip-format; `.sd7` would
//! require 7z packaging, which this binary doesn't build.)

use std::path::{Path, PathBuf};

use spring_map_gen::{Rgba, SmdBuilder, SmfBuilder, SmtBuilder, package_sdz};

const MAP_NAME: &str = "Showcase";
/// 1536 SMUs × 8 elmos = 12288 elmos per side (multiple of 128). Large
/// enough to space 10 units out by ~3000 elmos in every direction.
const MAP_SMU: i32 = 1536;
/// World size in elmos (derived from `MAP_SMU`).
const WORLD_SIZE: f32 = (MAP_SMU * 8) as f32;

/// 10 start positions spread across the full map: 4 columns × 3 rows with
/// one slot left empty. Cells are evenly distributed with ~10% inset from
/// the edges, giving roughly 3000 elmos between neighbouring positions on
/// the long axis and 4000 on the short — safely above the 768-elmo max
/// sight range so units stay invisible to each other.
fn start_grid() -> Vec<(f32, f32)> {
    const COLS: usize = 4;
    const ROWS: usize = 3;
    const MARGIN: f32 = 0.1;
    let usable = WORLD_SIZE * (1.0 - 2.0 * MARGIN);
    let step_x = usable / (COLS - 1) as f32;
    let step_z = usable / (ROWS - 1) as f32;
    let origin = WORLD_SIZE * MARGIN;
    let mut out = Vec::with_capacity(10);
    for row in 0..ROWS {
        for col in 0..COLS {
            out.push((origin + col as f32 * step_x, origin + row as f32 * step_z));
            if out.len() == 10 {
                return out;
            }
        }
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ---- Tiles ----
    let mut smt = SmtBuilder::new();
    let dark = smt.add_solid_tile(Rgba::new(2, 4, 6));
    let grid = smt.add_checker_tile(Rgba::new(0, 30, 60), Rgba::new(2, 4, 8), 16);
    let smt_data = smt.build()?;

    // ---- Heightmap (perfectly flat at y=0) ----
    let mut smf = SmfBuilder::new(MAP_SMU, MAP_SMU)?
        .height_range(-10.0, 10.0)
        .minimap_color(Rgba::new(2, 4, 6));
    smf.fill_heightmap(|_, _| 0_i16);

    // ---- Tilemap: faint grid pattern so the ground reads as a surface ----
    let tile_w = (MAP_SMU / 4) as usize;
    let tile_h = (MAP_SMU / 4) as usize;
    let mut tilemap = vec![dark; tile_w * tile_h];
    for tz in 0..tile_h {
        for tx in 0..tile_w {
            // Bright marker every 16 tiles (~512 elmos) so the floor isn't blank.
            if tx.is_multiple_of(16) || tz.is_multiple_of(16) {
                tilemap[tz * tile_w + tx] = grid;
            }
        }
    }
    smf.set_tilemap(tilemap, &format!("maps/{MAP_NAME}.smt"), smt.tile_count())?;

    let smf_data = smf.build()?;

    // ---- SMD: start positions for the showcase grid ----
    let mut smd = SmdBuilder::new()
        .description("Showcase — one of every mobile unit on a flat plain")
        .gravity(50.0)
        .sky_color([0.0, 0.0, 0.02])
        .sun_color([0.6, 0.8, 1.0])
        .fog_color([0.0, 0.05, 0.1])
        .fog_start(0.5)
        .sun_dir([0.3, 1.0, 0.4])
        .ground_ambient([0.3, 0.4, 0.5])
        .ground_sun_color([0.6, 0.8, 1.0]);

    for (team, (x, z)) in start_grid().into_iter().enumerate() {
        smd.add_start_position(team as u32, x, z);
    }
    let smd_text = smd.build();

    // ---- Package ----
    let out_path = output_path()?;
    package_sdz(&out_path, MAP_NAME, &smf_data, &smt_data, &smd_text)?;
    println!("Wrote {}", out_path.display());
    Ok(())
}

fn output_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    // Walk up from CARGO_MANIFEST_DIR to find the workspace root.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .ok_or("no workspace root above spring-map-gen")?;
    let maps_dir = workspace_root.join("kernel-panic/assets/maps");
    if !Path::new(&maps_dir).exists() {
        std::fs::create_dir_all(&maps_dir)?;
    }
    Ok(maps_dir.join(format!("{MAP_NAME}.sdz")))
}
