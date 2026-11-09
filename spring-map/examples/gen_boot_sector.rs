//! Generates `kernel-panic/assets/maps/boot_sector_v1.kpmap`.
//!
//! A tiny duel map, a chess opening trainer: a 2048-elmo chessboard of
//! eight by eight squares (one colour per square, all one height), a
//! home on each short side and a second vent a step out of it, one more
//! vent in the middle on a raised plateau whose two ramps face the
//! homes, and four kernel mesas at the plateau's corners to castle
//! behind. Two datavents per side and one between, as the old idea had
//! it. 180° rotationally symmetric, so the layout is fair although the
//! two second vents sit on opposite flanks.
//!
//! ```sh
//! cargo run -p spring-map --example gen_boot_sector --features write --release
//! ```

use spring_map::kpgen::{CELL, Cell, KpMap, Level, Ramp, Rect, Side};

/// Cells across and down: a 2048 × 2048 square map.
const CW: usize = 64;
const CD: usize = 64;

/// The board's two square colours (one height), the plateau, the mesas.
const LIGHT: usize = 0;
const DARK: usize = 1;
const PLATEAU: usize = 2;
const MESA: usize = 3;
const RIM: usize = 4;

fn levels() -> Vec<Level> {
    // A BIOS screen: phosphor greens, the firmware's blue, kernel olive.
    vec![
        Level {
            height: 150.0,
            fill: [10, 52, 30],
            line: [40, 150, 80],
        },
        Level {
            height: 150.0,
            fill: [20, 86, 48],
            line: [60, 190, 110],
        },
        Level {
            height: 260.0,
            fill: [24, 44, 120],
            line: [90, 140, 255],
        },
        Level {
            height: 340.0,
            fill: [96, 90, 20],
            line: [200, 190, 60],
        },
        Level {
            height: 520.0,
            fill: [200, 230, 210],
            line: [240, 255, 245],
        },
    ]
}

/// Cells to a chessboard square, and where the board starts.
const SQUARE: usize = 7;
const BOARD: usize = 4;

/// The middle plateau and its ramps toward the homes.
const CORE: Rect = Rect::new(27, 27, 37, 37);
const CORE_RAMPS: [(Rect, Side); 2] = [
    (Rect::new(21, 29, 27, 35), Side::West),
    (Rect::new(37, 29, 43, 35), Side::East),
];
/// The mesas at the plateau's corners (half of them; the rest are
/// these turned 180°).
const MESAS: [Rect; 2] = [Rect::new(20, 20, 26, 26), Rect::new(38, 20, 44, 26)];

/// The west half's datavents at cell corners: the home (the start), then
/// the second vent on its north flank. The east half is these turned
/// 180°; the middle vent is shared.
const VENTS: [(usize, usize); 2] = [(8, 32), (16, 20)];
const MIDDLE: (usize, usize) = (32, 32);

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/boot_sector_v1.kpmap"
        )
        .into()
    });
    let mut map = KpMap::new(CW, CD, levels(), LIGHT, RIM);
    for z in BOARD..CD - BOARD {
        for x in BOARD..CW - BOARD {
            let square = (x - BOARD) / SQUARE + (z - BOARD) / SQUARE;
            map.fill(
                Rect::new(x, z, x + 1, z + 1),
                Cell::Floor(if square.is_multiple_of(2) {
                    LIGHT
                } else {
                    DARK
                }),
            );
        }
    }
    map.fill(CORE, Cell::Floor(PLATEAU));
    for rect in MESAS {
        map.fill(rect, Cell::Floor(MESA));
        map.fill(rect.rot180(CW, CD), Cell::Floor(MESA));
    }
    for (rect, low_side) in CORE_RAMPS {
        map.add_ramp(Ramp {
            rect,
            low_side,
            low: LIGHT,
            high: PLATEAU,
        });
    }
    map.reset_rim();

    let corner = |(x, z): (usize, usize)| ((x * CELL) as f32, (z * CELL) as f32);
    let (w, d) = map.world();
    let west: Vec<(f32, f32)> = VENTS.iter().map(|&v| corner(v)).collect();
    let east: Vec<(f32, f32)> = west.iter().map(|&(x, z)| (w - x, d - z)).collect();
    map.starts = vec![west[0], east[0]];
    map.vents = west.into_iter().chain(east).collect();
    map.vents.push(corner(MIDDLE));
    map.description = "Boot Sector v1. Map for Kernel Panic. A tiny chessboard duel: two vents a \
                       side and one on the plateau between. Intended for 2 players."
        .into();
    let built = map.write(&out);

    // A factory on a vent faces the way the centre rule says, so a
    // symmetric map must give every vent's twin the turned facing; an
    // exact tie (a vent on a diagonal) breaks the symmetry.
    let facing = |p: (f32, f32)| {
        built
            .datavents
            .iter()
            .find(|v| (v.x - p.0).abs() < 1.0 && (v.z - p.1).abs() < 1.0)
            .expect("a built datavent")
            .facing
    };
    for &p in &map.vents {
        let twin = spring_map::kpgen::rot180(p, w, d);
        if twin == p {
            continue; // the middle vent
        }
        assert_eq!(
            facing(twin),
            (facing(p) + 2) % 4,
            "vent {p:?} and its twin face differently"
        );
    }

    // Fair: the two homes walk equally far to each vent of the other
    // side and to the middle (the layout is a half turn).
    let dist = |from: (f32, f32), to: (f32, f32)| {
        let d = spring_map::kpgen::walk_distances(&built.parsed, &built.parsed.heights, from);
        let mx = built.parsed.header.map_x as usize;
        d[(to.1 / 8.0) as usize * mx + (to.0 / 8.0) as usize]
    };
    let (a, b) = (map.starts[0], map.starts[1]);
    let middle = corner(MIDDLE);
    let (da, db) = (dist(a, middle), dist(b, middle));
    println!("walk to the middle: {da} / {db} elmos");
    assert!((da - db).abs() < 1.0, "the homes are not equidistant");
    for &p in &map.vents {
        let twin = spring_map::kpgen::rot180(p, w, d);
        assert!(map.vents.contains(&twin), "vent {p:?} has no twin");
    }
}
