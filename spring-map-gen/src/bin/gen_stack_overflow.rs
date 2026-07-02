//! Generate **Stack Overflow** — a 4-player volcano map for Kernel Panic.
//!
//! A central cone with a flat caldera holds the prize datavent. Six more
//! datavents form an expansion ring on the surrounding circuit-board
//! plain. The map is meant to be paired with the in-engine *eruption*
//! system in [`kernel_panic::map_events`], which periodically spews bad
//! blocks, cloaked mines, viruses, ICMP packets, and faction nibbles
//! from the caldera.
//!
//! Run: `cargo run -p spring-map-gen --bin gen-stack-overflow`
//! Output: `kernel-panic/assets/maps/Stack_Overflow.sdz`.
//!
//! The Lua-driven gadget pattern from the upstream Spring engine
//! doesn't run in the Rust port — every behavior beyond raw terrain
//! lives in Bevy systems.

use std::path::{Path, PathBuf};

use clap::Parser;
use spring_map_gen::{Feature, MapGenError, Rgba, SmdBuilder, SmfBuilder, SmtBuilder, package_sdz};
use thiserror::Error;

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Output path for the `.sdz` archive. Defaults to
    /// `<workspace>/kernel-panic/assets/maps/Stack_Overflow.sdz`.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Error)]
enum StackOverflowGenError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("map generation failed: {0}")]
    MapGen(#[from] MapGenError),

    #[error("no workspace root above spring-map-gen")]
    NoWorkspaceRoot,
}

const MAP_NAME: &str = "Stack_Overflow";

/// 512 squares × 8 elmos = 4096-elmo side. Compact enough for a tense
/// 1v1 / 2v2 duel, large enough that the cone has a real plain around it.
const MAP_SMU: i32 = 512;
const WORLD: f32 = (MAP_SMU * 8) as f32;
const CENTER: f32 = WORLD * 0.5;

const MIN_HEIGHT: f32 = -50.0;
const MAX_HEIGHT: f32 = 1000.0;

/// Slope of the cone, radii in elmos.
const PEAK_HEIGHT: f32 = 720.0;
const CALDERA_RADIUS: f32 = 220.0;
const CONE_OUTER_RADIUS: f32 = 1200.0;
const PLAIN_HEIGHT: f32 = 20.0;
/// Caldera floor sits a touch below the rim so the rim reads as a lip.
const CALDERA_FLOOR_HEIGHT: f32 = 600.0;

/// Six ring datavents at this radius from center; one more on the
/// caldera floor.
const DATAVENT_RING_RADIUS: f32 = 1700.0;

/// Four start positions in the corners, pulled in from the edge by this
/// margin so homebases never clip the world boundary.
const START_MARGIN: f32 = 360.0;

fn main() -> Result<(), StackOverflowGenError> {
    let args = Args::parse();

    // ---- Tiles ----
    let mut smt = SmtBuilder::new();
    let dark_floor = smt.add_solid_tile(Rgba::new(6, 10, 18));
    let circuit_grid = smt.add_checker_tile(Rgba::new(0, 70, 95), Rgba::new(6, 10, 18), 8);
    let lava_low = smt.add_gradient_tile(Rgba::new(120, 30, 0), Rgba::new(180, 55, 5));
    let lava_high = smt.add_gradient_tile(Rgba::new(220, 80, 20), Rgba::new(255, 140, 40));
    let caldera_glow = smt.add_solid_tile(Rgba::new(255, 200, 80));
    let smt_data = smt.build()?;

    // ---- Heightmap (volcano cone) ----
    let mut smf = SmfBuilder::new(MAP_SMU, MAP_SMU)?
        .height_range(MIN_HEIGHT, MAX_HEIGHT)
        .minimap_color(Rgba::new(8, 12, 20));

    smf.fill_heightmap(|x, z| {
        let elmo_x = (x as f32) * 8.0;
        let elmo_z = (z as f32) * 8.0;
        encode_height(cone_height(elmo_x, elmo_z))
    });

    // ---- Tilemap (1 tile = 4 squares = 32 elmos) ----
    let tile_w = (MAP_SMU / 4) as usize;
    let tile_h = (MAP_SMU / 4) as usize;
    let mut tilemap = vec![dark_floor; tile_w * tile_h];
    for tz in 0..tile_h {
        for tx in 0..tile_w {
            let cx = (tx as f32) * 32.0 + 16.0;
            let cz = (tz as f32) * 32.0 + 16.0;
            let dx = cx - CENTER;
            let dz = cz - CENTER;
            let r = (dx * dx + dz * dz).sqrt();

            tilemap[tz * tile_w + tx] = if r < CALDERA_RADIUS {
                caldera_glow
            } else if r < CONE_OUTER_RADIUS * 0.55 {
                lava_high
            } else if r < CONE_OUTER_RADIUS {
                lava_low
            } else if (tx % 16 == 0) || (tz % 16 == 0) {
                // Sparse 512-elmo circuit grid so the plain has scale cues.
                circuit_grid
            } else {
                dark_floor
            };
        }
    }
    smf.set_tilemap(tilemap, &format!("maps/{MAP_NAME}.smt"), smt.tile_count())?;

    // ---- Datavents ----
    // 1 caldera datavent at the volcano summit, 6 ring datavents on the plain.
    let caldera_y = cone_height(CENTER, CENTER);
    smf.add_feature(Feature::geovent(CENTER, caldera_y, CENTER));
    for i in 0..6 {
        let theta = (i as f32) * std::f32::consts::TAU / 6.0;
        let x = CENTER + DATAVENT_RING_RADIUS * theta.cos();
        let z = CENTER + DATAVENT_RING_RADIUS * theta.sin();
        let y = cone_height(x, z);
        smf.add_feature(Feature::geovent(x, y, z));
    }

    let smf_data = smf.build()?;

    // ---- SMD: 4 corner start positions, dark digital atmosphere ----
    let mut smd = SmdBuilder::new()
        .description("Stack Overflow — central volcano erupts every 5 minutes, spewing bad blocks, mines, viruses, ICMP packets, and faction nibbles. For Kernel Panic.")
        .gravity(50.0)
        .sky_color([0.01, 0.01, 0.02])
        .sun_color([1.0, 0.85, 0.7])
        .fog_color([0.05, 0.0, 0.0])
        .fog_start(0.85)
        .sun_dir([0.2, 1.0, 0.3])
        .ground_ambient([0.35, 0.32, 0.4])
        .ground_sun_color([0.9, 0.75, 0.6]);

    let near = START_MARGIN;
    let far = WORLD - START_MARGIN;
    smd.add_start_position(0, near, near);
    smd.add_start_position(1, far, far);
    smd.add_start_position(2, far, near);
    smd.add_start_position(3, near, far);
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

/// Volcano profile in world space (elmos). Linear cone slope from
/// `PLAIN_HEIGHT` at `CONE_OUTER_RADIUS` up to `PEAK_HEIGHT` at the rim,
/// flat `CALDERA_FLOOR_HEIGHT` inside the caldera radius.
fn cone_height(elmo_x: f32, elmo_z: f32) -> f32 {
    let dx = elmo_x - CENTER;
    let dz = elmo_z - CENTER;
    let r = (dx * dx + dz * dz).sqrt();
    if r >= CONE_OUTER_RADIUS {
        PLAIN_HEIGHT
    } else if r <= CALDERA_RADIUS {
        CALDERA_FLOOR_HEIGHT
    } else {
        // Two-segment profile: outer slope rises to the rim, inner lip
        // dips to the caldera floor.
        let rim_radius = CALDERA_RADIUS + 90.0;
        if r >= rim_radius {
            let t = (CONE_OUTER_RADIUS - r) / (CONE_OUTER_RADIUS - rim_radius);
            PLAIN_HEIGHT + (PEAK_HEIGHT - PLAIN_HEIGHT) * t
        } else {
            let t = (rim_radius - r) / (rim_radius - CALDERA_RADIUS);
            PEAK_HEIGHT + (CALDERA_FLOOR_HEIGHT - PEAK_HEIGHT) * t
        }
    }
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

fn default_output_path() -> Result<PathBuf, StackOverflowGenError> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .ok_or(StackOverflowGenError::NoWorkspaceRoot)?;
    let maps_dir = workspace_root.join("kernel-panic/assets/maps");
    if !Path::new(&maps_dir).exists() {
        std::fs::create_dir_all(&maps_dir)?;
    }
    Ok(maps_dir.join(format!("{MAP_NAME}.sdz")))
}
