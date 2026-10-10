//! Right-click movement orders: single-click moves the selection to a point,
//! right-drag samples a path and distributes units along it. Shows a
//! formation preview during the drag and a move-target torus on release.
//! A right-click that lands on an enemy unit issues an attack order instead.

use std::collections::HashMap;

use bevy::prelude::*;

use super::core::{PickRayCast, Selected, SelectionSet, ground_hit, unit_hit};
use crate::interaction::movement::{CommandQueue, MoveTarget, Orders, QueuedCommand};
use crate::rendering::camera::RtsCamera;
use crate::units::components::{TeamId, UnitStats, UnitType, is_friendly};
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

/// Squared screen distance (pixels²) the cursor must travel before the
/// drag adds a formation node (CustomFormations2 `MouseMove`'s
/// `totaldxy > 40`). A press that never moves this far is a plain
/// click, however far the cursor's ground point moved, so a zoomed-out
/// click with a pixel of jitter doesn't become a formation line.
const NODE_SCREEN_STEP_SQ: f32 = 40.0;

/// Waypoint spacing when a single unit is given a drawn path
/// (CustomFormations2 `minPathSpacingSq = 50 * 50`).
const SINGLE_UNIT_PATH_SPACING: f32 = 50.0;

/// Tracks right-click drag path for move commands.
#[derive(Resource, Default)]
pub struct RightDragPath {
    /// World-space points sampled along the drag path.
    points: Vec<Vec3>,
    /// Whether we're actively dragging.
    active: bool,
    /// Cursor position (window pixels) where the last node was added.
    last_cursor: Option<Vec2>,
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
/// Separated into two systems because `PickRayCast` holds
/// `Res<Assets<Mesh>>` which conflicts with `ResMut`.
#[derive(Resource, Default)]
pub struct PendingMoveIndicators {
    pub markers: Vec<(Vec3, OrderMarker)>,
}

/// Grouped lookup queries for `handle_right_click`, keeping the system
/// under Bevy's 16-parameter limit.
#[derive(bevy::ecs::system::SystemParam)]
#[allow(clippy::type_complexity)]
struct RightClickLookups<'w, 's> {
    windows: Query<'w, 's, &'static Window>,
    camera_q: Query<'w, 's, (&'static Camera, &'static GlobalTransform), With<RtsCamera>>,
    unit_root_q: Query<'w, 's, Entity, With<UnitType>>,
    parent_q: Query<'w, 's, &'static ChildOf>,
    unit_info_q: Query<'w, 's, &'static TeamId>,
    target_gtf_q: Query<'w, 's, &'static GlobalTransform>,
    target_type_q: Query<'w, 's, &'static UnitType, Without<Selected>>,
    orders: Orders<'w, 's>,
    queue_end_q: Query<'w, 's, (Option<&'static MoveTarget>, Option<&'static CommandQueue>)>,
}

/// Right-click: single click moves all selected to one point — unless the
/// click lands on an enemy unit, in which case every selected *armed* unit
/// attacks it instead (Spring's default attack order). Right-drag: sample a
/// path, distribute selected units along it on release.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn handle_right_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut ray_cast: PickRayCast,
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
        orders,
        queue_end_q,
    } = lookups;
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    if mouse.just_pressed(MouseButton::Right) {
        drag_path.points.clear();
        drag_path.active = true;
        drag_path.last_cursor = cursor;

        if let Some(point) = ground_hit(&windows, &camera_q, &mut ray_cast) {
            drag_path.points.push(point);
        }
    }

    // A node per stretch of cursor travel on screen (`MouseMove`); on
    // the release frame too (`MouseRelease` adds the release point to a
    // drawn line), but a release never turns a click into a line.
    let released = mouse.just_released(MouseButton::Right);
    let moved_enough = match (cursor, drag_path.last_cursor) {
        (Some(c), Some(last)) => c.distance_squared(last) > NODE_SCREEN_STEP_SQ,
        _ => false,
    };
    if (mouse.pressed(MouseButton::Right) || (released && drag_path.points.len() > 1))
        && drag_path.active
        && moved_enough
        && let Some(point) = ground_hit(&windows, &camera_q, &mut ray_cast)
    {
        drag_path.points.push(point);
        drag_path.last_cursor = cursor;
    }

    if released && drag_path.active {
        drag_path.active = false;

        // Only units that can carry out a move take part
        // (CustomFormations2 `commandApplies`: `canMove`); a selected
        // building would claim a formation node and leave a gap.
        let units: Vec<(Entity, Vec3)> = selected_q
            .iter()
            .filter(|(_, _, _, stats)| stats.speed > 0.0)
            .map(|(e, tf, _, _)| (e, tf.translation))
            .collect();
        if selected_q.is_empty() || drag_path.points.is_empty() {
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
                            .zip(
                                selected_q
                                    .iter()
                                    .next()
                                    .and_then(|(e, ..)| unit_info_q.get(e).ok()),
                            )
                            .is_some_and(|(t_team, m_team)| !is_friendly(m_team.0, t_team.0))
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
                    // Shift queues the attack behind the current orders
                    // (the chase retargets live when it is promoted).
                    apply_ordered_command(
                        entity,
                        QueuedCommand::AttackUnit {
                            target: target_unit,
                            pos: target_pos,
                        },
                        shift,
                        &orders,
                        &mut commands,
                    );
                }
                if any_armed {
                    pending.markers.push((target_pos, OrderMarker::Attack));
                    drag_path.points.clear();
                    return;
                }
            }

            // Plain ground move: every unit gets the clicked point, as a
            // single right-click does in Spring (KP's CustomFormations2
            // widget passes single clicks straight to the engine). The
            // group spreads out by collision response and
            // `HandleUnitCollisionsAux`'s arrival rules.
            let target = drag_path.points[0];
            let movers: Vec<(Entity, Vec3, f32)> = selected_q
                .iter()
                .map(|(e, tf, _, stats)| (e, tf.translation, stats.radius))
                .collect();
            for (entity, slot) in group_move_slots(&movers, target) {
                apply_ordered_command(
                    entity,
                    QueuedCommand::Move(slot),
                    shift,
                    &orders,
                    &mut commands,
                );
            }
            pending.markers.push((target, OrderMarker::Move));
        } else if let [(entity, _)] = units[..] {
            // A single unit follows the drawn path itself: one move per
            // `minPathSpacing` of line, queued behind the first
            // (CustomFormations2's `draggingPath`).
            let waypoints = path_waypoints(&drag_path.points, SINGLE_UNIT_PATH_SPACING);
            if let Some((&first, rest)) = waypoints.split_first() {
                apply_ordered_command(
                    entity,
                    QueuedCommand::Move(first),
                    shift,
                    &orders,
                    &mut commands,
                );
                // Behind the first whatever `orders` said: it reads the
                // world before this frame's commands land.
                for &point in rest {
                    queue_command(entity, QueuedCommand::Move(point), &mut commands);
                }
            }
            pending
                .markers
                .extend(waypoints.into_iter().map(|t| (t, OrderMarker::Move)));
        } else if !units.is_empty() {
            // Drawn formation (KP `unit_customformations2.lua`): nodes
            // evenly spaced along the drag, matched to units by
            // `GetOrdersHungarian` (min total distance) up to 20 units,
            // `GetOrdersNoX` (greedy + uncross) beyond. Shift matches
            // from the end of each unit's queue.
            let from: Vec<Vec3> = units
                .iter()
                .map(|(e, p)| {
                    if shift {
                        queue_end_q.get(*e).ok().and_then(|(t, q)| {
                            q.and_then(|q| q.commands.last().map(|c| c.position()))
                                .or(t.map(|t| t.0))
                        })
                    } else {
                        None
                    }
                    .unwrap_or(*p)
                })
                .collect();
            let targets = sample_path_evenly(&drag_path.points, units.len());
            let assignment = assign_formation(&from, &targets);
            for (i, (entity, _)) in units.iter().enumerate() {
                apply_ordered_command(
                    *entity,
                    QueuedCommand::Move(targets[assignment[i]]),
                    shift,
                    &orders,
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

/// Give a unit a command: with `enqueue` (Shift) it goes behind
/// whatever the unit is doing (any active order or queued commands,
/// [`Orders::busy`]), otherwise it replaces the unit's orders — Spring's
/// `CCommandAI::GiveAllowedCommand` with and without `SHIFT_KEY`.
pub(crate) fn apply_ordered_command(
    entity: Entity,
    cmd: QueuedCommand,
    enqueue: bool,
    orders: &Orders,
    commands: &mut Commands,
) {
    if enqueue && orders.busy(entity) {
        queue_command(entity, cmd, commands);
    } else {
        // Replace (or enqueue with no active order): install as active
        // order behind an empty queue, invalidating any computed path.
        crate::interaction::replace_order(&mut commands.entity(entity), cmd);
    }
}

/// Append `cmd` to the unit's queue (creating it); `advance_command_queue`
/// promotes it when the orders ahead of it end. For follow-ups of a
/// command just given this frame, which [`Orders::busy`] can't see yet.
pub(crate) fn queue_command(entity: Entity, cmd: QueuedCommand, commands: &mut Commands) {
    commands
        .entity(entity)
        .entry::<CommandQueue>()
        .or_default()
        .and_modify(move |mut queue: Mut<CommandQueue>| {
            queue.push(cmd);
        });
}

/// Drains `PendingMoveIndicators` and creates torus visuals colored by
/// order kind (move green / attack red / patrol blue / guard white).
///
/// Separated from `handle_right_click` because its `PickRayCast` holds an
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

/// Show/update/hide preview dots during a right-drag formation draw.
///
/// The tori are pooled: a drag reuses the entities of the last one
/// (moving them, unhiding as many as the selection needs) and spawns
/// only when the selection outgrew the pool. Respawning them per frame
/// churned archetypes and render-world extraction for every torus.
fn update_formation_preview(
    drag_path: Res<RightDragPath>,
    selected_q: Query<&UnitStats, With<Selected>>,
    mut preview_q: Query<(&mut Transform, &mut Visibility), With<FormationPreview>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    assets: Option<Res<FormationPreviewAssets>>,
) {
    // If drag is not active or path has fewer than 2 points, hide any existing previews.
    // The units the release will order: the movers.
    let unit_count = selected_q.iter().filter(|s| s.speed > 0.0).count();
    if !drag_path.active || drag_path.points.len() < 2 || unit_count == 0 {
        for (_, mut vis) in &mut preview_q {
            vis.set_if_neq(Visibility::Hidden);
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

    let targets = if unit_count == 1 {
        path_waypoints(&drag_path.points, SINGLE_UNIT_PATH_SPACING)
    } else {
        sample_path_evenly(&drag_path.points, unit_count)
    };
    let placement = |target: &Vec3| {
        Transform::from_translation(*target + Vec3::Y * 1.0)
            .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2))
    };

    // Reuse pooled tori first, hide the surplus, spawn the shortfall.
    let mut targets = targets.iter();
    for (mut transform, mut vis) in &mut preview_q {
        match targets.next() {
            Some(target) => {
                transform.set_if_neq(placement(target));
                vis.set_if_neq(Visibility::Inherited);
            }
            None => {
                vis.set_if_neq(Visibility::Hidden);
            }
        }
    }
    for target in targets {
        commands.spawn((
            FormationPreview,
            Mesh3d(preview_assets.mesh.clone()),
            MeshMaterial3d(preview_assets.material.clone()),
            placement(target),
        ));
    }
}

/// Group move to one point: every unit is sent to `target` itself
/// (Spring's plain right-click), as `(unit, goal)`.
pub(crate) fn group_move_slots(units: &[(Entity, Vec3, f32)], target: Vec3) -> Vec<(Entity, Vec3)> {
    units.iter().map(|u| (u.0, target)).collect()
}

/// Unit count up to which the optimal assignment is used
/// (CustomFormations2 `defaultHungarianUnits`).
const MAX_HUNGARIAN_UNITS: usize = 20;

/// Match `units` (positions) to formation `nodes` one-to-one; returns
/// the node index per unit. Minimum total Euclidean distance
/// (`GetOrdersHungarian`) for small groups — a min-cost matching never
/// has two crossing paths, since uncrossing shortens both — and
/// `GetOrdersNoX` (each unit to its nearest free node, then swap any
/// pair whose straight paths cross until none do) for large ones.
pub(crate) fn assign_formation(units: &[Vec3], nodes: &[Vec3]) -> Vec<usize> {
    let n = units.len().min(nodes.len());
    // A non-finite distance (a NaN position) would make every
    // comparison in `hungarian` false and its search loop endless;
    // a large finite cost just ranks that pair last.
    let cost = |u: usize, t: usize| {
        let d = units[u].xz().distance(nodes[t].xz());
        if d.is_finite() { d } else { 1.0e12 }
    };
    if n <= MAX_HUNGARIAN_UNITS {
        hungarian(n, cost)
    } else {
        no_crossings(units, nodes)
    }
}

/// Min-cost perfect matching (Kuhn–Munkres with potentials, O(n³)).
/// `cost(row, col)`; returns the column assigned to each row.
fn hungarian(n: usize, cost: impl Fn(usize, usize) -> f32) -> Vec<usize> {
    let inf = f64::INFINITY;
    let mut u = vec![0.0f64; n + 1];
    let mut v = vec![0.0f64; n + 1];
    let mut p = vec![0usize; n + 1];
    let mut way = vec![0usize; n + 1];
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0usize;
        let mut minv = vec![inf; n + 1];
        let mut used = vec![false; n + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = inf;
            let mut j1 = 0usize;
            for j in 1..=n {
                if !used[j] {
                    let cur = cost(i0 - 1, j - 1) as f64 - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=n {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut out = vec![0usize; n];
    for j in 1..=n {
        if p[j] > 0 {
            out[p[j] - 1] = j - 1;
        }
    }
    out
}

/// Do segments `a0→a1` and `b0→b1` (XZ) properly cross?
fn segments_cross(a0: Vec2, a1: Vec2, b0: Vec2, b1: Vec2) -> bool {
    let orient = |p: Vec2, q: Vec2, r: Vec2| (q - p).perp_dot(r - p);
    let (d1, d2) = (orient(b0, b1, a0), orient(b0, b1, a1));
    let (d3, d4) = (orient(a0, a1, b0), orient(a0, a1, b1));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

/// `GetOrdersNoX`: nearest free node per unit, then swap crossing
/// pairs. Each swap strictly shortens the total length, so it
/// terminates; the pass count is capped like the widget's time cap.
fn no_crossings(units: &[Vec3], nodes: &[Vec3]) -> Vec<usize> {
    let n = units.len().min(nodes.len());
    let mut taken = vec![false; n];
    let mut m = vec![0usize; n];
    for (u, pos) in units.iter().enumerate().take(n) {
        let best = (0..n)
            .filter(|&t| !taken[t])
            .min_by(|&a, &b| {
                pos.xz()
                    .distance_squared(nodes[a].xz())
                    .total_cmp(&pos.xz().distance_squared(nodes[b].xz()))
            })
            .unwrap_or(0);
        taken[best] = true;
        m[u] = best;
    }
    for _ in 0..(n * n).max(1) {
        let mut swapped = false;
        for a in 0..n {
            for b in a + 1..n {
                if segments_cross(
                    units[a].xz(),
                    nodes[m[a]].xz(),
                    units[b].xz(),
                    nodes[m[b]].xz(),
                ) {
                    m.swap(a, b);
                    swapped = true;
                }
            }
        }
        if !swapped {
            break;
        }
    }
    m
}

/// A drawn path as one unit's waypoints: its start, then every point at
/// least `spacing` from the last waypoint kept.
fn path_waypoints(path: &[Vec3], spacing: f32) -> Vec<Vec3> {
    let mut out: Vec<Vec3> = Vec::new();
    for &p in path {
        if out
            .last()
            .is_none_or(|last| last.xz().distance(p.xz()) > spacing)
        {
            out.push(p);
        }
    }
    out
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
    use bevy::math::{DVec2, Vec3A};
    use bevy::mesh::Mesh;
    use bevy::shape::Cuboid;

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
                mid_y: 0.0,
                speed: 90.0,
                acc_rate: 0.03,
                dec_rate: 0.067,
                turn_rate: 3.0,
                can_fly: false,
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

    /// Formation assignment never sends two units along crossing
    /// paths — optimal for small groups, uncrossed for large ones — and
    /// uses every node once.
    #[test]
    fn formation_assignment_has_no_crossings() {
        let mut seed = 7u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        for n in [5usize, 16, 40] {
            let units: Vec<Vec3> = (0..n)
                .map(|_| Vec3::new(rnd() * 200.0, 0.0, rnd() * 200.0))
                .collect();
            let line = [Vec3::new(400.0, 0.0, 0.0), Vec3::new(500.0, 0.0, 300.0)];
            let nodes = sample_path_evenly(&line, n);
            let m = assign_formation(&units, &nodes);
            let mut seen = m.clone();
            seen.sort();
            assert_eq!(seen, (0..n).collect::<Vec<_>>(), "a permutation");
            for a in 0..n {
                for b in a + 1..n {
                    assert!(
                        !segments_cross(
                            units[a].xz(),
                            nodes[m[a]].xz(),
                            units[b].xz(),
                            nodes[m[b]].xz()
                        ),
                        "n={n}: paths {a} and {b} cross"
                    );
                }
            }
        }
    }

    /// The optimal matcher finds the minimum-total-distance assignment.
    #[test]
    fn hungarian_is_optimal() {
        let units = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(20.0, 0.0, 0.0),
        ];
        let nodes = [
            Vec3::new(21.0, 0.0, 5.0),
            Vec3::new(1.0, 0.0, 5.0),
            Vec3::new(11.0, 0.0, 5.0),
        ];
        assert_eq!(assign_formation(&units, &nodes), vec![1, 2, 0]);
    }

    /// Shift appends behind any active order: an attack or a build in
    /// progress keeps running with the move queued after it (it used to
    /// be replaced unless the unit had a `MoveTarget`). Without an order
    /// or queue the command starts at once.
    #[test]
    fn shift_queues_behind_any_active_order() {
        use crate::units::combat::AttackTargetOrder;
        use crate::units::lifecycle::construction::Constructing;
        let mut world = World::new();
        let enemy = world.spawn_empty().id();
        let attacker = world.spawn(AttackTargetOrder { target: enemy }).id();
        let builder = world
            .spawn(Constructing {
                kind: UnitKind::BadBlock,
                site: Vec3::ZERO,
                building: None,
            })
            .id();
        let idle = world.spawn_empty().id();
        let goal = Vec3::new(50.0, 0.0, 50.0);
        world
            .run_system_once(move |orders: Orders, mut commands: Commands| {
                for e in [attacker, builder, idle] {
                    apply_ordered_command(
                        e,
                        QueuedCommand::Move(goal),
                        true,
                        &orders,
                        &mut commands,
                    );
                }
            })
            .unwrap();
        assert!(world.get::<AttackTargetOrder>(attacker).is_some());
        assert!(world.get::<Constructing>(builder).is_some());
        for e in [attacker, builder] {
            assert!(world.get::<MoveTarget>(e).is_none());
            let queue = world.get::<CommandQueue>(e).expect("queued");
            assert!(matches!(queue.commands[..], [QueuedCommand::Move(g)] if g == goal));
        }
        assert_eq!(world.get::<MoveTarget>(idle).map(|t| t.0), Some(goal));

        // Without Shift the order is replaced, build included.
        world
            .run_system_once(move |orders: Orders, mut commands: Commands| {
                apply_ordered_command(
                    builder,
                    QueuedCommand::Move(goal),
                    false,
                    &orders,
                    &mut commands,
                );
            })
            .unwrap();
        assert!(world.get::<Constructing>(builder).is_none());
        assert_eq!(world.get::<MoveTarget>(builder).map(|t| t.0), Some(goal));
    }

    /// A single unit's drawn path keeps a waypoint per 50 elmos of line.
    #[test]
    fn single_unit_path_spacing() {
        let line: Vec<Vec3> = (0..=10)
            .map(|i| Vec3::new(i as f32 * 20.0, 0.0, 0.0))
            .collect();
        let wps = path_waypoints(&line, SINGLE_UNIT_PATH_SPACING);
        let xs: Vec<f32> = wps.iter().map(|w| w.x).collect();
        assert_eq!(xs, [0.0, 60.0, 120.0, 180.0]);
    }

    /// A plain right-click sends every selected unit to the clicked
    /// point (Spring), not to invented slots.
    #[test]
    fn single_click_shares_the_goal() {
        let e = [
            Entity::from_raw_u32(1).unwrap(),
            Entity::from_raw_u32(2).unwrap(),
        ];
        let t = Vec3::new(5.0, 0.0, 9.0);
        let orders = group_move_slots(&[(e[0], Vec3::ZERO, 12.0), (e[1], Vec3::X, 12.0)], t);
        assert!(orders.iter().all(|(_, g)| *g == t));
    }
}
