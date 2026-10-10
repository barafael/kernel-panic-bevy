//! Per-unit translucency on top of the shared model materials: the
//! emerge fade-in of factory products and nanoframes
//! (`production::install_fade_materials`) and the half-alpha look of
//! friendly cloaked units (`cloak::install_cloak_fade_materials`).
//!
//! A unit's pieces share one (model, faction) material, so fading one
//! unit means giving its pieces a per-unit clone and putting the shared
//! handle back afterwards. [`ModelMaterials::fade`] is the one walk over
//! a unit's piece tree both callers use.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::animation::PieceIndex;

/// Marks the per-unit node every top-level model piece hangs under
/// (the 180° yaw that turns S3O +Z front into Bevy's −Z forward,
/// `spawning::spawn_unit_facing`). It carries no mesh and no
/// [`PieceIndex`], so a walk that only descends through pieces stops
/// at it — which is how the fade walks once missed every model piece.
#[derive(Component)]
pub struct ModelRoot;

/// What [`ModelMaterials::fade`] swapped: the per-unit clones it made
/// (one per distinct source material) and each piece's original
/// handle, to put back with [`restore_model_materials`].
pub struct FadedModel {
    pub faded: Vec<Handle<StandardMaterial>>,
    pub overrides: Vec<(Entity, Handle<StandardMaterial>)>,
}

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
}

impl ModelMaterials<'_, '_> {
    /// Give every model piece under `unit_children` a per-unit clone of
    /// its material with `alpha` and `AlphaMode::Blend`, one clone per
    /// *distinct* source material (a unit's pieces all share one, so one
    /// per unit — cloning per piece cost a Kernel 23 materials, each
    /// re-uploaded every tick of a fade).
    pub fn fade(
        &mut self,
        unit_children: &Children,
        alpha: f32,
        commands: &mut Commands,
    ) -> FadedModel {
        let mut faded: Vec<(AssetId<StandardMaterial>, Handle<StandardMaterial>)> = Vec::new();
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
            let shared = match faded.iter().find(|(id, _)| *id == original.id()) {
                Some((_, handle)) => handle.clone(),
                None => {
                    let Some(src) = self.materials.get(&original) else {
                        continue;
                    };
                    let clone = StandardMaterial {
                        base_color: src.base_color.with_alpha(alpha),
                        base_color_texture: src.base_color_texture.clone(),
                        emissive: src.emissive,
                        alpha_mode: AlphaMode::Blend,
                        unlit: src.unlit,
                        ..default()
                    };
                    let handle = self.materials.add(clone);
                    faded.push((original.id(), handle.clone()));
                    handle
                }
            };
            commands.entity(node).insert(MeshMaterial3d(shared));
            overrides.push((node, original));
        }
        FadedModel {
            faded: faded.into_iter().map(|(_, handle)| handle).collect(),
            overrides,
        }
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

    /// Regression: the walk started at the unit's children and only
    /// descended through `PieceIndex` nodes, so it stopped at the model
    /// root and never faded a single S3O piece.
    #[test]
    fn fade_reaches_pieces_under_the_model_root_only() {
        let mut world = World::new();
        world.init_resource::<Assets<StandardMaterial>>();
        let layout = spawn_layout(&mut world);
        let unit = layout.unit;

        let faded = world
            .run_system_once(
                move |mut mats: ModelMaterials,
                      children: Query<&Children>,
                      mut commands: Commands| {
                    mats.fade(children.get(unit).unwrap(), 0.25, &mut commands)
                },
            )
            .unwrap();

        assert_eq!(faded.faded.len(), 1, "one clone for the shared material");
        assert_eq!(faded.overrides.len(), 2);
        let clone = faded.faded[0].clone();
        for piece in layout.pieces {
            let mat = world
                .get::<MeshMaterial3d<StandardMaterial>>(piece)
                .unwrap();
            assert_eq!(mat.0, clone);
        }
        let mats = world.resource::<Assets<StandardMaterial>>();
        assert_eq!(mats.get(&clone).unwrap().base_color.alpha(), 0.25);
        // Not a model piece: untouched.
        let other = world.get::<MeshMaterial3d<StandardMaterial>>(layout.other);
        assert_eq!(other.unwrap().0, layout.shared);

        let overrides = faded.overrides;
        world
            .run_system_once(move |mut commands: Commands| {
                restore_model_materials(&overrides, &mut commands);
            })
            .unwrap();
        for piece in layout.pieces {
            let mat = world
                .get::<MeshMaterial3d<StandardMaterial>>(piece)
                .unwrap();
            assert_eq!(mat.0, layout.shared);
        }
    }
}
