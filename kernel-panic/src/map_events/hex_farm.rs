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
//!   (invisible, ray-cast) terrain mesh and the minimap for just the
//!   touched area — and add/remove datavents.
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
use bevy::prelude::*;

use spring_map::hexfarm::{HexFarm, HexFarmEvent, Poly};
use spring_map::lua_layout::HexFarmLayout;

use crate::interaction::movement::NavGridSet;
use crate::map_loading::TerrainChunkCoord;
use crate::map_loading::lua_compositing::{
    HexDraw, QuadBuffer, Rgba, WHITE, atlas_material, minimap_pixels, push_hex, push_rect,
    team_colored_atlas, upload_atlas,
};
use crate::rendering::camera::RtsCamera;
use crate::terrain::geovent::{GeoventSmoker, spawn_smoker_at};
use crate::terrain::heightmap::Heightmap;
use crate::terrain::mesh::{CHUNK_SIZE, build_chunk};
use crate::ui::minimap::MinimapState;
use crate::units::combat::Dying;
use crate::units::components::{Faction, Health, UnitType};
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::spawning::Emerging;

/// Resolution of the footprint image the minimap is painted from.
pub const MINIMAP_RES: usize = 400;

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
    owner_timer: Timer,
}

/// A `%` indicator label.
#[derive(Component)]
struct PercentLabel(Poly);

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
/// (or clear a previous Hex Farm's when `farm` is `None`).
pub fn install(
    farm: Option<HexFarm>,
    atlas: Option<&spring_map::lua_skin::SkinAtlas>,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    let (Some(farm), Some(atlas)) = (farm, atlas) else {
        commands.remove_resource::<HexFarmState>();
        commands.remove_resource::<HexFarmInbox>();
        commands.remove_resource::<HexFarmView>();
        return;
    };
    let layout = farm.layout();
    let team_colored = farm.team_colored;
    let atlas = if team_colored {
        upload_atlas(&team_colored_atlas(atlas), images)
    } else {
        upload_atlas(atlas, images)
    };
    let material = atlas_material(atlas, materials);
    let first = &layout.hexes[0].corners;
    let side_hex = ((first[1][0] - first[0][0]).powi(2) + (first[1][2] - first[0][2]).powi(2)).sqrt();
    // The meshes are rebuilt in place, so their bounds would go stale:
    // opt out of culling (the gadget draws them unculled too).
    let mut spawn_mesh = |commands: &mut Commands| {
        let handle = meshes.add(QuadBuffer::default().into_mesh());
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
        owner_timer: Timer::from_seconds(0.5, TimerMode::Repeating),
    });
    commands.insert_resource(HexFarmState { farm, frame: 0 });
    commands.insert_resource(HexFarmInbox::default());
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
    mut minimap: Option<ResMut<MinimapState>>,
    mut units: Query<(&mut Transform, &UnitType, &mut Health), Without<Dying>>,
    registry: Res<UnitRegistry>,
    smokers: Query<(Entity, &GeoventSmoker)>,
    chunks: Query<(Entity, &TerrainChunkCoord, &Mesh3d)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
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
                    info!("Hex Farm: a {:?} fell into the void at ({x:.0}, {z:.0})", kind.0);
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
                let (hw, _) = hm.grid_size();
                if let Some(nav) = nav.as_deref_mut() {
                    for bucket in &mut nav.buckets {
                        bucket.speed_map.update_region(
                            hm.heights(),
                            hw as u32,
                            bucket.max_slope,
                            spring_pathfinding::slope_mod_from_max_slope(bucket.max_slope),
                            x0 as u32,
                            z0 as u32,
                            x1 as u32,
                            z1 as u32,
                        );
                    }
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
                    if farm.poly_at(smoker.pos.x as f64, smoker.pos.z as f64) == Some(Poly::Hex(k)) {
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
                && let Some(m) = meshes.get_mut(&mesh.0)
            {
                *m = build_chunk(hm.heights(), hw, hh, coord.0, coord.1).mesh;
                // Recomputed from the new mesh (ray-cast culling).
                commands.entity(e).remove::<Aabb>();
            }
        }
    }
    if reshaped && let Some(mm) = minimap.as_deref_mut() {
        mm.set_base(&minimap_pixels(farm, MINIMAP_RES), MINIMAP_RES, MINIMAP_RES);
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
                        PercentLabel(p),
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

/// `TeamColors[hex.owner]` in team-coloured games. The gadget sets a
/// tower's owner when a big (≥5×5) immobile unit finishes on it, and on
/// its death hands it to the nearest remaining one; this polls the
/// same thing: the nearest finished big building within `TowerRadius`
/// of the tower's centre, coloured by its faction.
fn hex_farm_owners(
    time: Res<Time>,
    mut view: ResMut<HexFarmView>,
    state: Res<HexFarmState>,
    registry: Res<UnitRegistry>,
    buildings: Query<(&Transform, &UnitType, &Faction), (Without<Emerging>, Without<Dying>)>,
) {
    if !view.team_colored || !view.owner_timer.tick(time.delta()).just_finished() {
        return;
    }
    let farm = &state.farm;
    let r2 = (farm.tower_radius * farm.tower_radius) as f32;
    let mut owners = vec![WHITE; farm.hexes.len()];
    let mut best = vec![f32::INFINITY; farm.hexes.len()];
    for (tf, kind, faction) in &buildings {
        let fp = registry.footprint_elmos(kind.0);
        if !registry.is_building(kind.0) || fp.x < 40.0 || fp.y < 40.0 {
            continue;
        }
        let Some(Poly::Hex(k)) = farm.poly_at(tf.translation.x as f64, tf.translation.z as f64)
        else {
            continue;
        };
        let h = &farm.hexes[k];
        let d2 = (tf.translation.x - h.x as f32).powi(2) + (tf.translation.z - h.z as f32).powi(2);
        if d2 <= r2 && d2 < best[k] {
            best[k] = d2;
            owners[k] = faction.color().to_linear().to_f32_array();
        }
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
                push_hex(&mut still, &view.hex_draw(k), view.side_hex, view.team_colored, None);
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
    if let Some(mesh) = meshes.get_mut(&handle) {
        *mesh = buf.into_mesh();
    }
    vis.set_if_neq(Visibility::Inherited);
}

/// `DrawScreenEffects`: each `%` indicator at its polygon's centre —
/// red→yellow while a polygon is being shot down, cyan→green while a
/// sunk one is being rebuilt.
fn hex_farm_labels(
    view: Res<HexFarmView>,
    camera: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut labels: Query<(&PercentLabel, &mut Text, &mut TextColor, &mut Node, &mut Visibility)>,
) {
    let Ok((camera, cam_tf)) = camera.single() else {
        return;
    };
    for (label, mut text, mut color, mut node, mut vis) in &mut labels {
        let Some(&(value, _)) = view.percent.get(&label.0) else {
            continue;
        };
        let corners: Vec<[f32; 3]> = match label.0 {
            Poly::Hex(k) => view.layout.hexes[k].corners.to_vec(),
            Poly::Rect(k) => view.layout.bridges[k].corners.to_vec(),
        };
        let center = corners.iter().fold(Vec3::ZERO, |a, c| a + Vec3::from_array(*c))
            / corners.len() as f32;
        let hidden = match label.0 {
            Poly::Hex(k) => view.hexes[k].hidden,
            Poly::Rect(k) => view.rects[k].hidden,
        };
        let v = (value as f32 * 2.5 / 255.0).clamp(0.0, 1.0);
        color.0 = if hidden {
            Color::srgb(0.0, 1.0, (253.0 / 255.0 - v).max(0.0))
        } else {
            Color::srgb(1.0, v, 0.0)
        };
        let text_now = format!("{value}%");
        if text.0 != text_now {
            text.0 = text_now;
        }
        match camera.world_to_viewport(cam_tf, center) {
            Ok(pos) => {
                node.left = Val::Px(pos.x);
                node.top = Val::Px(pos.y);
                vis.set_if_neq(Visibility::Inherited);
            }
            Err(_) => {
                vis.set_if_neq(Visibility::Hidden);
            }
        }
    }
}
