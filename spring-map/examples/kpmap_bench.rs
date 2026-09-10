//! Per-stage `.kpmap` decode benchmark + compression headroom probe.
//!
//! Run from the workspace root:
//!
//! ```sh
//! cargo run --release -p spring-map --example kpmap_bench -- kernel-panic/assets/maps [raw-out-dir]
//! ```
//!
//! For each map: file size, inflate time, postcard decode time, payload
//! component sizes, deflate-9 re-encode ratio, and (with the raw-out dir
//! argument) ruzstd decode timing of `<stem>.payload.zst` files produced
//! by the zstd CLI for codec comparisons.

use ruzstd::decoding::StreamingDecoder;
use std::io::Read;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: kpmap_bench <maps-dir> [raw-out-dir]");
    let raw_out = std::env::args().nth(2);

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read maps dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "kpmap"))
        .collect();
    paths.sort();

    println!(
        "{:<40} {:>10} {:>10} {:>10} {:>8} {:>9} {:>9} {:>9} {:>9}",
        "map", "file", "inflate", "postcard", "total", "heights", "metal", "tex_mib", "dfl9"
    );

    for path in paths {
        let bytes = std::fs::read(&path).expect("read map");
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();

        let t = Instant::now();
        let (payload, magic_v1) = inflate_body(&bytes);
        let inflate = t.elapsed();

        let t = Instant::now();
        let map = spring_map::baked::read_baked_map(&bytes).expect("decode");
        let postcard = t.elapsed();

        let heights = map.parsed.heights.len() * 4;
        let metal = map.parsed.metalmap.len();
        let tex_mib = map
            .ground_texture
            .as_ref()
            .map(|g| g.pixels.len() as f64 / 1024.0 / 1024.0)
            .unwrap_or(0.0);

        // deflate-9 re-encode of the raw payload (v2 stores level 6).
        use std::io::Write;
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
        enc.write_all(&payload).unwrap();
        let dfl9 = enc.finish().unwrap();

        if let Some(out) = &raw_out {
            let stem = path.file_stem().unwrap().to_string_lossy();
            std::fs::write(format!("{out}/{stem}.payload"), &payload).unwrap();
        }

        println!(
            "{name:<40} {:>10} {:>9.0}ms {:>9.0}ms {:>7.0}ms {:>7.1}kb {:>7.1}kb {:>7.1} {:>7.1}%",
            bytes.len(),
            inflate.as_secs_f64() * 1000.0,
            postcard.as_secs_f64() * 1000.0 - inflate.as_secs_f64() * 1000.0,
            (inflate + postcard).as_secs_f64() * 1000.0,
            heights as f64 / 1024.0,
            metal as f64 / 1024.0,
            tex_mib,
            100.0 * dfl9.len() as f64 / bytes.len() as f64,
        );

        // Pathfinding probe on the real terrain: from the KP-default
        // bucket's speed map, route the showcase homebase (224, 2848) to
        // the datavent build sites the showcase director actually uses,
        // and verify no path crosses a blocked cell.
        if name == "Data_Cache_L1" {
            use spring_pathfinding::{max_slope_from_degrees, slope_mod_from_max_slope};
            let cap = max_slope_from_degrees(36.0);
            let speed_map = spring_pathfinding::SpeedMap::from_heightmap(
                &map.parsed.heights,
                map.parsed.header.heightmap_width() as u32,
                map.parsed.header.heightmap_height() as u32,
                cap,
                slope_mod_from_max_slope(cap),
            );
            let targets: [([f32; 2], &str); 6] = [
                ([224.0, 1374.0], "vent south"),
                ([932.0, 1243.0], "vent far"),
                ([4400.0, 300.0], "map far corner"),
                ([4088.0, 3000.0], "corner B"),
                ([200.0, 200.0], "corner C"),
                ([4000.0, 500.0], "corner D"),
            ];
            for (dst, label) in targets {
                let t = Instant::now();
                match spring_pathfinding::find_path(&speed_map, [224.0, 2848.0], dst) {
                    Some(path) => {
                        let crossings = path.points.iter().any(|p| {
                            let cx = (p[0] / 8.0) as u32;
                            let cz = (p[1] / 8.0) as u32;
                            speed_map.get(cx, cz) <= 0.0
                        });
                        // Does the last waypoint sit ON the goal?
                        let reached = path
                            .points
                            .last()
                            .is_some_and(|p| {
                                ((p[0] - dst[0]).powi(2) + (p[1] - dst[1]).powi(2)).sqrt() < 8.0
                            });
                        println!(
                            "    path {label:<16} {:>5} waypoints, {:>7.0} elmos, {:>5.1}ms, reached goal: {}, crossings: {}",
                            path.len(),
                            path.total_length(),
                            t.elapsed().as_secs_f64() * 1000.0,
                            if reached { "yes" } else { "NO (partial)" },
                            if crossings { "YES (BUG)" } else { "no" },
                        );
                        assert!(!crossings, "path crossed a blocked cell");
                    }
                    None => println!("    path {label:<16} NONE"),
                }
            }
        }

        // ruzstd decode timing of a zstd-19 frame produced by the CLI.
        if let Some(out) = &raw_out {
            let zst = std::path::Path::new(out).join(format!("{name}.payload.zst"));
            if zst.exists() {
                let file = std::fs::File::open(&zst).unwrap();
                let t = Instant::now();
                let mut dec = StreamingDecoder::new(file).unwrap();
                let mut out_bytes = Vec::new();
                dec.read_to_end(&mut out_bytes).unwrap();
                assert_eq!(out_bytes.len(), payload.len());
                println!(
                    "    {:>38} ruzstd zstd-19: {:>7.0}ms ({} -> {} bytes)",
                    "",
                    t.elapsed().as_secs_f64() * 1000.0,
                    zst.metadata().unwrap().len(),
                    out_bytes.len()
                );
            }
        }
        let _ = magic_v1;
    }
}

/// Split a kpmap into its raw postcard payload (v1 raw, v2 deflate,
/// v3 zstd).
fn inflate_body(bytes: &[u8]) -> (Vec<u8>, bool) {
    let magic = &bytes[..8];
    let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let body = &bytes[12..12 + len];
    if magic == b"kpmapv1\0" {
        (body.to_vec(), true)
    } else if magic == b"kpmapv3\0" {
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(body).unwrap();
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        (out, false)
    } else {
        let mut decoder = flate2::read::DeflateDecoder::new(body);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        (out, false)
    }
}
