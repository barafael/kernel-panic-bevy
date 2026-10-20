//! Generates `kernel-panic/assets/maps/pipeline_v1.kpmap`.
//!
//! After Beyond All Reason's SpeedMetal: a long, thin map for two teams.
//! A courtyard at each end opens into one corridor over the void that
//! climbs in stages to a raised, stepped arena in the middle; off the first
//! stage, a cache pocket each side sits a step down. A bus lane (double
//! speed) runs end to end. Mirror symmetric; the arena's two datavents
//! are shared.
//!
//! ```sh
//! cargo run -p spring-map --example gen_pipeline --features write --release
//! ```

use spring_map::kpgen::{CELL, Cell, KpMap, Level, Ramp, Rect, Side};

/// Cells across and down: a 768 × 256 square map.
const CW: usize = 192;
const CD: usize = 64;

const BASE: usize = 0;
const MID: usize = 1;
const HIGH: usize = 2;
const RIM: usize = 3;

fn levels() -> Vec<Level> {
    // Steel.
    vec![
        Level {
            height: 200.0,
            fill: [60, 64, 80],
            line: [130, 140, 170],
        },
        Level {
            height: 280.0,
            fill: [80, 86, 104],
            line: [160, 170, 200],
        },
        Level {
            height: 360.0,
            fill: [110, 116, 136],
            line: [200, 210, 235],
        },
        Level {
            height: 520.0,
            fill: [200, 205, 220],
            line: [240, 245, 255],
        },
    ]
}

/// The west half; `main` mirrors it east (and the pocket south).
const COURTYARD: Rect = Rect::new(3, 3, 30, 61);
const CORRIDOR: Rect = Rect::new(30, 22, 96, 42);
const STAGE_1: Rect = Rect::new(36, 22, 62, 42);
const STAGE_2: Rect = Rect::new(68, 22, 96, 42);
const RAMPS: [(Rect, usize, usize); 2] = [
    (Rect::new(30, 22, 36, 42), BASE, MID),
    (Rect::new(62, 22, 68, 42), MID, HIGH),
];
const POCKET: Rect = Rect::new(42, 8, 58, 16);
const POCKET_RAMP: Rect = Rect::new(46, 16, 54, 22);
/// The arena round the map's centre: two blocks, stepping its corners.
const ARENA: [Rect; 2] = [Rect::new(81, 18, 111, 46), Rect::new(86, 14, 106, 50)];
const BUS: Rect = Rect::new(30, 31, 162, 33);
/// West datavents at cell corners: the courtyard (the first four are
/// starts), stage 1, the pocket.
const VENTS: [(usize, usize); 10] = [
    (10, 10),
    (10, 54),
    (10, 24),
    (10, 40),
    (22, 17),
    (22, 47),
    (49, 26),
    (49, 38),
    (50, 11),
    (50, 53),
];
const STARTS: usize = 4;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/pipeline_v1.kpmap"
        )
        .into()
    });
    let mut map = KpMap::new(CW, CD, levels(), BASE, RIM);
    map.fill(Rect::new(0, 0, CW, CD), Cell::Void);
    for (rect, level) in [
        (COURTYARD, BASE),
        (CORRIDOR, MID),
        (STAGE_1, MID),
        (STAGE_2, HIGH),
        (POCKET, BASE),
    ] {
        for r in [
            rect,
            rect.mirror_x(CW),
            rect.mirror_z(CD),
            rect.mirror_x(CW).mirror_z(CD),
        ] {
            map.fill(r, Cell::Floor(level));
        }
    }
    for rect in ARENA {
        map.fill(rect, Cell::Floor(HIGH));
    }
    let (w, d) = map.world();
    let centre = (w * 0.5, d * 0.5);
    let ramps = RAMPS
        .iter()
        .map(|&(rect, low, high)| Ramp {
            rect,
            low_side: Side::West,
            low,
            high,
        })
        .chain([Ramp {
            rect: POCKET_RAMP,
            low_side: Side::North,
            low: BASE,
            high: MID,
        }]);
    for r in ramps {
        let pocket = r.low_side == Side::North;
        map.add_ramp(r);
        map.add_ramp(r.mirror_x(CW));
        if pocket {
            map.add_ramp(r.mirror_z(CD));
            map.add_ramp(r.mirror_x(CW).mirror_z(CD));
        }
    }
    map.reset_rim();
    map.lanes = vec![BUS];

    let corner = |(x, z): (usize, usize)| ((x * CELL) as f32, (z * CELL) as f32);
    let west: Vec<(f32, f32)> = VENTS.iter().map(|&v| corner(v)).collect();
    let east: Vec<(f32, f32)> = west.iter().map(|&(x, z)| (w - x, z)).collect();
    map.starts = (0..STARTS).flat_map(|i| [west[i], east[i]]).collect();
    map.vents = west.into_iter().chain(east).collect();
    map.vents
        .extend([(centre.0, centre.1 - 256.0), (centre.0, centre.1 + 256.0)]);
    map.description = "Pipeline v1. Map for Kernel Panic, after SpeedMetal. Intended for 2-8 \
                       players in two teams."
        .into();
    map.write(&out);
}
