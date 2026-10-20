//! Generates `kernel-panic/assets/maps/three_way_handshake_v1.kpmap`.
//!
//! After Beyond All Reason's The Desert Triad and Pillar of Doom: a round
//! arena over the void for three players, one per faction. Each starts
//! on a raised plateau whose one ramp faces the centre; three holes
//! split the arena between them and a hill rises in the middle with a
//! datavent toward each. A third of a turn symmetric: the datavents and
//! starts exactly, the terraces as nearly as the 32-elmo grid allows
//! (radial ramps keep their slopes exact).
//!
//! ```sh
//! cargo run -p spring-map --example gen_three_way --features write --release
//! ```

use spring_detmath::DetFloat;
use spring_map::kpgen::{Cell, KpMap, Level, RadialRamp, rotate};

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

const ARENA: f32 = 1900.0;
/// Start plateaus: distance from the centre, radius, the ramp's outer
/// radius and its half-width (degrees, seen from the plateau).
const PLATEAU_AT: f32 = 1350.0;
const PLATEAU: f32 = 420.0;
const PLATEAU_RAMP: f32 = 680.0;
const RAMP_HALF_ANGLE: f32 = 35.0;
/// Holes between the players.
const HOLE_AT: f32 = 1150.0;
const HOLE: f32 = 260.0;
/// The hill: top radius and foot radius.
const HILL: f32 = 300.0;
const HILL_FOOT: f32 = 560.0;

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dz) = (a.0 - b.0, a.1 - b.1);
    (dx * dx + dz * dz).sqrt()
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/three_way_handshake_v1.kpmap"
        )
        .into()
    });
    let mut map = KpMap::new(N, N, levels(), BASE, RIM);
    let (s, _) = map.world();
    let c = (s * 0.5, s * 0.5);
    let turn = |p: (f32, f32), k: usize| rotate(p, c, 120.0 * k as f32);
    let toward = |r: f32, degrees: f32| rotate((c.0, c.1 - r), c, degrees);

    map.fill_where(Cell::Void, |_, _| true);
    map.fill_disc(c, ARENA, Cell::Floor(BASE));
    for k in 0..3 {
        map.fill_disc(turn(toward(HOLE_AT, 60.0), k), HOLE, Cell::Void);
        let p = turn(toward(PLATEAU_AT, 0.0), k);
        let ramp = map.add_radial(RadialRamp {
            centre: p,
            r_high: PLATEAU,
            r_low: PLATEAU_RAMP,
            low: BASE,
            high: HIGH,
        });
        let inward = ((c.0 - p.0) / PLATEAU_AT, (c.1 - p.1) / PLATEAU_AT);
        let cos_half = libm::cosf(RAMP_HALF_ANGLE.to_radians());
        map.fill_where(ramp, |x, z| {
            let d = dist((x, z), p);
            let along = ((x - p.0) * inward.0 + (z - p.1) * inward.1) / d.det_max(1.0);
            d <= PLATEAU_RAMP && along >= cos_half
        });
        map.fill_disc(p, PLATEAU, Cell::Floor(HIGH));
    }
    let hill = map.add_radial(RadialRamp {
        centre: c,
        r_high: HILL,
        r_low: HILL_FOOT,
        low: BASE,
        high: CORE,
    });
    map.fill_disc(c, HILL_FOOT, hill);
    map.fill_disc(c, HILL, Cell::Floor(CORE));
    map.reset_rim();

    // One player's datavents; the others' are these turned. Starts
    // first: the plateau, then the far side of the hole on its right.
    let p = toward(PLATEAU_AT, 0.0);
    let local = |back: f32, side: f32| (p.0 + side, p.1 - back);
    let third = [
        p,
        local(120.0, 200.0),
        local(120.0, -200.0),
        toward(950.0, 36.0),
        toward(950.0, -36.0),
        toward(1650.0, 60.0),
        toward(160.0, 0.0),
    ];
    for k in 0..3 {
        map.vents.extend(third.iter().map(|&v| turn(v, k)));
    }
    let start = |k: usize, i: usize| map.vents[k * third.len() + i];
    map.starts = (0..3)
        .map(|k| start(k, 0))
        .chain((0..3).map(|k| start(k, 5)))
        .collect();
    map.description = "Three-Way Handshake v1. Map for Kernel Panic, after The Desert Triad \
                       and Pillar of Doom. Intended for 3 players (up to 6)."
        .into();
    map.write(&out);
}
