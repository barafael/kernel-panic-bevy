//! Generates `kernel-panic/assets/maps/three_way_handshake_v1.kpmap`.
//!
//! After Beyond All Reason's The Desert Triad and Pillar of Doom: a
//! square arena with stepped corners over the void, for three players.
//! Each starts on a raised plateau, north, south-west and south-east,
//! whose one straight ramp faces the middle; three holes split the arena
//! between them, and a blue core rises in the middle with a ramp toward
//! each player and a cliff over the south hole. Mirror symmetric east to
//! west; the grid allows no third-of-a-turn symmetry, so the generator
//! checks the three players walk equally far to the core and to their
//! middle datavents instead.
//!
//! ```sh
//! cargo run -p spring-map --example gen_three_way --features write --release
//! ```

use spring_detmath::DetFloat;
use spring_map::kpgen::{CELL, Cell, KpMap, Level, Ramp, Rect, Side, walk_distances};

const N: usize = 128;
const BASE: usize = 0;
const HIGH: usize = 1;
const CORE: usize = 2;
const RIM: usize = 3;

fn levels() -> Vec<Level> {
    // Data Cache L1's yellows and blue.
    vec![
        Level {
            height: 200.0,
            fill: [127, 129, 0],
            line: [200, 200, 40],
        },
        Level {
            height: 360.0,
            fill: [193, 192, 0],
            line: [255, 255, 90],
        },
        Level {
            height: 360.0,
            fill: [10, 40, 170],
            line: [60, 140, 255],
        },
        Level {
            height: 560.0,
            fill: [226, 225, 0],
            line: [255, 255, 120],
        },
    ]
}

/// The arena, and the blocks cut from its north-west corner to step it
/// (mirrored into the other three).
const ARENA: Rect = Rect::new(10, 10, 118, 118);
const ARENA_STEPS: [Rect; 2] = [Rect::new(10, 10, 26, 18), Rect::new(10, 18, 18, 26)];
/// The core and its ramps: north, west and east; the south is a cliff.
const CORE_TOP: Rect = Rect::new(56, 57, 72, 73);
const CORE_RAMPS: [(Rect, Side); 3] = [
    (Rect::new(58, 49, 70, 57), Side::North),
    (Rect::new(48, 59, 56, 71), Side::West),
    (Rect::new(72, 59, 80, 71), Side::East),
];
/// The north and south-west start plateaus and their ramps; the
/// south-east one is the south-west's mirrored.
const PLATEAU_N: Rect = Rect::new(55, 10, 73, 28);
const RAMP_N: Rect = Rect::new(59, 28, 69, 36);
const PLATEAU_SW: Rect = Rect::new(17, 72, 35, 90);
const RAMP_SW: Rect = Rect::new(35, 76, 43, 86);
/// The north-west hole (mirrored north-east) and the south one.
const HOLE_NW: Rect = Rect::new(26, 38, 40, 52);
const HOLE_S: Rect = Rect::new(57, 96, 71, 110);

/// Datavents at cell corners. The north player's and the south-west
/// player's own (start first, then the plateau pair, the middle pair, the
/// core vent), the south-east's being the south-west's mirrored; then the
/// shared outer three, the extra starts.
const VENTS_N: [(usize, usize); 6] = [(64, 19), (59, 14), (69, 14), (46, 40), (82, 40), (64, 60)];
const VENTS_SW: [(usize, usize); 6] = [(26, 81), (21, 76), (21, 86), (32, 99), (40, 66), (59, 68)];
const OUTER: [(usize, usize); 3] = [(20, 34), (108, 34), (64, 115)];
/// Indices into a player's vents.
const MIDDLE: [usize; 2] = [3, 4];

/// How far apart the players' walks may be, as a fraction.
const FAIRNESS: f32 = 0.05;

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/three_way_handshake_v1.kpmap"
        )
        .into()
    });
    let mut map = KpMap::new(N, N, levels(), BASE, RIM);
    let mirrored = |r: Rect| [r, r.mirror_x(N)];
    map.fill(Rect::new(0, 0, N, N), Cell::Void);
    map.fill(ARENA, Cell::Floor(BASE));
    for step in ARENA_STEPS {
        for r in mirrored(step).into_iter().chain(mirrored(step.mirror_z(N))) {
            map.fill(r, Cell::Void);
        }
    }
    for hole in mirrored(HOLE_NW).into_iter().chain([HOLE_S]) {
        map.fill(hole, Cell::Void);
    }
    map.fill(PLATEAU_N, Cell::Floor(HIGH));
    for r in mirrored(PLATEAU_SW) {
        map.fill(r, Cell::Floor(HIGH));
    }
    map.fill(CORE_TOP, Cell::Floor(CORE));
    let ramp = |rect, low_side, high| Ramp {
        rect,
        low_side,
        low: BASE,
        high,
    };
    map.add_ramp(ramp(RAMP_N, Side::South, HIGH));
    let sw = ramp(RAMP_SW, Side::East, HIGH);
    map.add_ramp(sw);
    map.add_ramp(sw.mirror_x(N));
    for (rect, side) in CORE_RAMPS {
        map.add_ramp(ramp(rect, side, CORE));
    }
    map.reset_rim();

    let corner = |(x, z): (usize, usize)| ((x * CELL) as f32, (z * CELL) as f32);
    let players: [Vec<(f32, f32)>; 3] = [
        VENTS_N.map(corner).to_vec(),
        VENTS_SW.map(corner).to_vec(),
        VENTS_SW.map(|(x, z)| corner((N - x, z))).to_vec(),
    ];
    map.vents = players.iter().flatten().copied().collect();
    map.vents.extend(OUTER.map(corner));
    map.starts = players
        .iter()
        .map(|p| p[0])
        .chain(OUTER.map(corner))
        .collect();
    map.description = "Three-Way Handshake v1. Map for Kernel Panic, after The Desert Triad \
                       and Pillar of Doom. Intended for 3 players (up to 6)."
        .into();
    let built = map.write(&out);

    // Every player walks as far to the core and to its middle vents.
    let parsed = &built.parsed;
    let w = parsed.header.map_x as usize;
    let at = |dist: &[f32], (x, z): (f32, f32)| {
        // A cell corner: the nearest of the four squares round it.
        let (sx, sz) = ((x / 8.0) as usize, (z / 8.0) as usize);
        [(0, 0), (1, 0), (0, 1), (1, 1)]
            .iter()
            .map(|&(dx, dz)| dist[(sz - dz) * w + sx - dx])
            .fold(f32::INFINITY, |a, b| a.det_min(b))
    };
    let (s, _) = map.world();
    let core = (
        s * 0.5,
        (CORE_TOP.z0 + CORE_TOP.z1) as f32 * 0.5 * CELL as f32,
    );
    let walks: Vec<(f32, f32)> = players
        .iter()
        .map(|vents| {
            let dist = walk_distances(parsed, &parsed.heights, vents[0]);
            let middle = MIDDLE.iter().map(|&i| at(&dist, vents[i])).sum::<f32>() * 0.5;
            (at(&dist, core), middle)
        })
        .collect();
    for (name, (to_core, to_middle)) in ["north", "south-west", "south-east"].iter().zip(&walks) {
        println!("{name}: core {to_core:.0}, middle vents {to_middle:.0}");
    }
    for pick in [|w: &(f32, f32)| w.0, |w: &(f32, f32)| w.1] {
        let (lo, hi) = walks
            .iter()
            .map(pick)
            .fold((f32::MAX, 0.0f32), |(lo, hi), d| {
                (lo.det_min(d), hi.det_max(d))
            });
        assert!(
            hi.is_finite() && hi <= lo * (1.0 + FAIRNESS),
            "unfair walks: {walks:?}"
        );
    }
}
