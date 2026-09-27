//! One dynamic quad mesh per material for the camera-facing ribbon
//! effects: hit-scan beams, laser bolts and their end caps, lightning
//! arcs, CEG `explspike` streaks and smoke trails.
//!
//! Why: each of those used to own a private `Mesh` asset that the sim
//! tick rewrote every frame and that died with the effect a few frames
//! later. The renderer paid for every one of them in
//! `allocate_and_free_meshes` + extraction, plus one draw call per
//! quad — and build lasers made it permanent background churn (one
//! 0.08 s beam per emitter piece per sim frame while a factory
//! produces, ~120 mesh adds and frees per second for a Socket alone).
//!
//! Now the tick systems push each effect's corners into
//! [`FxQuadBatches`] under the material it draws with, and
//! [`flush_fx_quad_batches`] (last in the fx chain) writes every
//! batch's buffers into its single mesh: one `Assets<Mesh>` write and
//! one draw call per material per sim tick, no asset churn. The batch
//! entities sit at the origin with world-space vertices, exactly like
//! the per-effect meshes did, so nothing is interpolated between sim
//! ticks (`SimPose`) — same as before.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;

/// Per-quad vertex order is `[bl, br, tr, tl]`:
///
/// ```text
/// tl(3) --- tr(2)       (UV 0,1)    (UV 1,1)
///   |    \    |           .         .
///   |     \   |           .         .
/// bl(0) --- br(1)       (UV 0,0)    (UV 1,0)
/// ```
///
/// With these default UVs texture U runs bl→br (the ribbon's long
/// axis) and V bl→tl (its thickness), so one span of `arrow.tga`
/// stretches once along a bolt — upstream `CLaserProjectile::Draw`
/// assigns `tex1->xstart..xend` to lead..tail the same way.
const QUAD_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

/// Both triangle windings per quad, so a ribbon reads from either side
/// under the materials' default back-face culling.
const QUAD_INDICES: [u32; 12] = [0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2];

/// The quads every ribbon effect pushed this sim tick, grouped by the
/// material they render with. Filled by the tick systems, drained by
/// [`flush_fx_quad_batches`].
#[derive(Resource, Default)]
pub(super) struct FxQuadBatches {
    batches: HashMap<AssetId<StandardMaterial>, QuadBatch>,
}

/// Marker on a batch's render entity.
#[derive(Component)]
pub(super) struct FxQuadBatch;

struct QuadBatch {
    material: Handle<StandardMaterial>,
    /// The render entity and its mesh, created by the first flush that
    /// has content for this material. The entity is respawned if a
    /// game teardown removed it; the mesh handle lives as long as the
    /// resource.
    entity: Option<Entity>,
    mesh: Option<Handle<Mesh>>,
    /// Whether the entity is currently shown — flipped only on the
    /// empty ↔ non-empty edges so idle batches cost nothing.
    visible: bool,
    /// This tick's vertices; capacity is kept across ticks.
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
}

impl FxQuadBatches {
    /// Queue one quad (`[bl, br, tr, tl]` world corners, see
    /// [`QUAD_UVS`]) with per-vertex UVs and colours.
    pub(super) fn push_quad(
        &mut self,
        material: &Handle<StandardMaterial>,
        corners: [Vec3; 4],
        uvs: [[f32; 2]; 4],
        colors: [[f32; 4]; 4],
    ) {
        let batch = self
            .batches
            .entry(material.id())
            .or_insert_with(|| QuadBatch {
                material: material.clone(),
                entity: None,
                mesh: None,
                visible: false,
                positions: Vec::new(),
                uvs: Vec::new(),
                colors: Vec::new(),
            });
        batch
            .positions
            .extend(corners.iter().map(|c| c.to_array()));
        batch.uvs.extend_from_slice(&uvs);
        batch.colors.extend_from_slice(&colors);
    }

    /// [`push_quad`](Self::push_quad) with the default UVs and one
    /// colour for all four corners — beams, spikes, caps, arcs.
    pub(super) fn push_flat_quad(
        &mut self,
        material: &Handle<StandardMaterial>,
        corners: [Vec3; 4],
        color: [f32; 4],
    ) {
        self.push_quad(material, corners, QUAD_UVS, [color; 4]);
    }

    /// Quads queued for `material` since the last flush.
    #[cfg(test)]
    pub(super) fn pending_quads(&self, material: &Handle<StandardMaterial>) -> usize {
        self.batches
            .get(&material.id())
            .map_or(0, |b| b.positions.len() / 4)
    }
}

/// Write every batch's quads into its mesh and clear the buffers. Runs
/// once per sim tick at the end of the `WeaponFxPlugin` chain, after
/// every system that pushes quads.
pub(super) fn flush_fx_quad_batches(
    mut batches: ResMut<FxQuadBatches>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut visibility: Query<&mut Visibility, With<FxQuadBatch>>,
    mut commands: Commands,
) {
    for batch in batches.batches.values_mut() {
        if batch.positions.is_empty() {
            // Nothing to draw: hide the entity rather than upload an
            // empty mesh; the mesh keeps its last content untouched.
            if batch.visible
                && let Some(entity) = batch.entity
                && let Ok(mut vis) = visibility.get_mut(entity)
            {
                *vis = Visibility::Hidden;
                batch.visible = false;
            }
            continue;
        }

        let mesh = match &batch.mesh {
            Some(handle) => {
                if let Some(mesh) = meshes.get_mut(handle) {
                    write_batch_mesh(mesh, batch);
                }
                handle.clone()
            }
            None => {
                let mut mesh = Mesh::new(
                    PrimitiveTopology::TriangleList,
                    RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
                );
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new());
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, Vec::<[f32; 3]>::new());
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, Vec::<[f32; 2]>::new());
                mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, Vec::<[f32; 4]>::new());
                mesh.insert_indices(Indices::U32(Vec::new()));
                write_batch_mesh(&mut mesh, batch);
                let handle = meshes.add(mesh);
                batch.mesh = Some(handle.clone());
                handle
            }
        };

        // A stale entity (despawned by the game-world teardown) reads
        // as missing here — commands from the previous flush have long
        // been applied — so it is simply spawned again.
        let alive = batch
            .entity
            .is_some_and(|entity| visibility.get_mut(entity).is_ok());
        if !alive {
            let entity = commands
                .spawn((
                    FxQuadBatch,
                    Mesh3d(mesh),
                    MeshMaterial3d(batch.material.clone()),
                    // Vertices are world-space and the mesh's Aabb is
                    // never recomputed, so culling must be off.
                    Transform::IDENTITY,
                    NoFrustumCulling,
                ))
                .id();
            batch.entity = Some(entity);
            batch.visible = true;
        } else if !batch.visible
            && let Some(entity) = batch.entity
            && let Ok(mut vis) = visibility.get_mut(entity)
        {
            *vis = Visibility::Inherited;
            batch.visible = true;
        }

        batch.positions.clear();
        batch.uvs.clear();
        batch.colors.clear();
    }
}

/// Copy the batch buffers into the mesh's attribute vectors in place
/// (their capacity is kept) and size the index list to the quad count.
fn write_batch_mesh(mesh: &mut Mesh, batch: &QuadBatch) {
    let verts = batch.positions.len();
    for (attribute, values) in mesh.attributes_mut() {
        match values {
            VertexAttributeValues::Float32x3(v) if attribute.id == Mesh::ATTRIBUTE_POSITION.id => {
                v.clear();
                v.extend_from_slice(&batch.positions);
            }
            VertexAttributeValues::Float32x3(v) if attribute.id == Mesh::ATTRIBUTE_NORMAL.id => {
                // Unlit materials never read the normal; it only has to
                // exist for the pipeline's vertex layout.
                v.resize(verts, [0.0, 1.0, 0.0]);
            }
            VertexAttributeValues::Float32x2(v) if attribute.id == Mesh::ATTRIBUTE_UV_0.id => {
                v.clear();
                v.extend_from_slice(&batch.uvs);
            }
            VertexAttributeValues::Float32x4(v) if attribute.id == Mesh::ATTRIBUTE_COLOR.id => {
                v.clear();
                v.extend_from_slice(&batch.colors);
            }
            _ => {}
        }
    }
    if let Some(Indices::U32(indices)) = mesh.indices_mut() {
        let quads = verts / 4;
        let have = indices.len() / QUAD_INDICES.len();
        if have > quads {
            indices.truncate(quads * QUAD_INDICES.len());
        } else {
            for quad in have..quads {
                let base = (quad * 4) as u32;
                indices.extend(QUAD_INDICES.iter().map(|i| base + i));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn batch_app() -> App {
        let mut app = App::new();
        app.init_resource::<FxQuadBatches>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>();
        app
    }

    fn push(app: &mut App, material: &Handle<StandardMaterial>, n: usize) {
        let mut batches = app.world_mut().resource_mut::<FxQuadBatches>();
        for i in 0..n {
            let x = i as f32;
            batches.push_flat_quad(
                material,
                [
                    Vec3::new(x, 0.0, 0.0),
                    Vec3::new(x + 1.0, 0.0, 0.0),
                    Vec3::new(x + 1.0, 1.0, 0.0),
                    Vec3::new(x, 1.0, 0.0),
                ],
                [1.0; 4],
            );
        }
    }

    fn flush(app: &mut App) {
        app.world_mut()
            .run_system_once(flush_fx_quad_batches)
            .unwrap();
    }

    fn batch_entities(app: &mut App) -> Vec<(Entity, Visibility, Handle<Mesh>)> {
        let world = app.world_mut();
        let mut q = world.query_filtered::<(Entity, &Visibility, &Mesh3d), With<FxQuadBatch>>();
        q.iter(world)
            .map(|(e, v, m)| (e, *v, m.0.clone()))
            .collect()
    }

    /// One mesh + one entity per material, however many quads and
    /// ticks; the mesh is resized in place, never replaced.
    #[test]
    fn one_mesh_per_material_reused_across_ticks() {
        let mut app = batch_app();
        let mat_a = Handle::<StandardMaterial>::default();
        let mat_b = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());

        push(&mut app, &mat_a, 3);
        push(&mut app, &mat_b, 1);
        flush(&mut app);
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 2);
        let entities = batch_entities(&mut app);
        assert_eq!(entities.len(), 2);
        let mesh_a = entities
            .iter()
            .map(|(_, _, m)| m.clone())
            .find(|m| {
                app.world()
                    .resource::<Assets<Mesh>>()
                    .get(m)
                    .unwrap()
                    .count_vertices()
                    == 12
            })
            .expect("a 3-quad mesh");
        assert_eq!(
            app.world()
                .resource::<FxQuadBatches>()
                .pending_quads(&mat_a),
            0,
            "flush drains the buffers"
        );

        // Fewer quads next tick: same mesh, shrunk.
        push(&mut app, &mat_a, 1);
        flush(&mut app);
        let meshes = app.world().resource::<Assets<Mesh>>();
        assert_eq!(meshes.len(), 2);
        let mesh = meshes.get(&mesh_a).unwrap();
        assert_eq!(mesh.count_vertices(), 4);
        match mesh.indices() {
            Some(Indices::U32(i)) => assert_eq!(i.as_slice(), &QUAD_INDICES),
            other => panic!("unexpected indices {other:?}"),
        }
        assert_eq!(batch_entities(&mut app).len(), 2);
    }

    /// An empty batch hides its entity instead of uploading an empty
    /// mesh, and shows it again once it has quads.
    #[test]
    fn empty_batch_is_hidden_not_rebuilt() {
        let mut app = batch_app();
        let mat = Handle::<StandardMaterial>::default();
        push(&mut app, &mat, 2);
        flush(&mut app);
        let (entity, vis, _) = batch_entities(&mut app)[0].clone();
        assert_eq!(vis, Visibility::Inherited);

        flush(&mut app);
        assert_eq!(*app.world().get::<Visibility>(entity).unwrap(), Visibility::Hidden);
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 1);

        push(&mut app, &mat, 1);
        flush(&mut app);
        assert_eq!(*app.world().get::<Visibility>(entity).unwrap(), Visibility::Inherited);
        assert_eq!(batch_entities(&mut app).len(), 1, "no second entity");
    }

    /// The game-world teardown despawns the batch entity; the next
    /// flush with content spawns a new one on the same mesh.
    #[test]
    fn despawned_batch_entity_is_respawned() {
        let mut app = batch_app();
        let mat = Handle::<StandardMaterial>::default();
        push(&mut app, &mat, 1);
        flush(&mut app);
        let (entity, _, mesh) = batch_entities(&mut app)[0].clone();
        app.world_mut().despawn(entity);

        push(&mut app, &mat, 1);
        flush(&mut app);
        let entities = batch_entities(&mut app);
        assert_eq!(entities.len(), 1);
        assert_ne!(entities[0].0, entity);
        assert_eq!(entities[0].2, mesh, "same mesh asset");
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 1);
    }
}
