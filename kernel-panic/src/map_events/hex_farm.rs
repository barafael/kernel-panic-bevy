//! Hex Farm 8's dynamic mode: towers and bridges rise and sink during
//! play, exactly as `HexFarm8.lua` drives them.
//!
//! The synced rules live in [`spring_map::hexfarm::HexFarm`] (pure,
//! unit-tested); this module is the engine glue the gadget got from
//! Spring:
//!
//! - **Call-ins in**: every weapon impact (`gadget:Explosion`) and every
//!   build step (`gadget:AllowUnitBuildStep`) is queued in
//!   [`HexFarmInbox`] by the combat / construction / production systems;
//!   [`hex_farm_sim`] feeds them to the farm each sim frame.
//! - **The fall sweep** (`gadget:GameFrame`, `frame % 41 == 25`):
//!   non-flying units on the void are pushed 17 then 33 elmos back onto
//!   solid ground, or self-destruct ("fell into the void"); the others
//!   mark their polygon occupied, which drives exploration.
//! - **Effects out**: `SetOneHex`/`SetOneRect` rewrite the heightmap —
//!   mirrored into the [`Heightmap`] resource, the nav grids, the
//!   aircraft [`SmoothGround`] (queued `MapChanged`), the (invisible,
//!   ray-cast) terrain mesh and the minimap for just the touched area —
//!   and add/remove datavents.
//! - **The unsynced half**: [`hex_farm_draw`] keeps the still towers
//!   and bridges in one mesh (the gadget's display list, rebuilt on any
//!   change) and redraws the animated ones every frame (rise: top slides
//!   up from `-VisualPitDepth` and fades in; bridges swing up about
//!   their far end), plus the `%` indicators and, in team-coloured
//!   games, the owners' colours.
//!
//! Everything is inert unless the map loader inserted [`HexFarmState`].

use std::collections::{HashMap, HashSet};

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::prelude::*;

use spring_map::hexfarm::{HexFarm, HexFarmEvent, Poly};
use spring_map::lua_layout::HexFarmLayout;

use crate::interaction::movement::NavGridSet;
use crate::map_loading::TerrainChunkCoord;
use crate::map_loading::lua_compositing::{
    HexDraw, QuadBuffer, Rgba, WHITE, atlas_material, minimap_pixels, push_hex, push_rect,
};
use crate::rendering::camera::RtsCamera;
use crate::sim::SQUARE_SIZE;
use crate::terrain::geovent::{GeoventSmoker, spawn_smoker_at};
use crate::terrain::heightmap::Heightmap;
use crate::terrain::mesh::{CHUNK_SIZE, build_chunk};
use crate::terrain::smooth_ground::SmoothGround;
use crate::ui::minimap::MinimapState;
use crate::units::combat::Dying;
use crate::units::components::{Faction, Health, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::spawning::Emerging;
use crate::units::spatial::flat_dist_sq;

/// Resolution of the footprint image the minimap is painted from.
pub const MINIMAP_RES: usize = 400;

/// Minimum gap between full minimap repaints while reshapes keep
/// landing (a sinking tower fires `Reshaped` several times a second;
/// each full repaint allocates ~640 KB).
const MINIMAP_REPAINT_INTERVAL: f32 = 0.25;

/// Weapon impacts and build steps since the last sim frame, as the
/// engine would have called `Explosion` / `AllowUnitBuildStep`.
#[derive(Resource, Default)]
pub struct HexFarmInbox {
    explosions: Vec<(f64, f64, f64)>,
    build_steps: Vec<(f64, f64, f64)>,
}

impl HexFarmInbox {
    /// An explosion at `pos` of a weapon whose default damage is `damage`.
    pub fn explosion(&mut self, pos: Vec3, damage: f32) {
        self.explosions
            .push((pos.x as f64, pos.z as f64, damage as f64));
    }

    /// A unit being built at `pos` gained `points` (`Amount × buildTime`).
    pub fn build_step(&mut self, pos: Vec3, points: f32) {
        self.build_steps
            .push((pos.x as f64, pos.z as f64, points as f64));
    }
}

/// The synced gadget state and its sim-frame clock.
#[derive(Resource)]
pub struct HexFarmState {
    pub farm: HexFarm,
    /// `Spring.GetGameFrame()`: 30 Hz sim frames since the match began.
    pub frame: u64,
}

/// `h.firstframe`/`h.lastframe`/`h.direction`/`r.corner`.
#[derive(Debug, Clone, Copy)]
struct Anim {
    first: f64,
    last: f64,
    direction: i8,
    corner: u8,
}

impl Anim {
    fn progress(&self, frame: f64) -> f32 {
        let p = ((frame - self.first) / (self.last - self.first)).clamp(0.0, 1.0) as f32;
        if self.direction < 0 { 1.0 - p } else { p }
    }
}

/// The unsynced gadget's view of one polygon.
#[derive(Debug, Clone, Copy)]
struct PolyView {
    hidden: bool,
    anim: Option<Anim>,
}

/// The unsynced half: what's drawn, and how.
#[derive(Resource)]
pub struct HexFarmView {
    layout: HexFarmLayout,
    hexes: Vec<PolyView>,
    rects: Vec<PolyView>,
    /// `TeamColors[hex.owner]` (white unless team-coloured).
    owners: Vec<Rgba>,
    side_hex: f32,
    team_colored: bool,
    still: (Entity, Handle<Mesh>),
    moving: (Entity, Handle<Mesh>),
    /// `HexFarmDisplayList == nil`: rebuild the still mesh.
    still_dirty: bool,
    /// `%` indicators: value and label entity.
    percent: HashMap<Poly, (i32, Entity)>,
    /// Towers whose claimant ([`TowerClaim`]) died or went away since
    /// [`hex_farm_owners`] last ran, with that claimant.
    released: Vec<(usize, Entity)>,
}

/// A finished big building within `TowerRadius` of tower `.0`: it took
/// the tower on `UnitFinished`. Its removal is that building's
/// `UnitDestroyed` — `Dying` drops it at the kill, a despawn without the
/// death pipeline with the entity — and queues the tower for
/// [`hex_farm_owners`] to hand on. A hook, not an `Added<Dying>` read:
/// a building is usually despawned in the very sim tick it gets `Dying`,
/// before any later system (let alone one in `Update`) could see it.
#[derive(Component)]
#[component(storage = "SparseSet", on_remove = TowerClaim::on_remove)]
pub struct TowerClaim(usize);

impl TowerClaim {
    fn on_remove(mut world: DeferredWorld, ctx: HookContext) {
        let Some(&TowerClaim(tower)) = world.get::<TowerClaim>(ctx.entity) else {
            return;
        };
        if let Some(mut view) = world.get_resource_mut::<HexFarmView>() {
            view.released.push((tower, ctx.entity));
        }
    }
}

/// A `%` indicator label.
#[derive(Component)]
struct PercentLabel {
    poly: Poly,
    /// The value its text shows.
    shown: Option<i32>,
}

pub struct HexFarmPlugin;

impl Plugin for HexFarmPlugin {
    fn build(&self, app: &mut App) {
        use crate::units::GameplaySet;
        app.add_systems(
            FixedUpdate,
            hex_farm_sim
                .in_set(GameplaySet::Cleanup)
                .run_if(resource_exists::<HexFarmState>),
        )
        .add_systems(
            Update,
            (hex_farm_owners, hex_farm_draw, hex_farm_labels)
                .chain()
                .run_if(resource_exists::<HexFarmView>),
        );
    }
}

/// Map-load hook: spawn the drawing entities and install the resources
/// (or clear a previous Hex Farm's when `farm` is `None`). The image is
/// the skin atlas the loader built for this farm
/// (`lua_compositing::atlas_image`, team-coloured when the farm is).
pub fn install(
    farm: Option<(HexFarm, Image)>,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    let Some((farm, atlas)) = farm else {
        commands.remove_resource::<HexFarmState>();
        commands.remove_resource::<HexFarmInbox>();
        commands.remove_resource::<HexFarmView>();
        return;
    };
    let layout = farm.layout();
    let team_colored = farm.team_colored;
    let material = atlas_material(images.add(atlas), materials);
    let first = &layout.hexes[0].corners;
    let side_hex =
        ((first[1][0] - first[0][0]).powi(2) + (first[1][2] - first[0][2]).powi(2)).sqrt();
    // The meshes are rebuilt in place, so their bounds would go stale:
    // opt out of culling (the gadget draws them unculled too). The
    // handle is only reserved: `set_mesh` inserts the first non-empty
    // buffer, since an empty mesh asset makes the renderer warn
    // ("contains no vertices") — and the moving one stays empty until
    // a polygon animates.
    let spawn_mesh = |commands: &mut Commands| {
        let handle = meshes.reserve_handle();
        let entity = commands
            .spawn((
                Mesh3d(handle.clone()),
                MeshMaterial3d(material.clone()),
                NoFrustumCulling,
                Visibility::Hidden,
            ))
            .id();
        (entity, handle)
    };
    let still = spawn_mesh(commands);
    let moving = spawn_mesh(commands);
    let view = |hidden| PolyView { hidden, anim: None };
    commands.insert_resource(HexFarmView {
        hexes: layout.hexes.iter().map(|h| view(h.hidden)).collect(),
        rects: layout.bridges.iter().map(|r| view(r.hidden)).collect(),
        owners: vec![WHITE; layout.hexes.len()],
        layout,
        side_hex,
        team_colored,
        still,
        moving,
        still_dirty: true,
        percent: HashMap::new(),
        released: Vec::new(),
    });
    commands.insert_resource(HexFarmState { farm, frame: 0 });
    commands.insert_resource(HexFarmInbox::default());
}

/// Terrain type 0 "void" has `moveSpeeds` 0 for every move class
/// (mapinfo `terrainTypes`), so it is impassable whatever a unit's slope
/// tolerance — the spike bed alone would let an all-terrain unit path
/// across. Zero the nav cells over void squares in the cell box
/// `x0..=x1, z0..=z1` (8-elmo cells; terrain squares are 16).
pub fn mask_void(
    terrain: &[u8],
    type_w: usize,
    map: &mut spring_pathfinding::SpeedMap,
    [x0, z0, x1, z1]: [u32; 4],
) {
    for z in z0..=z1.min(map.height - 1) {
        for x in x0..=x1.min(map.width - 1) {
            let sq = (z / 2) as usize * type_w + (x / 2) as usize;
            if terrain
                .get(sq)
                .is_none_or(|&t| t == spring_map::hexfarm::TERRAIN_VOID)
            {
                map.speeds[(z * map.width + x) as usize] = 0.0;
            }
        }
    }
}

/// The nav side of a reshaped polygon: every bucket's cells over the
/// changed vertices recomputed (slope × terrain-type speed, see
/// [`NavGridSet::heights_changed`]), then void masked over exactly the
/// cells that pass rewrote — whole 2×2 slope blocks, wider than the
/// vertex box. Those cells, padded by the widest mover footprint, are
/// the change existing paths and QTPFS layers re-check (`PathUpdated` /
/// `UpdateNodeLayer` on `TerrainChange`).
fn reshape_nav(
    nav: &mut NavGridSet,
    terrain: &[u8],
    type_w: usize,
    heights: &[f32],
    heightmap_width: usize,
    vertices: [u32; 4],
) {
    let Some(cells) = nav.heights_changed(heights, heightmap_width as u32, vertices) else {
        return;
    };
    for bucket in &mut nav.buckets {
        mask_void(terrain, type_w, &mut bucket.speed_map, cells);
    }
    let h = nav.structures.max_xsizeh() + 1;
    let [x0, z0, x1, z1] = cells.map(|c| c as i32);
    nav.bump([x0 - h, z0 - h, x1 + h, z1 + h]);
}

/// Offsets the fall sweep tries, in order (`gadget:GameFrame` l.1788).
const PUSHES: [(f32, f32); 8] = [
    (-17.0, 0.0),
    (17.0, 0.0),
    (0.0, -17.0),
    (0.0, 17.0),
    (-33.0, 0.0),
    (33.0, 0.0),
    (0.0, -33.0),
    (0.0, 33.0),
];

/// One sim frame of the synced gadget.
#[allow(clippy::too_many_arguments)]
fn hex_farm_sim(
    mut state: ResMut<HexFarmState>,
    mut inbox: ResMut<HexFarmInbox>,
    mut view: ResMut<HexFarmView>,
    mut heightmap: Option<ResMut<Heightmap>>,
    mut nav: Option<ResMut<NavGridSet>>,
    mut smooth_ground: Option<ResMut<SmoothGround>>,
    mut minimap: Option<ResMut<MinimapState>>,
    mut units: Query<(&mut Transform, &UnitType, &mut Health), Without<Dying>>,
    registry: Res<UnitRegistry>,
    smokers: Query<(Entity, &GeoventSmoker)>,
    chunks: Query<(Entity, &TerrainChunkCoord, &Mesh3d)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
    // Full 400×400 repaint allocates ~640 KB and walks ~160 k polygon
    // lookups, and a busy tower sinking fires `Reshaped` several times
    // a second — repaint at most every [`MINIMAP_REPAINT_INTERVAL`],
    // carrying the request forward.
    mut minimap_pending: Local<bool>,
    mut minimap_cooldown: Local<f32>,
) {
    let state = &mut *state;
    state.frame += 1;
    let frame = state.frame as f64;
    let farm = &mut state.farm;

    for (x, z, damage) in inbox.explosions.drain(..) {
        farm.explosion(frame, x, z, damage);
    }
    for (x, z, points) in inbox.build_steps.drain(..) {
        farm.build_step(frame, x, z, points);
    }

    if state.frame % 41 == 25 {
        // "To remove units that fell"
        let mut occupied = HashSet::new();
        for (mut tf, kind, mut health) in &mut units {
            if registry.can_fly(kind.0) {
                continue;
            }
            let (x, z) = (tf.translation.x, tf.translation.z);
            if !farm.is_void(x as f64, z as f64) {
                occupied.extend(farm.poly_at(x as f64, z as f64));
                continue;
            }
            let solid = PUSHES
                .iter()
                .find(|(dx, dz)| !farm.is_void((x + dx) as f64, (z + dz) as f64));
            match solid {
                Some((dx, dz)) => {
                    tf.translation.x += dx;
                    tf.translation.z += dz;
                    if let Some(hm) = heightmap.as_deref() {
                        tf.translation.y = hm.sample(tf.translation.x, tf.translation.z);
                    }
                }
                None => {
                    // `Spring.DestroyUnit(u, true)`: self-destructed,
                    // so it goes out with its death explosion.
                    info!(
                        "Hex Farm: a {:?} fell into the void at ({x:.0}, {z:.0})",
                        kind.0
                    );
                    health.current = 0.0;
                }
            }
        }
        farm.game_frame(frame, &occupied);
    }

    let events = farm.drain_events();
    if events.is_empty() {
        return;
    }
    let mut touched_chunks: HashSet<(usize, usize)> = HashSet::new();
    let mut reshaped = false;
    for event in events {
        match event {
            HexFarmEvent::Reshaped(p) => {
                reshaped = true;
                let Some(hm) = heightmap.as_deref_mut() else {
                    continue;
                };
                let [x0, z0, x1, z1] = farm.write_poly_heights(p, hm.heights_mut());
                if x0 > x1 {
                    continue; // nothing inside the map
                }
                // Keep the precomputed square normals honest wherever a
                // corner moved (`CReadMap`'s TerrainChange recalc).
                hm.refresh_center_normals(x0, z0, x1, z1);
                let (hw, _) = hm.grid_size();
                // `RecalcArea` → `smoothGround.MapChanged`: the aircraft
                // mesh catches up over the next frames (and forgets the
                // gadget's flight profile there, as in Recoil).
                if let Some(sg) = smooth_ground.as_deref_mut() {
                    sg.map_changed(x0, z0, x1, z1);
                }
                if let Some(nav) = nav.as_deref_mut() {
                    let vertices = [x0 as u32, z0 as u32, x1 as u32, z1 as u32];
                    reshape_nav(nav, &farm.terrain, farm.type_w, hm.heights(), hw, vertices);
                }
                // Vertices on a chunk seam belong to both chunks.
                for cz in z0.saturating_sub(1) / CHUNK_SIZE..=z1 / CHUNK_SIZE {
                    for cx in x0.saturating_sub(1) / CHUNK_SIZE..=x1 / CHUNK_SIZE {
                        touched_chunks.insert((cx, cz));
                    }
                }
            }
            HexFarmEvent::VentAdded(k) => {
                let h = &farm.hexes[k];
                spawn_smoker_at(&mut commands, Vec3::new(h.x as f32, h.y as f32, h.z as f32));
            }
            HexFarmEvent::VentsRemoved(k) => {
                for (e, smoker) in &smokers {
                    if farm.poly_at(smoker.pos.x as f64, smoker.pos.z as f64) == Some(Poly::Hex(k))
                    {
                        commands.entity(e).despawn();
                    }
                }
            }
            HexFarmEvent::Moving {
                poly,
                first_frame,
                last_frame,
                direction,
                corner,
            } => {
                // `if firstframe>=lastframe then firstframe=lastframe-1`
                let first = first_frame.min(last_frame - 1.0);
                let v = view.poly_mut(poly);
                v.anim = Some(Anim {
                    first,
                    last: last_frame,
                    direction,
                    corner,
                });
                v.hidden = false;
                view.still_dirty = true;
            }
            HexFarmEvent::Percent(poly, value) => view.set_percent(poly, value, &mut commands),
        }
    }

    if let Some(hm) = heightmap.as_deref() {
        let (hw, hh) = hm.grid_size();
        for (e, coord, mesh) in &chunks {
            if touched_chunks.contains(&(coord.0, coord.1))
                && let Some(mut m) = meshes.get_mut(&mesh.0)
            {
                *m = build_chunk(hm.heights(), hw, hh, coord.0, coord.1).mesh;
                // Recomputed from the new mesh (ray-cast culling).
                commands.entity(e).remove::<Aabb>();
            }
        }
    }
    if reshaped {
        *minimap_pending = true;
    }
    *minimap_cooldown = (*minimap_cooldown - crate::sim::INV_GAME_SPEED).max(0.0);
    if *minimap_pending
        && *minimap_cooldown <= 0.0
        && let Some(mm) = minimap.as_deref_mut()
    {
        mm.set_base(&minimap_pixels(farm, MINIMAP_RES), MINIMAP_RES, MINIMAP_RES);
        *minimap_pending = false;
        *minimap_cooldown = MINIMAP_REPAINT_INTERVAL;
    }
}

impl HexFarmView {
    fn poly_mut(&mut self, p: Poly) -> &mut PolyView {
        match p {
            Poly::Hex(k) => &mut self.hexes[k],
            Poly::Rect(k) => &mut self.rects[k],
        }
    }

    fn set_percent(&mut self, p: Poly, value: Option<i32>, commands: &mut Commands) {
        match (value, self.percent.get_mut(&p)) {
            (Some(v), Some(entry)) => entry.0 = v,
            (Some(v), None) => {
                let label = commands
                    .spawn((
                        PercentLabel {
                            poly: p,
                            shown: None,
                        },
                        Text::new(""),
                        TextFont::from_font_size(16.0),
                        Node {
                            position_type: PositionType::Absolute,
                            ..default()
                        },
                        Visibility::Hidden,
                    ))
                    .id();
                self.percent.insert(p, (v, label));
            }
            (None, _) => {
                if let Some((_, label)) = self.percent.remove(&p) {
                    commands.entity(label).despawn();
                }
            }
        }
    }

    fn hex_draw(&self, k: usize) -> HexDraw<'_> {
        let h = &self.layout.hexes[k];
        HexDraw {
            corners: &h.corners,
            y: h.center[1],
            geo: h.g != 0,
            top_color: self.owners[k],
        }
    }

    fn rect_colors(&self, k: usize) -> (Rgba, Rgba) {
        let r = &self.layout.bridges[k];
        (
            self.owners[r.hex1 as usize - 1],
            self.owners[r.hex2 as usize - 1],
        )
    }
}

/// `TeamColors[hex.owner]` in team-coloured games, kept by the gadget's
/// `SendOwnerShipChangeToUnsynced` (l.1508) from its `UnitFinished` and
/// `UnitDestroyed` call-ins: a big (≥5×5) immobile unit finishing
/// within `TowerRadius` of a tower's centre (the first such tower)
/// makes its team the owner (and tags it [`TowerClaim`]); when one dies
/// the tower goes to the nearest remaining finished big building within
/// `TowerRadius`, or back to white.
///
/// Runs in `Update`, every rendered frame, so `RemovedComponents<Emerging>`
/// (cleared once per frame) loses nothing; deaths arrive through
/// [`TowerClaim`]'s hook.
#[allow(clippy::type_complexity)]
fn hex_farm_owners(
    mut view: ResMut<HexFarmView>,
    state: Res<HexFarmState>,
    registry: Res<UnitRegistry>,
    spawned_finished: Query<Entity, (Added<UnitType>, Without<Emerging>)>,
    mut finished_building: RemovedComponents<Emerging>,
    live: Query<(Entity, &Transform, &UnitType, &Faction), (Without<Emerging>, Without<Dying>)>,
    mut commands: Commands,
) {
    let finished: Vec<Entity> = spawned_finished
        .iter()
        .chain(finished_building.read())
        .collect();
    let released = std::mem::take(&mut view.released);
    if !view.team_colored || (finished.is_empty() && released.is_empty()) {
        return;
    }
    let farm = &state.farm;
    let r2 = (farm.tower_radius * farm.tower_radius) as f32;
    // `ud.canMove==false and ud.xsize>=5 and ud.zsize>=5`: `xsize` is in
    // heightmap squares, `FootprintX × SPRING_FOOTPRINT_SCALE (2)` — so
    // every 4×4 KP building counts, not just the homebases.
    let big = |kind: UnitKind| {
        registry.is_building(kind)
            && registry
                .footprint_elmos(kind)
                .cmpge(Vec2::splat(5.0 * SQUARE_SIZE))
                .all()
    };
    // `for n,h in ipairs(hex)`: the first tower whose centre is within
    // `TowerRadius`.
    let centre = |n: usize| Vec3::new(farm.hexes[n].x as f32, 0.0, farm.hexes[n].z as f32);
    let tower_of =
        |pos: Vec3| (0..farm.hexes.len()).position(|n| flat_dist_sq(pos, centre(n)) <= r2);
    let mut owners = view.owners.clone();

    // `UnitFinished`: the finisher's team owns the tower.
    for e in finished {
        let Ok((_, tf, kind, faction)) = live.get(e) else {
            continue; // died before finishing
        };
        if !big(kind.0) {
            continue;
        }
        if let Some(n) = tower_of(tf.translation) {
            owners[n] = faction.color().to_linear().to_f32_array();
            commands.entity(e).try_insert(TowerClaim(n));
        }
    }

    // `UnitDestroyed`: the nearest remaining finished big building.
    for (n, dead) in released {
        let mut best: Option<(f32, Rgba)> = None;
        for (e, otf, okind, faction) in &live {
            if e == dead || !big(okind.0) {
                continue;
            }
            let d2 = flat_dist_sq(otf.translation, centre(n));
            if d2 <= r2 && best.is_none_or(|(b, _)| d2 < b) {
                best = Some((d2, faction.color().to_linear().to_f32_array()));
            }
        }
        owners[n] = best.map_or(WHITE, |(_, c)| c);
    }

    if owners != view.owners {
        view.owners = owners;
        view.still_dirty = true;
    }
}

/// `DrawWorldPreUnit`: finish expired animations, redraw the moving
/// polygons, rebuild the still mesh when something changed.
fn hex_farm_draw(
    mut view: ResMut<HexFarmView>,
    state: Res<HexFarmState>,
    fixed: Res<Time<Fixed>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut visibility: Query<&mut Visibility>,
) {
    let view = &mut *view;
    // Between sim frames, interpolate (the gadget samples whole frames).
    let frame = state.frame as f64 + fixed.overstep_fraction() as f64;

    let mut moving = QuadBuffer::default();
    for k in 0..view.hexes.len() {
        let Some(anim) = view.hexes[k].anim else {
            continue;
        };
        push_hex(
            &mut moving,
            &view.hex_draw(k),
            view.side_hex,
            view.team_colored,
            Some(anim.progress(frame)),
        );
        if frame > anim.last {
            view.hexes[k].hidden = anim.direction < 0;
            view.hexes[k].anim = None;
            view.still_dirty |= anim.direction > 0;
        }
    }
    for k in 0..view.rects.len() {
        let Some(anim) = view.rects[k].anim else {
            continue;
        };
        let (c1, c2) = view.rect_colors(k);
        push_rect(
            &mut moving,
            &view.layout.bridges[k].corners,
            c1,
            c2,
            view.team_colored,
            Some((anim.progress(frame), anim.corner)),
        );
        if frame > anim.last {
            view.rects[k].hidden = anim.direction < 0;
            view.rects[k].anim = None;
            view.still_dirty |= anim.direction > 0;
        }
    }
    set_mesh(view.moving.clone(), moving, &mut meshes, &mut visibility);

    if view.still_dirty {
        view.still_dirty = false;
        let mut still = QuadBuffer::default();
        for k in 0..view.hexes.len() {
            let v = view.hexes[k];
            if !v.hidden && v.anim.is_none() {
                push_hex(
                    &mut still,
                    &view.hex_draw(k),
                    view.side_hex,
                    view.team_colored,
                    None,
                );
            }
        }
        for k in 0..view.rects.len() {
            let v = view.rects[k];
            if !v.hidden && v.anim.is_none() {
                let (c1, c2) = view.rect_colors(k);
                push_rect(
                    &mut still,
                    &view.layout.bridges[k].corners,
                    c1,
                    c2,
                    view.team_colored,
                    None,
                );
            }
        }
        set_mesh(view.still.clone(), still, &mut meshes, &mut visibility);
    }
}

fn set_mesh(
    (entity, handle): (Entity, Handle<Mesh>),
    buf: QuadBuffer,
    meshes: &mut Assets<Mesh>,
    visibility: &mut Query<&mut Visibility>,
) {
    let Ok(mut vis) = visibility.get_mut(entity) else {
        return;
    };
    if buf.is_empty() {
        vis.set_if_neq(Visibility::Hidden);
        return;
    }
    // Replaces the mesh, or inserts the first one behind the reserved
    // handle; a live handle always has a valid index generation.
    let _ = meshes.insert(&handle, buf.into_mesh());
    vis.set_if_neq(Visibility::Inherited);
}

/// `DrawScreenEffects`: each `%` indicator at its polygon's centre —
/// red→yellow while a polygon is being shot down, cyan→green while a
/// sunk one is being rebuilt.
fn hex_farm_labels(
    view: Res<HexFarmView>,
    camera: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut labels: Query<(
        &mut PercentLabel,
        &mut Text,
        &mut TextColor,
        &mut Node,
        &mut Visibility,
    )>,
) {
    let Ok((camera, cam_tf)) = camera.single() else {
        return;
    };
    for (mut label, mut text, mut color, mut node, mut vis) in &mut labels {
        let Some(&(value, _)) = view.percent.get(&label.poly) else {
            continue;
        };
        let (corners, hidden): (&[[f32; 3]], bool) = match label.poly {
            Poly::Hex(k) => (&view.layout.hexes[k].corners, view.hexes[k].hidden),
            Poly::Rect(k) => (&view.layout.bridges[k].corners, view.rects[k].hidden),
        };
        let center =
            corners.iter().map(|c| Vec3::from_array(*c)).sum::<Vec3>() / corners.len() as f32;
        let v = (value as f32 * 2.5 / 255.0).clamp(0.0, 1.0);
        color.set_if_neq(TextColor(if hidden {
            Color::srgb(0.0, 1.0, (253.0 / 255.0 - v).max(0.0))
        } else {
            Color::srgb(1.0, v, 0.0)
        }));
        if label.shown != Some(value) {
            label.shown = Some(value);
            text.0 = format!("{value}%");
        }
        match camera.world_to_viewport(cam_tf, center) {
            Ok(pos) => {
                let (left, top) = (Val::Px(pos.x), Val::Px(pos.y));
                if node.left != left || node.top != top {
                    node.left = left;
                    node.top = top;
                }
                vis.set_if_neq(Visibility::Inherited);
            }
            Err(_) => {
                vis.set_if_neq(Visibility::Hidden);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spring_map::hexfarm::{HexFarmSetup, TERRAIN_TOWER};

    fn farm() -> HexFarm {
        HexFarm::generate(
            5,
            HexFarmSetup {
                map_size_x: 12288.0,
                map_size_z: 12288.0,
                teams: 2,
                median_health: 2000.0,
                median_build_time: 1600.0,
            },
        )
    }

    /// `SendOwnerShipChangeToUnsynced` from `UnitFinished` /
    /// `UnitDestroyed`, event by event.
    #[test]
    fn tower_ownership_follows_finish_and_death_events() {
        let farm = farm();
        let layout = farm.layout();
        let n = layout.hexes.len();
        let (hx, hz) = (farm.hexes[0].x as f32, farm.hexes[0].z as f32);
        let mut world = World::new();
        world.insert_resource(HexFarmView {
            hexes: vec![
                PolyView {
                    hidden: false,
                    anim: None
                };
                n
            ],
            rects: vec![],
            owners: vec![WHITE; n],
            layout,
            side_hex: 1.0,
            team_colored: true,
            still: (Entity::PLACEHOLDER, Handle::default()),
            moving: (Entity::PLACEHOLDER, Handle::default()),
            still_dirty: false,
            percent: HashMap::new(),
            released: Vec::new(),
        });
        world.insert_resource(HexFarmState { farm, frame: 0 });
        let mut defs = spring_tdf::UnitDefs::default();
        for (name, fp) in [("kernel", 8.0), ("socket", 4.0), ("badblock", 2.0)] {
            defs.units.insert(
                name.into(),
                spring_tdf::UnitDef {
                    id: name.into(),
                    footprint_x: fp,
                    footprint_z: fp,
                    ..Default::default()
                },
            );
        }
        world.insert_resource(UnitRegistry::for_test(defs));
        let sys = world.register_system(hex_farm_owners);
        let owner = |w: &World| w.resource::<HexFarmView>().owners[0];
        let color = |f: Faction| f.color().to_linear().to_f32_array();

        let at = |dx: f32| Transform::from_xyz(hx + dx, 0.0, hz);
        let kernel = world
            .spawn((UnitType(UnitKind::Kernel), Faction::System, at(40.0)))
            .id();
        // Too small to claim (FootprintX 2: xsize 4).
        world.spawn((UnitType(UnitKind::BadBlock), Faction::Hacker, at(0.0)));
        world.run_system(sys).unwrap();
        assert_eq!(owner(&world), color(Faction::System));

        // A 4×4 building (xsize 8 ≥ 5) claims it only once finished.
        let socket = world
            .spawn((
                UnitType(UnitKind::Socket),
                Faction::Network,
                at(-10.0),
                Emerging {
                    target_y: 0.0,
                    remaining: 1.0,
                    total: 1.0,
                    rally_point: None,
                    rally_then: None,
                    style: crate::units::lifecycle::spawning::EmergeStyle::Fade,
                },
            ))
            .id();
        world.run_system(sys).unwrap();
        assert_eq!(owner(&world), color(Faction::System));
        world.entity_mut(socket).remove::<Emerging>();
        world.run_system(sys).unwrap();
        assert_eq!(owner(&world), color(Faction::Network));

        // Its death hands the tower to the nearest remaining one...
        world.entity_mut(socket).insert(Dying { timer: 1.0 });
        world.run_system(sys).unwrap();
        assert_eq!(owner(&world), color(Faction::System));

        // Why: deaths were read as `Added<Dying>` in `Update`, but a
        // building without death choreography gets `Dying` and is
        // despawned in the same sim tick — the tower never reverted.
        let finished_socket = |world: &mut World| {
            let e = world
                .spawn((UnitType(UnitKind::Socket), Faction::Network, at(-10.0)))
                .id();
            world.run_system(sys).unwrap();
            assert_eq!(owner(world), color(Faction::Network));
            e
        };
        let killed = finished_socket(&mut world);
        let mut commands = world.commands();
        commands.entity(killed).insert(Dying { timer: 0.0 });
        commands.entity(killed).despawn();
        world.flush();
        world.run_system(sys).unwrap();
        assert_eq!(owner(&world), color(Faction::System), "killed + despawned");
        // A despawn without the death pipeline is a `UnitDestroyed` too.
        let removed = finished_socket(&mut world);
        world.entity_mut(removed).despawn();
        world.run_system(sys).unwrap();
        assert_eq!(owner(&world), color(Faction::System), "despawned");

        // ...and with none left it goes back to white.
        world.entity_mut(kernel).insert(Dying { timer: 1.0 });
        world.run_system(sys).unwrap();
        assert_eq!(owner(&world), WHITE);
    }

    /// A reshape re-masks void over every cell the slope pass rewrote:
    /// `update_region` widens the vertex box to whole 2×2 slope blocks,
    /// and a cell left out of the mask came back open over the void.
    #[test]
    fn reshape_masks_void_over_every_rewritten_cell() {
        let n = 16u32;
        let mut nav = NavGridSet::default();
        nav.buckets.push(crate::interaction::movement::NavBucket {
            max_slope: 1.0,
            speed_map: spring_pathfinding::SpeedMap::uniform(n, n, 0.0),
        });
        let heights = vec![0.0; ((n + 1) * (n + 1)) as usize];
        let terrain = vec![spring_map::hexfarm::TERRAIN_VOID; (n / 2 * n / 2) as usize];
        // Even vertex box: the rewritten cells reach one further out on
        // the low side (x0 - 2) and the high side (x1 + 1).
        reshape_nav(
            &mut nav,
            &terrain,
            (n / 2) as usize,
            &heights,
            (n + 1) as usize,
            [4, 4, 6, 6],
        );
        let map = &nav.buckets[0].speed_map;
        assert!(map.speeds.iter().all(|&s| s == 0.0), "{:?}", map.speeds);
        // The re-check covers every rewritten cell (2..=7), padded.
        let changed = nav.changed_since(0).expect("bumped");
        assert!(changed[0] <= 2 && changed[1] <= 2 && changed[2] >= 7 && changed[3] >= 7);
    }

    /// Every nav cell over a void square is blocked, even in a bucket
    /// whose slope cap accepts the spike bed; cells over towers aren't.
    #[test]
    fn void_is_impassable_for_every_slope_cap() {
        let farm = HexFarm::generate(
            5,
            HexFarmSetup {
                map_size_x: 12288.0,
                map_size_z: 12288.0,
                teams: 2,
                median_health: 2000.0,
                median_build_time: 1600.0,
            },
        );
        let n = 12288 / 8;
        let mut map = spring_pathfinding::SpeedMap::uniform(n, n, 1.0);
        mask_void(&farm.terrain, farm.type_w, &mut map, [0, 0, n - 1, n - 1]);
        for z in (0..n).step_by(7) {
            for x in (0..n).step_by(7) {
                let t = farm.terrain[(z / 2) as usize * farm.type_w + (x / 2) as usize];
                assert_eq!(map.get(x, z) > 0.0, t != spring_map::hexfarm::TERRAIN_VOID);
            }
        }
        let [sx, sz] = farm.start_positions[0];
        assert_eq!(
            farm.terrain[(sz / 16.0) as usize * farm.type_w + (sx / 16.0) as usize],
            TERRAIN_TOWER
        );
        assert!(map.get((sx / 8.0) as u32, (sz / 8.0) as u32) > 0.0);
    }

    /// Why: the overlay meshes were created as empty mesh assets, and
    /// the renderer warns about every vertex-less mesh it extracts —
    /// once per Hex Farm load, and the moving overlay stays empty until
    /// a polygon animates. The handle is now only reserved; an empty
    /// buffer hides the entity without creating an asset, and the first
    /// real buffer is inserted behind the reserved handle.
    #[test]
    fn overlay_mesh_exists_only_once_it_has_quads() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        let handle = world.resource::<Assets<Mesh>>().reserve_handle();
        let entity = world.spawn(Visibility::Hidden).id();
        let write = |world: &mut World, buf: QuadBuffer| {
            let slot = (entity, handle.clone());
            let mut buf = Some(buf);
            world
                .run_system_once(
                    move |mut meshes: ResMut<Assets<Mesh>>, mut vis: Query<&mut Visibility>| {
                        let buf = buf.take().expect("the system runs once");
                        set_mesh(slot.clone(), buf, &mut meshes, &mut vis);
                    },
                )
                .unwrap();
        };

        write(&mut world, QuadBuffer::default());
        assert!(world.resource::<Assets<Mesh>>().get(&handle).is_none());
        assert_eq!(world.get::<Visibility>(entity), Some(&Visibility::Hidden));

        let mut buf = QuadBuffer::default();
        let corners = [
            [0.0, 10.0, 0.0],
            [16.0, 10.0, 0.0],
            [16.0, 10.0, 48.0],
            [0.0, 10.0, 48.0],
        ];
        push_rect(&mut buf, &corners, WHITE, WHITE, false, None);
        assert!(!buf.is_empty());
        write(&mut world, buf);
        let mesh = world.resource::<Assets<Mesh>>().get(&handle);
        assert!(mesh.is_some_and(|m| m.count_vertices() > 0));
        assert_eq!(
            world.get::<Visibility>(entity),
            Some(&Visibility::Inherited)
        );
    }
}
