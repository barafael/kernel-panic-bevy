//! Generates `kernel-panic/assets/maps/heat_sink_v1.kpmap`.
//!
//! A CPU die in the middle of a copper plate and four banks of straight
//! fins radiating from it, one to each side. The fins are walls, so
//! every bank is three troughs running from the die's plenum out to the
//! apron round the rim, each with a datavent halfway; a bus lane (the
//! heat pipe) runs down the middle trough. Between the banks, in the
//! four corners, a start plateau with a second vent a ramp away and a
//! mounting hole (void) for cover. Quarter-turn symmetric.
//!
//! ```sh
//! cargo run -p spring-map --example gen_heat_sink --features write --release
//! ```

use spring_map::kpgen::{CELL, Cell, KpMap, Level, Ramp, Rect, Side, rot90};

/// Cells across and down: a 4096 × 4096 square map.
const N: usize = 128;

const BASE: usize = 0;
const FIN: usize = 1;
const DIE: usize = 2;
const SPREADER: usize = 3;
const RIM: usize = 4;

fn levels() -> Vec<Level> {
    // Copper plate, aluminium fins, the die's silicon blue.
    vec![
        Level {
            height: 120.0,
            fill: [88, 44, 22],
            line: [190, 110, 50],
        },
        Level {
            height: 330.0,
            fill: [150, 158, 175],
            line: [225, 232, 245],
        },
        Level {
            height: 220.0,
            fill: [30, 50, 140],
            line: [90, 130, 255],
        },
        Level {
            height: 260.0,
            fill: [170, 90, 30],
            line: [255, 170, 80],
        },
        Level {
            height: 520.0,
            fill: [226, 170, 40],
            line: [255, 220, 90],
        },
    ]
}

/// The north bank's four fins: three cells thick, six-cell troughs
/// between; the other banks are these turned. They stop short of the
/// diagonals, so every corner has a way through to the die's plenum.
const FINS: [Rect; 4] = [
    Rect::new(49, 14, 52, 46),
    Rect::new(58, 14, 61, 46),
    Rect::new(67, 14, 70, 46),
    Rect::new(76, 14, 79, 46),
];
/// The heat pipe along the middle trough, apron to plenum.
const PIPE: Rect = Rect::new(63, 6, 65, 54);
/// The die, and its north ramp (the others are it turned).
const DIE_RECT: Rect = Rect::new(58, 58, 70, 70);
const DIE_RAMP: Rect = Rect::new(58, 53, 70, 58);
/// The north-west corner's start plateau and its ramps (the other
/// corners get these turned), and the mounting hole.
const SPREADER_RECT: Rect = Rect::new(8, 8, 22, 22);
const SPREADER_RAMPS: [(Rect, Side); 2] = [
    (Rect::new(22, 11, 30, 19), Side::East),
    (Rect::new(11, 22, 19, 30), Side::South),
];
const HOLE: Rect = Rect::new(37, 37, 43, 43);

/// The north-west corner's vents (cell corners; the first is its
/// start) and the north bank's, one in each trough.
/// One cell off the diagonal: a vent exactly on it ties the facing rule
/// (toward the centre along the further axis), which breaks to the same
/// side in every corner and so is not symmetric.
const CORNER_VENTS: [(usize, usize); 2] = [(15, 16), (31, 30)];
const TROUGH_VENTS: [(usize, usize); 3] = [(55, 30), (64, 30), (73, 30)];
const DIE_VENT: (usize, usize) = (64, 64);

fn turned(r: Rect, k: usize) -> Rect {
    (0..k).fold(r, |r, _| r.rot90(N))
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/heat_sink_v1.kpmap"
        )
        .into()
    });
    let mut map = KpMap::new(N, N, levels(), BASE, RIM);
    for k in 0..4 {
        for fin in FINS {
            map.fill(turned(fin, k), Cell::Floor(FIN));
        }
        map.fill(turned(SPREADER_RECT, k), Cell::Floor(SPREADER));
        map.fill(turned(HOLE, k), Cell::Void);
    }
    map.fill(DIE_RECT, Cell::Floor(DIE));
    let turn_ramp = |mut ramp: Ramp, k: usize| {
        for _ in 0..k {
            ramp = ramp.rot90(N);
        }
        ramp
    };
    for k in 0..4 {
        map.add_ramp(turn_ramp(
            Ramp {
                rect: DIE_RAMP,
                low_side: Side::North,
                low: BASE,
                high: DIE,
            },
            k,
        ));
        for (rect, low_side) in SPREADER_RAMPS {
            map.add_ramp(turn_ramp(
                Ramp {
                    rect,
                    low_side,
                    low: BASE,
                    high: SPREADER,
                },
                k,
            ));
        }
    }
    map.reset_rim();
    map.lanes = (0..4).map(|k| turned(PIPE, k)).collect();

    let s = (N * CELL) as f32;
    let corner = |(x, z): (usize, usize)| ((x * CELL) as f32, (z * CELL) as f32);
    let turn = |p: (f32, f32), k: usize| (0..k).fold(p, |p, _| rot90(p, s));
    let corners: Vec<Vec<(f32, f32)>> = CORNER_VENTS
        .iter()
        .map(|&v| (0..4).map(|k| turn(corner(v), k)).collect())
        .collect();
    // Starts: the north-west corner first, then the south-east, so a
    // duel is across the plate.
    map.starts = [0, 2, 1, 3].map(|k| corners[0][k]).to_vec();
    map.vents = corners.into_iter().flatten().collect();
    for &v in &TROUGH_VENTS {
        map.vents.extend((0..4).map(|k| turn(corner(v), k)));
    }
    map.vents.push(corner(DIE_VENT));
    map.description = "Heat Sink v1. Map for Kernel Panic. Straight fins radiate from the die; \
                       vents sit in the troughs between them. Intended for 2-4 players."
        .into();
    let built = map.write(&out);

    for &(x, z) in &map.vents {
        let twin = rot90((x, z), s);
        assert!(
            map.vents.contains(&twin),
            "vent ({x}, {z}) has no quarter-turn twin"
        );
    }

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
        let twin = rot90(p, s);
        if twin == p {
            continue; // the middle vent
        }
        assert_eq!(
            facing(twin),
            (facing(p) + 3) % 4,
            "vent {p:?} and its twin face differently"
        );
    }
    let mx = built.parsed.header.map_x as usize;
    let to_die: Vec<f32> = map
        .starts
        .iter()
        .map(|&from| {
            let d = spring_map::kpgen::walk_distances(&built.parsed, &built.parsed.heights, from);
            let m = corner(DIE_VENT);
            d[(m.1 / 8.0) as usize * mx + (m.0 / 8.0) as usize]
        })
        .collect();
    println!("walk to the die: {to_die:?}");
    // A vent sits on a cell corner, so which of the squares round it a
    // walk starts on differs by a turn: a few elmos, not a layout skew.
    assert!(
        to_die
            .iter()
            .all(|d| (d - to_die[0]).abs() < 0.01 * to_die[0]),
        "the corners are not equidistant from the die"
    );
}
