//! Hex Farm 8's synced gadget (`LuaRules/Gadgets/HexFarm8.lua`, zwzsg),
//! ported to Rust.
//!
//! The map's SMF is a placeholder: at `Initialize()` the gadget rolls a
//! random layout (`UseMapOptions` + `DesignLayout`), puts the teams on
//! towers (`SetStartPos`), writes the heightmap and terrain types
//! (`SetWholeHeightMap`, `SetWholeTerrainAndMetalMap`), indexes the
//! polygons (`SetWholePolyMap`, `SetWholePolyHealth`) and places the
//! datavents (`RedoDatavents`). A new layout every game — so this runs
//! at match start from a seed instead of replaying one captured run.
//!
//! Everything here is plain data + arithmetic, deterministic per seed.
//! Lua's `math.random` is replaced by [`Rng`] (same call sequence and
//! ranges, different generator — a seed doesn't reproduce a Spring
//! game, it reproduces a port game). Metal (`SetMetalAmount`) is not
//! ported: Kernel Panic has no metal economy, and the port has nothing
//! that reads a metal map (upstream's only reader, `MetalToGeo.lua`, is
//! not ported). The aircraft smooth mesh is
//! ([`HexFarm::set_whole_smooth_mesh`]).
//!
//! Indices are 0-based where the gadget's are 1-based; each function's
//! doc cites the gadget function (and line) it ports.

use std::f64::consts::PI;

use crate::lua_layout::{HexBridge, HexFarmLayout, HexTower};

/// Heightmap / terrain granularity (`SQUARE_SIZE`).
const SQUARE: f64 = crate::map_types::SQUARE_SIZE as f64;
/// Spring's terrain-type map is one entry per 2×2 squares.
const TYPE_SQUARE: f64 = 16.0;

/// `math.random` stand-in: splitmix64. Only the *shape* of each draw
/// matters for faithfulness (uniform float in `[0,1)`, uniform integer
/// in `[m,n]`), not the exact stream.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `math.random()`: uniform in `[0, 1)`.
    pub fn random(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// `math.random(m, n)`: uniform integer in `[m, n]`.
    pub fn range(&mut self, m: i64, n: i64) -> i64 {
        m + (self.random() * (n - m + 1) as f64).floor() as i64
    }
}

/// `BoundaryShape` (gadget l.155).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    Circle,
    Hexagon,
    Star,
    Rectangle,
    Losange,
    Triangle,
}

/// `CenteredOn`: what sits on the map centre (gadget l.158).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenteredOn {
    Hole,
    Tower,
    Bridge,
}

/// Axis-aligned bounds plus the gadget's pre-snapped loop starts
/// (`FillPolygonBoundingRect`, l.264).
#[derive(Debug, Clone, Copy, Default)]
pub struct Bounds {
    pub xmin: f64,
    pub xmax: f64,
    pub zmin: f64,
    pub zmax: f64,
    /// First heightmap vertex column/row to scan.
    pub xmin8: f64,
    pub zmin8: f64,
    /// First terrain-square centre to scan.
    pub xmin16: f64,
    pub zmin16: f64,
}

impl Bounds {
    fn of(c: &[[f64; 2]]) -> Self {
        let mut b = Bounds {
            xmin: f64::INFINITY,
            xmax: f64::NEG_INFINITY,
            zmin: f64::INFINITY,
            zmax: f64::NEG_INFINITY,
            ..Default::default()
        };
        for p in c {
            b.xmin = b.xmin.min(p[0]);
            b.xmax = b.xmax.max(p[0]);
            b.zmin = b.zmin.min(p[1]);
            b.zmax = b.zmax.max(p[1]);
        }
        b.xmin8 = 8.0 * (b.xmin / 8.0).floor();
        b.zmin8 = 8.0 * (b.zmin / 8.0).floor();
        b.xmin16 = 8.0 + 16.0 * ((b.xmin - 8.0).max(0.0) / 16.0).floor();
        b.zmin16 = 8.0 + 16.0 * ((b.zmin - 8.0).max(0.0) / 16.0).floor();
        b
    }
}

/// `IsInsidePolygon` (l.282): even-odd ray cast to +x, with the
/// bounding-box early out.
pub fn is_inside_polygon(c: &[[f64; 2]], b: &Bounds, x: f64, z: f64) -> bool {
    if x < b.xmin || x > b.xmax || z < b.zmin || z > b.zmax {
        return false;
    }
    let mut inside = false;
    let mut j = c.len() - 1;
    for i in 0..c.len() {
        let (ci, cj) = (c[i], c[j]);
        if z >= ci[1].min(cj[1])
            && z < ci[1].max(cj[1])
            && x < (cj[0] - ci[0]) * (z - ci[1]) / (cj[1] - ci[1]) + ci[0]
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// One tower (`hex[k]`).
#[derive(Debug, Clone)]
pub struct Hex {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Carries a datavent.
    pub g: bool,
    /// The six top corners (x, z); all at height `y`.
    pub c: [[f64; 2]; 6],
    pub bounds: Bounds,
    /// Lattice coordinates.
    pub i: i64,
    pub j: i64,
    /// Neighbouring towers / connecting bridges (the gadget only builds
    /// these when `Dynamic`, which it always is for KP).
    pub tower_links: Vec<usize>,
    pub bridge_links: Vec<usize>,
    /// `cb`: which bridge leaves through each side (for `BridgeSupport`
    /// skins).
    pub cb: [Option<usize>; 6],
    /// Sunk into the void (not walkable, not drawn).
    pub hidden: bool,
    /// Rising/sinking: 0..1 progress and its per-sweep step (`dy`).
    pub progress: Option<f64>,
    pub dy: f64,
}

/// One bridge (`rect[k]`) between towers `hex1` and `hex2`.
#[derive(Debug, Clone)]
pub struct Rect {
    pub hex1: usize,
    pub hex2: usize,
    /// Four corners (x, z): c1/c4 on `hex1`'s edge, c2/c3 on `hex2`'s.
    pub c: [[f64; 2]; 4],
    pub bounds: Bounds,
    pub hidden: bool,
    pub progress: Option<f64>,
    pub dy: f64,
}

/// A polygon key as the gadget's `PolyMap`/`PolyHealth` use it:
/// towers positive, bridges negative, 0 = the void. Here 1-based like
/// the gadget so the sign trick works: `Hex(k)` ↔ `k+1`, `Rect(r)` ↔
/// `-(r+1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Poly {
    Hex(usize),
    Rect(usize),
}

impl Poly {
    fn key(self) -> i32 {
        match self {
            Poly::Hex(k) => k as i32 + 1,
            Poly::Rect(r) => -(r as i32 + 1),
        }
    }

    fn from_key(key: i32) -> Option<Self> {
        match key {
            0 => None,
            k if k > 0 => Some(Poly::Hex(k as usize - 1)),
            k => Some(Poly::Rect((-k) as usize - 1)),
        }
    }
}

/// Terrain types from the map's `mapinfo.lua`: 0 "void" (speed 0 for
/// every move class — the gadget's fall sweep keys on its name), 1
/// "Bridge", 2 "Tower".
pub const TERRAIN_VOID: u8 = 0;
pub const TERRAIN_BRIDGE: u8 = 1;
pub const TERRAIN_TOWER: u8 = 2;

/// Match inputs the gadget reads from the engine.
#[derive(Debug, Clone, Copy)]
pub struct HexFarmSetup {
    /// `Game.mapSizeX/Z` in elmos.
    pub map_size_x: f64,
    pub map_size_z: f64,
    /// Playing teams (`#Spring.GetTeamList()` minus Gaia).
    pub teams: usize,
    /// Median `UnitDefs[].health` / `.buildTime` as the gadget computes
    /// them (see [`lua_median`]).
    pub median_health: f64,
    pub median_build_time: f64,
}

/// `hp={1}; for ... hp[#hp+1]=v; table.sort(hp); hp[floor(1+#hp/2)]`
/// (l.214): the gadget seeds its list with a 1 before taking the
/// median.
pub fn lua_median(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = std::iter::once(1.0).chain(values).collect();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// The whole synced state of the gadget.
#[derive(Debug, Clone)]
pub struct HexFarm {
    // --- UseMapOptions ---
    pub map_dim_scale: f64,
    pub boundary: Boundary,
    pub centered_on: CenteredOn,
    /// 1 = sparse (every third lattice diagonal cut), 2 = full.
    pub density: u8,
    pub tower_radius: f64,
    pub inter_length: f64,
    pub rect_width: f64,
    pub center_height: f64,
    pub edge_height: f64,
    pub pit_depth: f64,
    pub lattice_angle: f64,
    pub team_colored: bool,
    pub skin: i64,
    /// Index into the gadget's `AllGeoSelectors` (1 = every tower, 7 =
    /// none).
    pub geo_density: usize,
    /// `Exploration` (seconds-ish; 15 by default).
    pub exploration: f64,
    /// Rise times in frames.
    pub tower_pop_time: f64,
    pub bridge_pop_time: f64,
    /// Explosion damage a polygon soaks before sinking.
    pub poly_hitpoint: f64,
    /// Build power needed to raise a sunk polygon.
    pub poly_buildpoint: f64,
    /// Show the % indicators.
    pub indication: bool,
    pub map_center_x: f64,
    pub map_center_z: f64,
    pub map_size_x: f64,
    pub map_size_z: f64,
    // --- DesignLayout ---
    pub hexes: Vec<Hex>,
    pub rects: Vec<Rect>,
    // --- SetStartPos ---
    /// Start position of each playing team, in team order.
    pub start_positions: Vec<[f64; 2]>,
    // --- PolyMap / terrain types (one entry per 16×16 elmo square) ---
    pub type_w: usize,
    pub type_h: usize,
    pub terrain: Vec<u8>,
    poly_map: Vec<i32>,
    /// `PolyHealth`, indexed by [`Poly::key`] through [`Self::health`].
    hex_health: Vec<f64>,
    rect_health: Vec<f64>,
    /// `PolyPercent`: last % indicator sent per polygon.
    percent: std::collections::HashMap<Poly, i32>,
    /// What the synced side told the rest of the game since the last
    /// [`Self::drain_events`] (the gadget's `SendToUnsynced` plus the
    /// engine-side effects of `SetOneHex` / `SetOneRect`).
    events: Vec<HexFarmEvent>,
    pub rng: Rng,
}

/// Side effects of the dynamic mode, for the renderer and the engine
/// glue to apply.
#[derive(Debug, Clone, PartialEq)]
pub enum HexFarmEvent {
    /// `SendToUnsynced("ReceiveHexFarmLayout",'moving',...)`: animate a
    /// polygon rising (`direction` 1) or sinking (-1) between the two
    /// sim frames. `corner` picks which end of a bridge swings.
    Moving {
        poly: Poly,
        first_frame: f64,
        last_frame: f64,
        direction: i8,
        corner: u8,
    },
    /// `SetOneHex` / `SetOneRect` ran: the polygon's heightmap (see
    /// [`HexFarm::write_poly_heights`]) and terrain type changed.
    Reshaped(Poly),
    /// A datavent appeared at the centre of this (just risen) tower.
    VentAdded(usize),
    /// Datavents inside this (just sunk) tower are gone.
    VentsRemoved(usize),
    /// `SendToUnsynced(...,"%",k,percent)`: the % indicator of a polygon
    /// being destroyed (health left) or rebuilt (build progress);
    /// `None` clears it.
    Percent(Poly, Option<i32>),
}

impl HexFarm {
    /// `gadget:Initialize()` (l.1400) for a Kernel Panic game with no
    /// map options set (the default `hexfarm_force_random=1`): roll the
    /// options, design the layout, place the teams, index everything.
    pub fn generate(seed: u64, setup: HexFarmSetup) -> Self {
        let mut farm = Self::use_map_options(Rng::new(seed), setup);
        farm.design_layout(setup.teams + 1);
        farm.set_start_pos(setup.teams);
        farm.set_whole_terrain();
        farm.set_whole_poly_map();
        farm.set_whole_poly_health();
        farm
    }

    /// `UseMapOptions` (l.141), random branch, KP-detected mod
    /// (`UnitDefNames["kernel"]`): area 25-50 %, skin 9 "Digital".
    fn use_map_options(mut rng: Rng, setup: HexFarmSetup) -> Self {
        // "Discard the first random numbers which may not be that random"
        for _ in 0..5 {
            rng.random();
        }
        let map_dim_scale = (rng.range(25, 50) as f64 / 100.0).sqrt();
        let boundary = [
            Boundary::Circle,
            Boundary::Hexagon,
            Boundary::Star,
            Boundary::Rectangle,
            Boundary::Losange,
            Boundary::Triangle,
        ][rng.range(1, 6) as usize - 1];
        let centered_on = if rng.random() > 0.5 {
            CenteredOn::Hole
        } else if rng.random() > 0.6 {
            CenteredOn::Tower
        } else {
            CenteredOn::Bridge
        };
        let density = if rng.random() > 0.75 { 1 } else { 2 };
        let tower_radius = (192.5 + 576.0 * rng.random()).floor();
        let inter_length = 2.0 * tower_radius + (4.5 + 380.0 * rng.random()).floor();
        let rect_width =
            (64.5 + (tower_radius - 64.0).max(0.0) * rng.random().powf(0.4)).floor();
        let mut bridge_angle = rng.range(-45, 60) as f64 * PI / 180.0;
        if (bridge_angle * 180.0 / PI).abs() < 1.9 {
            bridge_angle = 0.0;
        }

        let (cx, cz) = (setup.map_size_x / 2.0, setup.map_size_z / 2.0);
        let half_x = 16.0 * (map_dim_scale * setup.map_size_x / 32.0).ceil();
        let half_z = 16.0 * (map_dim_scale * setup.map_size_z / 32.0).ceil();
        let height_diff = half_x.max(half_z)
            * (1.0 - 2.0 * tower_radius / inter_length)
            * bridge_angle.tan();
        let pit_depth = 128.0 + tower_radius / cx.max(cz) * height_diff.abs();
        let (center_height, edge_height) = if height_diff > 0.0 {
            let center = pit_depth + 512.0;
            (center, center + height_diff)
        } else {
            let edge = pit_depth + 256.0;
            (edge - height_diff, edge)
        };
        let lattice_angle = rng.range(0, 360) as f64 * PI / 180.0;
        let team_colored = rng.range(1, 5) == 2;

        // Dynamic defaults (`TrueGet(...) or ...`, l.199-247).
        let exploration = 15.0;
        Self {
            map_dim_scale,
            boundary,
            centered_on,
            density,
            tower_radius,
            inter_length,
            rect_width,
            center_height,
            edge_height,
            pit_depth,
            lattice_angle,
            team_colored,
            skin: 9,
            geo_density: 0,
            exploration,
            tower_pop_time: (exploration * 20.0 + 0.5).floor(),
            bridge_pop_time: (exploration * 10.0 + 0.5).floor(),
            poly_hitpoint: 1000.0 * setup.median_health,
            poly_buildpoint: 1000.0 * setup.median_build_time,
            indication: true,
            map_center_x: cx,
            map_center_z: cz,
            map_size_x: setup.map_size_x,
            map_size_z: setup.map_size_z,
            hexes: Vec::new(),
            rects: Vec::new(),
            start_positions: Vec::new(),
            type_w: (setup.map_size_x / TYPE_SQUARE) as usize,
            type_h: (setup.map_size_z / TYPE_SQUARE) as usize,
            terrain: Vec::new(),
            poly_map: Vec::new(),
            hex_health: Vec::new(),
            rect_health: Vec::new(),
            percent: Default::default(),
            events: Vec::new(),
            rng,
        }
    }

    fn map_min_max(&self) -> (f64, f64, f64, f64) {
        let half_x = 16.0 * (self.map_dim_scale * self.map_size_x / 32.0).ceil();
        let half_z = 16.0 * (self.map_dim_scale * self.map_size_z / 32.0).ceil();
        (
            self.map_center_x - half_x,
            self.map_center_x + half_x,
            self.map_center_z - half_z,
            self.map_center_z + half_z,
        )
    }

    /// `GetHexCornersList` (l.310): `for a=LatticeAngle,LatticeAngle+6,
    /// math.pi/3` — six corners, the loop variable accumulated.
    fn hex_corners(&self, x: f64, z: f64, r: f64) -> [[f64; 2]; 6] {
        let mut c = [[0.0; 2]; 6];
        let mut a = self.lattice_angle;
        for corner in &mut c {
            *corner = [x + r * a.cos(), z + r * a.sin()];
            a += PI / 3.0;
        }
        c
    }

    fn push_hex(&mut self, x: f64, z: f64, r: f64, g: bool, i: i64, j: i64) {
        let y = self.cone_height(x, z);
        let c = self.hex_corners(x, z, r);
        self.hexes.push(Hex {
            x,
            y,
            z,
            g,
            c,
            bounds: Bounds::of(&c),
            i,
            j,
            tower_links: Vec::new(),
            bridge_links: Vec::new(),
            cb: [None; 6],
            hidden: false,
            progress: None,
            dy: 0.0,
        });
    }

    /// `GetHexRadius` (l.308): side length of a tower.
    fn hex_radius(&self, h: usize) -> f64 {
        let c = &self.hexes[h].c;
        ((c[1][0] - c[0][0]).powi(2) + (c[1][1] - c[0][1]).powi(2)).sqrt()
    }

    /// `GetBetterRectCornersList` (l.332): a `width` wide bridge from
    /// edge to edge (apothem to apothem) of the two towers.
    fn rect_corners(&self, h1: usize, h2: usize, width: f64) -> [[f64; 2]; 4] {
        let (a, b) = (&self.hexes[h1], &self.hexes[h2]);
        let r1 = self.hex_radius(h1) * 3f64.sqrt() / 2.0;
        let r2 = self.hex_radius(h2) * 3f64.sqrt() / 2.0;
        let len = ((b.x - a.x).powi(2) + (b.z - a.z).powi(2)).sqrt();
        let (dx, dz) = ((b.x - a.x) / len, (b.z - a.z) / len);
        let w = width / 2.0;
        [
            [a.x + dx * r1 + dz * w, a.z + dz * r1 - dx * w],
            [b.x - dx * r2 + dz * w, b.z - dz * r2 - dx * w],
            [b.x - dx * r2 - dz * w, b.z - dz * r2 + dx * w],
            [a.x + dx * r1 - dz * w, a.z + dz * r1 + dx * w],
        ]
    }

    fn push_rect(&mut self, h1: usize, h2: usize) {
        let c = self.rect_corners(h1, h2, self.rect_width);
        self.rects.push(Rect {
            hex1: h1,
            hex2: h2,
            c,
            bounds: Bounds::of(&c),
            hidden: false,
            progress: None,
            dy: 0.0,
        });
    }

    /// `GetConeHeight` (l.346): towers sit on a cone around the centre
    /// (rising or falling outwards with the rolled bridge slope).
    pub fn cone_height(&self, x: f64, z: f64) -> f64 {
        ((x - self.map_center_x).powi(2) + (z - self.map_center_z).powi(2)).sqrt()
            / self.map_center_x.max(self.map_center_z)
            * (self.edge_height - self.center_height)
            + self.center_height
    }

    /// `GetVoidHeight` (l.350): `PitDepth` under the cone, as a bed of
    /// spikes — every other vertex another 96 lower — so no square of
    /// the void is ever flat enough to walk on.
    pub fn void_height(&self, x: f64, z: f64) -> f64 {
        let spike = x.rem_euclid(16.0) == 0.0 && z.rem_euclid(16.0) == 0.0;
        self.cone_height(x, z) - self.pit_depth - if spike { 0.0 } else { 96.0 }
    }

    /// `GetRectHeight` (l.354): linear between the two towers' heights,
    /// stretched so it meets each tower's height at its edge.
    pub fn rect_height(&self, k: usize, x: f64, z: f64) -> f64 {
        let r = &self.rects[k];
        let (a, b) = (&self.hexes[r.hex1], &self.hexes[r.hex2]);
        let (x1, y1, z1, x2, y2, z2) = (a.x, a.y, a.z, b.x, b.y, b.z);
        let mut p = ((z1 - z) * (z1 - z2) - (x1 - x) * (x2 - x1))
            / ((z2 - z1).powi(2) + (x2 - x1).powi(2));
        let d = ((z2 - z1).powi(2) + (x2 - x1).powi(2)).sqrt() / 2.0;
        let med = self.tower_radius * (PI / 6.0).cos();
        p = ((2.0 * p - 1.0) * d / (d - med) + 1.0) / 2.0;
        y1 * (1.0 - p) + y2 * p
    }

    /// `DesignLayout` (l.400).
    fn design_layout(&mut self, team_list_len: usize) {
        let il = self.inter_length;
        let la = self.lattice_angle;
        let (min_x, max_x, min_z, max_z) = self.map_min_max();
        let (cx, cz) = (self.map_center_x, self.map_center_z);
        let (dx1, dz1) = (il * (la + PI / 6.0).cos(), il * (la + PI / 6.0).sin());
        let (dx2, dz2) = (il * (la - PI / 6.0).cos(), il * (la - PI / 6.0).sin());
        let m0 = il * 2.0 * (PI / 6.0).cos();
        let r0 = m0 * ((max_x - min_x).max(max_z - min_z) * 1.5 / m0).floor();
        let x0 = cx - r0 * la.cos();
        let z0 = cz - r0 * la.sin();
        let rms = ((max_x - min_x).min(max_z - min_z) / 2.0 - self.tower_radius).powi(2);
        let limit = (2.0 * (max_x - min_x).max(max_z - min_z) / il).floor() as i64;
        let com = match self.centered_on {
            CenteredOn::Hole => 0,
            CenteredOn::Tower => 2,
            CenteredOn::Bridge => 1,
        };

        // Bounding polygon, unit-sized then scaled and centred.
        let unit = |k: f64, scale: f64| {
            [
                (la + PI * k / 6.0).cos() * scale,
                (la + PI * k / 6.0).sin() * scale,
            ]
        };
        let mut poly: Vec<[f64; 2]> = match self.boundary {
            Boundary::Hexagon => (1..=6).map(|k| unit((1 + 2 * k) as f64, 1.0)).collect(),
            Boundary::Star => (1..=6)
                .flat_map(|k| [unit((2 * k) as f64, 0.5), unit((1 + 2 * k) as f64, 1.0)])
                .collect(),
            Boundary::Losange => {
                let smaller = (PI / 6.0).sin();
                vec![
                    unit(1.0, 1.0),
                    unit(4.0, smaller),
                    unit(7.0, 1.0),
                    unit(10.0, smaller),
                ]
            }
            Boundary::Triangle => vec![unit(1.0, 1.0), unit(5.0, 1.0), unit(9.0, 1.0)],
            Boundary::Circle | Boundary::Rectangle => Vec::new(),
        };
        if poly.len() > 1 {
            let largest = poly
                .iter()
                .fold(0.0f64, |m, p| m.max(p[0].abs()).max(p[1].abs()));
            let scale = self.map_dim_scale * (cx.max(cz) - self.tower_radius) / largest;
            for p in &mut poly {
                *p = [p[0] * scale + cx, p[1] * scale + cz];
            }
        }
        // The gadget initialises the polygon's bounds to the map area
        // inset by one tower radius (not the polygon's own extent).
        let poly_bounds = Bounds {
            xmin: min_x + self.tower_radius,
            xmax: max_x - self.tower_radius,
            zmin: min_z + self.tower_radius,
            zmax: max_z - self.tower_radius,
            ..Default::default()
        };
        let tr = self.tower_radius;
        let boundary = self.boundary;
        let boundary_check = |x: f64, z: f64| match boundary {
            Boundary::Circle => (x - cx).powi(2) + (z - cz).powi(2) < rms,
            Boundary::Rectangle => {
                x > min_x + tr && z > min_z + tr && x < max_x - tr && z < max_z - tr
            }
            _ => is_inside_polygon(&poly, &poly_bounds, x, z),
        };

        // "Placing Hexes"
        for i in 0..=limit {
            for j in 0..=limit {
                let x = x0 + i as f64 * dx1 + j as f64 * dx2;
                let z = z0 + i as f64 * dz1 + j as f64 * dz2;
                if (self.density == 2 || (i - j).rem_euclid(3) != com) && boundary_check(x, z) {
                    self.push_hex(x, z, tr, true, i, j);
                }
            }
        }
        if self.hexes.is_empty() {
            // "Some code of that gadget won't work without at least one hex"
            self.push_hex(cx, cz, tr, true, 0, 0);
        }

        // "Placing Geos"
        let per_team = self.hexes.len() / (team_list_len * if self.skin == 9 { 2 } else { 1 });
        self.geo_density = match per_team {
            0..=2 => 1,
            3..=4 => 2,
            5..=7 => 3,
            8..=12 => 4,
            13..=20 => 5,
            _ => 6,
        };
        let full = self.density == 2;
        let gd = self.geo_density;
        for h in &mut self.hexes {
            h.g = geo_selector(gd, full, com, h.i, h.j);
        }

        // "Placing Rects"
        let ceiled = (il * 1.2).powi(2);
        for i in 0..self.hexes.len() {
            for j in i + 1..self.hexes.len() {
                let (a, b) = (&self.hexes[i], &self.hexes[j]);
                if (b.x - a.x).powi(2) + (b.z - a.z).powi(2) < ceiled {
                    self.push_rect(i, j);
                }
            }
        }
        // "Sharp angles may create orphaned towers, need extra code to
        // bridge them"
        if self.density != 2 && self.hexes.len() >= 2 {
            let mut bridged: Vec<Vec<usize>> = vec![Vec::new(); self.hexes.len()];
            for r in &self.rects {
                bridged[r.hex1].push(r.hex2);
                bridged[r.hex2].push(r.hex1);
            }
            for i in 0..self.hexes.len() {
                let orphan = bridged[i].is_empty()
                    || (bridged[i].len() == 1 && bridged[bridged[i][0]].len() == 1);
                if !orphan {
                    continue;
                }
                for j in 0..self.hexes.len() {
                    let (a, b) = (&self.hexes[i], &self.hexes[j]);
                    let d = ((b.x - a.x).powi(2) + (b.z - a.z).powi(2)).sqrt();
                    if d < il * 2.1
                        && d > il * 1.9
                        && (bridged[i].is_empty() || bridged[i][0] != j)
                    {
                        self.push_rect(i, j);
                        bridged[i].push(j);
                        bridged[j].push(i);
                    }
                }
            }
            // "Special case with three misoriented towers"
            if self.hexes.len() == 3 && self.rects.is_empty() {
                let h = &self.hexes;
                let x = (h[0].x + h[1].x + h[2].x) / 3.0;
                let z = (h[0].z + h[1].z + h[2].z) / 3.0;
                let y = (h[0].y + h[1].y + h[2].y) / 3.0;
                let r = il * 3f64.sqrt() * 2.0 / 3.0 - tr;
                self.push_hex(x, z, r, false, 0, 0);
                self.hexes[3].y = y;
            }
        }

        // Links ("to hasten finding who's linked to who").
        for k in 0..self.rects.len() {
            let (a, b) = (self.rects[k].hex1, self.rects[k].hex2);
            self.hexes[a].tower_links.push(b);
            self.hexes[b].tower_links.push(a);
            self.hexes[a].bridge_links.push(k);
            self.hexes[b].bridge_links.push(k);
        }

        // "Note down which vertices of the towers are connected to a bridge"
        for k in 0..self.rects.len() {
            let ends = [
                (self.rects[k].hex1, self.rects[k].hex2),
                (self.rects[k].hex2, self.rects[k].hex1),
            ];
            for (i, j) in ends {
                let (hi, hj) = (&self.hexes[i], &self.hexes[j]);
                let dir = (hj.z - hi.z).atan2(hj.x - hi.x);
                let mut hits = Vec::new();
                for p in 0..6 {
                    let (c1, c2) = (hi.c[p], hi.c[(p + 1) % 6]);
                    let a1 = (c1[1] - hi.z).atan2(c1[0] - hi.x);
                    let a2 = (c2[1] - hi.z).atan2(c2[0] - hi.x);
                    let mean = if a1 - a2 > 4.0 {
                        (a1 + a2 - 2.0 * PI) / 2.0
                    } else {
                        (a1 + a2) / 2.0
                    };
                    let diff = (mean - dir)
                        .abs()
                        .min((mean - dir - 2.0 * PI).abs())
                        .min((mean - dir + 2.0 * PI).abs());
                    if diff < PI / 24.0 {
                        // `cb[1+(p+4)%6]` with the gadget's 1-based p.
                        hits.push((p + 5) % 6);
                    }
                }
                for s in hits {
                    self.hexes[i].cb[s] = Some(k);
                }
            }
        }
    }

    /// `SetStartPos` (l.1146), `startPosType==0` ("Fixed"): Kernel
    /// Panic's launcher writes `StartPosType` 0 (or 3, which the gadget
    /// also maps to 0 without a lobby). Gaia is placed on the tower
    /// nearest the centre, the first team on the furthest, and each
    /// next team on the tower furthest from the teams already placed.
    /// With `Exploration`, every tower but the teams' is then sunk.
    fn set_start_pos(&mut self, teams: usize) {
        let n = self.hexes.len();
        let team_list = teams + 1; // with Gaia in front
        let (cx, cz) = (self.map_center_x, self.map_center_z);
        let sd_center = |h: &Hex| (h.x - cx).powi(2) + (h.z - cz).powi(2);
        let mut used = vec![false; n];
        let mut team_hex: Vec<usize> = Vec::new();

        // Gaia: nearest to the centre.
        let mut best = 0;
        for h in 1..n {
            if sd_center(&self.hexes[h]) < sd_center(&self.hexes[best]) {
                best = h;
            }
        }
        used[best] = true;
        team_hex.push(best);
        // First team: furthest from the centre (Gaia's tower not excluded).
        let mut best = 0;
        for h in 1..n {
            if sd_center(&self.hexes[h]) > sd_center(&self.hexes[best]) {
                best = h;
            }
        }
        used[best] = true;
        team_hex.push(best);
        // The others: maximin distance to the non-Gaia teams placed.
        for _ in 2..team_list.min(n) {
            let mut best_sd = -1.0;
            let mut best = None;
            for h1 in (0..n).filter(|&h| !used[h]) {
                let (x1, z1) = (self.hexes[h1].x, self.hexes[h1].z);
                let sd2 = team_hex[1..]
                    .iter()
                    .map(|&t| (self.hexes[t].x - x1).powi(2) + (self.hexes[t].z - z1).powi(2))
                    .fold(f64::INFINITY, f64::min);
                if sd2 > best_sd {
                    best_sd = sd2;
                    best = Some(h1);
                }
            }
            // All towers taken (only when Gaia and the first team had to
            // share the lone tower): stack onto the first team's.
            let best = best.unwrap_or(team_hex[1]);
            used[best] = true;
            team_hex.push(best);
        }

        // Positions, for list entries 1.. (Gaia's is dropped).
        self.start_positions = (1..team_list)
            .map(|t| {
                if team_list <= n {
                    let h = &self.hexes[team_hex[t]];
                    [h.x, h.z]
                } else {
                    // More teams than towers: share, spread on a circle.
                    // (`t` is the gadget's 1-based list index minus one.)
                    let t1 = t + 1;
                    let angle_between = 2.0 * PI / (team_list as f64 / n as f64).ceil();
                    let a = self.lattice_angle + (t1 as f64 / n as f64).ceil() * angle_between;
                    let h = &self.hexes[team_hex[t1 % n]];
                    let r = self.tower_radius / 2.0;
                    [h.x + r * a.cos(), h.z + r * a.sin()]
                }
            })
            .collect();

        if self.exploration > 0.0 && team_list <= n {
            for h in &mut self.hexes {
                h.hidden = true;
            }
            for &t in &team_hex[1..] {
                self.hexes[t].hidden = false;
            }
            for k in 0..self.rects.len() {
                let (a, b) = (self.rects[k].hex1, self.rects[k].hex2);
                self.rects[k].hidden = self.hexes[a].hidden || self.hexes[b].hidden;
            }
        }
    }

    /// `SetWholeHeightMap` (l.673): the void's spike bed everywhere,
    /// then the visible bridges, then the visible towers on top.
    /// `heights` is the row-major `(mapx+1)×(mapy+1)` heightmap.
    pub fn write_whole_heightmap(&self, heights: &mut [f32]) {
        let w = (self.map_size_x / SQUARE) as usize + 1;
        for (i, h) in heights.iter_mut().enumerate() {
            let (x, z) = ((i % w) as f64 * SQUARE, (i / w) as f64 * SQUARE);
            *h = self.void_height(x, z) as f32;
        }
        for k in (0..self.rects.len()).filter(|&k| !self.rects[k].hidden) {
            self.write_poly_heights(Poly::Rect(k), heights);
        }
        for k in (0..self.hexes.len()).filter(|&k| !self.hexes[k].hidden) {
            self.write_poly_heights(Poly::Hex(k), heights);
        }
    }

    fn poly_shape(&self, p: Poly) -> (&[[f64; 2]], &Bounds, bool) {
        match p {
            Poly::Hex(k) => (&self.hexes[k].c, &self.hexes[k].bounds, self.hexes[k].hidden),
            Poly::Rect(k) => (&self.rects[k].c, &self.rects[k].bounds, self.rects[k].hidden),
        }
    }

    /// Heightmap half of `SetOneHex` / `SetOneRect` (l.861, l.936):
    /// every vertex inside the polygon gets its surface height, or the
    /// void's if the polygon is hidden. Returns the touched vertex box
    /// `(x0, z0, x1, z1)` (inclusive) for local re-syncs.
    pub fn write_poly_heights(&self, p: Poly, heights: &mut [f32]) -> [usize; 4] {
        let w = (self.map_size_x / SQUARE) as usize + 1;
        let hgt = (self.map_size_z / SQUARE) as usize + 1;
        let (c, b, hidden) = self.poly_shape(p);
        let mut touched = [usize::MAX, usize::MAX, 0, 0];
        let mut x = b.xmin8;
        while x <= b.xmax {
            let mut z = b.zmin8;
            while z <= b.zmax {
                let (ix, iz) = ((x / SQUARE) as i64, (z / SQUARE) as i64);
                if ix >= 0
                    && iz >= 0
                    && (ix as usize) < w
                    && (iz as usize) < hgt
                    && is_inside_polygon(c, b, x, z)
                {
                    let y = if hidden {
                        self.void_height(x, z)
                    } else {
                        match p {
                            Poly::Hex(k) => self.hexes[k].y,
                            Poly::Rect(k) => self.rect_height(k, x, z),
                        }
                    };
                    let (ix, iz) = (ix as usize, iz as usize);
                    heights[iz * w + ix] = y as f32;
                    touched = [
                        touched[0].min(ix),
                        touched[1].min(iz),
                        touched[2].max(ix),
                        touched[3].max(iz),
                    ];
                }
                z += SQUARE;
            }
            x += SQUARE;
        }
        touched
    }

    /// Visit the terrain squares whose centres lie inside `p` (the
    /// gadget's `xmin16`..`xmax` step-16 scans).
    fn for_poly_squares(&self, p: Poly, mut f: impl FnMut(usize)) {
        let (c, b, _) = self.poly_shape(p);
        let mut x = b.xmin16;
        while x <= b.xmax {
            let mut z = b.zmin16;
            while z <= b.zmax {
                let (sx, sz) = ((x / TYPE_SQUARE) as usize, (z / TYPE_SQUARE) as usize);
                if sx < self.type_w && sz < self.type_h && is_inside_polygon(c, b, x, z) {
                    f(sz * self.type_w + sx);
                }
                z += TYPE_SQUARE;
            }
            x += TYPE_SQUARE;
        }
    }

    /// `SetWholeTerrainAndMetalMap` (l.767), terrain half: void, then
    /// visible bridges, then visible towers.
    fn set_whole_terrain(&mut self) {
        let mut terrain = vec![TERRAIN_VOID; self.type_w * self.type_h];
        for k in (0..self.rects.len()).filter(|&k| !self.rects[k].hidden) {
            self.for_poly_squares(Poly::Rect(k), |i| terrain[i] = TERRAIN_BRIDGE);
        }
        for k in (0..self.hexes.len()).filter(|&k| !self.hexes[k].hidden) {
            self.for_poly_squares(Poly::Hex(k), |i| terrain[i] = TERRAIN_TOWER);
        }
        self.terrain = terrain;
    }

    /// Terrain half of `SetOneHex` / `SetOneRect`.
    fn set_poly_terrain(&mut self, p: Poly) {
        let (_, _, hidden) = self.poly_shape(p);
        let t = match (hidden, p) {
            (true, _) => TERRAIN_VOID,
            (false, Poly::Hex(_)) => TERRAIN_TOWER,
            (false, Poly::Rect(_)) => TERRAIN_BRIDGE,
        };
        let mut terrain = std::mem::take(&mut self.terrain);
        self.for_poly_squares(p, |i| terrain[i] = t);
        self.terrain = terrain;
    }

    /// `SetWholePolyMap` (l.960): which polygon each terrain square
    /// belongs to (bridges written last, so they win overlaps), hidden
    /// or not.
    fn set_whole_poly_map(&mut self) {
        let mut map = vec![0i32; self.type_w * self.type_h];
        for k in 0..self.hexes.len() {
            self.for_poly_squares(Poly::Hex(k), |i| map[i] = Poly::Hex(k).key());
        }
        for k in 0..self.rects.len() {
            self.for_poly_squares(Poly::Rect(k), |i| map[i] = Poly::Rect(k).key());
        }
        self.poly_map = map;
    }

    /// `SetWholePolyHealth` (l.990). With positive `Exploration`, hidden
    /// polygons start at full `PolyHitpoint` too: exploration raises
    /// them for free, and only a polygon sunk by damage (health set to
    /// `-PolyBuildpoint`) needs construction to come back.
    fn set_whole_poly_health(&mut self) {
        self.hex_health = vec![self.poly_hitpoint; self.hexes.len()];
        self.rect_health = vec![self.poly_hitpoint; self.rects.len()];
    }

    /// `PolyHealth[k]`.
    pub fn health(&self, p: Poly) -> f64 {
        match p {
            Poly::Hex(k) => self.hex_health[k],
            Poly::Rect(k) => self.rect_health[k],
        }
    }

    fn health_mut(&mut self, p: Poly) -> &mut f64 {
        match p {
            Poly::Hex(k) => &mut self.hex_health[k],
            Poly::Rect(k) => &mut self.rect_health[k],
        }
    }

    fn square_index(&self, x: f64, z: f64) -> Option<usize> {
        let (sx, sz) = ((x / TYPE_SQUARE).floor(), (z / TYPE_SQUARE).floor());
        (sx >= 0.0 && sz >= 0.0 && (sx as usize) < self.type_w && (sz as usize) < self.type_h)
            .then(|| sz as usize * self.type_w + sx as usize)
    }

    /// `PolyMap[floor(x/16)][floor(z/16)]`.
    pub fn poly_at(&self, x: f64, z: f64) -> Option<Poly> {
        self.square_index(x, z)
            .and_then(|i| Poly::from_key(self.poly_map[i]))
    }

    /// `Spring.GetGroundInfo(x,z)=="void"`. Off-map counts as void.
    pub fn is_void(&self, x: f64, z: f64) -> bool {
        self.square_index(x, z)
            .is_none_or(|i| self.terrain[i] == TERRAIN_VOID)
    }

    // ------------------------------------------------------------------
    // Dynamic mode (`Dynamic = Exploration or PolyHitpoint or
    // PolyBuildpoint`, always on for KP's defaults).
    // ------------------------------------------------------------------

    /// Take the events emitted since the last call.
    pub fn drain_events(&mut self) -> Vec<HexFarmEvent> {
        std::mem::take(&mut self.events)
    }

    fn hidden(&self, p: Poly) -> bool {
        match p {
            Poly::Hex(k) => self.hexes[k].hidden,
            Poly::Rect(k) => self.rects[k].hidden,
        }
    }

    fn progress(&self, p: Poly) -> Option<f64> {
        match p {
            Poly::Hex(k) => self.hexes[k].progress,
            Poly::Rect(k) => self.rects[k].progress,
        }
    }

    /// Set `hidden`/`progress`/`dy` together, as the gadget always does.
    fn set_state(&mut self, p: Poly, hidden: bool, progress: Option<f64>, dy: f64) {
        let (h, pr, d) = match p {
            Poly::Hex(k) => {
                let h = &mut self.hexes[k];
                (&mut h.hidden, &mut h.progress, &mut h.dy)
            }
            Poly::Rect(k) => {
                let r = &mut self.rects[k];
                (&mut r.hidden, &mut r.progress, &mut r.dy)
            }
        };
        (*h, *pr, *d) = (hidden, progress, dy);
    }

    fn moving(&mut self, poly: Poly, first_frame: f64, last_frame: f64, direction: i8, corner: u8) {
        self.events.push(HexFarmEvent::Moving {
            poly,
            first_frame,
            last_frame,
            direction,
            corner,
        });
    }

    /// Update `PolyPercent[k]`, emitting only on change (`PolyPercentUpdate`).
    fn set_percent(&mut self, p: Poly, percent: Option<i32>) {
        if !self.indication {
            return;
        }
        let changed = match percent {
            Some(v) => self.percent.insert(p, v) != Some(v),
            None => self.percent.remove(&p).is_some(),
        };
        if changed {
            self.events.push(HexFarmEvent::Percent(p, percent));
        }
    }

    /// `SetOneHex` / `SetOneRect` (l.861, l.936): rewrite the polygon's
    /// terrain type (and, through [`HexFarmEvent::Reshaped`], its
    /// heightmap) for its current visibility, and add/remove a tower's
    /// datavent. (`VentsMoveNotErase` only matters before frame 25,
    /// where parking a vent in the sky is the same as removing it.)
    fn set_one(&mut self, p: Poly) {
        self.set_poly_terrain(p);
        self.events.push(HexFarmEvent::Reshaped(p));
        if let Poly::Hex(k) = p
            && self.hexes[k].g
        {
            self.events.push(if self.hexes[k].hidden {
                HexFarmEvent::VentsRemoved(k)
            } else {
                HexFarmEvent::VentAdded(k)
            });
        }
    }

    /// `gadget:Explosion` (l.1727): with `PolyHitpoint`, every explosion
    /// damages the solid polygon under it (not one still rising) by the
    /// weapon's default damage; below zero it sinks — a tower taking
    /// its bridges down with it — and must be rebuilt with
    /// `PolyBuildpoint` of construction.
    pub fn explosion(&mut self, frame: f64, x: f64, z: f64, damage: f64) {
        let Some(p) = self.poly_at(x, z) else {
            return;
        };
        if self.hidden(p) || self.progress(p).is_some() {
            return;
        }
        *self.health_mut(p) -= damage;
        let percent = (0.5 + 100.0 * self.health(p) / self.poly_hitpoint).floor() as i32;
        self.set_percent(p, Some(percent));
        if self.health(p) >= 0.0 {
            return;
        }
        self.set_state(p, true, None, 0.0);
        let corner = if self.rng.range(1, 2) == 2 { 1 } else { 3 };
        self.moving(p, frame, frame + self.tower_pop_time / 4.0, -1, corner);
        *self.health_mut(p) = -self.poly_buildpoint;
        self.set_percent(p, None);
        self.set_one(p);
        if let Poly::Hex(k) = p {
            for r in self.hexes[k].bridge_links.clone() {
                if !self.rects[r].hidden || self.rects[r].progress.is_some() {
                    self.set_state(Poly::Rect(r), true, None, 0.0);
                    let corner = if self.rects[r].hex1 == k { 1 } else { 3 };
                    self.moving(
                        Poly::Rect(r),
                        frame,
                        frame + self.bridge_pop_time / 4.0,
                        -1,
                        corner,
                    );
                    *self.health_mut(Poly::Rect(r)) = -self.poly_buildpoint;
                    self.set_percent(Poly::Rect(r), None);
                    self.set_one(Poly::Rect(r));
                }
            }
        }
    }

    /// `gadget:AllowUnitBuildStep` (l.1662): a build step on a unit
    /// standing on tower `k` (`points` = `Amount × buildTime`) pays into
    /// every destroyed (health ≤ 0) sunk neighbour — bridges and
    /// towers alike — and raises the ones it completes.
    pub fn build_step(&mut self, frame: f64, x: f64, z: f64, points: f64) {
        let Some(Poly::Hex(k)) = self.poly_at(x, z) else {
            return;
        };
        let bp = self.poly_buildpoint;
        for r in self.hexes[k].bridge_links.clone() {
            let p = Poly::Rect(r);
            if !(self.rects[r].hidden && self.rects[r].progress.is_none() && self.health(p) <= 0.0) {
                continue;
            }
            *self.health_mut(p) += points;
            self.set_percent(p, Some((0.5 + 100.0 * (bp + self.health(p)) / bp).floor() as i32));
            if self.health(p) >= 0.0 {
                *self.health_mut(p) = self.poly_hitpoint;
                self.set_percent(p, None);
                self.set_state(p, false, Some(0.0), 41.0 / self.bridge_pop_time);
                let rect = &self.rects[r];
                let span = if self.hexes[rect.hex1].hidden || self.hexes[rect.hex2].hidden {
                    self.tower_pop_time.max(self.bridge_pop_time)
                } else {
                    self.bridge_pop_time
                };
                let corner = if rect.hex1 == k { 3 } else { 1 };
                self.moving(p, frame, frame + span, 1, corner);
            }
        }
        for h in self.hexes[k].tower_links.clone() {
            let p = Poly::Hex(h);
            if !(self.hexes[h].hidden && self.hexes[h].progress.is_none() && self.health(p) <= 0.0) {
                continue;
            }
            *self.health_mut(p) += points;
            self.set_percent(p, Some((0.5 + 100.0 * (bp + self.health(p)) / bp).floor() as i32));
            if self.health(p) >= 0.0 {
                *self.health_mut(p) = self.poly_hitpoint;
                self.set_percent(p, None);
                self.set_state(p, false, Some(0.0), 41.0 / self.tower_pop_time);
                self.moving(p, frame, frame + self.tower_pop_time, 1, 0);
            }
        }
    }

    /// The polygon part of `gadget:GameFrame` (l.1781), run on the
    /// frames where `frame % 41 == 25`, after the caller's fall sweep
    /// collected `occupied`: the polygons with a (non-flying, not
    /// fallen) unit on them. Exploration raises the towers next to
    /// occupied ones; rising polygons advance by `dy` per sweep (a tower
    /// nobody stands next to anymore sinks back), and finished ones
    /// become solid — a finished tower then raises its bridges to
    /// other solid towers.
    pub fn game_frame(&mut self, frame: f64, occupied: &std::collections::HashSet<Poly>) {
        let tpt = self.tower_pop_time;
        let bpt = self.bridge_pop_time;
        if self.exploration > 0.0 {
            for h1 in (0..self.hexes.len()).rev() {
                if !occupied.contains(&Poly::Hex(h1)) {
                    continue;
                }
                for n in self.hexes[h1].tower_links.clone().into_iter().rev() {
                    let h = &self.hexes[n];
                    if h.hidden && h.progress.is_none() && self.hex_health[n] > 0.0 {
                        self.set_state(Poly::Hex(n), false, Some(0.0), 41.0 / tpt);
                        self.moving(Poly::Hex(n), frame, frame + tpt, 1, 0);
                    }
                }
            }
        }
        for h in (0..self.hexes.len()).rev() {
            let Some(progress) = self.hexes[h].progress else {
                continue;
            };
            let neighbour_occupied = self.hexes[h]
                .tower_links
                .iter()
                .any(|&n| occupied.contains(&Poly::Hex(n)));
            if !neighbour_occupied && self.hexes[h].dy > 0.0 {
                // Sinking Hex
                self.hexes[h].dy = -41.0 / tpt;
                let first = frame - (1.0 - progress) * tpt;
                self.moving(Poly::Hex(h), first, first + tpt, -1, 0);
            }
            let progress = progress + self.hexes[h].dy;
            if progress < 0.0 {
                // Deleting Hex (its ground never became solid).
                self.set_state(Poly::Hex(h), true, None, 0.0);
            } else if progress > 1.0 {
                // Finishing Hex
                self.set_state(Poly::Hex(h), false, None, 0.0);
                self.set_one(Poly::Hex(h));
                if self.exploration > 0.0 {
                    for r in self.hexes[h].bridge_links.clone().into_iter().rev() {
                        let rect = &self.rects[r];
                        if rect.hidden
                            && rect.progress.is_none()
                            && !self.hexes[rect.hex1].hidden
                            && !self.hexes[rect.hex2].hidden
                            && self.rect_health[r] > 0.0
                        {
                            let corner = if rect.hex1 == h { 3 } else { 1 };
                            self.set_state(Poly::Rect(r), false, Some(0.0), 41.0 / bpt);
                            self.moving(Poly::Rect(r), frame, frame + bpt, 1, corner);
                        }
                    }
                }
            } else {
                self.hexes[h].progress = Some(progress);
            }
        }
        for r in (0..self.rects.len()).rev() {
            let Some(progress) = self.rects[r].progress else {
                continue;
            };
            let progress = progress + self.rects[r].dy;
            if progress < 0.0 {
                self.set_state(Poly::Rect(r), true, None, 0.0);
            } else if progress > 1.0 {
                self.set_state(Poly::Rect(r), false, None, 0.0);
                self.set_one(Poly::Rect(r));
            } else {
                self.rects[r].progress = Some(progress);
            }
        }
    }

    /// Datavents present on the visible towers (`RedoDatavents`,
    /// l.1117, after its frame-25 re-run: hidden towers' vents are
    /// erased rather than parked in the sky).
    ///
    /// Until frame 25 (`VentsMoveNotErase`) the gadget also creates a
    /// geovent for every *sunk* vent tower, `2 × mapSize` elmos up.
    /// Nothing can build there and its smoke is far off-screen; the
    /// point is that start-up scans of `Spring.GetAllFeatures()` count
    /// every vent the farm may ever show — Kernel Panic's `MetalToGeo`
    /// ("auto" turns metal spots into vents when a map has fewer than 4
    /// geos). The port has no such scan (no `MetalToGeo`, the AI reads
    /// the live vents), so they are not created.
    pub fn datavents(&self) -> Vec<[f64; 3]> {
        self.hexes
            .iter()
            .filter(|h| h.g && !h.hidden)
            .map(|h| [h.x, h.y, h.z])
            .collect()
    }

    /// `CalculateSmoothMeshProfile(step)` (l.1049): the aircraft flight
    /// profile by distance from the map centre, so planes keep a level
    /// averaged between the nearest towers instead of diving into the
    /// pits. Entry `k` is the height at radius `k * step`: the highest
    /// tower whose ring (`|s - tower distance| <= TowerRadius`) covers
    /// it, else a line between the nearest tower rings inside and out.
    /// Every tower counts, sunk (hidden) ones included.
    pub fn smooth_mesh_profile(&self, step: f64) -> Vec<f64> {
        struct Elem {
            s: f64,
            y: f64,
            r: f64,
        }
        let (xc, zc) = (self.map_center_x, self.map_center_z);
        let mut ep: Vec<Elem> = self
            .hexes
            .iter()
            .map(|h| Elem {
                s: ((h.x - xc).powi(2) + (h.z - zc).powi(2)).sqrt(),
                y: h.y,
                r: self.tower_radius,
            })
            .collect();
        // `table.sort` is unstable; a stable sort keeps the lowest tower
        // index among equal distances (which one survives the thinning).
        ep.sort_by(|a, b| a.s.total_cmp(&b.s));
        // Drop towers that are the same ring as the previous one (just
        // rotated about the centre).
        for k in (1..ep.len()).rev() {
            if (ep[k].r - ep[k - 1].r).abs() < 1.0 && (ep[k].s - ep[k - 1].s).abs() < 1.0 {
                ep.remove(k);
            }
        }
        let top = 2 + (1.42 * xc.max(zc) / step).floor() as usize;
        let mut smp = vec![0.0; top + 1];
        for (k, out) in smp.iter_mut().enumerate() {
            let s = step * k as f64;
            let mut y: Option<f64> = None;
            for e in &ep {
                if (s - e.s).abs() <= e.r && y.is_none_or(|y| y < e.y) {
                    y = Some(e.y);
                }
            }
            *out = y.unwrap_or_else(|| {
                // `h1, h2 = 1, #ep`: last ring at/inside `s`, first beyond.
                let (mut h1, mut h2) = (0, ep.len() - 1);
                for (k, e) in ep.iter().enumerate() {
                    if e.s <= s {
                        h1 = k;
                    } else {
                        h2 = k;
                        break;
                    }
                }
                let (s1, y1) = (ep[h1].s + ep[h1].r, ep[h1].y);
                let (s2, y2) = (ep[h2].s + ep[h2].r, ep[h2].y);
                if h1 != h2 && s2 != s1 {
                    (s - s1) * (y2 - y1) / (s2 - s1) + y1
                } else {
                    y1
                }
            });
        }
        smp
    }

    /// `SetWholeSmoothMesh` + `SetTheSmoothMesh` (l.1097-1117): every
    /// 16 elmos over the whole map, `Spring.SetSmoothMesh(x, z, h)` with
    /// the profile height at the (quantized) distance from the centre.
    /// `set(x, z, h)` is that call.
    pub fn set_whole_smooth_mesh(&self, mut set: impl FnMut(f64, f64, f64)) {
        const PROFILE_STEP: f64 = 7.0;
        let profile = self.smooth_mesh_profile(PROFILE_STEP);
        let (xc, zc) = (self.map_center_x, self.map_center_z);
        let mut x = 0.0;
        while x <= self.map_size_x {
            let mut z = 0.0;
            while z <= self.map_size_z {
                let d = ((x - xc).powi(2) + (z - zc).powi(2)).sqrt();
                let k = (PROFILE_STEP / 2.0 + d / PROFILE_STEP).floor() as usize;
                // Out of the profile: the gadget echoes an error and skips.
                if let Some(&h) = profile.get(k) {
                    set(x, z, h);
                }
                z += 16.0;
            }
            x += 16.0;
        }
    }

    /// The layout as the gadget sends it to its unsynced half
    /// (`SendHexFarmToUnsynced`, l.1466) — what the renderer draws.
    pub fn layout(&self) -> HexFarmLayout {
        HexFarmLayout {
            skin: Some(self.skin),
            team_colored: self.team_colored,
            hexes: self
                .hexes
                .iter()
                .map(|h| HexTower {
                    center: [h.x as f32, h.y as f32, h.z as f32],
                    g: h.g as i64,
                    corners: h.c.map(|c| [c[0] as f32, h.y as f32, c[1] as f32]),
                    corner_bridges: h.cb.map(|b| b.map_or(0, |k| k as i64 + 1)),
                    hidden: h.hidden,
                })
                .collect(),
            bridges: self
                .rects
                .iter()
                .map(|r| {
                    let (y1, y2) = (self.hexes[r.hex1].y as f32, self.hexes[r.hex2].y as f32);
                    let c = r.c;
                    HexBridge {
                        hex1: r.hex1 as i64 + 1,
                        hex2: r.hex2 as i64 + 1,
                        corners: [
                            [c[0][0] as f32, y1, c[0][1] as f32],
                            [c[1][0] as f32, y2, c[1][1] as f32],
                            [c[2][0] as f32, y2, c[2][1] as f32],
                            [c[3][0] as f32, y1, c[3][1] as f32],
                        ],
                        hidden: r.hidden,
                    }
                })
                .collect(),
        }
    }
}

/// `AllGeoSelectors[GeoDensity][TowerBridgeDensity==2 and 1 or 2]`
/// (l.529): which lattice nodes carry a datavent. Lua's `%` is floored,
/// hence `rem_euclid`.
fn geo_selector(density: usize, full: bool, com: i64, i: i64, j: i64) -> bool {
    let m = |a: i64, n: i64| a.rem_euclid(n);
    match (density, full) {
        (1, _) => true,
        (2, true) => m(i - j, 3) != 0,
        (2, false) => m(i, 3) > 1 || m(i - j - com, 3) != 1,
        (3, true) => m(i - j, 4) >= 2,
        (3, false) => m(i - j, 3) != m(com + 1, 3),
        (4, true) => m(i - j, 3) == 0,
        (4, false) => m(i, 3) <= 1 && m(i - j - com, 3) == 1,
        (5, true) => m(i, 2) == 1 && m(j, 2) == 1,
        (5, false) => m(i, 2) == 0 && m(i - j - com, 2) == 1,
        (6, true) => m(i, 3) == 0 && m(j, 2) == 0,
        (6, false) => m(i, 3) == 1 && m(i - j - com, 3) == 1,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: f64 = 12288.0;

    fn setup(teams: usize) -> HexFarmSetup {
        HexFarmSetup {
            map_size_x: SIZE,
            map_size_z: SIZE,
            teams,
            median_health: 400.0,
            median_build_time: 800.0,
        }
    }

    fn heights(farm: &HexFarm) -> Vec<f32> {
        let w = (SIZE / 8.0) as usize + 1;
        let mut h = vec![0.0; w * w];
        farm.write_whole_heightmap(&mut h);
        h
    }

    #[test]
    fn same_seed_same_layout() {
        let (a, b) = (HexFarm::generate(7, setup(2)), HexFarm::generate(7, setup(2)));
        assert_eq!(a.hexes.len(), b.hexes.len());
        assert_eq!(a.rects.len(), b.rects.len());
        assert_eq!(a.start_positions, b.start_positions);
        assert_eq!(heights(&a), heights(&b));
    }

    /// The flight profile bridges the pits: at every tower's distance
    /// from the centre it is at least that tower's height, and between
    /// rings it interpolates instead of dropping to the void.
    #[test]
    fn smooth_mesh_profile_bridges_the_pits() {
        for seed in 0..10u64 {
            let farm = HexFarm::generate(seed, setup(2 + (seed % 4) as usize));
            let step = 7.0;
            let p = farm.smooth_mesh_profile(step);
            let void_floor = farm.center_height.min(farm.edge_height) - farm.pit_depth;
            let (lo, hi) = farm.hexes.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), h| {
                (lo.min(h.y), hi.max(h.y))
            });
            for h in &farm.hexes {
                let s = ((h.x - farm.map_center_x).powi(2) + (h.z - farm.map_center_z).powi(2)).sqrt();
                let k = (s / step).round() as usize;
                assert!(p[k] >= h.y - 1e-9, "seed {seed}: ring below its tower");
            }
            for &y in &p {
                // Interpolation between rings stays within the towers'
                // span (bar extrapolation off the outer ring's edge).
                assert!(y > void_floor, "seed {seed}: profile dips into the void ({y})");
                assert!(y <= hi + (hi - lo) + 1.0);
            }
        }
    }

    /// `SetTheSmoothMesh` visits every 16-elmo mesh cell.
    #[test]
    fn whole_smooth_mesh_covers_every_cell() {
        let farm = HexFarm::generate(3, setup(2));
        let hm = heights(&farm);
        let w = (SIZE / 8.0) as usize + 1;
        let mut mesh = crate::smooth_mesh::SmoothHeightMesh::new(&hm, w, w);
        let (mx, my) = mesh.dims();
        let mut hit = vec![false; mx * my];
        farm.set_whole_smooth_mesh(|x, z, h| {
            if mesh.set_smooth_mesh(x as f32, z as f32, h as f32, None).is_some() {
                hit[(z / 16.0) as usize * mx + (x / 16.0) as usize] = true;
            }
        });
        assert!(hit.iter().all(|&b| b));
        // Over a tower the mesh is now the profile, not the max filter
        // of the surrounding cliffs.
        let t = &farm.hexes[0];
        let flat = farm.smooth_mesh_profile(7.0);
        let d = ((t.x - farm.map_center_x).powi(2) + (t.z - farm.map_center_z).powi(2)).sqrt();
        let k = (3.5 + d / 7.0).floor() as usize;
        let cell = (t.z / 16.0) as usize * mx + (t.x / 16.0) as usize;
        assert!((mesh.mesh()[cell] as f64 - flat[k]).abs() < 1e-3);
    }

    #[test]
    fn lua_median_seeds_with_one() {
        // {1, 2, 3, 10} -> hp[floor(1+4/2)] = hp[3] = 3
        assert_eq!(lua_median([10.0, 2.0, 3.0]), 3.0);
    }

    #[test]
    fn layout_invariants_over_many_seeds() {
        for seed in 0..60u64 {
            let teams = 2 + (seed % 5) as usize;
            let farm = HexFarm::generate(seed, setup(teams));
            assert!(!farm.hexes.is_empty(), "seed {seed}: no towers");
            let (min_x, max_x, min_z, max_z) = farm.map_min_max();

            // Towers (lattice ones) lie inside the rolled area.
            for h in farm.hexes.iter().filter(|h| h.g || h.i != 0 || h.j != 0) {
                assert!(
                    h.x >= min_x && h.x <= max_x && h.z >= min_z && h.z <= max_z,
                    "seed {seed}: tower off the area"
                );
                for c in &h.c {
                    assert!(c[0] > 0.0 && c[0] < SIZE && c[1] > 0.0 && c[1] < SIZE);
                }
            }

            // Bridges join neighbouring towers (1 or ~2 lattice steps).
            for (k, r) in farm.rects.iter().enumerate() {
                let (a, b) = (&farm.hexes[r.hex1], &farm.hexes[r.hex2]);
                let d = ((b.x - a.x).powi(2) + (b.z - a.z).powi(2)).sqrt();
                assert!(
                    d < farm.inter_length * 1.2 || (d > farm.inter_length * 1.9 && d < farm.inter_length * 2.1),
                    "seed {seed}: bridge spans {d} (inter {})",
                    farm.inter_length
                );
                assert!(a.bridge_links.contains(&k) && b.bridge_links.contains(&k));
                assert!(a.tower_links.contains(&r.hex2));
            }

            // Every team starts on its own visible tower, which the
            // heightmap makes flat at the tower height.
            assert_eq!(farm.start_positions.len(), teams);
            let hm = heights(&farm);
            let w = (SIZE / 8.0) as usize + 1;
            for (t, sp) in farm.start_positions.iter().enumerate() {
                let Some(Poly::Hex(k)) = farm.poly_at(sp[0], sp[1]) else {
                    panic!("seed {seed}: team {t} not on a tower");
                };
                let h = &farm.hexes[k];
                assert!(!h.hidden, "seed {seed}: start tower sunk");
                assert!(!farm.is_void(sp[0], sp[1]));
                let (ix, iz) = ((sp[0] / 8.0).round() as usize, (sp[1] / 8.0).round() as usize);
                for dz in 0..3 {
                    for dx in 0..3 {
                        let y = hm[(iz + dz - 1) * w + ix + dx - 1];
                        assert!((y as f64 - h.y).abs() < 0.01, "seed {seed}: start not flat");
                    }
                }
            }
            if farm.hexes.len() > teams {
                // Distinct towers when there are enough.
                let mut spots = farm.start_positions.clone();
                spots.sort_by(|a, b| a.partial_cmp(b).unwrap());
                spots.dedup();
                assert_eq!(spots.len(), teams, "seed {seed}: shared start tower");
            }

            // Vents sit on visible towers.
            for v in farm.datavents() {
                assert!(matches!(farm.poly_at(v[0], v[2]), Some(Poly::Hex(_))));
                assert!(!farm.is_void(v[0], v[2]));
            }

            // Exploration: only the start towers (and bridges between
            // them) are up; the void is below every tower top.
            let visible = farm.hexes.iter().filter(|h| !h.hidden).count();
            if farm.hexes.len() > teams {
                assert!(visible <= teams);
            }
            for r in farm.rects.iter().filter(|r| !r.hidden) {
                assert!(!farm.hexes[r.hex1].hidden && !farm.hexes[r.hex2].hidden);
            }
            for h in &farm.hexes {
                assert!(farm.void_height(h.x, h.z) < h.y - 100.0);
            }
        }
    }

    #[test]
    fn bridge_height_meets_tower_edges() {
        for seed in 0..40u64 {
            let farm = HexFarm::generate(seed, setup(2));
            for (k, r) in farm.rects.iter().enumerate() {
                let (a, b) = (&farm.hexes[r.hex1], &farm.hexes[r.hex2]);
                // Midpoints of the bridge's two tower-side edges.
                let e1 = [(r.c[0][0] + r.c[3][0]) / 2.0, (r.c[0][1] + r.c[3][1]) / 2.0];
                let e2 = [(r.c[1][0] + r.c[2][0]) / 2.0, (r.c[1][1] + r.c[2][1]) / 2.0];
                if (farm.hex_radius(r.hex1) - farm.tower_radius).abs() < 1e-6
                    && (farm.hex_radius(r.hex2) - farm.tower_radius).abs() < 1e-6
                {
                    assert!((farm.rect_height(k, e1[0], e1[1]) - a.y).abs() < 1e-6);
                    assert!((farm.rect_height(k, e2[0], e2[1]) - b.y).abs() < 1e-6);
                }
            }
        }
    }

    #[test]
    fn void_is_a_spike_bed() {
        let farm = HexFarm::generate(3, setup(2));
        let spike = farm.void_height(16.0, 32.0) - farm.cone_height(16.0, 32.0);
        let pit = farm.void_height(24.0, 32.0) - farm.cone_height(24.0, 32.0);
        assert!((spike - pit - 96.0).abs() < 1e-6);
        assert!(farm.is_void(1.0, 1.0));
        assert!(farm.poly_at(1.0, 1.0).is_none());
    }

    /// Run sweeps (every 41 frames from 25) with `occupied` fixed.
    fn sweep(farm: &mut HexFarm, frames: &mut f64, count: usize, occupied: &std::collections::HashSet<Poly>) {
        for _ in 0..count {
            *frames += 41.0;
            farm.game_frame(*frames, occupied);
        }
    }

    /// A seed whose layout has a start tower with at least one neighbour.
    fn farm_with_neighbours() -> (HexFarm, usize) {
        for seed in 0..100 {
            let farm = HexFarm::generate(seed, setup(2));
            if let Some(k) = (0..farm.hexes.len())
                .find(|&k| !farm.hexes[k].hidden && !farm.hexes[k].tower_links.is_empty())
            {
                return (farm, k);
            }
        }
        panic!("no layout with linked towers");
    }

    #[test]
    fn exploration_raises_neighbours_then_bridges() {
        let (mut farm, start) = farm_with_neighbours();
        let occupied = std::collections::HashSet::from([Poly::Hex(start)]);
        let n = farm.hexes[start].tower_links[0];
        assert!(farm.hexes[n].hidden);
        let mut frame = 25.0;
        farm.game_frame(frame, &occupied);
        let events = farm.drain_events();
        assert!(events.iter().any(|e| matches!(e, HexFarmEvent::Moving { poly: Poly::Hex(h), direction: 1, .. } if *h == n)));
        // Still rising: not walkable yet.
        assert!(farm.is_void(farm.hexes[n].x, farm.hexes[n].z));
        // TowerPopTime = 300 frames: done after ceil(300/41)+1 sweeps.
        sweep(&mut farm, &mut frame, 8, &occupied);
        assert!(!farm.hexes[n].hidden && farm.hexes[n].progress.is_none());
        assert!(!farm.is_void(farm.hexes[n].x, farm.hexes[n].z));
        let events = farm.drain_events();
        assert!(events.contains(&HexFarmEvent::Reshaped(Poly::Hex(n))));
        // The bridge between the two is now rising, then solid.
        let r = farm.hexes[start]
            .bridge_links
            .iter()
            .copied()
            .find(|&r| farm.rects[r].hex1 == n || farm.rects[r].hex2 == n)
            .unwrap();
        assert!(farm.rects[r].progress.is_some());
        sweep(&mut farm, &mut frame, 5, &occupied);
        assert!(!farm.rects[r].hidden && farm.rects[r].progress.is_none());
    }

    #[test]
    fn unattended_rising_tower_sinks_back() {
        let (mut farm, start) = farm_with_neighbours();
        let n = farm.hexes[start].tower_links[0];
        let mut frame = 25.0;
        farm.game_frame(frame, &std::collections::HashSet::from([Poly::Hex(start)]));
        sweep(&mut farm, &mut frame, 2, &std::collections::HashSet::from([Poly::Hex(start)]));
        sweep(&mut farm, &mut frame, 10, &Default::default());
        assert!(farm.hexes[n].hidden && farm.hexes[n].progress.is_none());
    }

    #[test]
    fn explosions_sink_and_construction_raises() {
        let (mut farm, start) = farm_with_neighbours();
        let w = (SIZE / 8.0) as usize + 1;
        let mut hm = heights(&farm);
        let n = farm.hexes[start].tower_links[0];
        // Explore n and its bridge first.
        let occupied = std::collections::HashSet::from([Poly::Hex(start)]);
        let mut frame = 25.0;
        sweep(&mut farm, &mut frame, 15, &occupied);
        assert!(!farm.hexes[n].hidden);
        farm.drain_events();

        let (x, z) = (farm.hexes[n].x, farm.hexes[n].z);
        farm.explosion(frame, x, z, farm.poly_hitpoint * 0.5);
        assert!(!farm.hexes[n].hidden);
        assert_eq!(farm.drain_events(), vec![HexFarmEvent::Percent(Poly::Hex(n), Some(50))]);
        farm.explosion(frame, x, z, farm.poly_hitpoint);
        assert!(farm.hexes[n].hidden && farm.is_void(x, z));
        assert_eq!(farm.health(Poly::Hex(n)), -farm.poly_buildpoint);
        let events = farm.drain_events();
        assert!(events.contains(&HexFarmEvent::Reshaped(Poly::Hex(n))));
        for p in events.iter().filter_map(|e| match e {
            HexFarmEvent::Reshaped(p) => Some(*p),
            _ => None,
        }) {
            farm.write_poly_heights(p, &mut hm);
        }
        let (ix, iz) = ((x / 8.0) as usize, (z / 8.0) as usize);
        assert!((hm[iz * w + ix] as f64) < farm.hexes[n].y - 100.0);
        // Its bridges went down with it.
        for &r in &farm.hexes[n].bridge_links {
            assert!(farm.rects[r].hidden);
        }
        // Destroyed towers are not re-explored...
        sweep(&mut farm, &mut frame, 10, &occupied);
        assert!(farm.hexes[n].hidden);
        // ...but building next to them raises them.
        let (sx, sz) = (farm.hexes[start].x, farm.hexes[start].z);
        farm.build_step(frame, sx, sz, farm.poly_buildpoint * 0.25);
        assert!(farm.hexes[n].hidden);
        farm.build_step(frame, sx, sz, farm.poly_buildpoint);
        assert!(!farm.hexes[n].hidden && farm.hexes[n].progress == Some(0.0));
        sweep(&mut farm, &mut frame, 9, &occupied);
        assert!(!farm.hexes[n].hidden && !farm.is_void(x, z));
        assert_eq!(farm.health(Poly::Hex(n)), farm.poly_hitpoint);
    }
}
