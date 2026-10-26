//! Translucency on top of the shared model materials: the emerge
//! fade-in of factory products and nanoframes
//! (`production::install_fade_materials`) and the half-alpha look of
//! friendly cloaked units (`cloak::install_cloak_fade_materials`).
//!
//! A unit's pieces share one (model, faction) material, so fading one
//! unit means dressing its pieces in a translucent variant of it and
//! putting the shared handle back afterwards. The variants are shared
//! too ([`FadeMaterialCache`]): one per source material and alpha step,
//! minted once and kept, so a fade swaps handles and never adds or
//! writes a material asset — each of those invalidates the bindless
//! material slab and re-uploads it, which a factory turning out units
//! non-stop did every frame. [`ModelMaterials::fade`] is the one walk
//! over a unit's piece tree both callers use.

use std::collections::HashMap;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::animation::PieceIndex;
use crate::interaction::selection::highlight::OriginalMaterial;
use crate::sim::DetFloat;

/// Marks the per-unit node every top-level model piece hangs under
/// (the 180° yaw that turns S3O +Z front into Bevy's −Z forward,
/// `spawning::spawn_unit_facing`). It carries no mesh and no
/// [`PieceIndex`], so a walk that only descends through pieces stops
/// at it — which is how the fade walks once missed every model piece.
#[derive(Component)]
pub struct ModelRoot;

/// Alpha steps of the fade variants: a fade-in moves through them one
/// by one, 1/32 of the way each, finer than the eye follows.
pub const FADE_STEPS: u8 = 32;

/// The step nearest `alpha`.
pub fn fade_step(alpha: f32) -> u8 {
    (alpha.det_clamp(0.0, 1.0) * f32::from(FADE_STEPS)).round() as u8
}

/// The translucent variants of the shared model materials, by source
/// material and alpha step ([`fade_step`]). The sources are the
/// app-lifetime unit materials (`meshes::unit_material`), so the
/// variants live as long.
#[derive(Resource, Default)]
pub struct FadeMaterialCache {
    variants: HashMap<(AssetId<StandardMaterial>, u8), Handle<StandardMaterial>>,
    /// Each variant's source and step, for a recolour mid-fade.
    sources: HashMap<AssetId<StandardMaterial>, (Handle<StandardMaterial>, u8)>,
}

impl FadeMaterialCache {
    /// `source` at alpha step `step`, blended; `None` if the source
    /// asset is gone.
    pub fn variant(
        &mut self,
        source: &Handle<StandardMaterial>,
        step: u8,
        materials: &mut Assets<StandardMaterial>,
    ) -> Option<Handle<StandardMaterial>> {
        if let Some(handle) = self.variants.get(&(source.id(), step)) {
            return Some(handle.clone());
        }
        let src = materials.get(source)?;
        let alpha = f32::from(step) / f32::from(FADE_STEPS);
        let handle = materials.add(StandardMaterial {
            base_color: src.base_color.with_alpha(alpha),
            base_color_texture: src.base_color_texture.clone(),
            emissive: src.emissive,
            alpha_mode: AlphaMode::Blend,
            unlit: src.unlit,
            ..default()
        });
        self.variants.insert((source.id(), step), handle.clone());
        self.sources.insert(handle.id(), (source.clone(), step));
        Some(handle)
    }

    /// The source and step `material` is a variant of, if it is one.
    pub fn source_of(
        &self,
        material: &Handle<StandardMaterial>,
    ) -> Option<(Handle<StandardMaterial>, u8)> {
        self.sources.get(&material.id()).cloned()
    }
}

/// Each faded piece and the shared material it gets back
/// ([`restore_model_materials`]).
pub type FadeOverrides = Vec<(Entity, Handle<StandardMaterial>)>;

/// Interior nodes of a unit's model: its [`ModelRoot`] and every piece.
/// Selection volumes, shield shells, health bars and other unit
/// children are neither, so they keep their own materials.
type ModelNode = Or<(With<PieceIndex>, With<ModelRoot>)>;

/// The queries one fade walk needs.
#[derive(SystemParam)]
pub struct ModelMaterials<'w, 's> {
    nodes: Query<'w, 's, &'static Children, ModelNode>,
    leaves: Query<'w, 's, &'static MeshMaterial3d<StandardMaterial>, With<PieceIndex>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    cache: ResMut<'w, FadeMaterialCache>,
}

impl ModelMaterials<'_, '_> {
    /// Dress every model piece under `unit_children` in the
    /// [`FadeMaterialCache`] variant of its material at `alpha`.
    pub fn fade(
        &mut self,
        unit_children: &Children,
        alpha: f32,
        commands: &mut Commands,
    ) -> FadeOverrides {
        let step = fade_step(alpha);
        let mut overrides = Vec::new();
        let mut stack: Vec<Entity> = unit_children.iter().collect();
        while let Some(node) = stack.pop() {
            // Descend through the model root and nested pieces so deep
            // s3o hierarchies (gunbase → gun → gunpoint) all fade.
            if let Ok(children) = self.nodes.get(node) {
                stack.extend(children.iter());
            }
            let Ok(mat_handle) = self.leaves.get(node) else {
                continue;
            };
            let original = mat_handle.0.clone();
            let Some(faded) = self.cache.variant(&original, step, &mut self.materials) else {
                continue;
            };
            commands.entity(node).insert(MeshMaterial3d(faded));
            overrides.push((node, original));
        }
        overrides
    }
}

/// What a model piece wears, and what its hover highlight puts back.
pub type WornMaterials<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut MeshMaterial3d<StandardMaterial>,
        Option<&'static mut OriginalMaterial>,
    ),
>;

/// Dress `piece` in `material`, in place. A highlighted piece keeps its
/// bright variant and takes `material` as what the highlight restores:
/// then this is `true`, and the caller drops the unit's `Highlighted`
/// so the highlight brightens the new material instead.
pub fn wear(piece: Entity, material: Handle<StandardMaterial>, worn: &mut WornMaterials) -> bool {
    match worn.get_mut(piece) {
        Ok((_, Some(mut original))) => {
            original.0 = material;
            true
        }
        Ok((mut wearing, None)) => {
            wearing.0 = material;
            false
        }
        Err(_) => false,
    }
}

/// Put each piece's shared material back (pieces that died in the
/// meantime are skipped).
pub fn restore_model_materials(
    overrides: &[(Entity, Handle<StandardMaterial>)],
    commands: &mut Commands,
) {
    for (piece, original) in overrides {
        commands
            .entity(*piece)
            .try_insert(MeshMaterial3d(original.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// The spawn layout: unit → model root → piece → nested piece, plus
    /// a non-model child with its own material (a shield shell).
    struct Layout {
        unit: Entity,
        pieces: [Entity; 2],
        other: Entity,
        shared: Handle<StandardMaterial>,
    }

    fn spawn_layout(world: &mut World) -> Layout {
        let shared = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let unit = world.spawn_empty().id();
        let root = world.spawn((ModelRoot, ChildOf(unit))).id();
        let body = world
            .spawn((PieceIndex, MeshMaterial3d(shared.clone()), ChildOf(root)))
            .id();
        let gun = world
            .spawn((PieceIndex, MeshMaterial3d(shared.clone()), ChildOf(body)))
            .id();
        let other = world
            .spawn((MeshMaterial3d(shared.clone()), ChildOf(unit)))
            .id();
        Layout {
            unit,
            pieces: [body, gun],
            other,
            shared,
        }
    }

    fn fade_unit(world: &mut World, unit: Entity, alpha: f32) -> FadeOverrides {
        world
            .run_system_once(
                move |mut mats: ModelMaterials,
                      children: Query<&Children>,
                      mut commands: Commands| {
                    mats.fade(children.get(unit).unwrap(), alpha, &mut commands)
                },
            )
            .unwrap()
    }

    fn wearing(world: &World, piece: Entity) -> Handle<StandardMaterial> {
        world
            .get::<MeshMaterial3d<StandardMaterial>>(piece)
            .unwrap()
            .0
            .clone()
    }

    /// Regression: the walk started at the unit's children and only
    /// descended through `PieceIndex` nodes, so it stopped at the model
    /// root and never faded a single S3O piece.
    #[test]
    fn fade_reaches_pieces_under_the_model_root_only() {
        let mut world = World::new();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<FadeMaterialCache>();
        let layout = spawn_layout(&mut world);

        let overrides = fade_unit(&mut world, layout.unit, 0.25);

        assert_eq!(overrides.len(), 2);
        let faded = wearing(&world, layout.pieces[0]);
        assert_eq!(wearing(&world, layout.pieces[1]), faded, "one variant");
        let mats = world.resource::<Assets<StandardMaterial>>();
        assert_eq!(mats.get(&faded).unwrap().base_color.alpha(), 0.25);
        // Not a model piece: untouched.
        assert_eq!(wearing(&world, layout.other), layout.shared);

        world
            .run_system_once(move |mut commands: Commands| {
                restore_model_materials(&overrides, &mut commands);
            })
            .unwrap();
        for piece in layout.pieces {
            assert_eq!(wearing(&world, piece), layout.shared);
        }
    }

    /// Two units fading at the same alpha wear the same variant, and
    /// fading adds no material once the variant exists.
    #[test]
    fn fades_share_their_variants() {
        let mut world = World::new();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<FadeMaterialCache>();
        let a = spawn_layout(&mut world);
        fade_unit(&mut world, a.unit, 0.5);
        let count = world.resource::<Assets<StandardMaterial>>().len();
        let b = {
            let unit = world.spawn_empty().id();
            let root = world.spawn((ModelRoot, ChildOf(unit))).id();
            let piece = world
                .spawn((PieceIndex, MeshMaterial3d(a.shared.clone()), ChildOf(root)))
                .id();
            (unit, piece)
        };
        fade_unit(&mut world, b.0, 0.5);
        assert_eq!(world.resource::<Assets<StandardMaterial>>().len(), count);
        assert_eq!(wearing(&world, b.1), wearing(&world, a.pieces[0]));
        let cache = world.resource::<FadeMaterialCache>();
        let (source, step) = cache.source_of(&wearing(&world, b.1)).unwrap();
        assert_eq!((source, step), (a.shared.clone(), FADE_STEPS / 2));
    }
}
