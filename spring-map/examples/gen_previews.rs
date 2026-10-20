//! Generates the map selector's previews: for every
//! `kernel-panic/assets/maps/<stem>.kpmap`, a small
//! `kernel-panic/assets/maps/previews/<stem>.png` whose colour is the
//! ground texture and whose alpha is the height (0 = the map's lowest
//! ground, 255 = its highest), and one line per map in
//! `previews/relief.txt`: `<stem> <low> <high> <width> <depth> <hash>` (elmos),
//! which turns the alpha back into heights, and the map file's FNV-1a
//! hash, by which the game's tests tell a preview gone stale.
//!
//! Hex Farm has no ground texture (its towers are rolled each match):
//! its preview is one farm, rolled from a fixed seed, in the minimap's
//! colours.
//!
//! Rerun it whenever a map changes (`kernel-panic`'s
//! `previews_match_the_maps` test says when):
//!
//! ```sh
//! cargo run -p spring-map --example gen_previews --features write --release
//! ```

use spring_detmath::DetFloat;
use std::fs;
use std::path::Path;

use spring_map::hexfarm::{HexFarm, HexFarmSetup, is_inside_polygon};

/// The longest side of a preview, in pixels.
const SIZE: usize = 384;

fn main() {
    let maps = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../kernel-panic/assets/maps"
    ));
    let out = maps.join("previews");
    fs::create_dir_all(&out).expect("previews directory");
    let mut stems: Vec<_> = fs::read_dir(maps)
        .expect("maps directory")
        .filter_map(|e| {
            let path = e.ok()?.path();
            (path.extension()? == "kpmap").then(|| path.file_stem()?.to_str().map(String::from))?
        })
        .collect();
    stems.sort();
    let mut relief = String::new();
    for stem in &stems {
        let bytes = fs::read(maps.join(format!("{stem}.kpmap"))).expect("read map");
        let map = spring_map::baked::read_baked_map(&bytes).expect("decode map");
        let header = &map.parsed.header;
        let (world_w, world_d) = (header.world_width(), header.world_depth());
        let (hw, hd) = (header.heightmap_width(), header.heightmap_height());
        let heights = &map.parsed.heights;
        // The richest of a few rolls: the preview should show what a
        // farm can be, not its smallest.
        let farm = map.lua_compositing.is_some().then(|| {
            (1..=32u64)
                .map(|seed| {
                    HexFarm::generate(
                        seed,
                        HexFarmSetup {
                            map_size_x: world_w as f64,
                            map_size_z: world_d as f64,
                            teams: 4,
                            median_health: 2000.0,
                            median_build_time: 30.0,
                        },
                    )
                })
                .max_by_key(|f| f.hexes.len())
                .expect("a farm")
        });

        let (w, d) = if world_w >= world_d {
            (
                SIZE,
                ((SIZE as f32 * world_d / world_w).round() as usize).max(1),
            )
        } else {
            (
                ((SIZE as f32 * world_w / world_d).round() as usize).max(1),
                SIZE,
            )
        };
        // Colour and height of every texel, then the heights scaled to
        // the alpha's 0..=255.
        let mut texels = Vec::with_capacity(w * d);
        for z in 0..d {
            for x in 0..w {
                // The texel's footprint in elmos.
                let (x0, x1) = (
                    x as f32 / w as f32 * world_w,
                    (x + 1) as f32 / w as f32 * world_w,
                );
                let (z0, z1) = (
                    z as f32 / d as f32 * world_d,
                    (z + 1) as f32 / d as f32 * world_d,
                );
                let (cx, cz) = ((x0 + x1) * 0.5, (z0 + z1) * 0.5);
                texels.push(match (&farm, &map.ground_texture) {
                    (Some(farm), _) => farm_texel(farm, cx, cz),
                    (None, texture) => (
                        texture.as_ref().map_or([40, 40, 40], |t| {
                            average(t, (x0, x1), (z0, z1), (world_w, world_d))
                        }),
                        sample(heights, hw, hd, cx / 8.0, cz / 8.0),
                    ),
                });
            }
        }
        // The void (NaN) is not drawn in the game: it sits at the bottom.
        let (low, high) = texels
            .iter()
            .filter(|(_, h)| !h.is_nan())
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &(_, h)| {
                (if h < lo { h } else { lo }, if h > hi { h } else { hi })
            });
        let span = if high - low > 1e-3 { high - low } else { 1.0 };
        let mut px = vec![0u8; w * d * 4];
        for (i, (rgb, h)) in texels.iter().enumerate() {
            let a = if h.is_nan() {
                0
            } else {
                (((h - low) / span) * 255.0).round().det_clamp(0.0, 255.0) as u8
            };
            px[i * 4..][..4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
        write_png(&out.join(format!("{stem}.png")), w, d, &px);
        relief.push_str(&format!(
            "{stem} {low} {high} {world_w} {world_d} {:016x}\n",
            fnv64(&bytes)
        ));
        println!("{stem}: {w}x{d}, heights {low}..{high}");
    }
    fs::write(out.join("relief.txt"), relief).expect("write relief.txt");
}

/// The mean colour of the ground texture over a footprint (elmos).
fn average(
    t: &spring_map::map_types::GroundTexture,
    (x0, x1): (f32, f32),
    (z0, z1): (f32, f32),
    (world_w, world_d): (f32, f32),
) -> [u8; 3] {
    let tx = |x: f32| ((x / world_w * t.width as f32) as usize).min(t.width - 1);
    let tz = |z: f32| ((z / world_d * t.height as f32) as usize).min(t.height - 1);
    let (ax, bx) = (tx(x0), tx(x1).max(tx(x0) + 1).min(t.width));
    let (az, bz) = (tz(z0), tz(z1).max(tz(z0) + 1).min(t.height));
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for z in az..bz {
        for x in ax..bx {
            let p = &t.pixels[(z * t.width + x) * 4..][..3];
            for c in 0..3 {
                sum[c] += p[c] as u64;
            }
            n += 1;
        }
    }
    let n = n.max(1);
    [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]
}

/// Hex Farm with every tower and bridge standing, in the minimap's
/// colours (towers, bridges, vents, void): colour and height at `(x, z)`.
fn farm_texel(farm: &HexFarm, x: f32, z: f32) -> ([u8; 3], f32) {
    let (x, z) = (x as f64, z as f64);
    for h in &farm.hexes {
        if is_inside_polygon(&h.c, &h.bounds, x, z) {
            let vent = h.g && (h.x - x).abs() < 48.0 && (h.z - z).abs() < 48.0;
            let colour = if vent { [40, 150, 40] } else { [150, 24, 24] };
            return (colour, h.y as f32);
        }
    }
    for (k, r) in farm.rects.iter().enumerate() {
        if is_inside_polygon(&r.c, &r.bounds, x, z) {
            return ([95, 40, 40], farm.rect_height(k, x, z) as f32);
        }
    }
    ([0, 0, 0], f32::NAN)
}

/// Bilinear height at heightmap coordinates `(x, z)` (squares).
fn sample(h: &[f32], w: usize, d: usize, x: f32, z: f32) -> f32 {
    let x = x.det_clamp(0.0, (w - 1) as f32);
    let z = z.det_clamp(0.0, (d - 1) as f32);
    let (x0, z0) = (x.floor() as usize, z.floor() as usize);
    let (x1, z1) = ((x0 + 1).min(w - 1), (z0 + 1).min(d - 1));
    let (fx, fz) = (x - x0 as f32, z - z0 as f32);
    let top = h[z0 * w + x0] * (1.0 - fx) + h[z0 * w + x1] * fx;
    let bottom = h[z1 * w + x0] * (1.0 - fx) + h[z1 * w + x1] * fx;
    top * (1.0 - fz) + bottom * fz
}

fn write_png(path: &Path, w: usize, h: usize, rgba: &[u8]) {
    let file = fs::File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    encoder
        .write_header()
        .expect("png header")
        .write_image_data(rgba)
        .expect("png data");
}

/// FNV-1a, 64 bit (the game's `map_preview` tests hash the same way).
fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
