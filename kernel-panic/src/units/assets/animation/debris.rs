//! The pieces a dying unit's script throws off (`explode piece type
//! ...`, `CUnitScript::Explode`, UnitScript.cpp:892): a `FALL` (or any
//! non-shatter) piece flies off whole and tumbles until it hits the
//! ground (`CPieceProjectile`); a `SHATTER` piece bursts into up to ten
//! textured chunks that drift out and sink into the terrain
//! (`S3DModelPiece::Shatter` → `FlyingPiece`).
//!
//! Visual only: nothing in the sim reads a piece. Like the weapon
//! effects, they run in the fixed tick on their own match-seeded random
//! stream ([`DebrisRng`]), so a replay throws the same pieces, and draw
//! interpolated between ticks ([`crate::rendering::interpolation`]).
//! Speeds are in elmos per sim frame, as the engine keeps them; the
//! tick is one frame.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;

use crate::interaction::ground_move::GroundMover;
use crate::net::NetId;
use crate::rng::{next_f32, next_signed, random_unit_sphere, xorshift32};
use crate::sim::{DetFloat, MapGravity, RngStream, SimRng};
use crate::terrain::heightmap::Heightmap;

/// `explode` flags (`springdefs.h`).
pub const SHATTER: i32 = 1;
pub const FALL: i32 = 4;
/// No debris at all, only the heat cloud (`UnitScript.cpp:910`).
pub const BITMAPONLY: i32 = 32;

/// How an `explode` with `flags` breaks its piece off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Breakup {
    /// `BITMAPONLY`: nothing flies.
    None,
    Shatter,
    /// Whole, tumbling (`FALL` and every other non-shatter flag).
    Whole,
}

impl Breakup {
    /// `CUnitScript::Explode`'s order: `BITMAPONLY` returns first, then
    /// `SHATTER` wins over everything else.
    pub fn of(flags: i32) -> Self {
        if flags & BITMAPONLY != 0 {
            Breakup::None
        } else if flags & SHATTER != 0 {
            Breakup::Shatter
        } else {
            Breakup::Whole
        }
    }
}

/// The debris spawner's random stream.
pub struct DebrisRng;
impl RngStream for DebrisRng {
    const SALT: u64 = 0xDEB2_15A7;
}

/// One `explode` waiting for [`spawn_debris`]: the piece entity (still
/// alive: the unit is despawned in `Cleanup`, after `Animate`), its
/// index in the unit's piece table, and its unit.
#[derive(Clone, Copy, Debug)]
pub struct DebrisRequest {
    pub piece: Entity,
    pub index: usize,
    pub unit: Entity,
    pub flags: i32,
}

/// The pieces [`super::animation_system`] exploded this tick.
#[derive(Resource, Default)]
pub struct PendingDebris(pub Vec<DebrisRequest>);

/// Engine constants, per frame.
/// `CPieceProjectile`: `speed *= 0.997` every frame.
const PIECE_DRAG: f32 = 0.997;
/// Spin: a random axis at up to 20° a frame.
const PIECE_SPIN_MAX: f32 = 20.0 * std::f32::consts::PI / 180.0;
/// Ground collisions are only checked from this age on.
const PIECE_NO_COLLIDE_FRAMES: u32 = 10;
/// A piece hitting the ground is gone with this chance, else it bounces.
const PIECE_REMOVE_CHANCE: f32 = 0.666;
/// The bounce: `speed - n·(speed·n)·1.6`.
const PIECE_BOUNCE: f32 = 1.6;
/// A unit higher than this above the ground throws its pieces every way,
/// not up.
const AIRBORNE_HEIGHT: f32 = 15.0;
/// `FlyingPiece`: drag per frame, spin up to 0.1 rad a frame, outward
/// speed `EXPLOSION_SPEED` at most.
const SHARD_DRAG: f32 = 0.995;
const SHARD_SPIN_MAX: f32 = 0.1;
const EXPLOSION_SPEED: f32 = 2.0;
/// A piece shatters into at most this many chunks, in one of this many
/// precomputed ways (`3DModelMisc.hpp`).
const SHARDS_MAX: usize = 10;
const VARIATIONS: usize = 2;
/// Safety net: nothing the engine throws flies longer than this, but a
/// piece thrown off the map edge would never hit the ground here.
const MAX_FRAMES: u32 = 450;

/// A whole piece in flight (`CPieceProjectile`).
#[derive(Component)]
pub struct Debris {
    /// Elmos per frame.
    velocity: Vec3,
    axis: Vec3,
    /// Radians per frame.
    spin: f32,
    age: u32,
    /// The piece's own random stream (its bounce rolls), so how long it
    /// lives doesn't hang on the order the tick visits pieces in.
    rng: u32,
}

/// One chunk of a shattered piece (`FlyingPiece`).
#[derive(Component)]
pub struct Shard {
    velocity: Vec3,
    axis: Vec3,
    spin: f32,
    age: u32,
    /// Gone once this far under the terrain: twice the piece's radius.
    sink: f32,
}

/// `CPieceProjectile`'s launch speed: up and out (`x, z ∈ ±3`, `y ∈
/// 1.2..6.2`), or every way (`y ∈ ±3`) for an airborne unit; plus the
/// unit's speed, its part over 3 square-rooted.
pub fn launch_velocity(airborne: bool, unit_speed: Vec3, rng: &mut u32) -> Vec3 {
    let x = (0.5 - next_f32(rng)) * 6.0;
    let z = (0.5 - next_f32(rng)) * 6.0;
    let y = if airborne {
        (0.5 - next_f32(rng)) * 6.0
    } else {
        1.2 + next_f32(rng) * 5.0
    };
    Vec3::new(x, y, z) + compressed(unit_speed)
}

/// The unit's speed with its part over 3 elmos a frame square-rooted.
fn compressed(speed: Vec3) -> Vec3 {
    let l = speed.length();
    if l > 3.0 {
        speed * ((3.0 + (l - 3.0).sqrt()) / l)
    } else {
        speed
    }
}

/// A random axis and spin rate up to `max` radians a frame.
fn tumble(rng: &mut u32, max: f32) -> (Vec3, f32) {
    let axis = random_unit_sphere(rng).normalize_or(Vec3::Y);
    (axis, next_f32(rng) * max)
}

/// One chunk of a shattered piece: its mesh (vertices relative to the
/// chunk's centre), where that centre sits in the piece, and the
/// chunk's direction out of it.
#[derive(Clone)]
struct ShardMesh {
    mesh: Handle<Mesh>,
    centre: Vec3,
    dir: Vec3,
}

/// A piece mesh's precomputed breakups.
struct PieceShards {
    radius: f32,
    variations: Vec<Vec<ShardMesh>>,
}

/// The shatter breakups per piece mesh, and the double-sided copy of
/// each unit material the chunks are drawn with (a chunk is an open
/// shell: its inside shows).
#[derive(Resource, Default)]
pub struct ShatterCache {
    pieces: HashMap<AssetId<Mesh>, PieceShards>,
    double_sided: HashMap<AssetId<StandardMaterial>, Handle<StandardMaterial>>,
}

/// A triangle mesh's data, as the S3O pieces carry it.
struct TriMesh {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl TriMesh {
    fn of(mesh: &Mesh) -> Option<Self> {
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION)? {
            VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => return None,
        };
        let normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(VertexAttributeValues::Float32x3(v)) => v.clone(),
            _ => vec![[0.0, 1.0, 0.0]; positions.len()],
        };
        let uvs = match mesh.attribute(Mesh::ATTRIBUTE_UV_0) {
            Some(VertexAttributeValues::Float32x2(v)) => v.clone(),
            _ => vec![[0.0, 0.0]; positions.len()],
        };
        let indices: Vec<u32> = match mesh.indices()? {
            Indices::U16(i) => i.iter().map(|&i| i as u32).collect(),
            Indices::U32(i) => i.clone(),
        };
        Some(Self {
            positions,
            normals,
            uvs,
            indices,
        })
    }

    fn vertex(&self, i: u32) -> Vec3 {
        Vec3::from_array(self.positions[i as usize])
    }
}

/// `S3DModelPiece::CreateShatterPiecesVariation`: `count` random
/// directions; each triangle joins the one closest to the direction of
/// its centroid from the piece's origin. Returns each non-empty
/// cluster's triangles (indices into `tris`) and its direction.
fn cluster_triangles(centroids: &[Vec3], count: usize, rng: &mut u32) -> Vec<(Vec3, Vec<usize>)> {
    let dirs: Vec<Vec3> = (0..count)
        .map(|_| random_unit_sphere(rng).normalize_or(Vec3::Y))
        .collect();
    let mut clusters: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (t, c) in centroids.iter().enumerate() {
        let d = c.normalize_or(Vec3::Y);
        let best = (0..count)
            .max_by(|&a, &b| d.dot(dirs[a]).total_cmp(&d.dot(dirs[b])))
            .unwrap_or(0);
        clusters[best].push(t);
    }
    dirs.into_iter()
        .zip(clusters)
        .filter(|(_, tris)| !tris.is_empty())
        .collect()
}

/// Build a piece's [`VARIATIONS`] breakups. The random directions come
/// from a stream seeded by the mesh's size, not the match's: the cache
/// outlives a match, and the same piece must break the same way in a
/// replay whichever match first shattered it.
fn build_shards(mesh: &TriMesh, meshes: &mut Assets<Mesh>) -> PieceShards {
    let tris: Vec<[u32; 3]> = mesh.indices.as_chunks::<3>().0.to_vec();
    let centroids: Vec<Vec3> = tris
        .iter()
        .map(|t| (mesh.vertex(t[0]) + mesh.vertex(t[1]) + mesh.vertex(t[2])) / 3.0)
        .collect();
    let radius = mesh
        .positions
        .iter()
        .map(|p| Vec3::from_array(*p).length())
        .fold(1.0f32, |m, r| m.det_max(r));
    let mut rng = (0x5A7E_u32
        ^ (mesh.positions.len() as u32).wrapping_mul(2_654_435_761)
        ^ (mesh.indices.len() as u32))
        | 1;
    xorshift32(&mut rng);
    let count = SHARDS_MAX.min(tris.len().max(1));
    let variations = (0..VARIATIONS)
        .map(|_| {
            cluster_triangles(&centroids, count, &mut rng)
                .into_iter()
                .map(|(dir, members)| {
                    let centre =
                        members.iter().map(|&t| centroids[t]).sum::<Vec3>() / members.len() as f32;
                    let mut positions = Vec::with_capacity(members.len() * 3);
                    let mut normals = Vec::with_capacity(members.len() * 3);
                    let mut uvs = Vec::with_capacity(members.len() * 3);
                    for &t in &members {
                        for &i in &tris[t] {
                            positions.push((mesh.vertex(i) - centre).to_array());
                            normals.push(mesh.normals[i as usize]);
                            uvs.push(mesh.uvs[i as usize]);
                        }
                    }
                    let n = positions.len() as u32;
                    let mut chunk = Mesh::new(
                        PrimitiveTopology::TriangleList,
                        RenderAssetUsages::RENDER_WORLD,
                    );
                    chunk.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
                    chunk.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
                    chunk.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
                    chunk.insert_indices(Indices::U32((0..n).collect()));
                    ShardMesh {
                        mesh: meshes.add(chunk),
                        centre,
                        dir,
                    }
                })
                .collect()
        })
        .collect();
    PieceShards { radius, variations }
}

/// An exploded piece's pose, mesh and material.
type PieceLook = (
    &'static GlobalTransform,
    Option<&'static Mesh3d>,
    Option<&'static MeshMaterial3d<StandardMaterial>>,
);

/// `Animate`, after [`super::animation_system`]: turn this tick's
/// exploded pieces into debris and chunks at the pieces' last world
/// pose.
#[allow(clippy::too_many_arguments)]
pub fn spawn_debris(
    mut pending: ResMut<PendingDebris>,
    pieces: Query<PieceLook>,
    units: Query<(&GlobalTransform, Option<&GroundMover>, Option<&NetId>)>,
    heightmap: Option<Res<Heightmap>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<ShatterCache>,
    mut rng: ResMut<SimRng<DebrisRng>>,
    mut commands: Commands,
) {
    // The animators' order is the ECS's, which differs between peers:
    // draw the random numbers in unit-id order instead.
    let mut requests = std::mem::take(&mut pending.0);
    requests.sort_by_key(|r| {
        let id = units.get(r.unit).ok().and_then(|(_, _, id)| id.copied());
        (id.map_or(u32::MAX, |id| id.0), r.index)
    });
    for req in requests {
        let breakup = Breakup::of(req.flags);
        if breakup == Breakup::None {
            continue;
        }
        let Ok((gtf, Some(mesh), Some(material))) = pieces.get(req.piece) else {
            continue;
        };
        let (unit_pos, unit_speed) = units
            .get(req.unit)
            .map_or((gtf.translation(), Vec3::ZERO), |(u, mover, _)| {
                (u.translation(), mover.map_or(Vec3::ZERO, |m| m.velocity))
            });
        // A unit spawned this tick has no propagated pose yet.
        if gtf.translation() == Vec3::ZERO {
            continue;
        }
        let pose = gtf.compute_transform();
        match breakup {
            Breakup::Whole => {
                let airborne = heightmap.as_deref().is_some_and(|h| {
                    unit_pos.y - h.sample(unit_pos.x, unit_pos.z) > AIRBORNE_HEIGHT
                });
                let velocity = launch_velocity(airborne, unit_speed, &mut rng);
                let (axis, spin) = tumble(&mut rng, PIECE_SPIN_MAX);
                commands.spawn((
                    Debris {
                        velocity,
                        axis,
                        spin,
                        age: 0,
                        rng: xorshift32(&mut rng) | 1,
                    },
                    Mesh3d(mesh.0.clone()),
                    MeshMaterial3d(material.0.clone()),
                    pose,
                    Visibility::Visible,
                ));
            }
            Breakup::Shatter => {
                let id = mesh.0.id();
                if let std::collections::hash_map::Entry::Vacant(slot) = cache.pieces.entry(id) {
                    let Some(data) = meshes.get(id).and_then(TriMesh::of) else {
                        continue;
                    };
                    slot.insert(build_shards(&data, &mut meshes));
                }
                let double_sided = cache
                    .double_sided
                    .entry(material.0.id())
                    .or_insert_with(|| {
                        let mut m = materials.get(material.0.id()).cloned().unwrap_or_default();
                        m.cull_mode = None;
                        m.double_sided = true;
                        materials.add(m)
                    })
                    .clone();
                let shards = &cache.pieces[&id];
                let pick = (next_f32(&mut rng) * shards.variations.len() as f32) as usize;
                let unit_speed = compressed(unit_speed);
                for shard in &shards.variations[pick.min(shards.variations.len() - 1)] {
                    let jitter = Vec3::new(
                        next_signed(&mut rng),
                        next_signed(&mut rng),
                        next_signed(&mut rng),
                    );
                    let out = pose.rotation * (shard.dir + jitter * 0.3).normalize_or(shard.dir);
                    let speed = 1.0 + next_f32(&mut rng) * (EXPLOSION_SPEED - 1.0);
                    let (axis, spin) = tumble(&mut rng, SHARD_SPIN_MAX);
                    commands.spawn((
                        Shard {
                            velocity: unit_speed + out * speed,
                            axis,
                            spin,
                            age: 0,
                            sink: 2.0 * shards.radius * pose.scale.max_element(),
                        },
                        Mesh3d(shard.mesh.clone()),
                        MeshMaterial3d(double_sided.clone()),
                        Transform {
                            translation: pose.transform_point(shard.centre),
                            ..pose
                        },
                        Visibility::Visible,
                    ));
                }
            }
            Breakup::None => {}
        }
    }
}

/// One frame of a whole piece's flight; `false` once it is gone.
/// `ground` gives the terrain height and normal under a point;
/// `roll` a `0..1` random.
fn fly_piece(
    d: &mut Debris,
    tf: &mut Transform,
    gravity: f32,
    ground: impl Fn(Vec3) -> (f32, Vec3),
    roll: f32,
) -> bool {
    d.age += 1;
    d.velocity.y += gravity;
    d.velocity *= PIECE_DRAG;
    tf.translation += d.velocity;
    tf.rotation = Quat::from_axis_angle(d.axis, d.spin) * tf.rotation;
    if d.age >= MAX_FRAMES {
        return false;
    }
    if d.age < PIECE_NO_COLLIDE_FRAMES {
        return true;
    }
    let (height, normal) = ground(tf.translation);
    if tf.translation.y > height {
        return true;
    }
    if roll < PIECE_REMOVE_CHANCE {
        return false;
    }
    tf.translation.y = height;
    d.velocity -= normal * d.velocity.dot(normal) * PIECE_BOUNCE;
    true
}

/// One frame of a chunk's drift; `false` once it is gone.
fn fly_shard(
    s: &mut Shard,
    tf: &mut Transform,
    gravity: f32,
    height: impl Fn(Vec3) -> f32,
) -> bool {
    s.age += 1;
    s.velocity.y += gravity;
    s.velocity *= SHARD_DRAG;
    tf.translation += s.velocity;
    tf.rotation = Quat::from_axis_angle(s.axis, s.spin) * tf.rotation;
    s.age < MAX_FRAMES && tf.translation.y > height(tf.translation) - s.sink
}

/// `Animate`: fly every piece and chunk one frame; drop the finished.
pub fn tick_debris(
    mut pieces: Query<(Entity, &mut Debris, &mut Transform), Without<Shard>>,
    mut shards: Query<(Entity, &mut Shard, &mut Transform), Without<Debris>>,
    heightmap: Option<Res<Heightmap>>,
    gravity: Option<Res<MapGravity>>,
    mut commands: Commands,
) {
    if pieces.is_empty() && shards.is_empty() {
        return;
    }
    let g = gravity.map_or(MapGravity::default().0, |g| g.0);
    let hm = heightmap.as_deref();
    let height = |p: Vec3| hm.map_or(0.0, |h| h.sample(p.x, p.z));
    for (e, mut d, mut tf) in &mut pieces {
        let roll = next_f32(&mut d.rng);
        let ground = |p: Vec3| (height(p), hm.map_or(Vec3::Y, |h| h.normal(p.x, p.z)));
        if !fly_piece(&mut d, &mut tf, g, ground, roll) {
            commands.entity(e).despawn();
        }
    }
    for (e, mut s, mut tf) in &mut shards {
        if !fly_shard(&mut s, &mut tf, g, height) {
            commands.entity(e).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_pick_the_breakup() {
        assert_eq!(Breakup::of(SHATTER), Breakup::Shatter);
        assert_eq!(Breakup::of(FALL), Breakup::Whole);
        assert_eq!(Breakup::of(SHATTER | FALL), Breakup::Shatter);
        assert_eq!(Breakup::of(0), Breakup::Whole);
        assert_eq!(Breakup::of(SHATTER | BITMAPONLY), Breakup::None);
    }

    #[test]
    fn pieces_launch_up_and_out_or_every_way_in_the_air() {
        let mut rng = 0x1234_5678;
        for _ in 0..500 {
            let v = launch_velocity(false, Vec3::ZERO, &mut rng);
            assert!(v.x.abs() <= 3.0 && v.z.abs() <= 3.0);
            assert!((1.2..=6.2).contains(&v.y), "{v}");
            let a = launch_velocity(true, Vec3::ZERO, &mut rng);
            assert!(a.y.abs() <= 3.0);
        }
        // A fast unit's speed is added square-rooted past 3: 7 → 3 + 2.
        let v = compressed(Vec3::new(7.0, 0.0, 0.0));
        assert!((v.x - 5.0).abs() < 1e-5);
        assert_eq!(
            compressed(Vec3::new(2.0, 0.0, 0.0)),
            Vec3::new(2.0, 0.0, 0.0)
        );
    }

    #[test]
    fn every_triangle_lands_in_one_of_at_most_ten_chunks() {
        // A sphere-ish cloud of triangle centroids.
        let mut rng = 99u32;
        let centroids: Vec<Vec3> = (0..300)
            .map(|_| random_unit_sphere(&mut rng) * 10.0)
            .collect();
        let clusters = cluster_triangles(&centroids, SHARDS_MAX, &mut rng);
        assert!(!clusters.is_empty() && clusters.len() <= SHARDS_MAX);
        let mut seen: Vec<usize> = clusters.iter().flat_map(|(_, t)| t.clone()).collect();
        seen.sort_unstable();
        assert_eq!(
            seen,
            (0..300).collect::<Vec<_>>(),
            "each triangle exactly once"
        );
    }

    #[test]
    fn a_piece_falls_to_the_ground_and_goes_or_bounces() {
        let flat = |_: Vec3| (0.0, Vec3::Y);
        let start = Transform::from_xyz(0.0, 20.0, 0.0);
        let mut d = Debris {
            velocity: Vec3::new(1.0, 2.0, 0.0),
            axis: Vec3::Y,
            spin: 0.1,
            age: 0,
            rng: 1,
        };
        let mut tf = start;
        let mut frames = 0;
        // Never removed by the roll (1.0 ≥ the chance): it bounces.
        while tf.translation.y > 0.0 {
            assert!(fly_piece(&mut d, &mut tf, -0.2, flat, 1.0));
            frames += 1;
            assert!(frames < MAX_FRAMES);
        }
        assert_eq!(tf.translation.y, 0.0, "landed on the ground");
        assert!(d.velocity.y > 0.0, "and bounced up");
        // With a low roll the next landing removes it.
        let mut alive = true;
        for _ in 0..MAX_FRAMES {
            alive = fly_piece(&mut d, &mut tf, -0.2, flat, 0.0);
            if !alive {
                break;
            }
        }
        assert!(!alive);
    }

    #[test]
    fn a_shard_sinks_out_of_sight() {
        let mut s = Shard {
            velocity: Vec3::ZERO,
            axis: Vec3::X,
            spin: 0.05,
            age: 0,
            sink: 8.0,
        };
        let mut tf = Transform::from_xyz(0.0, 5.0, 0.0);
        let mut frames = 0;
        while fly_shard(&mut s, &mut tf, -0.2, |_| 0.0) {
            frames += 1;
        }
        assert!(tf.translation.y <= -8.0, "gone {} under", tf.translation.y);
        assert!(frames < MAX_FRAMES);
    }
}
