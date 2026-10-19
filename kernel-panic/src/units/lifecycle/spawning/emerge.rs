//! "Emerging" lifecycle: fresh-built units rise out of the factory or
//! fade into view before becoming playable. `production_system` attaches
//! [`Emerging`] with a style matching the unit's faction; `emerge_system`
//! ticks it forward and removes the component when the animation
//! completes.

use crate::sim::DetFloat;
use bevy::prelude::*;

/// Marks a freshly-built unit that hasn't finished emerging from its
/// construction site. `emerge_system` ticks `remaining` toward 0 over
/// `total` seconds; how the visible model arrives depends on `style`.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct Emerging {
    /// Final Y coordinate the unit should reach when fully emerged.
    pub target_y: f32,
    /// Seconds remaining in the emerge animation.
    pub remaining: f32,
    /// Total duration of the emerge animation (used to compute lerp t).
    pub total: f32,
    /// World point the unit should walk to once it has emerged. `None` for
    /// stationary units that don't need to clear the factory.
    pub rally_point: Option<Vec3>,
    /// `CFactory::SendToEmptySpot`'s second waypoint: the free spot on
    /// the factory's exit arc, queued behind `rally_point` (the point
    /// just outside the factory).
    pub rally_then: Option<Vec3>,
    /// How the model becomes visible during the rise window.
    pub style: EmergeStyle,
}

/// Per-faction emergence visual.
///
/// - `Rise` — System units (Kernel-built). Spawn underground at
///   `target_y - EMERGE_DEPTH` and lerp Y up to surface, with their own
///   COB `Create()` script also moving the `base` piece up via
///   `BUILD_PERCENT_LEFT`.
/// - `Fade` — Hacker / Network units (Hole, Connection, Window, Port).
///   Spawn at surface but materialize via an alpha ramp on a per-unit
///   cloned material. Mirrors upstream's `lua_SetAlphaThreshold(255 → 0)`
///   pattern in bug.bos / packet.bos / connection.bos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmergeStyle {
    Rise,
    Fade,
}

/// An [`Emerging`] unit whose build clock its builder drives (Spring's
/// `AddBuildPower`): `emerge_system` leaves `remaining` alone and only
/// applies it, so the build stalls when the builder does — and
/// `construction::decay_nanoframes` winds it back once nobody builds.
#[derive(Component, Default, Debug)]
#[component(storage = "SparseSet")]
pub struct BuildDriven {
    /// Seconds since a builder last tended this frame (`lastNanoAdd`).
    pub unpowered: f32,
}

/// Per-unit faded material clones plus the per-piece originals to put
/// back when the entity finishes fading in. Spawned alongside
/// `Emerging { Fade }` so the per-unit alpha ramp doesn't bleed into the
/// shared faction-colored material.
#[derive(Component)]
pub struct FadeMaterials {
    /// The faded clones `emerge_system` ramps — one per *distinct*
    /// source material the unit's pieces used, shared by every piece
    /// that used it. A unit's pieces all carry the same (model,
    /// faction) material, so this is one handle: one clone and one
    /// material write (re-upload) per tick per unit, not one per piece.
    pub faded: Vec<Handle<StandardMaterial>>,
    /// (piece_entity, original) — the shared material each piece gets
    /// back once the fade completes.
    pub overrides: Vec<(Entity, Handle<StandardMaterial>)>,
}

/// Tick `Emerging` units forward. A `Rise`-style unit stands at its
/// final height from the first frame: Kernel Panic sets
/// `ShowNanoFrame=0` on every unit, so the engine draws the complete
/// model in place and all construction motion comes from the unit's own
/// script (`BUILD_PERCENT_LEFT` piece moves). `Fade` ramps per-piece
/// alpha. When the timer expires the component is removed, faded
/// materials are restored to the shared originals, and the unit gets its
/// rally-walk command if any.
#[allow(clippy::type_complexity)]
pub fn emerge_system(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(
        Entity,
        &mut Transform,
        &mut Emerging,
        Option<&FadeMaterials>,
        Has<crate::interaction::movement::MoveTarget>,
        Option<&crate::interaction::movement::CommandQueue>,
        Has<BuildDriven>,
        Option<&crate::units::lifecycle::production::NewUnitOrders>,
    )>,
    piece_mats: Query<&MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let dt = time.delta_secs();
    for (entity, mut transform, mut emerging, fade, has_move, queue, driven, factory_orders) in
        &mut q
    {
        if !driven {
            emerging.remaining = (emerging.remaining - dt).det_max(0.0);
        }
        // t goes 0 → 1 over the duration.
        let t = (1.0 - emerging.remaining / emerging.total).det_clamp(0.0, 1.0);

        match emerging.style {
            EmergeStyle::Rise => {
                if transform.translation.y != emerging.target_y {
                    transform.translation.y = emerging.target_y;
                }
            }
            EmergeStyle::Fade => {
                // Linear alpha ramp; pieces stay at surface y throughout.
                if let Some(fade) = fade {
                    for faded_handle in &fade.faded {
                        if let Some(mut mat) = materials.get_mut(faded_handle) {
                            mat.base_color = mat.base_color.with_alpha(t);
                        }
                    }
                }
            }
        }

        if emerging.remaining <= 0.0 {
            if matches!(emerging.style, EmergeStyle::Rise) {
                transform.translation.y = emerging.target_y;
            }
            // Restore the shared faction material on every piece we
            // overrode, so future asset swaps / faction recolors take
            // effect on this unit too. With the pieces back on their
            // originals and `FadeMaterials` gone, the faded clone's
            // last strong handle drops and the asset is freed.
            if let Some(fade) = fade {
                for (piece_entity, original) in &fade.overrides {
                    if piece_mats.get(*piece_entity).is_ok() {
                        commands
                            .entity(*piece_entity)
                            .insert(MeshMaterial3d(original.clone()));
                    }
                }
                commands.entity(entity).remove::<FadeMaterials>();
            }
            let (rally, then) = (emerging.rally_point, emerging.rally_then);
            commands.entity(entity).remove::<(
                Emerging,
                BuildDriven,
                crate::units::lifecycle::production::NewUnitOrders,
            )>();
            // `CFactory::AssignBuildeeOrders`: a unit with orders of its
            // own keeps them; else it leaves by the factory's front and
            // takes the factory's orders, or with none, goes to an empty
            // spot.
            let has_orders = has_move || queue.is_some_and(|q| !q.commands.is_empty());
            if !has_orders
                && let (Some(exit), Some(orders)) = (rally, factory_orders)
                && !orders.0.is_empty()
            {
                commands
                    .entity(entity)
                    .insert(crate::interaction::movement::MoveTarget(exit))
                    .remove::<crate::interaction::movement::MovePath>()
                    .insert(crate::interaction::movement::CommandQueue {
                        commands: orders_from_exit(exit, &orders.0),
                    });
            } else if let Some(target) = rally.filter(|_| !has_orders) {
                let mut unit = commands.entity(entity);
                unit.insert(crate::interaction::movement::MoveTarget(target))
                    .remove::<crate::interaction::movement::MovePath>();
                if let Some(then) = then {
                    unit.insert(crate::interaction::movement::CommandQueue {
                        commands: vec![crate::interaction::movement::QueuedCommand::Move(then)],
                    });
                }
            }
        }
    }
}

/// A new unit's queue from its factory's orders, for a unit that has
/// just walked out to `exit`. A patrol point becomes a patrol route from
/// where the orders before it end (`exit` for the first), as a patrol
/// click queued behind them would (`ability::patrol_commands`).
fn orders_from_exit(
    exit: Vec3,
    orders: &[crate::interaction::movement::QueuedCommand],
) -> Vec<crate::interaction::movement::QueuedCommand> {
    use crate::interaction::movement::QueuedCommand;
    let mut queue: Vec<QueuedCommand> = Vec::with_capacity(orders.len() + 1);
    for cmd in orders {
        match *cmd {
            QueuedCommand::Patrol(target) => {
                let leg = crate::interaction::ability::patrol_commands(
                    true,
                    &queue,
                    Some(exit),
                    exit,
                    target,
                );
                queue.extend(leg);
            }
            other => queue.push(other),
        }
    }
    queue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::movement::QueuedCommand;

    /// Moves and fights pass through; the first patrol pairs with where
    /// the orders before it end, a later one just adds its point.
    #[test]
    fn factory_orders_become_the_unit_queue() {
        let exit = Vec3::new(10.0, 0.0, 10.0);
        let a = Vec3::new(100.0, 0.0, 0.0);
        let b = Vec3::new(0.0, 0.0, 100.0);
        let c = Vec3::new(50.0, 0.0, 50.0);
        assert_eq!(
            orders_from_exit(exit, &[QueuedCommand::Patrol(a)]),
            vec![QueuedCommand::Patrol(exit), QueuedCommand::Patrol(a)]
        );
        assert_eq!(
            orders_from_exit(
                exit,
                &[
                    QueuedCommand::Move(a),
                    QueuedCommand::AttackMove(b),
                    QueuedCommand::Patrol(c),
                    QueuedCommand::Patrol(a),
                ]
            ),
            vec![
                QueuedCommand::Move(a),
                QueuedCommand::AttackMove(b),
                QueuedCommand::Patrol(b),
                QueuedCommand::Patrol(c),
                QueuedCommand::Patrol(a),
            ]
        );
    }
}
