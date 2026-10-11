//! The structure layer of the ground-movement map: which heightmap
//! squares buildings block.
//!
//! Spring marks every immobile object's yardmap squares
//! `BLOCK_STRUCTURE` on the ground-blocking map
//! (`GroundBlockingObjectMap::AddGroundBlockingObject`). The pathfinder
//! closes every node whose mover-footprint window touches such a square
//! (QTPFS `NodeLayer::Update`, `NodeLayer.cpp:120-190`: offsets
//! `-xsizeh..=xsizeh` in steps of 2 around the node), `UpdatePos` refuses
//! steps whose full footprint overlaps one
//! (`MoveDef::TestMovePositionForObjects`) and collisions strafe units
//! around them (`HandleStaticObjectCollision`). The layer is rebuilt
//! locally when a structure appears, moves or dies (see
//! [`StructureStamped`]), and paths crossing the changed area are
//! re-checked (`NavGridSet::revision`).
//!
//! Features a mover can crush (KP's Bad Block, `IsFeature=1`, crush
//! resistance `0.4·metal + 0.1·damage` = 65.6,
//! `FeatureDefHandler.cpp:146-149`) block only movers whose MOVEINFO
//! `CrushStrength` doesn't exceed it (`CMoveMath::CrushResistant`):
//! HEAVY (300) drives through and crushes them, LIGHT/MEDIUM path round.

use std::collections::HashMap;

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::prelude::*;
use spring_pathfinding::BlockMask;

use super::movement::NavGridSet;
use crate::net::NetId;
use crate::sim::SQUARE_SIZE;
use crate::units::combat::Dying;
use crate::units::components::{UnitStats, UnitType};
use crate::units::content::definitions::{ALL_UNIT_KINDS, UnitKind};
use crate::units::content::unit_registry::UnitRegistry;

/// Crush resistance of a KP Bad Block once it turned into its feature:
/// `defMass = metal·0.4 + health·0.1` with the feature's `metal=64`,
/// `damage=400` (`features/corpses/badblock.tdf`,
/// `FeatureDefHandler.cpp:146-149`).
pub const BADBLOCK_CRUSH_RESISTANCE: f32 = 64.0 * 0.4 + 400.0 * 0.1;

/// Squares one structure occupies.
#[derive(Clone, Debug)]
struct Stamp {
    squares: Vec<(u32, u32)>,
    crushable: bool,
}

/// A path mask for one mover class (footprint half-size, whether it can
/// crush features).
#[derive(Clone, Debug)]
struct ClassMask {
    xsizeh: i32,
    crushes: bool,
    mask: BlockMask,
}

/// Per-square structure blocking, plus the dilated path masks of every
/// mover class.
#[derive(Clone, Debug, Default)]
pub struct StructureLayer {
    width: u32,
    height: u32,
    /// Non-crushable structures on each square.
    solid: Vec<u16>,
    /// Crushable features on each square.
    crushable: Vec<u16>,
    stamps: HashMap<Entity, Stamp>,
    masks: Vec<ClassMask>,
}

/// Can a mover with `crush_strength` drive through crushable features?
pub fn crushes_features(crush_strength: f32) -> bool {
    crush_strength > BADBLOCK_CRUSH_RESISTANCE
}

impl StructureLayer {
    /// Size the layer for a `width × height` grid; `true` when it was
    /// (re)built empty.
    fn ensure_size(&mut self, width: u32, height: u32) -> bool {
        if self.width == width && self.height == height {
            return false;
        }
        *self = Self {
            width,
            height,
            solid: vec![0; (width * height) as usize],
            crushable: vec![0; (width * height) as usize],
            ..Self::default()
        };
        true
    }

    #[inline]
    fn idx(&self, x: i32, z: i32) -> Option<usize> {
        (x >= 0 && z >= 0 && (x as u32) < self.width && (z as u32) < self.height)
            .then(|| (z as u32 * self.width + x as u32) as usize)
    }

    /// `SquareIsBlocked(..) & BLOCK_STRUCTURE` for a mover that can /
    /// cannot crush features.
    pub fn square_blocked(&self, x: i32, z: i32, crushes: bool) -> bool {
        self.idx(x, z)
            .is_some_and(|i| self.solid[i] > 0 || (!crushes && self.crushable[i] > 0))
    }

    /// Any structure square in the footprint window around square
    /// `(x, z)` (`TestMovePositionForObjects` → `RangeIsBlocked`), which
    /// the engine samples at offsets `-h, -h+2, …, h` on both axes
    /// (`FOOTPRINT_XSTEP = 2`, MoveMath.cpp:21) — the same window the
    /// path masks use, so a square open to the pathfinder is open to
    /// `UpdatePos` too.
    pub fn footprint_blocked(&self, x: i32, z: i32, xsizeh: i32, crushes: bool) -> bool {
        if self.stamps.is_empty() {
            return false;
        }
        (z - xsizeh..=z + xsizeh).step_by(2).any(|sz| {
            (x - xsizeh..=x + xsizeh)
                .step_by(2)
                .any(|sx| self.square_blocked(sx, sz, crushes))
        })
    }

    /// The path mask of a mover class, if one was built.
    pub fn mask(&self, xsizeh: i32, crushes: bool) -> Option<&BlockMask> {
        self.masks
            .iter()
            .find(|m| m.xsizeh == xsizeh && m.crushes == crushes)
            .map(|m| &m.mask)
    }

    fn ensure_mask(&mut self, xsizeh: i32, crushes: bool) {
        if self.mask(xsizeh, crushes).is_some() {
            return;
        }
        let mut mask = ClassMask {
            xsizeh,
            crushes,
            mask: BlockMask::new(self.width, self.height),
        };
        // A layer without stamps closes nothing: the fresh mask is
        // already right, and the full-grid window scan it would
        // otherwise run (2.4M cells × 49 squares on Hex Farm, per
        // class) is what made the first sim tick of a match stall.
        if !self.stamps.is_empty() {
            self.fill_mask(
                &mut mask,
                0,
                0,
                self.width as i32 - 1,
                self.height as i32 - 1,
            );
        }
        self.masks.push(mask);
    }

    /// QTPFS `rangeIsBlocked`: a node is closed when any square at
    /// offsets `-h, -h+2, …, h` (both axes) around it is blocked.
    fn fill_mask(&self, m: &mut ClassMask, x0: i32, z0: i32, x1: i32, z1: i32) {
        let h = m.xsizeh;
        let (w, hgt) = (self.width as i32, self.height as i32);
        for z in z0.max(0)..=z1.min(hgt - 1) {
            for x in x0.max(0)..=x1.min(w - 1) {
                // Clamp so the window never hangs off the map edge.
                let cx = x.clamp(h, (w - 1 - h).max(h));
                let cz = z.clamp(h, (hgt - 1 - h).max(h));
                let mut closed = false;
                let mut dz = -h;
                'outer: while dz <= h {
                    let mut dx = -h;
                    while dx <= h {
                        if self.square_blocked(cx + dx, cz + dz, m.crushes) {
                            closed = true;
                            break 'outer;
                        }
                        dx += 2;
                    }
                    dz += 2;
                }
                m.mask.cells[(z * w + x) as usize] = closed;
            }
        }
    }

    /// Add (`add`) or remove a stamp and rebuild the masks around it.
    /// Returns the squares whose masks changed (dilated by the widest
    /// footprint), or `None` for an empty stamp.
    fn apply(&mut self, stamp: &Stamp, add: bool) -> Option<[i32; 4]> {
        let (mut x0, mut z0, mut x1, mut z1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(x, z) in &stamp.squares {
            let i = (z * self.width + x) as usize;
            let cell = if stamp.crushable {
                &mut self.crushable[i]
            } else {
                &mut self.solid[i]
            };
            *cell = if add {
                cell.saturating_add(1)
            } else {
                cell.saturating_sub(1)
            };
            x0 = x0.min(x as i32);
            z0 = z0.min(z as i32);
            x1 = x1.max(x as i32);
            z1 = z1.max(z as i32);
        }
        if stamp.squares.is_empty() {
            return None;
        }
        let mut masks = std::mem::take(&mut self.masks);
        for m in &mut masks {
            let h = m.xsizeh;
            self.fill_mask(m, x0 - h, z0 - h, x1 + h, z1 + h);
        }
        self.masks = masks;
        let h = self.max_xsizeh();
        Some([x0 - h, z0 - h, x1 + h, z1 + h])
    }

    /// The stamp a structure of `kind` at `pos` would leave.
    fn stamp_of(&self, registry: &UnitRegistry, kind: UnitKind, pos: Vec3) -> Stamp {
        Stamp {
            squares: self.squares_of(registry, kind, pos),
            crushable: registry.is_feature(kind),
        }
    }

    /// `CSolidObject::Block`: stamp `entity` (once). Returns the squares
    /// whose masks changed.
    fn block(&mut self, entity: Entity, stamp: Stamp) -> Option<[i32; 4]> {
        if self.stamps.contains_key(&entity) {
            return None;
        }
        let changed = self.apply(&stamp, true);
        self.stamps.insert(entity, stamp);
        changed
    }

    /// `CSolidObject::UnBlock`: clear `entity`'s stamp, if it has one.
    fn unblock(&mut self, entity: Entity) -> Option<[i32; 4]> {
        let stamp = self.stamps.remove(&entity)?;
        self.apply(&stamp, false)
    }

    /// The widest mover half-footprint any mask is dilated by.
    pub fn max_xsizeh(&self) -> i32 {
        self.masks.iter().map(|m| m.xsizeh).max().unwrap_or(0)
    }

    /// The squares a structure of `kind` centred at `pos` blocks: its FBI
    /// footprint at `SPRING_FOOTPRINT_SCALE` (×2 squares), each yardmap
    /// character covering 2×2 squares. Blocks on everything except the
    /// engine's open set — `y` (open) and `c` (factory yard, always open
    /// while producing: `Activate` → `YARD_OPEN`, kernel.bos/socket.bos)
    /// plus the terrain-style codes `e`/`i`/`s`/`b`/`u` that ship open in
    /// KP's yardmaps. A missing yardmap blocks everything (the `unwrap_or('o')`
    /// below matches `CGuiHandler`'s default-blocked reading).
    fn squares_of(&self, registry: &UnitRegistry, kind: UnitKind, pos: Vec3) -> Vec<(u32, u32)> {
        let Some(def) = registry.def(kind) else {
            return Vec::new();
        };
        let (fx, fz) = (
            def.footprint_x.max(1.0) as i32,
            def.footprint_z.max(1.0) as i32,
        );
        let (xsize, zsize) = (fx * 2, fz * 2);
        let x0 = ((pos.x - xsize as f32 * SQUARE_SIZE * 0.5) / SQUARE_SIZE).round() as i32;
        let z0 = ((pos.z - zsize as f32 * SQUARE_SIZE * 0.5) / SQUARE_SIZE).round() as i32;
        let chars: Vec<char> = def
            .yard_map
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        let mut out = Vec::new();
        for sz in 0..zsize {
            for sx in 0..xsize {
                let c = chars
                    .get(((sx / 2) + (sz / 2) * fx) as usize)
                    .copied()
                    .unwrap_or('o');
                let blocks = !matches!(c, 'y' | 'c' | 'e' | 'i' | 's' | 'b' | 'u');
                let (x, z) = (x0 + sx, z0 + sz);
                if blocks && self.idx(x, z).is_some() {
                    out.push((x as u32, z as u32));
                }
            }
        }
        out
    }
}

/// Mover classes of the roster: `(xsizeh, crushes features)`.
fn mover_classes(registry: &UnitRegistry) -> Vec<(i32, bool)> {
    let mut classes: Vec<(i32, bool)> = ALL_UNIT_KINDS
        .iter()
        .filter_map(|&k| registry.move_def(k))
        .map(|md| (md.xsizeh, crushes_features(md.crush_strength)))
        .collect();
    classes.sort();
    classes.dedup();
    classes
}

/// Lifetime handle of an entity's stamp in [`StructureLayer`]: the stamp
/// exists exactly while this marker does. Its `on_add` hook blocks the
/// structure's squares (`CSolidObject::Block`), its `on_remove` hook
/// clears them (`UnBlock`) — `Dying` drops the marker at the kill, and
/// a despawn on any path (the death pipeline, the Exploit packing up,
/// the map teardown) drops it with the entity. Hooks run at the command
/// flush in whatever schedule: the old `Added<Dying>` /
/// `RemovedComponents<UnitType>` reads in `FixedUpdate` missed every
/// building despawned in the tick it died, and most despawns above
/// ~60 fps, leaving phantom obstacles until the next map load.
#[derive(Component)]
#[component(
    storage = "SparseSet",
    on_add = StructureStamped::on_add,
    on_remove = StructureStamped::on_remove
)]
pub struct StructureStamped;

impl StructureStamped {
    fn on_add(mut world: DeferredWorld, ctx: HookContext) {
        let entity = ctx.entity;
        let (Some(kind), Some(tf)) = (
            world.get::<UnitType>(entity),
            world.get::<Transform>(entity),
        ) else {
            return;
        };
        let (kind, pos) = (kind.0, tf.translation);
        let (Some(registry), Some(nav)) = (
            world.get_resource::<UnitRegistry>(),
            world.get_resource::<NavGridSet>(),
        ) else {
            return;
        };
        let stamp = nav.structures.stamp_of(registry, kind, pos);
        let mut nav = world.resource_mut::<NavGridSet>();
        if let Some(bbox) = nav.structures.block(entity, stamp) {
            nav.bump(bbox);
        }
    }

    fn on_remove(mut world: DeferredWorld, ctx: HookContext) {
        let Some(mut nav) = world.get_resource_mut::<NavGridSet>() else {
            return;
        };
        if let Some(bbox) = nav.structures.unblock(ctx.entity) {
            nav.bump(bbox);
        }
    }
}

/// Keep [`StructureLayer`] in step with the living structures: tag new
/// ones [`StructureStamped`] (whose hooks stamp them, and clear them
/// when they die or despawn), re-stamp moved ones, and bump
/// [`NavGridSet::revision`] so paths through the change are re-checked.
/// Runs before the movement systems each sim frame. Only a freshly
/// (re)built layer scans every unit; after that `Added<UnitType>` (change
/// ticks, never missed) drives it.
///
/// In [`NetId`] order: every stamp bumps the nav revision with its own
/// rectangle, and the QTPFS node layers replay those rectangles in
/// revision order (`NodeLayer::update`), which shapes the quad tree. In
/// query order, two machines with different archetype histories built
/// different trees from the same blocked squares, and their paths
/// parted on the first tie (the golden replay's archetype fuzz,
/// `net-experiments/m0-golden-replay.md`).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update_structure_layer(
    nav: Option<ResMut<NavGridSet>>,
    registry: Res<UnitRegistry>,
    units: Query<(
        &UnitType,
        &UnitStats,
        &Transform,
        Has<Dying>,
        Has<StructureStamped>,
    )>,
    all: Query<(Entity, &NetId), With<UnitType>>,
    spawned: Query<(Entity, &NetId), Added<UnitType>>,
    moved: Query<
        (Entity, &NetId, &UnitType, &Transform),
        (Changed<Transform>, With<StructureStamped>),
    >,
    mut commands: Commands,
    mut classes: Local<Option<Vec<(i32, bool)>>>,
) {
    let Some(mut nav) = nav else { return };
    let Some(bucket) = nav.buckets.first() else {
        return;
    };
    let (w, h) = (bucket.speed_map.width, bucket.speed_map.height);
    let nav = &mut *nav;
    let layer = &mut nav.structures;
    let fresh = layer.ensure_size(w, h);
    for &(xsizeh, crushes) in classes
        .get_or_insert_with(|| mover_classes(&registry))
        .iter()
    {
        layer.ensure_mask(xsizeh, crushes);
    }
    let mut changed: Option<[i32; 4]> = None;
    let mut candidates: Vec<(Entity, &NetId)> = if fresh {
        all.iter().collect()
    } else {
        spawned.iter().collect()
    };
    candidates.sort_by_key(|(_, id)| **id);
    for (entity, _) in candidates {
        let Ok((kind, stats, tf, dying, stamped)) = units.get(entity) else {
            continue;
        };
        if stats.speed > 0.0 || stats.can_fly || dying {
            continue;
        }
        if stamped {
            // A layer rebuilt under a live, already-tagged structure:
            // its hook ran against the old layer, so stamp it here.
            let stamp = layer.stamp_of(&registry, kind.0, tf.translation);
            changed = union(changed, layer.block(entity, stamp));
        } else {
            commands.entity(entity).try_insert(StructureStamped);
        }
    }
    // `CUnit::ForcedMove` unblocks, moves and re-blocks: a structure
    // that was moved (Hex Farm's fall sweep pushes buildings off a
    // sinking tower) blocks where it stands now, not where it spawned.
    for (entity, _, kind, tf) in moved.iter().sort::<&NetId>() {
        let stamp = layer.stamp_of(&registry, kind.0, tf.translation);
        if layer
            .stamps
            .get(&entity)
            .is_some_and(|old| old.squares != stamp.squares)
        {
            changed = union(changed, layer.unblock(entity));
            changed = union(changed, layer.block(entity, stamp));
        }
    }
    if let Some(bbox) = changed {
        nav.bump(bbox);
    }
}

fn union(a: Option<[i32; 4]>, b: Option<[i32; 4]>) -> Option<[i32; 4]> {
    match (a, b) {
        (Some(a), Some(b)) => Some([
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::movement::{MovePath, MoveTarget};
    use crate::interaction::movement_harness::Harness;

    fn mask_at(h: &Harness, xsizeh: i32, crushes: bool, p: Vec3) -> bool {
        let nav = h.world.resource::<NavGridSet>();
        nav.structures
            .mask(xsizeh, crushes)
            .unwrap()
            .blocked((p.x / 8.0) as u32, (p.z / 8.0) as u32)
    }

    /// A Terminal (`gggg` yardmap, footprint 4 → 8×8 squares) closes its
    /// squares — dilated by the mover's footprint — for the pathfinder,
    /// paths route round it, and its death reopens them.
    #[test]
    fn structure_mask_blocks_paths_and_clears_on_death() {
        let mut h = Harness::flat();
        let centre = Vec3::new(800.0, 0.0, 600.0);
        let terminal = h.spawn_structure(UnitKind::Terminal, 1, centre);
        h.step();
        let rev = h.world.resource::<NavGridSet>().revision;
        assert!(mask_at(&h, 1, false, centre));
        // LIGHT (xsizeh 1) is kept a square further out, MEDIUM three.
        let edge = centre + Vec3::new(32.0 + 4.0, 0.0, 0.0);
        assert!(mask_at(&h, 1, false, edge));
        assert!(!mask_at(&h, 1, false, edge + Vec3::new(16.0, 0.0, 0.0)));
        assert!(mask_at(&h, 3, false, edge + Vec3::new(16.0, 0.0, 0.0)));

        let bit = h.spawn(UnitKind::Bit, 0, Vec3::new(700.0, 0.0, 600.0));
        h.step();
        h.world
            .entity_mut(bit)
            .insert(MoveTarget(Vec3::new(900.0, 0.0, 600.0)));
        h.step();
        let path = h.world.get::<MovePath>(bit).unwrap().clone();
        assert!(path.waypoints.len() > 2, "detours: {:?}", path.waypoints);
        for w in path.waypoints.windows(2) {
            let nav = h.world.resource::<NavGridSet>();
            assert!(
                nav.line_clear(1.0, 1, 40.0, w[0].xz(), w[1].xz()),
                "segment {w:?} crosses it"
            );
        }

        h.world.entity_mut(terminal).insert(Dying { timer: 1.0 });
        h.step();
        assert!(!mask_at(&h, 1, false, centre), "reopened after death");
        assert!(h.world.resource::<NavGridSet>().revision > rev);
    }

    /// Why: stamps were cleared from `Added<Dying>` and
    /// `RemovedComponents<UnitType>` read in `FixedUpdate`. A building
    /// without death choreography gets `Dying` and is despawned by
    /// `cleanup_dying` in the same tick (so `Added<Dying>` never sees
    /// it), and removal messages are dropped after two rendered frames —
    /// with no sim tick in between (any frame rate above ~60 fps) the
    /// despawn was never seen either, leaving a phantom obstacle for the
    /// rest of the match. Same for a despawn without `Dying` (the Exploit
    /// packing up).
    #[test]
    fn structure_unstamped_when_despawned_between_ticks() {
        let mut h = Harness::flat();
        let a = Vec3::new(800.0, 0.0, 600.0);
        let b = Vec3::new(1200.0, 0.0, 600.0);
        let killed = h.spawn_structure(UnitKind::Terminal, 1, a);
        let packed = h.spawn_structure(UnitKind::Terminal, 1, b);
        h.step();
        assert!(mask_at(&h, 1, false, a) && mask_at(&h, 1, false, b));

        // `death_system` then `cleanup_dying`, one tick.
        let mut commands = h.world.commands();
        commands.entity(killed).insert(Dying { timer: 0.0 });
        commands.entity(killed).despawn();
        commands.entity(packed).despawn();
        h.world.flush();
        // Two rendered frames pass before the next sim tick.
        h.world.clear_trackers();
        h.world.clear_trackers();
        h.step();
        assert!(!mask_at(&h, 1, false, a), "killed building reopened");
        assert!(!mask_at(&h, 1, false, b), "packed-up building reopened");
        let layer = &h.world.resource::<NavGridSet>().structures;
        assert!(layer.stamps.is_empty());
        assert!(layer.solid.iter().all(|&n| n == 0), "no refcount left");
    }

    /// `CUnit::ForcedMove` re-blocks: a structure moved after it was
    /// stamped (Hex Farm's void push) blocks its new squares only.
    #[test]
    fn moved_structure_is_restamped() {
        let mut h = Harness::flat();
        let from = Vec3::new(800.0, 0.0, 600.0);
        let to = Vec3::new(1200.0, 0.0, 600.0);
        let terminal = h.spawn_structure(UnitKind::Terminal, 1, from);
        h.step();
        let rev = h.world.resource::<NavGridSet>().revision;
        h.world.get_mut::<Transform>(terminal).unwrap().translation = to;
        h.step();
        assert!(!mask_at(&h, 1, false, from), "old squares reopened");
        assert!(mask_at(&h, 1, false, to), "new squares blocked");
        assert!(h.world.resource::<NavGridSet>().revision > rev);
    }

    /// Bad Block (a crushable feature, resistance 65.6): HEAVY movers
    /// (Byte, CrushStrength 300) path straight through and crush it on
    /// contact; LIGHT (40) path round it and leave it standing.
    #[test]
    fn heavy_crushes_badblock_light_is_blocked() {
        let block_at = Vec3::new(804.0, 0.0, 604.0);
        let goal = Vec3::new(900.0, 0.0, 604.0);

        let mut h = Harness::flat();
        let bb = h.spawn_structure(UnitKind::BadBlock, 1, block_at);
        let bit = h.spawn(UnitKind::Bit, 0, Vec3::new(700.0, 0.0, 604.0));
        h.step();
        assert!(mask_at(&h, 1, false, block_at));
        assert!(!mask_at(&h, 3, true, block_at), "HEAVY class ignores it");
        h.world.entity_mut(bit).insert(MoveTarget(goal));
        for _ in 0..300 {
            h.step();
        }
        assert!(h.world.get::<Dying>(bb).is_none(), "a Bit can't crush it");
        assert!(
            h.pos(bit).xz().distance(goal.xz()) < 20.0,
            "went round it: {}",
            h.pos(bit)
        );

        let mut h = Harness::flat();
        let bb = h.spawn_structure(UnitKind::BadBlock, 1, block_at);
        let byte = h.spawn(UnitKind::Byte, 0, Vec3::new(700.0, 0.0, 604.0));
        h.step();
        h.world.entity_mut(byte).insert(MoveTarget(goal));
        h.step();
        assert_eq!(
            h.world.get::<MovePath>(byte).unwrap().waypoints.len(),
            2,
            "straight through"
        );
        let mut crushed = false;
        for _ in 0..300 {
            h.step();
            if h.world.get_entity(bb).is_err() || h.world.get::<Dying>(bb).is_some() {
                crushed = true;
                break;
            }
        }
        assert!(crushed, "the Byte crushes the Bad Block");
    }
}
