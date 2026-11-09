//! Generates `kernel-panic/assets/maps/fork_bomb_v1.kpmap`.
//!
//! An eight-player free-for-all built to die fast and loud: every
//! player starts in a tiny alcove on the rim (two to a side) with one
//! datavent, and every alcove forks: its stem splits in a bar into two
//! tines that run into one open arena over the void, where five vents
//! are contested: one at the middle of each side, between the two forks
//! that reach it, and one on a raised plateau at the centre. Cover is
//! four kernel mesas on the arena's diagonals. Quarter-turn symmetric.
//!
//! ```sh
//! cargo run -p spring-map --example gen_fork_bomb --features write --release
//! ```

use spring_map::kpgen::{CELL, Cell, KpMap, Level, Ramp, Rect, Side, rot90};

/// Cells across and down: a 3072 × 3072 square map.
const N: usize = 96;

const BASE: usize = 0;
const BOMB: usize = 1;
const MESA: usize = 2;
const RIM: usize = 3;

fn levels() -> Vec<Level> {
    // Corrupted Core's reds.
    vec![
        Level {
            height: 200.0,
            fill: [84, 12, 12],
            line: [210, 56, 40],
        },
        Level {
            height: 300.0,
            fill: [130, 40, 8],
            line: [255, 140, 40],
        },
        Level {
            height: 420.0,
            fill: [40, 8, 8],
            line: [140, 30, 30],
        },
        Level {
            height: 520.0,
            fill: [226, 170, 40],
            line: [255, 220, 90],
        },
    ]
}

/// Where the two alcoves on the north side stand (their middle column);
/// the other sides are these turned.
const ALCOVES: [usize; 2] = [36, 60];

/// One fork of the north side, round column `cx`: the alcove on the
/// rim, its stem, the bar the stem splits in, the two tines.
fn fork(cx: usize) -> [Rect; 5] {
    [
        Rect::new(cx - 5, 3, cx + 5, 9),
        Rect::new(cx - 3, 9, cx + 3, 14),
        Rect::new(cx - 8, 14, cx + 8, 19),
        Rect::new(cx - 8, 19, cx - 2, 26),
        Rect::new(cx + 2, 19, cx + 8, 26),
    ]
}

const ARENA: Rect = Rect::new(26, 26, 70, 70);
const MESAS: Rect = Rect::new(34, 34, 40, 40);
const PLATEAU: Rect = Rect::new(43, 43, 53, 53);
/// The north ramp; the other three are it turned.
const PLATEAU_RAMP: Rect = Rect::new(45, 37, 51, 43);
/// The datavent between the north side's two forks (cell corners), and
/// the one in the middle.
const MOUTH: (usize, usize) = (48, 32);
const MIDDLE: (usize, usize) = (48, 48);
/// Where an alcove's vent stands: its middle.
const ALCOVE_VENT_Z: usize = 6;

/// `r` turned `k` quarters clockwise.
fn turned(r: Rect, k: usize) -> Rect {
    (0..k).fold(r, |r, _| r.rot90(N))
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/fork_bomb_v1.kpmap"
        )
        .into()
    });
    let mut map = KpMap::new(N, N, levels(), BASE, RIM);
    map.fill(Rect::new(0, 0, N, N), Cell::Void);
    map.fill(ARENA, Cell::Floor(BASE));
    for k in 0..4 {
        for cx in ALCOVES {
            for rect in fork(cx) {
                map.fill(turned(rect, k), Cell::Floor(BASE));
            }
        }
        map.fill(turned(MESAS, k), Cell::Floor(MESA));
    }
    map.fill(PLATEAU, Cell::Floor(BOMB));
    for k in 0..4 {
        let mut ramp = Ramp {
            rect: PLATEAU_RAMP,
            low_side: Side::North,
            low: BASE,
            high: BOMB,
        };
        for _ in 0..k {
            ramp = ramp.rot90(N);
        }
        map.add_ramp(ramp);
    }
    // The ramps rotate about the grid (the plateau is not on its centre
    // line cell, but on its middle: 43..53 is centred on 48).
    map.reset_rim();

    let s = (N * CELL) as f32;
    let corner = |(x, z): (usize, usize)| ((x * CELL) as f32, (z * CELL) as f32);
    let turn = |p: (f32, f32), k: usize| (0..k).fold(p, |p, _| rot90(p, s));
    // Alcove vents: for each alcove column the four sides in the order
    // north, south, east, west, so that the first players face across
    // the arena.
    let alcoves: Vec<(f32, f32)> = ALCOVES
        .iter()
        .flat_map(|&cx| {
            let p = corner((cx, ALCOVE_VENT_Z));
            [0, 2, 1, 3].map(|k| turn(p, k))
        })
        .collect();
    map.starts = alcoves.clone();
    map.vents = alcoves;
    map.vents.extend((0..4).map(|k| turn(corner(MOUTH), k)));
    map.vents.push(corner(MIDDLE));
    map.description = "Fork Bomb v1. Map for Kernel Panic. Eight alcoves fork into one open \
                       arena. Intended for 8 players, free for all."
        .into();
    let built = map.write(&out);

    for &(x, z) in &map.vents {
        let twin = rot90((x, z), s);
        assert!(
            map.vents.contains(&twin),
            "vent ({x}, {z}) has no quarter-turn twin"
        );
    }
    // Every alcove walks as far to the middle as every other.

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
    let to_middle: Vec<f32> = map
        .starts
        .iter()
        .map(|&from| {
            let d = spring_map::kpgen::walk_distances(&built.parsed, &built.parsed.heights, from);
            let m = corner(MIDDLE);
            d[(m.1 / 8.0) as usize * mx + (m.0 / 8.0) as usize]
        })
        .collect();
    println!("walk to the middle: {to_middle:?}");
    assert!(
        to_middle.iter().all(|d| (d - to_middle[0]).abs() < 1.0),
        "the alcoves are not equidistant from the middle"
    );
}
