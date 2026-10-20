//! Generates `kernel-panic/assets/maps/deadlock_v1.kpmap`.
//!
//! The layout follows Raghna's Hooked (Beyond All Reason): two hook
//! shaped plateaus wrapped around a low basin, a start corridor down
//! each short edge, the basin entered through the openings the hooks
//! leave, and a crater in the middle; 180° rotationally symmetric. Bus
//! lanes run along the hooks.
//!
//! ```sh
//! cargo run -p spring-map --example gen_deadlock --features write --release
//! ```

use spring_map::kpgen::{Cell, KpMap, Level, Ramp, Rect, Side};

/// Cells across and down: a 512 × 384 square map.
const CW: usize = 128;
const CD: usize = 96;

const PIT: usize = 0;
const BASE: usize = 1;
const HIGH: usize = 2;
const RIM: usize = 3;

fn levels() -> Vec<Level> {
    vec![
        Level {
            height: 50.0,
            fill: [8, 16, 96],
            line: [40, 110, 230],
        },
        Level {
            height: 170.0,
            fill: [138, 86, 8],
            line: [230, 150, 30],
        },
        Level {
            height: 330.0,
            fill: [96, 64, 140],
            line: [190, 150, 250],
        },
        Level {
            height: 560.0,
            fill: [226, 170, 40],
            line: [255, 220, 90],
        },
    ]
}

/// One hook and its share of the core; `main` adds the other half
/// turned 180°.
const HOOK_STRIP: Rect = Rect::new(22, 3, 113, 17);
const HOOK_ARM: Rect = Rect::new(97, 3, 113, 58);
const HOOK_RAMPS: [(Rect, Side); 2] = [
    (Rect::new(22, 3, 30, 17), Side::West),
    (Rect::new(97, 50, 113, 58), Side::South),
];
/// The bus lanes along the hook, ramps included.
const HOOK_LANES: [Rect; 2] = [Rect::new(22, 12, 100, 14), Rect::new(98, 12, 100, 58)];
const CORE: Rect = Rect::new(56, 44, 72, 52);
const CORE_RAMP: Rect = Rect::new(48, 44, 56, 52);

/// Datavents of one half, at cell corners (× 32 elmos); the other half
/// is these turned 180°.
const VENTS: [(usize, usize); 9] = [
    (120, 7), // north-east start
    (8, 7),   // north-west start
    (119, 38),
    (62, 7), // on the hook
    (105, 7),
    (45, 33), // basin
    (83, 33),
    (20, 28), // the west opening
    (58, 48), // core
];

/// Start positions, as indices into the vent list (half `0` = [`VENTS`],
/// `1` = turned): the duel pair first, then the other two corners, the
/// corridors, the openings.
const STARTS: [(usize, usize); 8] = [
    (1, 0),
    (0, 0),
    (0, 1),
    (1, 1),
    (1, 2),
    (0, 2),
    (0, 7),
    (1, 7),
];

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/deadlock_v1.kpmap"
        )
        .into()
    });
    let mut map = KpMap::new(CW, CD, levels(), BASE, RIM);
    for rect in [HOOK_STRIP, HOOK_ARM] {
        map.fill(rect, Cell::Floor(HIGH));
        map.fill(rect.rot180(CW, CD), Cell::Floor(HIGH));
    }
    map.fill(CORE, Cell::Floor(PIT));
    let hook_ramps = HOOK_RAMPS.map(|(rect, low_side)| Ramp {
        rect,
        low_side,
        low: BASE,
        high: HIGH,
    });
    let core_ramp = Ramp {
        rect: CORE_RAMP,
        low_side: Side::East,
        low: PIT,
        high: BASE,
    };
    for ramp in hook_ramps.into_iter().chain([core_ramp]) {
        map.add_ramp(ramp);
        map.add_ramp(ramp.rot180(CW, CD));
    }
    map.reset_rim();
    map.lanes = HOOK_LANES
        .iter()
        .flat_map(|&r| [r, r.rot180(CW, CD)])
        .collect();
    let corner = |(x, z): (usize, usize)| ((x * 32) as f32, (z * 32) as f32);
    map.vents = VENTS
        .iter()
        .map(|&v| corner(v))
        .chain(VENTS.iter().map(|&(x, z)| corner((CW - x, CD - z))))
        .collect();
    map.starts = STARTS
        .iter()
        .map(|&(half, i)| map.vents[half * VENTS.len() + i])
        .collect();
    map.description =
        "Deadlock v1. Map for Kernel Panic, after Raghna's Hooked. Intended for 2-4 players."
            .into();
    map.write(&out);
}
