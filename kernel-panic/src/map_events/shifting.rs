//! Shifting terrain on the port's own maps (Token Ring's firewalls,
//! Heap Fragmentation's allocator): blocks of ground that rise and sink
//! on a schedule fixed by the sim frame ([`spring_map::shifting`]).
//!
//! Every sim frame [`shifting_sim`] writes each block's height for the
//! frame into the [`Heightmap`] and carries the change the way Hex
//! Farm's reshapes are carried (`hex_farm`): the square normals, the nav
//! grids (a raised block's walls are too steep for every mover), the
//! aircraft [`SmoothGround`] and the terrain mesh chunks. While a block
//! is up, ground units on it are pushed off its nearer end and
//! structures on it are destroyed (the ground came up under them).
//!
//! Inert unless the map loader inserted [`ShiftingTerrain`].

use std::collections::HashSet;

use bevy::camera::primitives::Aabb;
use bevy::prelude::*;
use spring_map::shifting::Shifting;

use crate::interaction::movement::NavGridSet;
use crate::map_loading::TerrainChunkCoord;
use crate::terrain::heightmap::Heightmap;
use crate::terrain::mesh::{CHUNK_SIZE, build_chunk};
use crate::terrain::smooth_ground::SmoothGround;
use crate::units::combat::Dying;
use crate::units::components::{Health, UnitType};
use crate::units::content::unit_registry::UnitRegistry;

/// Block heights move in steps of `1 / LEVEL_STEPS` of their rise, so a
/// three-second rise rewrites the ground every few frames, not every one.
const LEVEL_STEPS: f32 = 15.0;

/// Frames between sweeps of the units on raised blocks.
const SWEEP_INTERVAL: u32 = 10;

/// How far past a block's end a pushed unit lands (elmos).
const PUSH_MARGIN: f32 = 24.0;

/// The loaded map's shifting terrain and how far it has moved.
#[derive(Resource)]
pub struct ShiftingTerrain {
    rules: Shifting,
    /// The map's own heights, which the blocks rise from.
    ground: Vec<f32>,
    /// Sim frames since the map loaded.
    frame: u32,
    /// Each block's level as last written (0 down, 1 up).
    written: Vec<f32>,
}

impl ShiftingTerrain {
    /// The map's shifting terrain, if `map_name` has any.
    pub fn for_map(map_name: &str, heightmap: &Heightmap) -> Option<Self> {
        let rules = Shifting::for_map(map_name)?;
        let written = vec![0.0; rules.blocks.len()];
        Some(Self {
            rules,
            ground: heightmap.heights().to_vec(),
            frame: 0,
            written,
        })
    }
}

pub struct ShiftingPlugin;

impl Plugin for ShiftingPlugin {
    fn build(&self, app: &mut App) {
        use crate::units::GameplaySet;
        app.add_systems(
            FixedUpdate,
            shifting_sim
                .run_if(resource_exists::<ShiftingTerrain>)
                .in_set(GameplaySet::Cleanup),
        );
    }
}

/// One sim frame: move the blocks, then sweep the raised ones.
#[allow(clippy::too_many_arguments)]
fn shifting_sim(
    mut state: ResMut<ShiftingTerrain>,
    mut heightmap: Option<ResMut<Heightmap>>,
    mut nav: Option<ResMut<NavGridSet>>,
    mut smooth_ground: Option<ResMut<SmoothGround>>,
    mut units: Query<(&mut Transform, &UnitType, &mut Health), Without<Dying>>,
    registry: Res<UnitRegistry>,
    chunks: Query<(Entity, &TerrainChunkCoord, &Mesh3d)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    let state = &mut *state;
    state.frame += 1;
    let frame = state.frame;
    let Some(hm) = heightmap.as_deref_mut() else {
        return;
    };

    let mut touched_chunks: HashSet<(usize, usize)> = HashSet::new();
    for k in 0..state.rules.blocks.len() {
        let level = (state.rules.level(frame, k) * LEVEL_STEPS).round() / LEVEL_STEPS;
        if level == state.written[k] {
            continue;
        }
        state.written[k] = level;
        let (hw, _) = hm.grid_size();
        let [x0, z0, x1, z1] =
            state
                .rules
                .write_block(k, level, &state.ground, hm.heights_mut(), hw);
        // The block's wall is the ring of squares round the moved
        // vertices: widen the box by one so their normals and slopes
        // are recomputed too.
        let (x0, z0, x1, z1) = (x0 - 1, z0 - 1, x1 + 1, z1 + 1);
        hm.refresh_center_normals(x0, z0, x1, z1);
        if let Some(sg) = smooth_ground.as_deref_mut() {
            sg.map_changed(x0, z0, x1, z1);
        }
        if let Some(nav) = nav.as_deref_mut()
            && let Some(cells) = nav.heights_changed(
                hm.heights(),
                hw as u32,
                [x0 as u32, z0 as u32, x1 as u32, z1 as u32],
            )
        {
            let h = nav.structures.max_xsizeh() + 1;
            let [cx0, cz0, cx1, cz1] = cells.map(|c| c as i32);
            nav.bump([cx0 - h, cz0 - h, cx1 + h, cz1 + h]);
        }
        // Vertices on a chunk seam belong to both chunks.
        for cz in z0.saturating_sub(1) / CHUNK_SIZE..=z1 / CHUNK_SIZE {
            for cx in x0.saturating_sub(1) / CHUNK_SIZE..=x1 / CHUNK_SIZE {
                touched_chunks.insert((cx, cz));
            }
        }
    }
    if !touched_chunks.is_empty() {
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

    if !frame.is_multiple_of(SWEEP_INTERVAL) {
        return;
    }
    for (mut tf, kind, mut health) in &mut units {
        if registry.can_fly(kind.0) {
            continue;
        }
        let (x, z) = (tf.translation.x, tf.translation.z);
        let Some(k) = state.rules.raised_block_at(frame, x, z) else {
            continue;
        };
        if registry.is_building(kind.0) {
            info!(
                "shifting terrain: a {:?} was crushed at ({x:.0}, {z:.0})",
                kind.0
            );
            health.current = 0.0;
            continue;
        }
        let (nx, nz) = state.rules.push_out(k, x, z, PUSH_MARGIN);
        tf.translation.x = nx;
        tf.translation.z = nz;
        tf.translation.y = hm.sample(nx, nz);
    }
}
