//! Right-click movement orders: single-click moves the selection to a point,
//! right-drag samples a path and distributes units along it. Shows a
//! formation preview during the drag and a move-target torus on release.
//! A right-click that lands on an enemy unit issues an attack order instead.

use std::collections::HashMap;

use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use super::core::{Selected, SelectionSet, ground_hit, unit_hit};
use crate::interaction::movement::{
    AttackMoveActive, CommandQueue, GuardTarget, MovePath, MoveTarget, QueuedCommand,
};
use crate::rendering::camera::RtsCamera;
use crate::units::combat::AttackTargetOrder;
use crate::units::components::{Faction, TeamId, UnitStats, UnitType, is_friendly};
use crate::units::content::unit_registry::UnitRegistry;

pub(super) struct RightClickPlugin;

impl Plugin for RightClickPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RightDragPath>()
            .init_resource::<PendingMoveIndicators>()
            .add_systems(
                Update,
                (
                    handle_right_click,
                    spawn_move_indicator_visuals.after(handle_right_click),
                    update_formation_preview.after(handle_right_click),
                    decay_move_indicators,
                )
                    .in_set(SelectionSet::RightClick),
            );
    }
}

/// Marker for temporary move-target indicators on the ground.
#[derive(Component)]
pub struct MoveIndicator {
    pub lifetime: Timer,
}

/// Marker for ephemeral formation preview dots shown during right-drag.
#[derive(Component)]
pub struct FormationPreview;

/// Despawn move indicators after their lifetime expires.
fn decay_move_indicators(
    time: Res<Time>,
    mut query: Query<(Entity, &mut MoveIndicator)>,
    mut commands: Commands,
) {
    for (entity, mut indicator) in &mut query {
        indicator.lifetime.tick(time.delta());
        if indicator.lifetime.is_finished() {
            commands.entity(entity).despawn();
        }
    }
}

/// Minimum distance between sampled path points (world units).
const PATH_SAMPLE_MIN_DISTANCE: f32 = 20.0;

/// Tracks right-click drag path for move commands.
#[derive(Resource, Default)]
pub struct RightDragPath {
    /// World-space points sampled along the drag path.
    points: Vec<Vec3>,
    /// Whether we're actively dragging.
    active: bool,
}

/// Order-commit marker kinds, rendered as colored torus rings at the
/// clicked point / target unit. Colors mirror the command-line palette in
/// `movement::draw_selected_command_lines` so a click's ring matches the
/// line color Spring draws for that order kind.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum OrderMarker {
    Move,
    Attack,
    Patrol,
    Guard,
    /// Manual target designation (`T`).
    Target,
}

impl OrderMarker {
    fn color(self) -> Color {
        match self {
            OrderMarker::Move => Color::srgb(0.2, 1.0, 0.3),
            OrderMarker::Attack => Color::srgb(1.0, 0.3, 0.25),
            OrderMarker::Patrol => Color::srgb(0.4, 0.7, 1.0),
            OrderMarker::Guard => Color::srgb(0.85, 0.95, 1.0),
            OrderMarker::Target => Color::srgb(1.0, 0.65, 0.2),
        }
    }
}

/// Buffered order-commit markers written by `handle_right_click` (and the
/// ability order modes), consumed by `spawn_move_indicator_visuals`.
/// Separated into two systems because `MeshRayCast` holds
/// `Res<Assets<Mesh>>` which conflicts with `ResMut`.
#[derive(Resource, Default)]
pub struct PendingMoveIndicators {
    pub markers: Vec<(Vec3, OrderMarker)>,
}

/// Grouped lookup queries for `handle_right_click`, keeping the system
/// under Bevy's 16-parameter limit (adding the UI-interaction guard and
/// target-type lookup pushed the flat signature to 17).
#[derive(bevy::ecs::system::SystemParam)]
#[allow(clippy::type_complexity)]
struct RightClickLookups<'w, 's> {
    windows: Query<'w, 's, &'static Window>,
    camera_q: Query<'w, 's, (&'static Camera, &'static GlobalTransform), With<RtsCamera>>,
    unit_root_q: Query<'w, 's, Entity, With<UnitType>>,
    parent_q: Query<'w, 's, &'static ChildOf>,
    unit_info_q: Query<'w, 's, (&'static TeamId, &'static Faction)>,
    target_gtf_q: Query<'w, 's, &'static GlobalTransform>,
    target_type_q: Query<'w, 's, &'static UnitType, Without<Selected>>,
    move_target_q: Query<'w, 's, (), With<MoveTarget>>,
    ui_interactions: Query<'w, 's, &'static Interaction>,
}

/// Right-click: single click moves all selected to one point — unless the
/// click lands on an enemy unit, in which case every selected *armed* unit
/// attacks it instead (Spring's default attack order). Right-drag: sample a
/// path, distribute selected units along it on release.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn handle_right_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut ray_cast: MeshRayCast,
    selected_q: Query<(Entity, &Transform, &UnitType, &UnitStats), With<Selected>>,
    lookups: RightClickLookups,
    unit_registry: Res<UnitRegistry>,
    mut commands: Commands,
    mut drag_path: ResMut<RightDragPath>,
    mut pending: ResMut<PendingMoveIndicators>,
) {
    let RightClickLookups {
        windows,
        camera_q,
        unit_root_q,
        parent_q,
        unit_info_q,
        target_gtf_q,
        target_type_q,
        move_target_q,
        ui_interactions,
    } = lookups;
    if mouse.just_pressed(MouseButton::Right) {
        // A press that starts over a live UI node (minimap, order
        // palette, HUD) belongs to the UI. Without this guard the
        // cursor's ray still reaches the terrain underneath the panel
        // and the player right-clicking the minimap quietly issues a
        // move order to the world point below it.
        if ui_interactions
            .iter()
            .any(|i| matches!(i, Interaction::Pressed | Interaction::Hovered))
        {
            return;
        }

        drag_path.points.clear();
        drag_path.active = true;

        if let Some(point) = ground_hit(&windows, &camera_q, &mut ray_cast) {
            drag_path.points.push(point);
        }
    }

    if mouse.pressed(MouseButton::Right)
        && drag_path.active
        && let Some(point) = ground_hit(&windows, &camera_q, &mut ray_cast)
    {
        let dominated = drag_path
            .points
            .last()
            .is_some_and(|last| last.distance(point) < PATH_SAMPLE_MIN_DISTANCE);
        if !dominated {
            drag_path.points.push(point);
        }
    }

    if mouse.just_released(MouseButton::Right) && drag_path.active {
        drag_path.active = false;

        let mut units: Vec<(Entity, Vec3)> = selected_q
            .iter()
            .map(|(e, tf, _, _)| (e, tf.translation))
            .collect();
        if units.is_empty() || drag_path.points.is_empty() {
            return;
        }

        let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

        if drag_path.points.len() == 1 {
            // Single-point order. First check for a right-click ON a unit:
            // an enemy under the cursor turns the order into an attack for
            // every selected armed unit (unarmed units move as usual).
            let clicked_enemy =
                unit_hit(&windows, &camera_q, &mut ray_cast, &unit_root_q, &parent_q).filter(
                    |&target| {
                        unit_info_q
                            .get(target)
                            .ok()
                            .zip(units.first().and_then(|(e, _)| unit_info_q.get(*e).ok()))
                            .is_some_and(|((t_team, t_faction), (m_team, m_faction))| {
                                !is_friendly(m_team.0, *m_faction, t_team.0, *t_faction)
                            })
                    },
                );

            if let Some(target_unit) = clicked_enemy {
                let target_pos = target_gtf_q
                    .get(target_unit)
                    .map(|t| t.translation())
                    .unwrap_or(drag_path.points[0]);
                let mut any_armed = false;
                for (entity, _, unit, _) in &selected_q {
                    if unit_registry.weapon(unit.0).is_empty() {
                        continue;
                    }
                    // Upstream `OnlyTargetCategory1` gates explicit
                    // attack orders too — a DOS physically cannot
                    // attack a building, so issuing one is refused and
                    // the unit holds position rather than chasing.
                    let target_kind = target_type_q.get(target_unit).map(|ut| ut.0);
                    if let Ok(target_kind) = target_kind
                        && !unit_registry.can_attack(unit.0, target_kind)
                    {
                        continue;
                    }
                    any_armed = true;
                    if shift && move_target_q.contains(entity) {
                        // Shift-queue: walk to the target's current position
                        // first, then attack it (the chase retargets live).
                        commands
                            .entity(entity)
                            .entry::<CommandQueue>()
                            .or_default()
                            .and_modify(move |mut queue: Mut<CommandQueue>| {
                                queue.push(QueuedCommand::AttackUnit {
                                    target: target_unit,
                                    pos: target_pos,
                                });
                            });
                    } else {
                        issue_attack_order(entity, target_unit, &mut commands);
                    }
                }
                if any_armed {
                    pending.markers.push((target_pos, OrderMarker::Attack));
                    drag_path.points.clear();
                    return;
                }
            }

            // Plain ground move: fan the group out around the clicked
            // point (single units go exactly there).
            let target = drag_path.points[0];
            let max_radius = selected_q
                .iter()
                .map(|(_, _, _, stats)| stats.radius)
                .fold(0.0_f32, f32::max);
            let targets = spread_targets(target, units.len(), 1.6 * max_radius);
            for ((entity, _), slot) in units.iter().zip(targets.iter()) {
                apply_ordered_command(
                    *entity,
                    QueuedCommand::Move(*slot),
                    shift,
                    &move_target_q,
                    &mut commands,
                );
            }
            pending
                .markers
                .extend(targets.into_iter().map(|t| (t, OrderMarker::Move)));
        } else {
            // Path-based formation: sort units by projection onto the drag's
            // principal axis (start → end) so the nearest-to-start unit gets
            // the first target. This keeps movement lines roughly parallel
            // instead of letting arbitrary ECS ordering cause paths to cross.
            let path_start = *drag_path.points.first().unwrap();
            let path_end = *drag_path.points.last().unwrap();
            let axis = Vec3::new(path_end.x - path_start.x, 0.0, path_end.z - path_start.z);
            let axis_len_sq = axis.length_squared();

            if axis_len_sq > 0.01 {
                units.sort_by(|(_, a), (_, b)| {
                    let pa = (Vec3::new(a.x - path_start.x, 0.0, a.z - path_start.z)).dot(axis);
                    let pb = (Vec3::new(b.x - path_start.x, 0.0, b.z - path_start.z)).dot(axis);
                    pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                });
            }

            let targets = sample_path_evenly(&drag_path.points, units.len());
            for ((entity, _), target) in units.iter().zip(targets.iter()) {
                apply_ordered_command(
                    *entity,
                    QueuedCommand::Move(*target),
                    shift,
                    &move_target_q,
                    &mut commands,
                );
            }
            pending
                .markers
                .extend(targets.into_iter().map(|t| (t, OrderMarker::Move)));
        }

        drag_path.points.clear();
    }
}

/// Replace a unit's current order with an attack on `target`. Mirrors the
/// replace branch of [`apply_ordered_command`] for every order component
/// attack supersedes. The explicit attack also supersedes a manual (T)
/// target designation.
fn issue_attack_order(entity: Entity, target: Entity, commands: &mut Commands) {
    super::super::clear_orders(&mut commands.entity(entity)).insert(AttackTargetOrder { target });
}

/// Apply a positional command to a unit, either replacing its current order
/// or enqueuing it behind the currently running order (and any already queued).
pub(crate) fn apply_ordered_command(
    entity: Entity,
    cmd: QueuedCommand,
    enqueue: bool,
    move_target_q: &Query<(), With<MoveTarget>>,
    commands: &mut Commands,
) {
    if enqueue && move_target_q.contains(entity) {
        // Unit has an active order — append to (or create) the queue.
        commands
            .entity(entity)
            .entry::<CommandQueue>()
            .or_default()
            .and_modify(move |mut queue: Mut<CommandQueue>| {
                queue.push(cmd);
            });
    } else {
        // Replace (or enqueue with no active order): install as active order,
        // reset the queue, and invalidate any computed path. For BuildAt
        // orders we also stamp PendingBuild so the construction system can
        // pick the unit up once it has arrived at the site. Any stale
        // PendingBuild from a previous order is cleared on plain moves.
        let mut ec = commands.entity(entity);
        ec.insert(MoveTarget(cmd.position()))
            .insert(CommandQueue::default())
            .remove::<MovePath>()
            .remove::<crate::units::combat::AttackGroundOrder>()
            .remove::<crate::units::combat::AttackTargetOrder>()
            .remove::<GuardTarget>()
            .remove::<AttackMoveActive>();
        match cmd {
            QueuedCommand::BuildAt { kind, site } => {
                ec.insert(crate::units::lifecycle::construction::PendingBuild { kind, site });
            }
            QueuedCommand::Move(_) | QueuedCommand::Patrol(_) | QueuedCommand::Guard(_) => {
                ec.remove::<crate::units::lifecycle::construction::PendingBuild>();
            }
            QueuedCommand::AttackMove(_) => {
                ec.remove::<crate::units::lifecycle::construction::PendingBuild>()
                    .insert(AttackMoveActive);
            }
            QueuedCommand::AttackUnit { target, .. } => {
                ec.remove::<crate::units::lifecycle::construction::PendingBuild>()
                    .remove::<MoveTarget>()
                    .remove::<crate::units::combat::ForcedTarget>()
                    .insert(crate::units::combat::AttackTargetOrder { target });
            }
        }
    }
}

/// Drains `PendingMoveIndicators` and creates torus visuals colored by
/// order kind (move green / attack red / patrol blue / guard white).
///
/// Separated from `handle_right_click` because `MeshRayCast` holds an
/// immutable `Res<Assets<Mesh>>` that conflicts with `ResMut<Assets<Mesh>>`.
#[allow(clippy::type_complexity)]
fn spawn_move_indicator_visuals(
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut mesh_cache: Local<Option<Handle<Mesh>>>,
    mut material_cache: Local<HashMap<OrderMarker, Handle<StandardMaterial>>>,
) {
    if pending.markers.is_empty() {
        return;
    }

    let mesh = mesh_cache
        .get_or_insert_with(|| meshes.add(Torus::new(2.0, 4.0)))
        .clone();

    for (target, marker) in pending.markers.drain(..) {
        let material = material_cache
            .entry(marker)
            .or_insert_with(|| {
                let color = marker.color();
                materials.add(StandardMaterial {
                    base_color: color.with_alpha(0.6),
                    emissive: LinearRgba::from(color) * 2.0,
                    unlit: true,
                    alpha_mode: AlphaMode::Blend,
                    ..default()
                })
            })
            .clone();
        commands.spawn((
            MoveIndicator {
                lifetime: Timer::from_seconds(1.5, TimerMode::Once),
            },
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material),
            Transform::from_translation(target + Vec3::Y * 1.0)
                .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        ));
    }
}

/// Shared assets for formation preview indicators.
#[derive(Resource, Clone)]
pub(crate) struct FormationPreviewAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// Show/update/remove preview dots during a right-drag formation draw.
fn update_formation_preview(
    drag_path: Res<RightDragPath>,
    selected_q: Query<Entity, With<Selected>>,
    preview_q: Query<Entity, With<FormationPreview>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    assets: Option<Res<FormationPreviewAssets>>,
) {
    // If drag is not active or path has fewer than 2 points, despawn any existing previews.
    let unit_count = selected_q.iter().count();
    if !drag_path.active || drag_path.points.len() < 2 || unit_count == 0 {
        for entity in &preview_q {
            commands.entity(entity).despawn();
        }
        return;
    }

    // Lazy-init shared assets.
    let preview_assets = if let Some(a) = assets {
        a.clone()
    } else {
        let a = FormationPreviewAssets {
            mesh: meshes.add(Torus::new(1.5, 3.0)),
            material: materials.add(StandardMaterial {
                base_color: Color::srgba(1.0, 1.0, 0.3, 0.4),
                emissive: LinearRgba::new(1.0, 1.0, 0.3, 1.0) * 1.5,
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                ..default()
            }),
        };
        commands.insert_resource(a.clone());
        a
    };

    let targets = sample_path_evenly(&drag_path.points, unit_count);

    // Despawn old previews and spawn fresh ones.
    for entity in &preview_q {
        commands.entity(entity).despawn();
    }
    for target in &targets {
        commands.spawn((
            FormationPreview,
            Mesh3d(preview_assets.mesh.clone()),
            MeshMaterial3d(preview_assets.material.clone()),
            Transform::from_translation(*target + Vec3::Y * 1.0)
                .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        ));
    }
}

/// Sunflower-spiral slot layout for a multi-unit move to a single
/// point: slot 0 sits on the point, slot i at `r = spacing·√i`,
/// `θ = i·goldenAngle`. Neighbor slots stay roughly `spacing` apart
/// at any unit count, so a group ordered to one spot forms a packed
/// disc instead of a shove-match that only the collision push can
/// untangle. Units sorted by the caller land on slots in ECS order;
/// the spiral keeps them apart either way.
fn spread_targets(center: Vec3, count: usize, spacing: f32) -> Vec<Vec3> {
    const GOLDEN_ANGLE: f32 = 2.399_963_2;
    (0..count)
        .map(|i| {
            if i == 0 {
                return center;
            }
            let i = i as f32;
            let r = spacing * i.sqrt();
            let theta = i * GOLDEN_ANGLE;
            center + Vec3::new(r * theta.cos(), 0.0, r * theta.sin())
        })
        .collect()
}

fn sample_path_evenly(path: &[Vec3], count: usize) -> Vec<Vec3> {
    if count == 0 || path.is_empty() {
        return vec![];
    }
    if count == 1 {
        return vec![*path.last().unwrap()];
    }

    let mut cumulative = vec![0.0f32];
    for i in 1..path.len() {
        let prev = cumulative[i - 1];
        cumulative.push(prev + path[i - 1].distance(path[i]));
    }
    let total_length = *cumulative.last().unwrap();

    if total_length < 0.01 {
        return vec![path[0]; count];
    }

    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let target_dist = (i as f32 / (count - 1) as f32) * total_length;

        let seg = cumulative
            .windows(2)
            .position(|w| w[0] <= target_dist && target_dist <= w[1])
            .unwrap_or(path.len() - 2);

        let seg_start_dist = cumulative[seg];
        let seg_length = cumulative[seg + 1] - seg_start_dist;
        let t = if seg_length > 0.0 {
            (target_dist - seg_start_dist) / seg_length
        } else {
            0.0
        };

        result.push(path[seg].lerp(path[seg + 1], t));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_loading::TerrainChunkMarker;
    use crate::units::content::definitions::UnitKind;
    use bevy::camera::primitives::Aabb;
    use bevy::camera::visibility::SetViewVisibility;
    use bevy::camera::{ComputedCameraValues, RenderTargetInfo, Viewport};
    use bevy::ecs::system::RunSystemOnce;
    use bevy::math::primitives::Cuboid;
    use bevy::math::{DVec2, Vec3A};
    use bevy::mesh::Mesh;

    /// Headless scene: 100x100 viewport with the cursor dead-centre,
    /// camera 100 units above the origin looking straight down, and a
    /// wide flat terrain box — so a right-click ray hits (0, 0.5, 0).
    fn world_with_camera_and_terrain() -> World {
        // `cast_ray` culls candidates with `par_iter`.
        bevy::tasks::ComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);

        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<ButtonInput<MouseButton>>();
        world.init_resource::<ButtonInput<KeyCode>>();
        world.insert_resource(UnitRegistry::empty());
        world.init_resource::<RightDragPath>();
        world.init_resource::<PendingMoveIndicators>();

        let mut window = Window::default();
        window.set_physical_cursor_position(Some(DVec2::new(50.0, 50.0)));
        world.spawn(window);
        world.spawn((
            RtsCamera,
            Camera {
                viewport: Some(Viewport {
                    physical_position: UVec2::ZERO,
                    physical_size: UVec2::splat(100),
                    ..default()
                }),
                computed: ComputedCameraValues {
                    target_info: Some(RenderTargetInfo {
                        physical_size: UVec2::splat(100),
                        scale_factor: 1.0,
                    }),
                    ..default()
                },
                ..default()
            },
            GlobalTransform::from(
                Transform::from_xyz(0.0, 100.0, 0.0).looking_at(Vec3::ZERO, Vec3::Z),
            ),
        ));

        let terrain_mesh = world
            .resource_mut::<Assets<Mesh>>()
            .add(Mesh::from(Cuboid::new(200.0, 1.0, 200.0)));
        let terrain = world
            .spawn((
                TerrainChunkMarker,
                Mesh3d(terrain_mesh),
                Transform::default(),
                GlobalTransform::default(),
                InheritedVisibility::VISIBLE,
                ViewVisibility::default(),
                Aabb {
                    center: Vec3A::ZERO,
                    half_extents: Vec3A::new(100.0, 0.5, 100.0),
                },
            ))
            .id();
        {
            let mut vv = world.get_mut::<ViewVisibility>(terrain).unwrap();
            vv.set_visible();
        }
        world
    }

    /// Right-click on open ground moves the selection to the point
    /// under the cursor.
    #[test]
    fn world_right_click_moves_selection() {
        let mut world = world_with_camera_and_terrain();
        world.spawn((
            UnitType(UnitKind::Bit),
            Selected,
            UnitStats {
                radius: 12.0,
                hit_radius: 20.0,
                speed: 90.0,
                accel: 27.0,
                brake: 60.0,
                turn_rate: 3.0,
                can_fly: false,
                cruise_alt: 0.0,
                no_chase_vtol: true,
            },
            Transform::from_xyz(10.0, 0.0, 10.0),
        ));

        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        world.run_system_once(handle_right_click).unwrap();

        assert!(
            world.resource::<RightDragPath>().active,
            "press on open ground must start a drag",
        );

        world.resource_mut::<ButtonInput<MouseButton>>().clear();
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .release(MouseButton::Right);
        world.run_system_once(handle_right_click).unwrap();

        let target = world
            .query_filtered::<&MoveTarget, With<UnitType>>()
            .single(&world)
            .unwrap()
            .0;
        // Ray lands on the terrain box top (y=0.5) at the cursor's
        // world point — dead centre of the viewport.
        assert!((target - Vec3::new(0.0, 0.5, 0.0)).length() < 1.0);
    }

    /// The sunflower spread packs any unit count into a disc whose
    /// slots stay roughly one spacing apart, and sends slot 0 exactly
    /// to the clicked point.
    #[test]
    fn spread_targets_form_a_packed_disc() {
        let center = Vec3::new(500.0, 2.0, -300.0);
        let slots = spread_targets(center, 16, 24.0);

        assert_eq!(slots.len(), 16);
        assert_eq!(slots[0], center, "slot 0 takes the clicked point");

        // Everything stays inside the spiral's bounding radius, on the
        // ground plane (Y untouched).
        let max_r = 24.0 * (15.0 as f32).sqrt();
        for slot in &slots {
            assert!((slot.y - center.y).abs() < 1e-5);
            assert!(
                slot.distance(center) <= max_r + 1e-3,
                "slot {slot:?} escapes the disc radius {max_r}"
            );
        }

        // No two slots closer than half a spacing — the point of the
        // fan-out is not stacking the army.
        for (i, a) in slots.iter().enumerate() {
            for b in &slots[i + 1..] {
                let d = a.distance(*b);
                assert!(d >= 12.0, "slots {i} and {} are only {d} apart", i + 1);
            }
        }
    }

    /// Regression: a right-click that starts over a live UI node (e.g.
    /// the minimap) must not leak a move order to the terrain hidden
    /// underneath the panel.
    #[test]
    fn ui_right_click_issues_no_move_order() {
        let mut world = world_with_camera_and_terrain();
        world.spawn((
            UnitType(UnitKind::Bit),
            Selected,
            UnitStats {
                radius: 12.0,
                hit_radius: 20.0,
                speed: 90.0,
                accel: 27.0,
                brake: 60.0,
                turn_rate: 3.0,
                can_fly: false,
                cruise_alt: 0.0,
                no_chase_vtol: true,
            },
            Transform::from_xyz(10.0, 0.0, 10.0),
        ));
        // The UI element under the cursor.
        world.spawn(Interaction::Hovered);

        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        world.run_system_once(handle_right_click).unwrap();

        assert!(
            !world.resource::<RightDragPath>().active,
            "press over UI must not start a drag",
        );

        world.resource_mut::<ButtonInput<MouseButton>>().clear();
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .release(MouseButton::Right);
        world.run_system_once(handle_right_click).unwrap();

        assert!(
            world
                .query_filtered::<Entity, With<MoveTarget>>()
                .iter(&world)
                .next()
                .is_none(),
            "no move order may leak through a UI click",
        );
    }
}
