//! Generate **Circular Buffer** — a 4-player ring-corridor map.
//!
//! Topology: a central impassable void surrounded by a flat ring
//! corridor surrounded by high outer cliffs. The only place ground units
//! can move is the ring. Eight datavents sit on the corridor centerline,
//! four player starts fill the alternating quadrants.
//!
//! Pair with the in-engine [`CircularFlow`] resource: with the swirl
//! installed, units travelling with the favored rotation move ~1.6×
//! base speed; against it, ~0.4× — a 4× ratio. Producer-consumer
//! dynamics: you naturally chase the player ahead of you and get chased
//! by the one behind.
//!
//! Run: `cargo run -p spring-map-gen --bin gen-circular-buffer`
//! Output: `kernel-panic/assets/maps/Circular_Buffer.sdz`.
//!
//! [`CircularFlow`]: kernel_panic::map_events::CircularFlow

use std::path::{Path, PathBuf};

use clap::Parser;
use spring_map_gen::{Feature, MapGenError, Rgba, SmdBuilder, SmfBuilder, SmtBuilder, package_sdz};
use thiserror::Error;

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Output path for the `.sdz` archive. Defaults to
    /// `<workspace>/kernel-panic/assets/maps/Circular_Buffer.sdz`.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Error)]
enum CircularBufferGenError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("map generation failed: {0}")]
    MapGen(#[from] MapGenError),

    #[error("no workspace root above spring-map-gen")]
    NoWorkspaceRoot,
}

const MAP_NAME: &str = "Circular_Buffer";

/// 384 squares × 8 elmos = 3072-elmo side. Smaller than Stack_Overflow:
/// the playable surface is a thin ring, so most of the bounding square
/// is wasted on void and outer wall regardless.
const MAP_SMU: i32 = 384;
const WORLD: f32 = (MAP_SMU * 8) as f32;
const CENTER: f32 = WORLD * 0.5;

const MIN_HEIGHT: f32 = -400.0;
const MAX_HEIGHT: f32 = 800.0;

/// Inner void: any radius below this drops to a deep crater no unit can
/// pathfind through, forcing the entire game onto the ring.
const VOID_RADIUS: f32 = 520.0;
const VOID_FLOOR: f32 = -380.0;

/// Outer wall: any radius above this rises to a ring of cliffs that
/// caps the playable surface from the bounding square.
const WALL_RADIUS: f32 = 1240.0;
const WALL_HEIGHT: f32 = 700.0;

/// Corridor (ring) lives between [`VOID_RADIUS`] and [`WALL_RADIUS`],
/// at a flat altitude. Centerline radius is what we use for datavent
/// and start placement.
const CORRIDOR_HEIGHT: f32 = 30.0;
const CORRIDOR_CENTERLINE: f32 = (VOID_RADIUS + WALL_RADIUS) * 0.5;

/// Width (in elmos) of the smooth transition zones between corridor and
/// the void/wall on either side. Keeps the slope under the universal
/// Kbot `MaxSlope` cap (~0.412 from the runtime nav buckets) so units
/// don't get stuck on the corridor edge.
const VOID_BLEND: f32 = 80.0;
const WALL_BLEND: f32 = 110.0;

/// Eight datavents on the corridor centerline, evenly spaced.
const DATAVENT_COUNT: usize = 8;

fn main() -> Result<(), CircularBufferGenError> {
    let args = Args::parse();

    // ---- Tiles ----
    let mut smt = SmtBuilder::new();
    let void_dark = smt.add_solid_tile(Rgba::new(2, 4, 10));
    let corridor_grid = smt.add_checker_tile(Rgba::new(0, 60, 90), Rgba::new(8, 18, 28), 8);
    let corridor_arrow = smt.add_gradient_tile(Rgba::new(0, 110, 160), Rgba::new(0, 60, 90));
    let wall_dark = smt.add_solid_tile(Rgba::new(20, 20, 28));
    let smt_data = smt.build()?;

    // ---- Heightmap ----
    let mut smf = SmfBuilder::new(MAP_SMU, MAP_SMU)?
        .height_range(MIN_HEIGHT, MAX_HEIGHT)
        .minimap_color(Rgba::new(4, 8, 16));

    smf.fill_heightmap(|x, z| {
        let elmo_x = (x as f32) * 8.0;
        let elmo_z = (z as f32) * 8.0;
        encode_height(corridor_height(elmo_x, elmo_z))
    });

    // ---- Tilemap (1 tile = 4 squares = 32 elmos) ----
    let tile_w = (MAP_SMU / 4) as usize;
    let tile_h = (MAP_SMU / 4) as usize;
    let mut tilemap = vec![void_dark; tile_w * tile_h];
    for tz in 0..tile_h {
        for tx in 0..tile_w {
            let cx = (tx as f32) * 32.0 + 16.0;
            let cz = (tz as f32) * 32.0 + 16.0;
            let dx = cx - CENTER;
            let dz = cz - CENTER;
            let r = (dx * dx + dz * dz).sqrt();

            tilemap[tz * tile_w + tx] = if r < VOID_RADIUS - 8.0 {
                void_dark
            } else if r > WALL_RADIUS + 8.0 {
                wall_dark
            } else {
                // Sparse "flow arrows" — every fourth tile in the angular
                // direction reads as a brighter chevron. With 8 cardinals
                // around the ring this gives ~32 chevrons total, dense
                // enough that the swirl direction reads from the minimap.
                let theta = dz.atan2(dx);
                let arrow_idx =
                    ((theta + std::f32::consts::PI) / std::f32::consts::TAU * 32.0) as i32;
                if arrow_idx % 4 == 0 {
                    corridor_arrow
                } else {
                    corridor_grid
                }
            };
        }
    }
    smf.set_tilemap(tilemap, &format!("maps/{MAP_NAME}.smt"), smt.tile_count())?;

    // ---- Datavents — 8 evenly spaced on the corridor centerline ----
    for i in 0..DATAVENT_COUNT {
        let theta = (i as f32) * std::f32::consts::TAU / (DATAVENT_COUNT as f32);
        let x = CENTER + CORRIDOR_CENTERLINE * theta.cos();
        let z = CENTER + CORRIDOR_CENTERLINE * theta.sin();
        let y = corridor_height(x, z);
        smf.add_feature(Feature::geovent(x, y, z));
    }

    let smf_data = smf.build()?;

    // ---- SMD: 4 starts on the centerline, 90° apart, offset between
    // datavents so each player gets a "near" and a "far" vent in either
    // travel direction.
    let mut smd = SmdBuilder::new()
        .description("Circular Buffer — flat ring corridor with a one-way swirl. Travel with the flow is fast; against it crawls. For Kernel Panic.")
        .gravity(50.0)
        .sky_color([0.01, 0.02, 0.03])
        .sun_color([0.85, 0.95, 1.0])
        .fog_color([0.0, 0.02, 0.04])
        .fog_start(0.85)
        .sun_dir([0.3, 1.0, 0.2])
        .ground_ambient([0.32, 0.36, 0.45])
        .ground_sun_color([0.7, 0.8, 0.95]);

    // Start angles offset by π/8 so each player sits *between* two
    // datavents rather than on top of one — gives them an immediate
    // choice of expansion direction.
    let start_offset = std::f32::consts::PI / 8.0;
    for i in 0..4 {
        let theta = (i as f32) * std::f32::consts::FRAC_PI_2 + start_offset;
        let x = CENTER + CORRIDOR_CENTERLINE * theta.cos();
        let z = CENTER + CORRIDOR_CENTERLINE * theta.sin();
        smd.add_start_position(i, x, z);
    }
    let smd_text = smd.build();

    // ---- Package ----
    let out_path = match args.output {
        Some(p) => p,
        None => default_output_path()?,
    };
    package_sdz(&out_path, MAP_NAME, &smf_data, &smt_data, &smd_text)?;
    println!("Wrote {}", out_path.display());
    Ok(())
}

/// Radial profile in world space (elmos). Flat corridor at
/// `CORRIDOR_HEIGHT` between `VOID_RADIUS` and `WALL_RADIUS`, with
/// smooth ramps into the void crater on the inside and the cliff wall
/// on the outside.
fn corridor_height(elmo_x: f32, elmo_z: f32) -> f32 {
    let dx = elmo_x - CENTER;
    let dz = elmo_z - CENTER;
    let r = (dx * dx + dz * dz).sqrt();

    if r < VOID_RADIUS - VOID_BLEND {
        VOID_FLOOR
    } else if r < VOID_RADIUS {
        // Smoothstep from void floor up to the corridor surface.
        let t = (r - (VOID_RADIUS - VOID_BLEND)) / VOID_BLEND;
        let s = smoothstep(t);
        VOID_FLOOR + (CORRIDOR_HEIGHT - VOID_FLOOR) * s
    } else if r < WALL_RADIUS {
        CORRIDOR_HEIGHT
    } else if r < WALL_RADIUS + WALL_BLEND {
        let t = (r - WALL_RADIUS) / WALL_BLEND;
        let s = smoothstep(t);
        CORRIDOR_HEIGHT + (WALL_HEIGHT - CORRIDOR_HEIGHT) * s
    } else {
        WALL_HEIGHT
    }
}

/// Cubic Hermite smoothstep: zero slope at both endpoints, max slope
/// at the midpoint. Keeps corridor edges drivable.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Encode a world height (elmos) into the SMF i16 cell. Spring stores
/// the heightmap as 16-bit unsigned values [0, 65535] mapped linearly
/// onto [`MIN_HEIGHT`, `MAX_HEIGHT`]; we write i16 because that's what
/// `SmfBuilder::set_height` expects, and the engine reinterprets the
/// bit pattern as u16 at load time.
fn encode_height(world_y: f32) -> i16 {
    let t = ((world_y - MIN_HEIGHT) / (MAX_HEIGHT - MIN_HEIGHT)).clamp(0.0, 1.0);
    let raw_u = (t * 65535.0) as u16;
    raw_u as i16
}

fn default_output_path() -> Result<PathBuf, CircularBufferGenError> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .ok_or(CircularBufferGenError::NoWorkspaceRoot)?;
    let maps_dir = workspace_root.join("kernel-panic/assets/maps");
    if !Path::new(&maps_dir).exists() {
        std::fs::create_dir_all(&maps_dir)?;
    }
    Ok(maps_dir.join(format!("{MAP_NAME}.sdz")))
}
