//! Datavent facings: which way a factory built on a vent faces.
//!
//! The original decides it with one rule, in `kp_autospam.lua` (the
//! `buildfacing` it sets before the first build) and
//! `hand_autobuild.lua`'s `GetFacing`: toward the map centre, along the
//! axis the vent is further out on. That is the preference here; the
//! ground gets a veto the way the port's homebases get one
//! (`TurnTowardBarycenter`'s `exit_heading`): a facing whose exit square
//! is unwalkable, or whose half circle in front is mostly blocked, loses
//! to one where the products can walk out, and among those the one with
//! the most room to the map edge wins, ties going to the one nearest the
//! centre rule. On most maps every facing is open and the rule decides;
//! in a niche there is only one answer and the ground gives it.
//!
//! Computed once by `bake-map` from the heightmap and typemap and stored
//! in the `.kpmap` ([`Datavent`]), so every build and every lockstep peer
//! agrees; the game reads it back and only computes facings itself for
//! vents a map adds at runtime (Hex Farm).

use spring_pathfinding::{SQUARE_SIZE, max_slope_from_degrees, slope_map};

use crate::map_types::{Datavent, ParsedMap, TerrainTypes};

/// Spring's build facings: `0` south (+z), `1` east (+x), `2` north
/// (−z), `3` west (−x).
pub const FACING_SOUTH: u8 = 0;
pub const FACING_EAST: u8 = 1;
pub const FACING_NORTH: u8 = 2;
pub const FACING_WEST: u8 = 3;

/// The front direction of a facing, in XZ.
pub fn facing_vector(facing: u8) -> (f32, f32) {
    match facing % 4 {
        FACING_SOUTH => (0.0, 1.0),
        FACING_EAST => (1.0, 0.0),
        FACING_NORTH => (0.0, -1.0),
        _ => (-1.0, 0.0),
    }
}

/// The quarter-snapped direction from `from` to `to`: the axis of the
/// larger offset (`GetFacing`'s `|mapSizeX − 2x| > |mapSizeZ − 2z|`),
/// south when the two coincide.
pub fn facing_toward(from: (f32, f32), to: (f32, f32)) -> u8 {
    let (dx, dz) = (to.0 - from.0, to.1 - from.1);
    if dx.abs() > dz.abs() {
        if dx > 0.0 { FACING_EAST } else { FACING_WEST }
    } else if dz < 0.0 {
        FACING_NORTH
    } else {
        FACING_SOUTH
    }
}

/// The sizes an exit scan works with: the factory's radius (its
/// products appear at `factory_radius + unit_radius` in front) and its
/// products' radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExitScan {
    pub factory_radius: f32,
    pub unit_radius: f32,
}

/// A datavent factory (Socket, Window, Port: `FootprintX/Z=4`, 32 elmos
/// across) and its 2×2-footprint products (Bit, Bug, Packet).
pub const SECONDARY_FACTORY: ExitScan = ExitScan {
    factory_radius: 24.0,
    unit_radius: 8.0,
};

/// `MOVEINFO.TDF`: every KP mover class has `MaxSlope=36`.
pub const MOVER_MAX_SLOPE_DEGREES: f32 = 36.0;

/// The facing a factory at `from` should take so that its products walk
/// cleanly into the map: `preferred` unless the ground says otherwise.
/// `world` is the map's size in elmos; `walkable(x, z)` is the mover's
/// view of a point.
///
/// Scoring (the port's `exit_heading`): a facing scores nothing when its
/// exit point is unwalkable, else one plus the walkable samples of the
/// half circle in front (the engine's `SendToEmptySpot` scan). Facings
/// scoring at least three quarters of the best are candidates; the
/// preferred one is kept while it is a candidate and no other candidate
/// has a quarter more room to the map edge ahead of it, otherwise the
/// candidate with the most room wins, ties going to the one nearest the
/// map centre's facing.
pub fn exit_facing(
    from: (f32, f32),
    preferred: u8,
    world: (f32, f32),
    scan: &ExitScan,
    walkable: impl Fn(f32, f32) -> bool,
) -> u8 {
    const KEEP_FRACTION: f32 = 0.75;
    const ROOM_SWITCH: f32 = 1.25;
    const STEPS: usize = 100;
    let centre = (world.0 * 0.5, world.1 * 0.5);
    let toward_centre = facing_toward(from, centre);
    let arc = 4.0 * scan.factory_radius + 4.0 * scan.unit_radius;
    let exit = scan.factory_radius + scan.unit_radius;
    let inside = |x: f32, z: f32| x >= 0.0 && z >= 0.0 && x < world.0 && z < world.1;
    let open = |x: f32, z: f32| inside(x, z) && walkable(x, z);

    let score = |facing: u8| -> usize {
        let (fx, fz) = facing_vector(facing);
        // `rightdir = frontdir × updir`: facing +Z, right is −X.
        let (rx, rz) = (-fz, fx);
        if !open(from.0 + fx * exit, from.1 + fz * exit) {
            return 0;
        }
        let mut n = 1;
        for i in 0..STEPS {
            let a = i as f32 * core::f32::consts::PI / (STEPS as f32 * 0.5);
            let (c, s) = (libm::cosf(a), libm::sinf(a));
            if c < 0.0 {
                continue;
            }
            let x = from.0 + fx * (arc * c) + rx * (arc * s);
            let z = from.1 + fz * (arc * c) + rz * (arc * s);
            if open(x, z) {
                n += 1;
            }
        }
        n
    };
    // Distance to the map edge straight ahead.
    let room = |facing: u8| -> f32 {
        match facing % 4 {
            FACING_SOUTH => world.1 - from.1,
            FACING_EAST => world.0 - from.0,
            FACING_NORTH => from.1,
            _ => from.0,
        }
        .max(0.0)
    };
    let turns_from_centre = |facing: u8| -> u8 {
        let d = (facing + 4 - toward_centre) % 4;
        d.min(4 - d)
    };

    let scored: Vec<(u8, usize)> = (0..4).map(|f| (f, score(f))).collect();
    let best = scored.iter().map(|(_, n)| *n).max().unwrap_or(0);
    if best == 0 {
        return preferred;
    }
    let band: Vec<u8> = scored
        .iter()
        .filter(|(_, n)| *n as f32 >= best as f32 * KEEP_FRACTION)
        .map(|(f, _)| *f)
        .collect();
    let roomiest = band
        .iter()
        .copied()
        .max_by(|a, b| {
            room(*a)
                .total_cmp(&room(*b))
                .then_with(|| turns_from_centre(*b).cmp(&turns_from_centre(*a)))
        })
        .unwrap_or(preferred);
    if band.contains(&preferred) && room(roomiest) < room(preferred) * ROOM_SWITCH {
        preferred
    } else {
        roomiest
    }
}

/// Every geovent of `parsed` with its facing, from the heightmap's
/// slopes (a square steeper than the movers' `MaxSlope` is blocked) and
/// the typemap (a terrain type with zero kbot speed is void).
pub fn datavent_facings(parsed: &ParsedMap, types: &TerrainTypes) -> Vec<Datavent> {
    let hm_w = parsed.header.heightmap_width();
    let hm_h = parsed.header.heightmap_height();
    let world = (
        parsed.header.map_x as f32 * SQUARE_SIZE,
        parsed.header.map_y as f32 * SQUARE_SIZE,
    );
    let slopes = if parsed.heights.len() == hm_w * hm_h && hm_w > 1 && hm_h > 1 {
        slope_map(&parsed.heights, hm_w as u32, hm_h as u32)
    } else {
        Vec::new()
    };
    let (sq_w, sq_h) = (hm_w - 1, hm_h - 1);
    let cap = max_slope_from_degrees(MOVER_MAX_SLOPE_DEGREES);
    let (tm_w, tm_h) = (
        parsed.header.typemap_width(),
        parsed.header.typemap_height(),
    );
    let typed = parsed.typemap.len() == tm_w * tm_h && !types.is_neutral();
    let walkable = |x: f32, z: f32| {
        let (sx, sz) = ((x / SQUARE_SIZE) as usize, (z / SQUARE_SIZE) as usize);
        if sx >= sq_w || sz >= sq_h {
            return false;
        }
        if !slopes.is_empty() && slopes[sz * sq_w + sx] > cap {
            return false;
        }
        if typed {
            let (tx, tz) = ((sx / 2).min(tm_w - 1), (sz / 2).min(tm_h - 1));
            if types.get(parsed.typemap[tz * tm_w + tx]).kbot <= 0.0 {
                return false;
            }
        }
        true
    };
    parsed
        .features
        .iter()
        .filter(|f| f.feature_type.is_geovent())
        .map(|f| {
            let from = (f.x, f.z);
            let preferred = facing_toward(from, (world.0 * 0.5, world.1 * 0.5));
            Datavent {
                x: f.x,
                z: f.z,
                facing: exit_facing(from, preferred, world, &SECONDARY_FACTORY, walkable),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facing_toward_takes_the_larger_axis() {
        let centre = (512.0, 512.0);
        assert_eq!(facing_toward((100.0, 500.0), centre), FACING_EAST);
        assert_eq!(facing_toward((900.0, 520.0), centre), FACING_WEST);
        assert_eq!(facing_toward((500.0, 100.0), centre), FACING_SOUTH);
        assert_eq!(facing_toward((520.0, 900.0), centre), FACING_NORTH);
        // The corner: equal offsets go south/north (the `>` in GetFacing).
        assert_eq!(facing_toward((100.0, 100.0), centre), FACING_SOUTH);
        assert_eq!(facing_toward(centre, centre), FACING_SOUTH);
    }

    #[test]
    fn facing_vectors_are_the_spring_quarters() {
        assert_eq!(facing_vector(FACING_SOUTH), (0.0, 1.0));
        assert_eq!(facing_vector(FACING_EAST), (1.0, 0.0));
        assert_eq!(facing_vector(FACING_NORTH), (0.0, -1.0));
        assert_eq!(facing_vector(FACING_WEST), (-1.0, 0.0));
    }

    /// Open ground: the original's rule, toward the centre.
    #[test]
    fn open_ground_faces_the_centre() {
        let world = (1024.0, 1024.0);
        let f = |from| {
            exit_facing(
                from,
                facing_toward(from, (512.0, 512.0)),
                world,
                &SECONDARY_FACTORY,
                |_, _| true,
            )
        };
        assert_eq!(f((100.0, 500.0)), FACING_EAST);
        assert_eq!(f((900.0, 500.0)), FACING_WEST);
        assert_eq!(f((500.0, 100.0)), FACING_SOUTH);
    }

    /// A vent in a niche open only to the north: the one right answer,
    /// whatever the centre rule says.
    #[test]
    fn a_niche_has_one_answer() {
        let world = (1024.0, 1024.0);
        let from = (500.0, 900.0);
        // Walls east, west and south of the vent, 40 elmos out.
        let walkable = |x: f32, z: f32| (x - from.0).abs() < 40.0 && z < from.1 + 40.0;
        let preferred = facing_toward(from, (512.0, 512.0));
        assert_eq!(preferred, FACING_NORTH);
        assert_eq!(
            exit_facing(from, preferred, world, &SECONDARY_FACTORY, walkable),
            FACING_NORTH
        );
        // Rotate the niche: open only to the east, though the centre is north.
        let walkable = |x: f32, z: f32| (z - from.1).abs() < 40.0 && x > from.0 - 40.0;
        assert_eq!(
            exit_facing(from, preferred, world, &SECONDARY_FACTORY, walkable),
            FACING_EAST
        );
    }

    /// Nothing walkable anywhere: the rule stands.
    #[test]
    fn sealed_vent_keeps_the_rule() {
        let world = (1024.0, 1024.0);
        assert_eq!(
            exit_facing(
                (100.0, 500.0),
                FACING_EAST,
                world,
                &SECONDARY_FACTORY,
                |_, _| false
            ),
            FACING_EAST
        );
    }

    /// Near the map edge the room ahead breaks a tie away from the edge.
    #[test]
    fn edge_vent_turns_into_the_map() {
        let world = (1024.0, 1024.0);
        // In the south-west corner, equal offsets: the rule says south,
        // but south has 60 elmos of room and east has 964.
        let from = (60.0, 964.0);
        let preferred = facing_toward(from, (512.0, 512.0));
        assert_eq!(preferred, FACING_NORTH);
        let got = exit_facing(from, preferred, world, &SECONDARY_FACTORY, |_, _| true);
        assert!(got == FACING_NORTH || got == FACING_EAST, "{got}");
    }
}
