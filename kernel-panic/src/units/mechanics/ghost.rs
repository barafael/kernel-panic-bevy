//! Ghosted buildings (`GhostedBuildings`, on by default): an enemy
//! building seen once stays where it was last seen until that spot is
//! seen again — and is drawn dimmed, grey and translucent
//! (`CUnitDrawer::DrawGhostedBuildings`: colour `0.6`).
//!
//! - [`Ghosted`] marks a building that is only remembered right now
//!   ([`cloak::update_fog_visibility`] keeps it up to date);
//!   [`install_ghost_materials`] dresses its pieces in the shared
//!   translucent variant of their material, [`restore_ghost_materials`]
//!   puts them back when it is seen again.
//! - A ghosted building that dies unseen leaves a [`DeadGhost`]: a
//!   visual copy of its model as it stood, which stays until a friendly
//!   unit sees the spot (`CUnitDrawerData::UpdateGhostedBuildings`),
//!   instead of vanishing the moment it dies (which would also show the
//!   death through the fog).
//!
//! Rendering only: nothing here reads or writes sim state.
//!
//! [`cloak::update_fog_visibility`]: super::cloak::update_fog_visibility

use bevy::prelude::*;

use super::cloak::{CloakFadeMaterials, FogEnabled};
use crate::units::assets::model_fade::{ModelMaterials, ModelRoot, restore_model_materials};
use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::production::PendingFadeInstall;
use crate::units::lifecycle::spawning::FadeMaterials;
use crate::units::player::LocalTeam;
use crate::units::teams::Alliances;

/// A building the local player remembers but does not see now.
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct Ghosted;

/// Alpha of a ghost's pieces.
pub const GHOST_ALPHA: f32 = 0.6;

/// Swap records of [`install_ghost_materials`].
#[derive(Component)]
pub struct GhostFadeMaterials {
    overrides: Vec<(Entity, Handle<StandardMaterial>)>,
}

/// A destroyed building's remembered model, until its spot is seen.
#[derive(Component, Clone, Copy, Debug)]
pub struct DeadGhost;

/// Dress a ghosted building in the translucent variant of its material.
/// Waits out another fade (emerge, cloak): they swap the same pieces.
#[allow(clippy::type_complexity)]
pub fn install_ghost_materials(
    mut commands: Commands,
    ghosts: Query<
        (Entity, &Children),
        (
            With<Ghosted>,
            Without<GhostFadeMaterials>,
            Without<CloakFadeMaterials>,
            Without<FadeMaterials>,
            Without<PendingFadeInstall>,
        ),
    >,
    mut model: ModelMaterials,
) {
    for (entity, children) in &ghosts {
        let overrides = model.fade(children, GHOST_ALPHA, &mut commands);
        commands
            .entity(entity)
            .insert(GhostFadeMaterials { overrides });
    }
}

/// A building in sight again wears its own material.
pub fn restore_ghost_materials(
    mut commands: Commands,
    seen: Query<(Entity, &GhostFadeMaterials), Without<Ghosted>>,
) {
    for (entity, fade) in &seen {
        restore_model_materials(&fade.overrides, &mut commands);
        commands.entity(entity).remove::<GhostFadeMaterials>();
    }
}

/// A ghosted building dies: its model stays behind as a [`DeadGhost`]
/// and the dying unit itself is hidden (its death is out of sight).
/// An observer: the unit and its pieces are still there when `Dying` is
/// added, even if the death pipeline despawns it the same tick.
#[allow(clippy::type_complexity)]
pub fn leave_dead_ghost(
    add: On<Add<Dying>>,
    ghosted: Query<(&GlobalTransform, &Children), With<Ghosted>>,
    models: Query<(), With<ModelRoot>>,
    mut commands: Commands,
) {
    let Ok((gtf, children)) = ghosted.get(add.entity) else {
        return;
    };
    let ghost = commands
        .spawn((DeadGhost, gtf.compute_transform(), Visibility::Visible))
        .id();
    for child in children.iter().filter(|&c| models.contains(c)) {
        commands
            .entity(child)
            .clone_and_spawn_with_opt_in(|builder| {
                builder
                    .allow::<(
                        Transform,
                        Visibility,
                        Mesh3d,
                        MeshMaterial3d<StandardMaterial>,
                        Children,
                        ChildOf,
                    )>()
                    .linked_cloning(true);
            })
            .insert(ChildOf(ghost));
    }
    commands.entity(add.entity).insert(Visibility::Hidden);
}

/// Seconds between scans for a seen spot.
const SCAN_INTERVAL: f32 = 0.25;

/// A dead ghost goes when a friendly unit sees its spot
/// (`InLos(gso->pos)`), and with the fog.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn clear_seen_dead_ghosts(
    time: Res<Time>,
    mut since: Local<f32>,
    fog: Res<FogEnabled>,
    player: Res<LocalTeam>,
    alliances: Res<Alliances>,
    registry: Res<UnitRegistry>,
    ghosts: Query<(Entity, &GlobalTransform), With<DeadGhost>>,
    viewers: Query<
        (&TeamId, &UnitType, &GlobalTransform),
        (
            Without<Dying>,
            Without<crate::units::lifecycle::spawning::Emerging>,
        ),
    >,
    mut commands: Commands,
) {
    *since += time.delta_secs();
    if *since < SCAN_INTERVAL {
        return;
    }
    *since = 0.0;
    for (ghost, gtf) in &ghosts {
        let seen = !fog.0
            || viewers.iter().any(|(team, kind, v)| {
                let sight = registry.sight_distance(kind.0);
                alliances.friendly(team.0, player.0)
                    && v.translation()
                        .xz()
                        .distance_squared(gtf.translation().xz())
                        <= sight * sight
            });
        if seen {
            commands.entity(ghost).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// A ghosted building that dies leaves a copy of its model (not of
    /// anything else) behind and hides itself; one in plain sight does
    /// not.
    #[test]
    fn a_ghosted_building_dying_leaves_its_model() {
        let mut world = World::new();
        world.add_observer(leave_dead_ghost);
        let building = |world: &mut World, ghosted: bool| {
            let unit = world
                .spawn((
                    Transform::from_xyz(7.0, 0.0, 0.0),
                    GlobalTransform::from_xyz(7.0, 0.0, 0.0),
                    Visibility::Visible,
                ))
                .id();
            if ghosted {
                world.entity_mut(unit).insert(Ghosted);
            }
            let root = world.spawn((ModelRoot, Transform::default())).id();
            let piece = world
                .spawn((
                    Mesh3d::default(),
                    MeshMaterial3d::<StandardMaterial>::default(),
                    Transform::from_xyz(0.0, 3.0, 0.0),
                ))
                .id();
            let other = world.spawn(Transform::default()).id();
            world.entity_mut(root).add_child(piece);
            world.entity_mut(unit).add_children(&[root, other]);
            unit
        };
        let seen = building(&mut world, false);
        let remembered = building(&mut world, true);
        world.flush();
        world.entity_mut(seen).insert(Dying { timer: 0.0 });
        assert_eq!(world.query::<&DeadGhost>().iter(&world).count(), 0);
        world.entity_mut(remembered).insert(Dying { timer: 0.0 });
        world.flush();
        let (ghost, tf) = world
            .query_filtered::<(Entity, &Transform), With<DeadGhost>>()
            .single(&world)
            .map(|(e, t)| (e, *t))
            .unwrap();
        assert_eq!(tf.translation.x, 7.0);
        let roots = world.get::<Children>(ghost).expect("model").len();
        assert_eq!(roots, 1, "only the model root, not the other child");
        let root = world.get::<Children>(ghost).unwrap()[0];
        let piece = world.get::<Children>(root).expect("pieces")[0];
        assert!(world.get::<Mesh3d>(piece).is_some());
        assert_eq!(world.get::<Transform>(piece).unwrap().translation.y, 3.0);
        assert_eq!(
            *world.get::<Visibility>(remembered).unwrap(),
            Visibility::Hidden
        );
    }

    /// A seen spot clears its ghost; an unseen one keeps it; no fog
    /// clears all.
    #[test]
    fn a_dead_ghost_stays_until_its_spot_is_seen() {
        use crate::units::content::definitions::UnitKind;
        let mut world = World::new();
        world.init_resource::<Time>();
        world
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        world.insert_resource(FogEnabled(true));
        world.insert_resource(LocalTeam(0));
        world.insert_resource(Alliances::from_seats([(0, 0), (1, 1)]));
        world.insert_resource(UnitRegistry::load());
        let near = world
            .spawn((DeadGhost, GlobalTransform::from_xyz(100.0, 0.0, 0.0)))
            .id();
        let far = world
            .spawn((DeadGhost, GlobalTransform::from_xyz(9000.0, 0.0, 0.0)))
            .id();
        world.spawn((
            UnitType(UnitKind::Bit),
            TeamId(0),
            GlobalTransform::from_xyz(0.0, 0.0, 0.0),
        ));
        world.run_system_once(clear_seen_dead_ghosts).unwrap();
        assert!(world.get_entity(near).is_err(), "seen: gone");
        assert!(world.get_entity(far).is_ok(), "unseen: stays");
        world.insert_resource(FogEnabled(false));
        world.run_system_once(clear_seen_dead_ghosts).unwrap();
        assert!(world.get_entity(far).is_err(), "no fog: gone");
    }
}
