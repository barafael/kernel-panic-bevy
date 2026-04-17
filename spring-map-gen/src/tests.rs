//! Integration tests: generate a test map and validate it through spring-map.

use crate::sdz_writer::package_sdz_to_memory;
use crate::smd_writer::SmdBuilder;
use crate::smf_writer::SmfBuilder;
use crate::smt_writer::SmtBuilder;
use crate::{Feature, Rgba};

/// Map size: 2×2 Spring Map Units = mapx=mapy=256.
/// This is the smallest practical map (2048×2048 elmos).
const MAP_X: i32 = 256;
const MAP_Y: i32 = 256;
const MAP_NAME: &str = "TestBench";

// ---- Verifiable constants baked into the generated map ----

/// Height range chosen so we can verify exact values after round-trip.
const MIN_HEIGHT: f32 = -50.0;
const MAX_HEIGHT: f32 = 200.0;

/// Gravity in the SMD metadata.
const GRAVITY: f32 = 75.0;

/// Number of start positions (one per Kernel Panic faction + extras).
const NUM_TEAMS: u32 = 4;

/// Specific metalmap values at known positions for verification.
const METAL_HOTSPOT_X: usize = 32;
const METAL_HOTSPOT_Z: usize = 32;
const METAL_HOTSPOT_VALUE: u8 = 200;

/// Feature placement for verification: geovents and trees.
const NUM_GEOVENTS: usize = 6;
const NUM_TREES: usize = 4;

/// Number of distinct tile types we generate.
const NUM_TILE_TYPES: usize = 5;

/// Generate a complete test map as an in-memory SDZ archive.
///
/// The map exercises every format feature:
/// - Non-trivial heightmap with hills, valleys, and a flat plateau
/// - Multiple distinct tile types (solid, checker, striped, gradient)
/// - Metalmap with known hotspot values
/// - Typemap with distinct terrain types in each quadrant
/// - Multiple feature types: geovents and trees at verifiable positions
/// - SMD metadata with atmosphere, lighting, and start positions
/// - 4 team start positions at map corners
fn generate_test_map() -> (Vec<u8>, TestMapSpec) {
    // ---- 1. Build SMT tiles ----
    let mut smt = SmtBuilder::new();

    let black_tile = smt.add_solid_tile(Rgba::new(2, 2, 2));
    let checker_tile = smt.add_checker_tile(Rgba::new(0, 40, 0), Rgba::new(0, 10, 0), 8);
    let stripe_tile = smt.add_striped_tile(Rgba::new(30, 0, 0), Rgba::new(10, 0, 0), 4);
    let gradient_tile = smt.add_gradient_tile(Rgba::new(0, 0, 40), Rgba::new(0, 0, 10));
    let bright_tile = smt.add_solid_tile(Rgba::new(0, 60, 60));

    let smt_data = smt.build().unwrap();

    // ---- 2. Build SMF ----
    let mut smf = SmfBuilder::new(MAP_X, MAP_Y)
        .unwrap()
        .height_range(MIN_HEIGHT, MAX_HEIGHT)
        .minimap_color(Rgba::new(2, 2, 2));

    // -- Heightmap: 4 quadrants with distinct terrain --
    let hm_w = (MAP_X + 1) as usize;
    let hm_h = (MAP_Y + 1) as usize;
    let half_x = hm_w / 2;
    let half_z = hm_h / 2;

    smf.fill_heightmap(|x, z| {
        if x < half_x && z < half_z {
            // NW quadrant: hill (sine curve).
            let fx = x as f32 / half_x as f32;
            let fz = z as f32 / half_z as f32;
            let h = (fx * std::f32::consts::PI).sin() * (fz * std::f32::consts::PI).sin();
            (h * i16::MAX as f32 * 0.6) as i16
        } else if x >= half_x && z < half_z {
            // NE quadrant: flat plateau at mid-height.
            (i16::MAX as f32 * 0.3) as i16
        } else if x < half_x && z >= half_z {
            // SW quadrant: valley (negative heights).
            let fx = (x as f32 / half_x as f32 - 0.5).abs();
            (-(fx * i16::MAX as f32 * 0.2) as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16
        } else {
            // SE quadrant: ramp from low to high.
            let t = (x - half_x) as f32 / half_x as f32;
            (t * i16::MAX as f32 * 0.5) as i16
        }
    });

    // -- Tilemap: assign different tile types to quadrants --
    let tile_w = (MAP_X / 4) as usize;
    let tile_h = (MAP_Y / 4) as usize;
    let tile_half_x = tile_w / 2;
    let tile_half_z = tile_h / 2;

    let mut tilemap = vec![black_tile; tile_w * tile_h];
    for tz in 0..tile_h {
        for tx in 0..tile_w {
            tilemap[tz * tile_w + tx] = if tx < tile_half_x && tz < tile_half_z {
                checker_tile // NW
            } else if tx >= tile_half_x && tz < tile_half_z {
                stripe_tile // NE
            } else if tx < tile_half_x && tz >= tile_half_z {
                gradient_tile // SW
            } else {
                bright_tile // SE
            };
        }
    }

    smf.set_tilemap(tilemap, &format!("maps/{MAP_NAME}.smt"), smt.tile_count())
        .unwrap();

    // -- Metalmap: hotspot at known position + gradient strip --
    let metal_w = (MAP_X / 2) as usize;
    smf.set_metal(METAL_HOTSPOT_X, METAL_HOTSPOT_Z, METAL_HOTSPOT_VALUE);
    // Add a gradient strip along the bottom edge for visual verification.
    let metal_h = (MAP_Y / 2) as usize;
    for x in 0..metal_w {
        let v = (x as f32 / metal_w as f32 * 255.0) as u8;
        smf.set_metal(x, metal_h - 1, v);
    }

    // -- Typemap: different terrain type per quadrant --
    let type_w = (MAP_X / 2) as usize;
    let type_h = (MAP_Y / 2) as usize;
    let type_half_x = type_w / 2;
    let type_half_z = type_h / 2;
    for tz in 0..type_h {
        for tx in 0..type_w {
            let terrain_type = if tx < type_half_x && tz < type_half_z {
                0 // NW: default
            } else if tx >= type_half_x && tz < type_half_z {
                1 // NE
            } else if tx < type_half_x && tz >= type_half_z {
                2 // SW
            } else {
                3 // SE
            };
            smf.set_type(tx, tz, terrain_type);
        }
    }

    // -- Features: geovents at strategic positions, trees scattered --
    let world_w = (MAP_X * 8) as f32;
    let world_d = (MAP_Y * 8) as f32;

    // Geovents near each start position + 2 in the center.
    let geovent_positions = [
        (world_w * 0.15, world_d * 0.15),
        (world_w * 0.85, world_d * 0.15),
        (world_w * 0.15, world_d * 0.85),
        (world_w * 0.85, world_d * 0.85),
        (world_w * 0.45, world_d * 0.50),
        (world_w * 0.55, world_d * 0.50),
    ];
    for &(x, z) in &geovent_positions {
        smf.add_feature(Feature::geovent(x, 10.0, z));
    }

    // Trees of different types.
    let tree_positions = [
        (0u8, world_w * 0.30, world_d * 0.30),
        (3, world_w * 0.70, world_d * 0.30),
        (7, world_w * 0.30, world_d * 0.70),
        (15, world_w * 0.70, world_d * 0.70),
    ];
    for &(tree_type, x, z) in &tree_positions {
        smf.add_feature(Feature::tree(tree_type, x, 5.0, z));
    }

    let smf_data = smf.build().unwrap();

    // ---- 3. Build SMD metadata ----
    let mut smd = SmdBuilder::new()
        .description("TestBench - verifiable map for integration testing")
        .gravity(GRAVITY)
        .sky_color([0.01, 0.01, 0.01])
        .sun_color([1.0, 1.0, 1.0])
        .fog_color([0.0, 0.0, 0.0])
        .fog_start(0.001)
        .sun_dir([0.0, 1.0, 1.0])
        .ground_ambient([0.5, 0.5, 0.5])
        .ground_sun_color([0.5, 0.5, 0.5]);

    // Start positions at map corners (inset by 10%).
    let margin_x = world_w * 0.1;
    let margin_z = world_d * 0.1;
    smd.add_start_position(0, margin_x, margin_z);
    smd.add_start_position(1, world_w - margin_x, margin_z);
    smd.add_start_position(2, margin_x, world_d - margin_z);
    smd.add_start_position(3, world_w - margin_x, world_d - margin_z);

    let smd_text = smd.build();

    // ---- 4. Package as SDZ ----
    let sdz = package_sdz_to_memory(MAP_NAME, &smf_data, &smt_data, &smd_text).unwrap();

    let spec = TestMapSpec {
        map_x: MAP_X,
        map_y: MAP_Y,
        min_height: MIN_HEIGHT,
        max_height: MAX_HEIGHT,
        gravity: GRAVITY,
        num_teams: NUM_TEAMS,
        num_geovents: NUM_GEOVENTS,
        num_trees: NUM_TREES,
        num_tile_types: NUM_TILE_TYPES,
        metal_hotspot_x: METAL_HOTSPOT_X,
        metal_hotspot_z: METAL_HOTSPOT_Z,
        metal_hotspot_value: METAL_HOTSPOT_VALUE,
        world_width: world_w,
        world_depth: world_d,
    };

    (sdz, spec)
}

/// Describes the expected properties of the generated test map.
struct TestMapSpec {
    map_x: i32,
    map_y: i32,
    min_height: f32,
    max_height: f32,
    gravity: f32,
    num_teams: u32,
    num_geovents: usize,
    num_trees: usize,
    #[allow(dead_code)] // kept for documentation: number of distinct tile patterns
    num_tile_types: usize,
    metal_hotspot_x: usize,
    metal_hotspot_z: usize,
    metal_hotspot_value: u8,
    world_width: f32,
    world_depth: f32,
}

// ==========================================================================
// Tests
// ==========================================================================

/// Write SDZ to a temp file and load it through spring_map::load_map.
fn load_generated_map(sdz_data: &[u8]) -> spring_map::SpringMap {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_dir = std::env::temp_dir().join(format!("spring_map_gen_test_{id}"));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let sdz_path = tmp_dir.join(format!("{MAP_NAME}_{id}.sdz"));
    std::fs::write(&sdz_path, sdz_data).unwrap();

    let result = spring_map::load_map(&sdz_path);
    // Clean up.
    let _ = std::fs::remove_file(&sdz_path);
    let _ = std::fs::remove_dir(&tmp_dir);

    result.expect("generated map should load without errors")
}

#[test]
fn smf_builder_rejects_bad_dimensions() {
    assert!(SmfBuilder::new(0, 128).is_err());
    assert!(SmfBuilder::new(128, 0).is_err());
    assert!(SmfBuilder::new(100, 128).is_err()); // not divisible by 128
    assert!(SmfBuilder::new(128, 128).is_ok());
    assert!(SmfBuilder::new(256, 256).is_ok());
}

#[test]
fn smf_raw_bytes_have_correct_magic_and_header() {
    let smf = SmfBuilder::new(128, 128).unwrap().height_range(0.0, 100.0);
    let data = smf.build().unwrap();

    assert_eq!(&data[..16], b"spring map file\0");
    // Version at offset 16.
    let version = i32::from_le_bytes(data[16..20].try_into().unwrap());
    assert_eq!(version, 1);
    // mapx at offset 24.
    let mapx = i32::from_le_bytes(data[24..28].try_into().unwrap());
    assert_eq!(mapx, 128);
}

#[test]
fn smt_raw_bytes_have_correct_magic() {
    let mut smt = SmtBuilder::new();
    smt.add_solid_tile(Rgba::BLACK);
    let data = smt.build().unwrap();

    assert_eq!(&data[..16], b"spring tilefile\0");
    let num_tiles = i32::from_le_bytes(data[20..24].try_into().unwrap());
    assert_eq!(num_tiles, 1);
}

#[test]
fn smd_roundtrip_through_parser() {
    let mut smd = SmdBuilder::new()
        .description("test map")
        .gravity(42.0)
        .sky_color([0.1, 0.2, 0.3]);
    smd.add_start_position(0, 100.0, 200.0);
    smd.add_start_position(1, 300.0, 400.0);

    let text = smd.build();
    let parsed = spring_map::smd_parser::parse_smd(&text);

    assert!((parsed.gravity - 42.0).abs() < 0.01);
    assert_eq!(parsed.start_positions.len(), 2);
    assert!((parsed.start_positions[0].x - 100.0).abs() < 0.1);
    assert!((parsed.start_positions[0].z - 200.0).abs() < 0.1);
    assert!((parsed.start_positions[1].x - 300.0).abs() < 0.1);
    assert!((parsed.atmosphere.sky_color[0] - 0.1).abs() < 0.01);
}

#[test]
fn generated_map_loads_through_spring_map() {
    let (sdz, spec) = generate_test_map();
    let spring_map = load_generated_map(&sdz);
    let parsed = &spring_map.parsed;

    // -- Header dimensions --
    assert_eq!(parsed.header.map_x, spec.map_x);
    assert_eq!(parsed.header.map_y, spec.map_y);

    // -- Height range --
    assert!(
        (parsed.header.min_height - spec.min_height).abs() < 0.01,
        "min_height: expected {}, got {}",
        spec.min_height,
        parsed.header.min_height
    );
    assert!(
        (parsed.header.max_height - spec.max_height).abs() < 0.01,
        "max_height: expected {}, got {}",
        spec.max_height,
        parsed.header.max_height
    );
}

#[test]
fn generated_map_heightmap_is_correct_size() {
    let (sdz, spec) = generate_test_map();
    let spring_map = load_generated_map(&sdz);
    let parsed = &spring_map.parsed;

    let expected_len = ((spec.map_x + 1) * (spec.map_y + 1)) as usize;
    assert_eq!(parsed.heights.len(), expected_len);
}

#[test]
fn generated_map_heightmap_has_terrain_variation() {
    let (sdz, _spec) = generate_test_map();
    let spring_map = load_generated_map(&sdz);
    let parsed = &spring_map.parsed;

    let min_h = parsed.heights.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_h = parsed
        .heights
        .iter()
        .cloned()
        .fold(f32::NEG_INFINITY, f32::max);

    // The map should have significant terrain variation.
    let range = max_h - min_h;
    assert!(
        range > 10.0,
        "heightmap should have meaningful variation, but range is only {range:.1}"
    );

    // NW quadrant should have a hill (positive heights above midpoint).
    let hm_w = (MAP_X + 1) as usize;
    let center_nw = parsed.heights[(hm_w / 4) * hm_w + hm_w / 4];
    // NE quadrant is a flat plateau — all values should be similar.
    let ne_corner = parsed.heights[(hm_w / 4) * hm_w + hm_w * 3 / 4];
    let ne_center = parsed.heights[(hm_w / 4) * hm_w + hm_w * 5 / 8];
    assert!(
        (ne_corner - ne_center).abs() < 1.0,
        "NE quadrant should be flat (plateau), but corner={ne_corner:.1} center={ne_center:.1}"
    );

    // NW hill peak should be higher than the NE plateau.
    assert!(
        center_nw > ne_corner,
        "NW hill ({center_nw:.1}) should be higher than NE plateau ({ne_corner:.1})"
    );
}

#[test]
fn generated_map_features_are_correct() {
    let (sdz, spec) = generate_test_map();
    let spring_map = load_generated_map(&sdz);
    let parsed = &spring_map.parsed;

    let total_features = spec.num_geovents + spec.num_trees;
    assert_eq!(
        parsed.features.len(),
        total_features,
        "expected {} features, got {}",
        total_features,
        parsed.features.len()
    );

    let geovents: Vec<_> = parsed
        .features
        .iter()
        .filter(|f| f.feature_type.is_geovent())
        .collect();
    assert_eq!(geovents.len(), spec.num_geovents);

    let trees: Vec<_> = parsed
        .features
        .iter()
        .filter(|f| f.feature_type.is_tree())
        .collect();
    assert_eq!(trees.len(), spec.num_trees);

    // All features should be within map bounds.
    for f in &parsed.features {
        assert!(
            f.x >= 0.0 && f.x <= spec.world_width,
            "feature x={} out of bounds (0..{})",
            f.x,
            spec.world_width
        );
        assert!(
            f.z >= 0.0 && f.z <= spec.world_depth,
            "feature z={} out of bounds (0..{})",
            f.z,
            spec.world_depth
        );
    }
}

#[test]
fn generated_map_metalmap_is_correct() {
    let (sdz, spec) = generate_test_map();
    let spring_map = load_generated_map(&sdz);
    let parsed = &spring_map.parsed;

    let metal_w = (spec.map_x / 2) as usize;
    let metal_h = (spec.map_y / 2) as usize;

    assert_eq!(parsed.metalmap.len(), metal_w * metal_h);

    // Verify the known hotspot.
    let hotspot_value = parsed.metalmap[spec.metal_hotspot_z * metal_w + spec.metal_hotspot_x];
    assert_eq!(
        hotspot_value, spec.metal_hotspot_value,
        "metalmap hotspot at ({},{}) should be {}, got {}",
        spec.metal_hotspot_x, spec.metal_hotspot_z, spec.metal_hotspot_value, hotspot_value
    );

    // Most of the metalmap should be zero.
    let nonzero_count = parsed.metalmap.iter().filter(|&&v| v > 0).count();
    let total_cells = metal_w * metal_h;
    assert!(
        nonzero_count < total_cells / 2,
        "metalmap should be mostly zero, but {nonzero_count}/{total_cells} cells are nonzero"
    );
}

#[test]
fn generated_map_ground_texture_assembles() {
    let (sdz, spec) = generate_test_map();
    let spring_map = load_generated_map(&sdz);

    let ground = spring_map
        .ground_texture
        .as_ref()
        .expect("ground texture should be present");

    // Expected texture dimensions: (mapx/4 * 32) x (mapy/4 * 32).
    let expected_w = (spec.map_x / 4) as usize * 32;
    let expected_h = (spec.map_y / 4) as usize * 32;
    assert_eq!(ground.width, expected_w);
    assert_eq!(ground.height, expected_h);
    assert_eq!(ground.pixels.len(), expected_w * expected_h * 4);
}

#[test]
fn generated_map_smd_metadata_is_correct() {
    let (sdz, spec) = generate_test_map();
    let spring_map = load_generated_map(&sdz);

    let map_info = spring_map
        .map_info
        .as_ref()
        .expect(".smd metadata should be present");

    assert!(
        (map_info.gravity - spec.gravity).abs() < 0.1,
        "gravity: expected {}, got {}",
        spec.gravity,
        map_info.gravity
    );

    assert_eq!(
        map_info.start_positions.len(),
        spec.num_teams as usize,
        "expected {} start positions, got {}",
        spec.num_teams,
        map_info.start_positions.len()
    );

    // All start positions should be within map bounds.
    for sp in &map_info.start_positions {
        assert!(
            sp.x >= 0.0 && sp.x <= spec.world_width,
            "start pos team {} x={} out of bounds (0..{})",
            sp.team,
            sp.x,
            spec.world_width
        );
        assert!(
            sp.z >= 0.0 && sp.z <= spec.world_depth,
            "start pos team {} z={} out of bounds (0..{})",
            sp.team,
            sp.z,
            spec.world_depth
        );
    }
}

#[test]
fn generated_map_atmosphere_is_dark() {
    let (sdz, _spec) = generate_test_map();
    let spring_map = load_generated_map(&sdz);

    let map_info = spring_map.map_info.as_ref().unwrap();

    // Kernel Panic style: dark sky.
    for &c in &map_info.atmosphere.sky_color {
        assert!(c < 0.1, "sky color should be dark, got {c}");
    }

    // Fog should be close to zero color.
    for &c in &map_info.atmosphere.fog_color {
        assert!(c < 0.1, "fog color should be dark, got {c}");
    }
}

#[test]
fn generated_map_lighting_is_reasonable() {
    let (sdz, _spec) = generate_test_map();
    let spring_map = load_generated_map(&sdz);

    let map_info = spring_map.map_info.as_ref().unwrap();

    // Sun direction should have a positive Y component (pointing somewhat up).
    assert!(
        map_info.lighting.sun_dir[1] > 0.0,
        "sun should point upward"
    );

    // Ambient color should be nonzero.
    let ambient_sum: f32 = map_info.lighting.ground_ambient.iter().sum();
    assert!(ambient_sum > 0.0, "ambient light should be nonzero");
}

/// Verify the map can be written to disk as an SDZ and loaded back.
#[test]
fn generated_map_sdz_file_roundtrip() {
    let (sdz, spec) = generate_test_map();

    let tmp_dir = std::env::temp_dir().join("spring_map_gen_roundtrip");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let sdz_path = tmp_dir.join(format!("{MAP_NAME}.sdz"));

    // Write to disk.
    std::fs::write(&sdz_path, &sdz).unwrap();

    // Load from disk.
    let result = spring_map::load_map(&sdz_path);
    let _ = std::fs::remove_file(&sdz_path);
    let _ = std::fs::remove_dir(&tmp_dir);

    let spring_map = result.expect("SDZ file roundtrip should succeed");
    assert_eq!(spring_map.parsed.header.map_x, spec.map_x);
    assert_eq!(
        spring_map.parsed.features.len(),
        spec.num_geovents + spec.num_trees
    );
}

/// Verify that package_sdz writes a valid zip to disk.
#[test]
fn package_sdz_to_disk() {
    let mut smt = SmtBuilder::new();
    smt.add_solid_tile(Rgba::BLACK);
    let smt_data = smt.build().unwrap();

    let smf = SmfBuilder::new(128, 128).unwrap();
    let smf_data = smf.build().unwrap();

    let mut smd = SmdBuilder::new();
    smd.add_start_position(0, 100.0, 100.0);
    smd.add_start_position(1, 900.0, 900.0);
    let smd_text = smd.build();

    let tmp_dir = std::env::temp_dir().join("spring_map_gen_disk_test");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let path = tmp_dir.join("Minimal.sdz");

    crate::package_sdz(&path, "Minimal", &smf_data, &smt_data, &smd_text).unwrap();

    let loaded = spring_map::load_map(&path).expect("disk SDZ should load");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&tmp_dir);

    assert_eq!(loaded.parsed.header.map_x, 128);
    assert_eq!(loaded.parsed.header.map_y, 128);
}

/// Quick check: the SMT builder produces tiles of the correct byte size.
#[test]
fn smt_tile_byte_sizes() {
    let mut smt = SmtBuilder::new();
    smt.add_solid_tile(Rgba::new(100, 0, 0));
    smt.add_checker_tile(Rgba::new(0, 100, 0), Rgba::new(0, 50, 0), 4);
    smt.add_striped_tile(Rgba::new(0, 0, 100), Rgba::new(0, 0, 50), 8);
    smt.add_gradient_tile(Rgba::new(100, 100, 0), Rgba::new(0, 0, 100));

    let data = smt.build().unwrap();
    // Header (32) + 4 tiles * 680 bytes each.
    assert_eq!(data.len(), 32 + 4 * 680);
}
