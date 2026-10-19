//! Generates `kernel-panic/assets/maps/deadlock_v1.kpmap`.
//!
//! The layout follows Raghna's Hooked (Beyond All Reason): two hook
//! shaped plateaus wrapped around a low basin, a start corridor down
//! each short edge, the basin entered through the openings the hooks
//! leave, and a crater in the middle; 180° rotationally symmetric. The
//! look follows FireStorm's Dual Core / Quad Core / Data Cache L1: flat
//! terraces on a 32-elmo grid, cliffs drawn as dark cells, ramps
//! hatched, datavents on pads, a raised rim, and Dual Core's 2× speed
//! bus lanes (terrain type 3) along the hooks.
//!
//! ```sh
//! cargo run -p spring-map --example gen_deadlock --features write --release
//! ```

use spring_detmath::DetFloat;
use spring_map::SpringMap;
use spring_map::baked::write_baked_map;
use spring_map::datavent::{MOVER_MAX_SLOPE_DEGREES, datavent_facings};
use spring_map::map_info::{Atmosphere, Lighting, MapInfo, StartPosition};
use spring_map::map_types::{
    FeatureType, GroundTexture, MapFeature, MapHeader, ParsedMap, TerrainMoveSpeeds, TerrainTypes,
};
use spring_pathfinding::{max_slope_from_degrees, slope_map};

/// Elmos per layout cell (and texels: the texture is one texel per elmo).
const CELL: usize = 32;
/// Map squares per cell.
const CELL_SQ: usize = CELL / 8;
/// Layout cells across and down: a 512 × 384 square map.
const CW: usize = 128;
const CD: usize = 96;
const MAP_X: usize = CW * CELL_SQ;
const MAP_Y: usize = CD * CELL_SQ;

/// A height difference over one cell larger than this is a cliff; a
/// ramp rises at most 20 per cell.
const CLIFF_STEP: f32 = 60.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Level {
    Pit,
    Base,
    High,
    Rim,
}

impl Level {
    fn height(self) -> f32 {
        match self {
            Level::Pit => 50.0,
            Level::Base => 170.0,
            Level::High => 330.0,
            Level::Rim => 560.0,
        }
    }

    /// Grid fill and line colours.
    fn colours(self) -> ([u8; 3], [u8; 3]) {
        match self {
            Level::Pit => ([8, 16, 96], [40, 110, 230]),
            Level::Base => ([138, 86, 8], [230, 150, 30]),
            Level::High => ([96, 64, 140], [190, 150, 250]),
            Level::Rim => ([226, 170, 40], [255, 220, 90]),
        }
    }
}

/// Which side of a ramp is its low end.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Side {
    West,
    East,
    North,
    South,
}

impl Side {
    fn opposite(self) -> Side {
        match self {
            Side::West => Side::East,
            Side::East => Side::West,
            Side::North => Side::South,
            Side::South => Side::North,
        }
    }
}

/// A half-open rectangle of cells.
#[derive(Clone, Copy, Debug)]
struct Rect {
    x0: usize,
    z0: usize,
    x1: usize,
    z1: usize,
}

impl Rect {
    const fn new(x0: usize, z0: usize, x1: usize, z1: usize) -> Self {
        Self { x0, z0, x1, z1 }
    }

    /// The rectangle turned 180° about the map centre.
    fn rotated(self) -> Self {
        Self::new(CW - self.x1, CD - self.z1, CW - self.x0, CD - self.z0)
    }

    fn contains(&self, x: usize, z: usize) -> bool {
        (self.x0..self.x1).contains(&x) && (self.z0..self.z1).contains(&z)
    }
}

#[derive(Clone, Copy, Debug)]
struct Ramp {
    rect: Rect,
    low_side: Side,
    low: Level,
    high: Level,
}

impl Ramp {
    fn rotated(self) -> Self {
        Self {
            rect: self.rect.rotated(),
            low_side: self.low_side.opposite(),
            ..self
        }
    }

    /// Height at world point `(x, z)` (elmos), clamped to the ramp.
    fn height_at(&self, x: f32, z: f32) -> f32 {
        let r = self.rect;
        let (x0, x1) = ((r.x0 * CELL) as f32, (r.x1 * CELL) as f32);
        let (z0, z1) = ((r.z0 * CELL) as f32, (r.z1 * CELL) as f32);
        let t = match self.low_side {
            Side::West => (x - x0) / (x1 - x0),
            Side::East => (x1 - x) / (x1 - x0),
            Side::North => (z - z0) / (z1 - z0),
            Side::South => (z1 - z) / (z1 - z0),
        }
        .det_clamp(0.0, 1.0);
        self.low.height() + (self.high.height() - self.low.height()) * t
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cell {
    /// The outer ring, falling off the rim to the map edge.
    Out,
    Floor(Level),
    Ramp(usize),
}

/// One hook and its share of the core; [`Layout::new`] adds the other
/// half turned 180°.
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

struct Layout {
    cells: Vec<Cell>,
    ramps: Vec<Ramp>,
    lanes: Vec<Rect>,
    vents: Vec<(f32, f32)>,
}

impl Layout {
    fn new() -> Self {
        let mut cells = vec![Cell::Floor(Level::Base); CW * CD];
        let mut ramps = Vec::new();
        let fill = |cells: &mut Vec<Cell>, r: Rect, c: Cell| {
            for z in r.z0..r.z1 {
                for x in r.x0..r.x1 {
                    cells[z * CW + x] = c;
                }
            }
        };
        for rect in [HOOK_STRIP, HOOK_ARM] {
            fill(&mut cells, rect, Cell::Floor(Level::High));
            fill(&mut cells, rect.rotated(), Cell::Floor(Level::High));
        }
        let hook_ramps = HOOK_RAMPS.map(|(rect, low_side)| Ramp {
            rect,
            low_side,
            low: Level::Base,
            high: Level::High,
        });
        let core_ramp = Ramp {
            rect: CORE_RAMP,
            low_side: Side::East,
            low: Level::Pit,
            high: Level::Base,
        };
        fill(&mut cells, CORE, Cell::Floor(Level::Pit));
        for ramp in hook_ramps.into_iter().chain([core_ramp]) {
            for r in [ramp, ramp.rotated()] {
                fill(&mut cells, r.rect, Cell::Ramp(ramps.len()));
                ramps.push(r);
            }
        }
        for z in 0..CD {
            for x in 0..CW {
                let ring = x.min(z).min(CW - 1 - x).min(CD - 1 - z);
                if ring == 0 {
                    cells[z * CW + x] = Cell::Out;
                } else if ring <= 2 {
                    cells[z * CW + x] = Cell::Floor(Level::Rim);
                }
            }
        }
        let lanes = HOOK_LANES.iter().flat_map(|&r| [r, r.rotated()]).collect();
        let corner = |(x, z): (usize, usize)| ((x * CELL) as f32, (z * CELL) as f32);
        let vents = VENTS
            .iter()
            .map(|&v| corner(v))
            .chain(VENTS.iter().map(|&(x, z)| corner((CW - x, CD - z))))
            .collect();
        Self {
            cells,
            ramps,
            lanes,
            vents,
        }
    }

    fn cell(&self, x: usize, z: usize) -> Cell {
        self.cells[z * CW + x]
    }

    /// The height a cell gives world point `(x, z)`, if it is not the
    /// outer ring.
    fn cell_height(&self, cell: Cell, x: f32, z: f32) -> Option<f32> {
        match cell {
            Cell::Out => None,
            Cell::Floor(level) => Some(level.height()),
            Cell::Ramp(i) => Some(self.ramps[i].height_at(x, z)),
        }
    }

    /// The level whose colours a cell is drawn in.
    fn level(&self, cell: Cell) -> Level {
        match cell {
            Cell::Out | Cell::Floor(Level::Rim) => Level::Rim,
            Cell::Floor(level) => level,
            Cell::Ramp(i) => self.ramps[i].high,
        }
    }

    fn vent_start(&self, (half, i): (usize, usize)) -> (f32, f32) {
        self.vents[half * VENTS.len() + i]
    }
}

/// The heightmap, and per vertex how far the cliffs lowered it.
fn heights(layout: &Layout) -> (Vec<f32>, Vec<f32>) {
    let (w, d) = (MAP_X + 1, MAP_Y + 1);
    // Every vertex takes the lowest of the cells it touches, so a cell
    // edge sits at the lower level and a cliff drops inside the upper
    // cell.
    let mut base = vec![0.0f32; w * d];
    for sz in 0..d {
        for sx in 0..w {
            let (x, z) = ((sx * 8) as f32, (sz * 8) as f32);
            let edge = sx.min(sz).min(MAP_X - sx).min(MAP_Y - sz);
            base[sz * w + sx] = if edge < CELL_SQ {
                // The outer ring: down from the rim to the map edge.
                Level::Rim.height() * (0.5 + 0.5 * edge as f32 / CELL_SQ as f32)
            } else {
                let (cx, cz) = (sx / CELL_SQ, sz / CELL_SQ);
                let touching = [(cx, cz), (cx.wrapping_sub(1), cz), (cx, cz.wrapping_sub(1))]
                    .into_iter()
                    .chain([(cx.wrapping_sub(1), cz.wrapping_sub(1))])
                    .filter(|&(x, z)| x < CW && z < CD)
                    .filter(|&(x, z)| {
                        // Only the cells whose closed rectangle holds the vertex.
                        let (lx, lz) = (
                            sx as isize - (x * CELL_SQ) as isize,
                            sz as isize - (z * CELL_SQ) as isize,
                        );
                        (0..=CELL_SQ as isize).contains(&lx) && (0..=CELL_SQ as isize).contains(&lz)
                    });
                touching
                    .filter_map(|(cx, cz)| layout.cell_height(layout.cell(cx, cz), x, z))
                    .fold(f32::MAX, |a, b| a.det_min(b))
            };
        }
    }
    // Cliffs: a vertex within one cell of ground more than a cliff step
    // lower slopes down to it across that cell (Chebyshev distance, so
    // the bands stay square like the grid).
    let mut heights = base.clone();
    let r = CELL_SQ as isize;
    for sz in 0..d {
        for sx in 0..w {
            let edge = sx.min(sz).min(MAP_X - sx).min(MAP_Y - sz);
            if edge < CELL_SQ {
                continue;
            }
            let here = base[sz * w + sx];
            let mut h = here;
            for dz in -r..=r {
                for dx in -r..=r {
                    let (ux, uz) = (sx as isize + dx, sz as isize + dz);
                    if ux < r || uz < r || ux > (MAP_X as isize - r) || uz > (MAP_Y as isize - r) {
                        continue;
                    }
                    let low = base[uz as usize * w + ux as usize];
                    if here - low > CLIFF_STEP {
                        let dist = dx.abs().max(dz.abs()) as f32 / CELL_SQ as f32;
                        h = h.det_min(low + (here - low) * dist);
                    }
                }
            }
            heights[sz * w + sx] = h;
        }
    }
    let lowered = base.iter().zip(&heights).map(|(b, h)| b - h).collect();
    (heights, lowered)
}

fn scale([r, g, b]: [u8; 3], k: f32) -> [u8; 3] {
    [r, g, b].map(|c| (c as f32 * k).det_min(255.0) as u8)
}

/// The ground texture, one texel per elmo.
fn texture(layout: &Layout, lowered: &[f32]) -> GroundTexture {
    let (tw, th) = (CW * CELL, CD * CELL);
    let mut pixels = vec![0u8; tw * th * 4];
    let w = MAP_X + 1;
    let lane_line = [255, 255, 60];
    // Does the cell at `(x, z)` overlap the square of half-side `reach` round a vent?
    let pad = |x: usize, z: usize, reach: usize| {
        layout.vents.iter().any(|&(vx, vz)| {
            let (vx, vz) = (vx as usize, vz as usize);
            x + CELL + reach > vx && x < vx + reach && z + CELL + reach > vz && z < vz + reach
        })
    };
    for cz in 0..CD {
        for cx in 0..CW {
            let cell = layout.cell(cx, cz);
            let level = layout.level(cell);
            let (fill, line) = level.colours();
            // A cell a cliff runs through is drawn dark.
            let cliff = (0..=CELL_SQ).any(|dz| {
                (0..=CELL_SQ).any(|dx| lowered[(cz * CELL_SQ + dz) * w + cx * CELL_SQ + dx] > 1.0)
            });
            let lane = layout.lanes.iter().any(|l| l.contains(cx, cz));
            let (x0, z0) = (cx * CELL, cz * CELL);
            let outer_pad = pad(x0, z0, 3 * CELL);
            let inner_pad = pad(x0, z0, 2 * CELL);
            for lz in 0..CELL {
                for lx in 0..CELL {
                    let (x, z) = (x0 + lx, z0 + lz);
                    let grid = lx == 0 || lz == 0 || lx == CELL - 1 || lz == CELL - 1;
                    let rgb = match cell {
                        Cell::Out => [0, 0, 0],
                        _ if cliff => {
                            if grid {
                                line
                            } else {
                                scale(fill, 0.3)
                            }
                        }
                        Cell::Ramp(i) => {
                            let ramp = &layout.ramps[i];
                            let (low_fill, _) = ramp.low.colours();
                            let hatch = (x + z) % CELL < 3 || (x + CELL - z % CELL) % CELL < 3;
                            if lane && grid {
                                lane_line
                            } else if hatch {
                                if ramp.low == Level::Pit {
                                    line
                                } else {
                                    [255, 170, 0]
                                }
                            } else {
                                scale(low_fill, 0.7)
                            }
                        }
                        _ if inner_pad => {
                            if grid {
                                [255, 255, 200]
                            } else {
                                [255, 236, 170]
                            }
                        }
                        _ if outer_pad => {
                            if grid {
                                line
                            } else {
                                scale(fill, 1.6)
                            }
                        }
                        _ if lane => {
                            if grid {
                                lane_line
                            } else {
                                scale(fill, 0.8)
                            }
                        }
                        _ => {
                            if grid {
                                line
                            } else {
                                fill
                            }
                        }
                    };
                    let i = (z * tw + x) * 4;
                    pixels[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                }
            }
        }
    }
    for &(vx, vz) in &layout.vents {
        draw_vent_icon(&mut pixels, tw, vx, vz);
    }
    GroundTexture {
        width: tw,
        height: th,
        pixels,
    }
}

/// L1's vent mark: a green ring with four brackets, on the pale pad.
fn draw_vent_icon(pixels: &mut [u8], tw: usize, vx: f32, vz: f32) {
    let green = [110u8, 220, 80];
    for dz in -60i32..60 {
        for dx in -60i32..60 {
            let (fx, fz) = (dx as f32 + 0.5, dz as f32 + 0.5);
            let r = (fx * fx + fz * fz).sqrt();
            let ring = (9.0..14.0).contains(&r);
            // Brackets: arcs of radius 42 across each diagonal.
            let diag = (fx.abs() - fz.abs()).abs() < 16.0 && fx != 0.0 && fz != 0.0;
            let bracket = diag && (38.0..46.0).contains(&r);
            if ring || bracket || r < 5.0 {
                let (x, z) = ((vx as i32 + dx) as usize, (vz as i32 + dz) as usize);
                let i = (z * tw + x) * 4;
                pixels[i..i + 3].copy_from_slice(&green);
            }
        }
    }
}

/// Squares a mover can reach from `from`, by the movers' slope limit.
fn reachable(parsed: &ParsedMap, from: (f32, f32)) -> Vec<bool> {
    let (w, d) = (MAP_X, MAP_Y);
    let slopes = slope_map(&parsed.heights, (w + 1) as u32, (d + 1) as u32);
    let cap = max_slope_from_degrees(MOVER_MAX_SLOPE_DEGREES);
    let mut seen = vec![false; w * d];
    let start = (from.1 as usize / 8) * w + from.0 as usize / 8;
    let mut stack = vec![start];
    seen[start] = true;
    while let Some(i) = stack.pop() {
        let (x, z) = (i % w, i / w);
        let next = [
            (x.wrapping_sub(1), z),
            (x + 1, z),
            (x, z.wrapping_sub(1)),
            (x, z + 1),
        ];
        for (nx, nz) in next {
            if nx < w && nz < d {
                let n = nz * w + nx;
                if !seen[n] && slopes[n] <= cap {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }
    }
    seen
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../kernel-panic/assets/maps/deadlock_v1.kpmap"
        )
        .into()
    });
    let layout = Layout::new();
    let (heights, lowered) = heights(&layout);
    let (lo, hi) = heights.iter().fold((f32::MAX, f32::MIN), |(a, b), &h| {
        (a.det_min(h), b.det_max(h))
    });

    // Terrain type 3 is Dual Core's bus: double speed for every class.
    let mut types = TerrainTypes::default();
    types.move_speeds[3] = TerrainMoveSpeeds {
        tank: 2.0,
        kbot: 2.0,
        hover: 2.0,
        ship: 2.0,
    };
    let header = MapHeader::new(MAP_X as i32, MAP_Y as i32, lo, hi);
    let typemap = (0..header.typemap_height())
        .flat_map(|tz| (0..header.typemap_width()).map(move |tx| (tx, tz)))
        .map(|(tx, tz)| {
            let (cx, cz) = (tx * 2 / CELL_SQ, tz * 2 / CELL_SQ);
            if layout.lanes.iter().any(|l| l.contains(cx, cz)) {
                3
            } else {
                2
            }
        })
        .collect();
    let features = layout
        .vents
        .iter()
        .map(|&(x, z)| MapFeature::new(FeatureType::GeoVent, x, 0.0, z, 0.0, 1.0))
        .collect();
    let parsed = ParsedMap {
        metalmap: vec![0; header.metalmap_width() * header.metalmap_height()],
        header,
        heights,
        features,
        typemap,
    };
    let datavents = datavent_facings(&parsed, &types);

    // Every vent and start must be reachable from the first start.
    let reach = reachable(&parsed, layout.vent_start(STARTS[0]));
    for &(x, z) in &layout.vents {
        // The vent sits on a cell corner; check the square beside it.
        let i = (z as usize / 8) * MAP_X + x as usize / 8;
        assert!(
            reach[i],
            "datavent ({x}, {z}) is cut off from the first start"
        );
    }
    for (a, b) in layout
        .vents
        .iter()
        .zip(layout.vents.iter().skip(VENTS.len()))
    {
        assert_eq!(
            (a.0 + b.0, a.1 + b.1),
            ((CW * CELL) as f32, (CD * CELL) as f32)
        );
    }

    let map = SpringMap {
        ground_texture: Some(texture(&layout, &lowered)),
        map_info: Some(MapInfo {
            description: "Deadlock v1. Map for Kernel Panic, after Raghna's Hooked. \
                          Intended for 2-4 players."
                .into(),
            gravity: 50.0,
            start_positions: STARTS
                .iter()
                .zip(0..)
                .map(|(&s, team)| {
                    let (x, z) = layout.vent_start(s);
                    StartPosition { team, x, z }
                })
                .collect(),
            // Dual Core's, Quad Core's and Data Cache L1's.
            atmosphere: Atmosphere {
                fog_color: [0.0, 0.0, 0.0],
                fog_start: 0.001,
                sky_color: [0.01, 0.01, 0.01],
                sun_color: [0.12, 0.025, 0.33],
                cloud_density: 0.0,
            },
            lighting: Lighting {
                sun_dir: [-2.0, 7.0, -4.0],
                ground_ambient: [0.6, 0.6, 0.6],
                ground_sun_color: [1.0, 1.0, 1.0],
                ground_shadow_density: 1.0,
            },
        }),
        parsed,
        terrain_types: types,
        lua_compositing: None,
        datavents,
    };
    let bytes = write_baked_map(&map).expect("encode");
    std::fs::write(&out, &bytes).expect("write");
    println!("{out}: {} bytes, heights {lo}..{hi}", bytes.len());
}
