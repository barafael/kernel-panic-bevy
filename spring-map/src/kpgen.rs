//! The port's map generator: maps drawn the way FireStorm drew Dual
//! Core, Quad Core and Data Cache L1, written straight to `.kpmap`.
//!
//! A map is a grid of 32-elmo cells (one ground-texture texel per elmo,
//! four map squares per cell), each a flat terrace of some [`Level`], a
//! [`Ramp`] between two levels, the void, or the outer ring falling off
//! the raised rim. From that the generator derives
//!
//! - the heightmap: a terrace edge more than [`CLIFF_STEP`] high drops
//!   across the upper cell, steep enough to stop every mover, while the
//!   ramps stay under the movers' slope limit;
//! - the ground texture: each level's fill with a two-texel grid line,
//!   cliff cells dark, ramps hatched, the void black, datavents on pads
//!   with L1's vent mark, bus lanes (terrain type 3, double speed, as
//!   Dual Core's) outlined, shifting blocks ([`crate::shifting`]) in
//!   hazard stripes;
//! - the typemap (void is terrain type 1, impassable), the datavent
//!   facings, and the metadata in the house style (gravity 50, the
//!   FireStorm maps' dark sky and lighting).
//!
//! [`KpMap::check`] proves every datavent can be walked to; the map
//! scripts in `examples/` add their own symmetry checks.

use spring_detmath::DetFloat;
use spring_pathfinding::{max_slope_from_degrees, slope_map};

use crate::SpringMap;
use crate::baked::write_baked_map;
pub use crate::cells::{CELL, CELL_SQ, Rect, rot90, rot180, rotate};
use crate::datavent::{MOVER_MAX_SLOPE_DEGREES, datavent_facings};
use crate::map_info::{Atmosphere, Lighting, MapInfo, StartPosition};
use crate::map_types::{
    FeatureType, GroundTexture, MapFeature, MapHeader, ParsedMap, TerrainMoveSpeeds, TerrainTypes,
};

/// A height difference over one cell larger than this is a cliff; a
/// ramp rises at most about 20 per cell.
pub const CLIFF_STEP: f32 = 60.0;

/// Typemap values: the void (no class can enter), plain ground, the bus.
pub const TYPE_VOID: u8 = 1;
pub const TYPE_GROUND: u8 = 2;
pub const TYPE_BUS: u8 = 3;

/// Height of the void floor.
pub const VOID_HEIGHT: f32 = 0.0;

/// A terrace: its height and its grid colours.
#[derive(Clone, Copy, Debug)]
pub struct Level {
    pub height: f32,
    pub fill: [u8; 3],
    pub line: [u8; 3],
}

/// Which side of a ramp is its low end.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Side {
    West,
    East,
    North,
    South,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::West => Side::East,
            Side::East => Side::West,
            Side::North => Side::South,
            Side::South => Side::North,
        }
    }

    /// The side a quarter turn clockwise (seen from above, +z down the
    /// map: north → east → south → west).
    pub fn clockwise(self) -> Side {
        match self {
            Side::North => Side::East,
            Side::East => Side::South,
            Side::South => Side::West,
            Side::West => Side::North,
        }
    }
}

/// A straight ramp between two levels.
#[derive(Clone, Copy, Debug)]
pub struct Ramp {
    pub rect: Rect,
    pub low_side: Side,
    /// Level indices.
    pub low: usize,
    pub high: usize,
}

impl Ramp {
    pub fn rot180(self, cw: usize, cd: usize) -> Self {
        Self {
            rect: self.rect.rot180(cw, cd),
            low_side: self.low_side.opposite(),
            ..self
        }
    }

    pub fn rot90(self, n: usize) -> Self {
        Self {
            rect: self.rect.rot90(n),
            low_side: self.low_side.clockwise(),
            ..self
        }
    }

    pub fn mirror_x(self, cw: usize) -> Self {
        let low_side = match self.low_side {
            Side::West | Side::East => self.low_side.opposite(),
            s => s,
        };
        Self {
            rect: self.rect.mirror_x(cw),
            low_side,
            ..self
        }
    }

    pub fn mirror_z(self, cd: usize) -> Self {
        let low_side = match self.low_side {
            Side::North | Side::South => self.low_side.opposite(),
            s => s,
        };
        Self {
            rect: self.rect.mirror_z(cd),
            low_side,
            ..self
        }
    }
}

/// A ramp whose height goes with the distance from a centre: `high` at
/// `r_high` elmos out, `low` at `r_low` (either may be the inner one),
/// clamped beyond. It keeps a rotated layout exact where a straight
/// ramp can only follow the grid; the cells it covers are the caller's
/// choice ([`KpMap::fill_where`] with [`Cell::Radial`]).
#[derive(Clone, Copy, Debug)]
pub struct RadialRamp {
    pub centre: (f32, f32),
    pub r_high: f32,
    pub r_low: f32,
    /// Level indices.
    pub low: usize,
    pub high: usize,
}

/// A directional lane cell: unit direction of travel and chevron colour.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Arrow {
    pub dir: (f32, f32),
    pub colour: [u8; 3],
}

/// How shifting blocks are drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BlockStyle {
    /// Red and black hazard stripes: a few blocks that matter.
    Hazard,
    /// The ground's own colours in faint stripes: ground that mostly
    /// shifts.
    Faint,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cell {
    /// The outer ring, falling off the rim to the map edge.
    Out,
    /// Impassable void (black, terrain type [`TYPE_VOID`]).
    Void,
    /// A terrace of the level with this index.
    Floor(usize),
    /// The ramp with this index.
    Ramp(usize),
    /// The radial ramp with this index.
    Radial(usize),
}

/// A map being drawn.
pub struct KpMap {
    /// Cells across and down.
    pub cw: usize,
    pub cd: usize,
    pub levels: Vec<Level>,
    /// The rim's level index (the two rings inside the outer one).
    pub rim: usize,
    pub cells: Vec<Cell>,
    pub ramps: Vec<Ramp>,
    pub radials: Vec<RadialRamp>,
    /// Bus lanes: terrain type 3, double speed.
    pub lanes: Vec<Rect>,
    /// Directional lanes, per cell (empty: none): the direction of
    /// travel, drawn as a chevron in the given colour. Movement on them
    /// is the game's business (`crate::ring`); the typemap stays ground.
    pub arrows: Vec<Option<Arrow>>,
    /// Shifting blocks ([`crate::shifting`]), drawn in `block_style`.
    pub blocks: Vec<Rect>,
    pub block_style: BlockStyle,
    /// Datavents, world elmos.
    pub vents: Vec<(f32, f32)>,
    /// Start positions in team order, world elmos.
    pub starts: Vec<(f32, f32)>,
    pub description: String,
}

impl KpMap {
    /// A `cw × cd` map of `levels[base]` ground inside a rim of
    /// `levels[rim]`.
    pub fn new(cw: usize, cd: usize, levels: Vec<Level>, base: usize, rim: usize) -> Self {
        let mut map = Self {
            cw,
            cd,
            levels,
            rim,
            cells: vec![Cell::Floor(base); cw * cd],
            ramps: Vec::new(),
            radials: Vec::new(),
            lanes: Vec::new(),
            arrows: Vec::new(),
            blocks: Vec::new(),
            block_style: BlockStyle::Hazard,
            vents: Vec::new(),
            starts: Vec::new(),
            description: String::new(),
        };
        map.reset_rim();
        map
    }

    /// Redraw the outer ring and the rim (after fills that reached them).
    pub fn reset_rim(&mut self) {
        for z in 0..self.cd {
            for x in 0..self.cw {
                let ring = x.min(z).min(self.cw - 1 - x).min(self.cd - 1 - z);
                if ring == 0 {
                    self.cells[z * self.cw + x] = Cell::Out;
                } else if ring <= 2 {
                    self.cells[z * self.cw + x] = Cell::Floor(self.rim);
                }
            }
        }
    }

    /// World size in elmos.
    pub fn world(&self) -> (f32, f32) {
        ((self.cw * CELL) as f32, (self.cd * CELL) as f32)
    }

    pub fn cell(&self, x: usize, z: usize) -> Cell {
        self.cells[z * self.cw + x]
    }

    pub fn fill(&mut self, r: Rect, c: Cell) {
        for z in r.z0..r.z1.min(self.cd) {
            for x in r.x0..r.x1.min(self.cw) {
                self.cells[z * self.cw + x] = c;
            }
        }
    }

    /// Every cell whose centre lies within `radius` elmos of `centre`.
    pub fn fill_disc(&mut self, centre: (f32, f32), radius: f32, c: Cell) {
        self.fill_where(c, |x, z| {
            let (dx, dz) = (x - centre.0, z - centre.1);
            dx * dx + dz * dz <= radius * radius
        });
    }

    /// Every cell whose centre (world elmos) passes `inside`.
    pub fn fill_where(&mut self, c: Cell, inside: impl Fn(f32, f32) -> bool) {
        for z in 0..self.cd {
            for x in 0..self.cw {
                let centre = ((x * CELL + CELL / 2) as f32, (z * CELL + CELL / 2) as f32);
                if inside(centre.0, centre.1) {
                    self.cells[z * self.cw + x] = c;
                }
            }
        }
    }

    /// Add a radial ramp; fill its cells with the returned
    /// [`Cell::Radial`].
    pub fn add_radial(&mut self, ramp: RadialRamp) -> Cell {
        self.radials.push(ramp);
        Cell::Radial(self.radials.len() - 1)
    }

    pub fn add_ramp(&mut self, ramp: Ramp) {
        self.fill(ramp.rect, Cell::Ramp(self.ramps.len()));
        self.ramps.push(ramp);
    }

    /// The height a cell gives world point `(x, z)`, if it is not the
    /// outer ring.
    fn cell_height(&self, cell: Cell, x: f32, z: f32) -> Option<f32> {
        match cell {
            Cell::Out => None,
            Cell::Void => Some(VOID_HEIGHT),
            Cell::Floor(level) => Some(self.levels[level].height),
            Cell::Ramp(i) => Some(self.ramp_height(&self.ramps[i], x, z)),
            Cell::Radial(i) => {
                let r = &self.radials[i];
                let (dx, dz) = (x - r.centre.0, z - r.centre.1);
                let d = (dx * dx + dz * dz).sqrt();
                let t = ((r.r_low - d) / (r.r_low - r.r_high)).det_clamp(0.0, 1.0);
                let (low, high) = (self.levels[r.low].height, self.levels[r.high].height);
                Some(low + (high - low) * t)
            }
        }
    }

    /// A ramp cell's low and high level indices.
    fn ramp_levels(&self, cell: Cell) -> Option<(usize, usize)> {
        match cell {
            Cell::Ramp(i) => Some((self.ramps[i].low, self.ramps[i].high)),
            Cell::Radial(i) => Some((self.radials[i].low, self.radials[i].high)),
            _ => None,
        }
    }

    fn ramp_height(&self, ramp: &Ramp, x: f32, z: f32) -> f32 {
        let r = ramp.rect;
        let (x0, x1) = ((r.x0 * CELL) as f32, (r.x1 * CELL) as f32);
        let (z0, z1) = ((r.z0 * CELL) as f32, (r.z1 * CELL) as f32);
        let t = match ramp.low_side {
            Side::West => (x - x0) / (x1 - x0),
            Side::East => (x1 - x) / (x1 - x0),
            Side::North => (z - z0) / (z1 - z0),
            Side::South => (z1 - z) / (z1 - z0),
        }
        .det_clamp(0.0, 1.0);
        let (low, high) = (self.levels[ramp.low].height, self.levels[ramp.high].height);
        low + (high - low) * t
    }

    /// The level whose colours a cell is drawn in (the void and the
    /// outer ring have their own).
    fn level(&self, cell: Cell) -> Level {
        match cell {
            Cell::Out | Cell::Void => self.levels[self.rim],
            Cell::Floor(level) => self.levels[level],
            Cell::Ramp(_) | Cell::Radial(_) => {
                self.levels[self.ramp_levels(cell).expect("a ramp").1]
            }
        }
    }

    fn map_squares(&self) -> (usize, usize) {
        (self.cw * CELL_SQ, self.cd * CELL_SQ)
    }

    /// The heightmap, and per vertex how far the cliffs lowered it.
    pub fn heights(&self) -> (Vec<f32>, Vec<f32>) {
        let (mx, my) = self.map_squares();
        let (w, d) = (mx + 1, my + 1);
        let rim = self.levels[self.rim].height;
        // Every vertex takes the lowest of the cells it touches, so a
        // cell edge sits at the lower level and a cliff drops inside the
        // upper cell.
        let mut base = vec![0.0f32; w * d];
        for sz in 0..d {
            for sx in 0..w {
                let (x, z) = ((sx * 8) as f32, (sz * 8) as f32);
                let edge = sx.min(sz).min(mx - sx).min(my - sz);
                base[sz * w + sx] = if edge < CELL_SQ {
                    // The outer ring: down from the rim to the map edge.
                    rim * (0.5 + 0.5 * edge as f32 / CELL_SQ as f32)
                } else {
                    let (cx, cz) = (sx / CELL_SQ, sz / CELL_SQ);
                    [(0, 0), (1, 0), (0, 1), (1, 1)]
                        .into_iter()
                        .filter_map(|(dx, dz)| Some((cx.checked_sub(dx)?, cz.checked_sub(dz)?)))
                        .filter(|&(x, z)| x < self.cw && z < self.cd)
                        // Only the cells whose closed rectangle holds the vertex.
                        .filter(|&(x, z)| sx <= (x + 1) * CELL_SQ && sz <= (z + 1) * CELL_SQ)
                        .filter_map(|(cx, cz)| self.cell_height(self.cell(cx, cz), x, z))
                        .fold(f32::MAX, |a, b| a.det_min(b))
                };
            }
        }
        // Cliffs: a vertex within one cell of ground more than a cliff
        // step lower slopes down to it across that cell (Chebyshev
        // distance, so the bands stay square like the grid).
        let mut heights = base.clone();
        let r = CELL_SQ as isize;
        for sz in 0..d {
            for sx in 0..w {
                let edge = sx.min(sz).min(mx - sx).min(my - sz);
                if edge < CELL_SQ {
                    continue;
                }
                let here = base[sz * w + sx];
                let mut h = here;
                for dz in -r..=r {
                    for dx in -r..=r {
                        let (ux, uz) = (sx as isize + dx, sz as isize + dz);
                        if ux < r || uz < r || ux > (mx as isize - r) || uz > (my as isize - r) {
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

    /// The ground texture, one texel per elmo.
    pub fn texture(&self, lowered: &[f32]) -> GroundTexture {
        let (tw, th) = (self.cw * CELL, self.cd * CELL);
        let mut pixels = vec![0u8; tw * th * 4];
        let w = self.map_squares().0 + 1;
        let lane_line = [255, 255, 60];
        // Is texel `(x, z)` within `reach` texels of a vent, per axis?
        let pad = |x: usize, z: usize, reach: f32| {
            let (fx, fz) = (x as f32 + 0.5, z as f32 + 0.5);
            self.vents
                .iter()
                .any(|&(vx, vz)| (fx - vx).abs() < reach && (fz - vz).abs() < reach)
        };
        for cz in 0..self.cd {
            for cx in 0..self.cw {
                let cell = self.cell(cx, cz);
                let level = self.level(cell);
                let (fill, line) = (level.fill, level.line);
                // A cell a cliff runs through is drawn dark.
                let cliff = (0..=CELL_SQ).any(|dz| {
                    (0..=CELL_SQ)
                        .any(|dx| lowered[(cz * CELL_SQ + dz) * w + cx * CELL_SQ + dx] > 1.0)
                });
                let lane = self.lanes.iter().any(|l| l.contains(cx, cz));
                let arrow = self.arrows.get(cz * self.cw + cx).copied().flatten();
                let block = self.blocks.iter().any(|b| b.contains(cx, cz));
                let (x0, z0) = (cx * CELL, cz * CELL);
                for lz in 0..CELL {
                    for lx in 0..CELL {
                        let (x, z) = (x0 + lx, z0 + lz);
                        let grid = lx == 0 || lz == 0 || lx == CELL - 1 || lz == CELL - 1;
                        let rgb = match cell {
                            Cell::Out | Cell::Void => [0, 0, 0],
                            _ if cliff => {
                                if grid {
                                    line
                                } else {
                                    scale(fill, 0.3)
                                }
                            }
                            Cell::Ramp(_) | Cell::Radial(_) => {
                                let (low, _) = self.ramp_levels(cell).expect("a ramp");
                                let low_fill = self.levels[low].fill;
                                let hatch = (x + z) % CELL < 3 || (x + CELL - z % CELL) % CELL < 3;
                                let low_is_lowest = self
                                    .levels
                                    .iter()
                                    .all(|l| l.height >= self.levels[low].height);
                                if lane && grid {
                                    lane_line
                                } else if hatch {
                                    if low_is_lowest { line } else { [255, 170, 0] }
                                } else {
                                    scale(low_fill, 0.7)
                                }
                            }
                            _ if block && self.block_style == BlockStyle::Hazard => {
                                // Hazard stripes, 8 texels wide.
                                if grid {
                                    [255, 60, 40]
                                } else if (x + z) / 8 % 2 == 0 {
                                    [200, 30, 20]
                                } else {
                                    [24, 8, 8]
                                }
                            }
                            _ if pad(x, z, 2.0 * CELL as f32) => {
                                if grid {
                                    [255, 255, 200]
                                } else {
                                    [255, 236, 170]
                                }
                            }
                            _ if pad(x, z, 3.0 * CELL as f32) => {
                                if grid {
                                    line
                                } else {
                                    scale(fill, 1.6)
                                }
                            }
                            _ if arrow.is_some() => {
                                let a = arrow.expect("an arrow");
                                // A chevron pointing along `dir`, centred in the cell.
                                let (u, v) = (lx as f32 - 15.5, lz as f32 - 15.5);
                                let along = u * a.dir.0 + v * a.dir.1;
                                let across = (v * a.dir.0 - u * a.dir.1).abs();
                                let chevron =
                                    across < 11.0 && (along + across * 0.9 - 4.0).abs() < 2.5;
                                if grid {
                                    scale(a.colour, 0.6)
                                } else if chevron {
                                    a.colour
                                } else {
                                    scale(fill, 0.75)
                                }
                            }
                            _ if lane => {
                                if grid {
                                    lane_line
                                } else {
                                    scale(fill, 0.8)
                                }
                            }
                            _ if block => {
                                if grid {
                                    line
                                } else if (x + z) / 8 % 2 == 0 {
                                    scale(fill, 0.8)
                                } else {
                                    fill
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
        for &(vx, vz) in &self.vents {
            draw_vent_icon(&mut pixels, tw, vx, vz);
        }
        GroundTexture {
            width: tw,
            height: th,
            pixels,
        }
    }

    /// The terrain types: Dual Core's bus (type 3) doubles every class's
    /// speed, the void (type 1) stops them all.
    pub fn terrain_types() -> TerrainTypes {
        let mut types = TerrainTypes::default();
        types.move_speeds[TYPE_BUS as usize] = TerrainMoveSpeeds {
            tank: 2.0,
            kbot: 2.0,
            hover: 2.0,
            ship: 2.0,
        };
        types.move_speeds[TYPE_VOID as usize] = TerrainMoveSpeeds {
            tank: 0.0,
            kbot: 0.0,
            hover: 0.0,
            ship: 0.0,
        };
        types
    }

    fn typemap(&self, header: &MapHeader) -> Vec<u8> {
        (0..header.typemap_height())
            .flat_map(|tz| (0..header.typemap_width()).map(move |tx| (tx, tz)))
            .map(|(tx, tz)| {
                let (cx, cz) = (tx * 2 / CELL_SQ, tz * 2 / CELL_SQ);
                if self.cell(cx, cz) == Cell::Void {
                    TYPE_VOID
                } else if self.lanes.iter().any(|l| l.contains(cx, cz)) {
                    TYPE_BUS
                } else {
                    TYPE_GROUND
                }
            })
            .collect()
    }

    /// Build the map.
    pub fn build(&self) -> SpringMap {
        let (heights, lowered) = self.heights();
        let (lo, hi) = heights.iter().fold((f32::MAX, f32::MIN), |(a, b), &h| {
            (a.det_min(h), b.det_max(h))
        });
        let (mx, my) = self.map_squares();
        let header = MapHeader::new(mx as i32, my as i32, lo, hi);
        let types = Self::terrain_types();
        let features = self
            .vents
            .iter()
            .map(|&(x, z)| MapFeature::new(FeatureType::GeoVent, x, 0.0, z, 0.0, 1.0))
            .collect();
        let parsed = ParsedMap {
            metalmap: vec![0; header.metalmap_width() * header.metalmap_height()],
            typemap: self.typemap(&header),
            header,
            heights,
            features,
        };
        let datavents = datavent_facings(&parsed, &types);
        SpringMap {
            ground_texture: Some(self.texture(&lowered)),
            map_info: Some(MapInfo {
                description: self.description.clone(),
                gravity: 50.0,
                start_positions: self
                    .starts
                    .iter()
                    .zip(0..)
                    .map(|(&(x, z), team)| StartPosition { team, x, z })
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
        }
    }

    /// Panic unless every datavent and start can be walked to from the
    /// first start on `heights` (the built map's, or a shifted state of
    /// them).
    pub fn check_reachable(&self, map: &SpringMap, heights: &[f32], what: &str) {
        let first = self.starts.first().copied().expect("a start position");
        let reach = reachable(&map.parsed, heights, first);
        let (mx, _) = self.map_squares();
        for &(x, z) in self.vents.iter().chain(&self.starts) {
            // A vent on a cell corner: check the four squares round it.
            let (sx, sz) = ((x / 8.0) as usize, (z / 8.0) as usize);
            let ok = [(0, 0), (1, 0), (0, 1), (1, 1)].iter().any(|&(dx, dz)| {
                let (qx, qz) = (sx.saturating_sub(dx), sz.saturating_sub(dz));
                reach[qz * mx + qx]
            });
            assert!(ok, "{what}: ({x}, {z}) is cut off from the first start");
        }
    }

    /// Build, check and write the map to `path`; returns the built map.
    pub fn write(&self, path: &str) -> SpringMap {
        let map = self.build();
        self.check_reachable(&map, &map.parsed.heights, "built map");
        let bytes = write_baked_map(&map).expect("encode");
        std::fs::write(path, &bytes).expect("write");
        let (lo, hi) = (map.parsed.header.min_height, map.parsed.header.max_height);
        println!("{path}: {} bytes, heights {lo}..{hi}", bytes.len());
        map
    }
}

fn scale([r, g, b]: [u8; 3], k: f32) -> [u8; 3] {
    [r, g, b].map(|c| (c as f32 * k).det_min(255.0) as u8)
}

/// L1's vent mark: a green ring with four brackets, on the pale pad.
fn draw_vent_icon(pixels: &mut [u8], tw: usize, vx: f32, vz: f32) {
    let green = [110u8, 220, 80];
    let (cx, cz) = (vx.round() as i32, vz.round() as i32);
    for dz in -60i32..60 {
        for dx in -60i32..60 {
            let (fx, fz) = (dx as f32 + 0.5, dz as f32 + 0.5);
            let r = (fx * fx + fz * fz).sqrt();
            let ring = (9.0..14.0).contains(&r);
            // Brackets: arcs of radius 42 across each diagonal.
            let diag = (fx.abs() - fz.abs()).abs() < 16.0 && fx != 0.0 && fz != 0.0;
            let bracket = diag && (38.0..46.0).contains(&r);
            if ring || bracket || r < 5.0 {
                let (x, z) = ((cx + dx) as usize, (cz + dz) as usize);
                let i = (z * tw + x) * 4;
                pixels[i..i + 3].copy_from_slice(&green);
            }
        }
    }
}

/// Squares a mover can reach from `from` (world elmos) on `heights`, by
/// the movers' slope limit and the void.
pub fn reachable(parsed: &ParsedMap, heights: &[f32], from: (f32, f32)) -> Vec<bool> {
    let (w, d) = (parsed.header.map_x as usize, parsed.header.map_y as usize);
    let slopes = slope_map(heights, (w + 1) as u32, (d + 1) as u32);
    let cap = max_slope_from_degrees(MOVER_MAX_SLOPE_DEGREES);
    let tw = parsed.header.typemap_width();
    let open = |i: usize| {
        let (x, z) = (i % w, i / w);
        slopes[i] <= cap && parsed.typemap.get((z / 2) * tw + x / 2) != Some(&TYPE_VOID)
    };
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
                if !seen[n] && open(n) {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }
    }
    seen
}
