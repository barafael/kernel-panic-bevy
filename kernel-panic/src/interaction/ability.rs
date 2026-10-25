//! Order modes and their map clicks.
//!
//! Every order hotkey (Stop, Attack, Move, Fight, Patrol, Guard, Enter,
//! self-destruct, and `D` for the abilities) goes through the command
//! panel's single activation path (`ui::hud::command_panel::activation`),
//! exactly like a click on the matching panel button. This module owns
//! the sticky [`OrderCursorModes`] they arm and the map-click handlers
//! that turn an armed mode into an order. The aimed abilities (NX Flag,
//! Infection, SIGTERM, Firewall, Dispatch) arm [`Mode::Ability`], and
//! the next ground click casts them there ([`cast_aimed_abilities`]);
//! Launch Mines and Deploy run at once.

use crate::map_loading::MatchScopeAppExt;
use bevy::prelude::*;

use super::movement::QueuedCommand;
use super::selection::{
    OrderMarker, PendingMoveIndicators, PickRayCast, Selected, SelectionSet, ground_hit, unit_hit,
};
use crate::interaction::cursor::{CursorKind, CursorRequest};
use crate::net::{BombardTarget, Issue, NetLen, NetPos, Order, SimCommand};
use crate::rendering::camera::RtsCamera;
use crate::sim::DetFloat;
use crate::terrain::heightmap::Heightmap;
use crate::ui::hud::command_panel::commands::{CmdId, can_attack};
use crate::units::components::{TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::teams::Alliances;

pub struct AbilityHotkeyPlugin;

impl Plugin for AbilityHotkeyPlugin {
    fn build(&self, app: &mut App) {
        // An armed order mode doesn't outlive its match.
        app.init_match_resource::<OrderCursorModes>()
            .init_match_resource::<AreaDrag>()
            .add_systems(
                Update,
                (
                    // The armed-mode click handlers run before selection and
                    // consume the press they commit, so the click never also
                    // reaches click-to-select (a plain order click used to
                    // deselect everything, a guard click select the guarded
                    // unit).
                    (
                        trigger_bombard_click,
                        trigger_ability_click,
                        trigger_patrol_click,
                        trigger_attack_press_release,
                        trigger_clear_press_release,
                        trigger_attack_move_click,
                        trigger_guard_click,
                        trigger_move_click,
                        trigger_set_target_click,
                    )
                        .before(SelectionSet::Select),
                    draw_area_drag_preview,
                    update_mode_cursor,
                ),
            );
    }
}

/// Sticky order-targeting modes armed from the hotkeys / command panel.
///
/// At most one mode is armed at a time. The armed mode forces the
/// cursor glyph and the next left-click is consumed by that mode's click
/// handler as an order for the selection:
/// - `AttackGround` (`A` / Attack button): fire at a static ground point.
/// - `AttackMove` (`F` / Fight button): march to a point, fighting en
///   route.
/// - `Patrol` (`P` / button): shuttle between the click point and where
///   the unit started.
/// - `Ability` (Ability buttons only — `D` casts at the cursor directly):
///   cast the selection's aimed abilities at the clicked point.
///
/// Modes are cleared by re-pressing the key, Escape, right-click (both
/// handled by the command panel's input, like Spring's `CGuiHandler`),
/// the committing click, or a Stop order.
#[derive(Resource, Default)]
pub struct OrderCursorModes {
    pub mode: Option<Mode>,
}

impl OrderCursorModes {
    pub fn any_active(&self) -> bool {
        self.mode.is_some()
    }

    pub fn is(&self, mode: Mode) -> bool {
        self.mode == Some(mode)
    }

    /// Arm exactly one mode, replacing any other (they share the cursor).
    pub fn arm(&mut self, mode: Mode) {
        self.mode = Some(mode);
    }

    /// Disarm every mode.
    pub fn clear(&mut self) {
        self.mode = None;
    }

    /// After a committing click: Shift keeps the mode armed.
    fn committed(&mut self, keys: &ButtonInput<KeyCode>) {
        if !shift_held(keys) {
            self.clear();
        }
    }
}

/// The sticky order-targeting modes, for [`OrderCursorModes::arm`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    AttackGround,
    AttackMove,
    Patrol,
    Guard,
    Move,
    SetTarget,
    /// The constructors' Clear (`E` / button): a feature under the click,
    /// or every one in a dragged disc.
    Clear,
    /// Cast the aimed abilities of the units this panel command lists
    /// (NX Flag, SIGTERM, Dispatch …), like a Spring command goes only
    /// to units that list it.
    Ability(CmdId),
}

impl Mode {
    /// The glyph the armed mode forces.
    pub(crate) fn cursor(self) -> CursorKind {
        match self {
            Mode::Patrol => CursorKind::Patrol,
            Mode::Guard => CursorKind::Defend,
            Mode::Move => CursorKind::Move,
            Mode::Clear => CursorKind::Reclamate,
            // The gadget commands' `cursor`: "Capture" for Bombard
            // (`bug_bombard.lua`) and the NX Flag (`specialattack.lua`),
            // "Unload units" for Dispatch (`network_dispatch.lua`),
            // "Guard" for the Firewall (`network_reflectorshield.lua`);
            // SIGTERM (`airstrike.lua`) and the Obelisk's Attack are
            // "Attack".
            Mode::Ability(CmdId::Bombard | CmdId::NxFlag) => CursorKind::Capture,
            Mode::Ability(CmdId::Dispatch) => CursorKind::Unload,
            Mode::Ability(CmdId::Firewall) => CursorKind::Defend,
            // Spring renders set-target with the attack crosshair too.
            Mode::AttackGround | Mode::AttackMove | Mode::SetTarget | Mode::Ability(_) => {
                CursorKind::Attack
            }
        }
    }
}

/// `CMouseHandler::dragCircleCommandThreshold`: a press that travels
/// farther than this on screen before its release is an area order.
pub const AREA_DRAG_THRESHOLD_PX: f32 = 4.0;

/// An Attack or Clear press in flight: where it landed, how far the
/// cursor has gone since, and the disc it has drawn
/// (`CGuiHandler::GetCommand`'s `CMDTYPE_ICON_UNIT_OR_AREA` case).
#[derive(Resource, Default, Debug)]
pub struct AreaDrag {
    pub mode: Option<Mode>,
    pub press_cursor: Vec2,
    /// The ground under the press: the disc's centre.
    pub center: Vec3,
    /// The disc's radius, the ground distance to the cursor.
    pub radius: f32,
    /// The cursor left the press by more than [`AREA_DRAG_THRESHOLD_PX`].
    pub is_area: bool,
}

/// What a press/drag/release in an area-capable mode came to.
struct AreaRelease {
    center: Vec3,
    /// The disc, when the press was dragged; else a click.
    area: Option<f32>,
    /// The ground under the release (a click's target).
    at: Vec3,
}

/// The press / drag / release of an area-capable armed mode, shared by
/// Attack and Clear: the press is taken (and the disc started) while
/// `mode` is armed, the disc follows the cursor, and the release
/// commits — or is swallowed when the mode was disarmed meanwhile.
/// `Some` on the committing release only.
#[allow(clippy::too_many_arguments)]
fn area_press_release(
    mode: Mode,
    mouse: &mut ButtonInput<MouseButton>,
    modes: &OrderCursorModes,
    drag: &mut AreaDrag,
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    ray_cast: &mut PickRayCast,
) -> Option<AreaRelease> {
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    if modes.is(mode) && mouse.just_pressed(MouseButton::Left) {
        let center = ground_hit(windows, camera_q, ray_cast)?;
        *drag = AreaDrag {
            mode: Some(mode),
            press_cursor: cursor.unwrap_or_default(),
            center,
            radius: 0.0,
            is_area: false,
        };
        consume_click(mouse);
        // A press and release inside one frame is a click.
        if !mouse.just_released(MouseButton::Left) {
            return None;
        }
    }
    if drag.mode != Some(mode) {
        return None;
    }
    if !modes.is(mode) {
        // Disarmed mid-press (Escape, right-click): the release still
        // belongs to the order, not to click-to-select.
        if mouse.just_released(MouseButton::Left) {
            mouse.clear_just_released(MouseButton::Left);
            drag.mode = None;
        }
        return None;
    }
    if let Some(c) = cursor
        && c.distance(drag.press_cursor) > AREA_DRAG_THRESHOLD_PX
    {
        drag.is_area = true;
    }
    let under = ground_hit(windows, camera_q, ray_cast);
    if drag.is_area
        && let Some(g) = under
    {
        drag.radius = g.xz().distance(drag.center.xz());
    }
    if !mouse.just_released(MouseButton::Left) {
        return None;
    }
    mouse.clear_just_released(MouseButton::Left);
    let done = std::mem::take(drag);
    Some(AreaRelease {
        center: done.center,
        area: (done.is_area && done.radius > 0.0).then_some(done.radius),
        at: under.unwrap_or(done.center),
    })
}

/// The lookups an area-capable click handler needs, under Bevy's
/// parameter cap.
#[derive(bevy::ecs::system::SystemParam)]
struct AreaClickLookups<'w, 's> {
    windows: Query<'w, 's, &'static Window>,
    camera_q: Query<'w, 's, (&'static Camera, &'static GlobalTransform), With<RtsCamera>>,
    unit_root_q: Query<'w, 's, Entity, With<UnitType>>,
    parent_q: Query<'w, 's, &'static ChildOf>,
    unit_info_q: Query<'w, 's, (&'static TeamId, &'static UnitType, &'static GlobalTransform)>,
    unit_registry: Res<'w, UnitRegistry>,
    alliances: Res<'w, Alliances>,
}

/// While a disc is being dragged: its rim on the ground, in the order's
/// colour (`CGuiHandler::DrawArea`).
fn draw_area_drag_preview(
    drag: Res<AreaDrag>,
    heightmap: Option<Res<Heightmap>>,
    mut gizmos: Gizmos<crate::interaction::movement::CommandLineGizmos>,
) {
    let Some(mode) = drag.mode.filter(|_| drag.is_area && drag.radius > 0.0) else {
        return;
    };
    let color = match mode {
        Mode::Clear => OrderMarker::Clear(None).color(),
        _ => OrderMarker::Attack.color(),
    };
    crate::interaction::movement::draw_ground_ring(
        &mut gizmos,
        drag.center,
        drag.radius,
        color,
        heightmap.as_deref(),
    );
}

/// Force the armed mode's cursor glyph. The high priority beats the
/// default context resolver; with nothing armed it regains control.
fn update_mode_cursor(modes: Res<OrderCursorModes>, mut request: ResMut<CursorRequest>) {
    if let Some(mode) = modes.mode {
        request.set(mode.cursor(), 10);
    }
}

/// Whether `kind`'s `D` ability is aimed at a map point: command-fire
/// casters (NX Flag, Infection, Protect, SIGTERM) and the packet
/// teleporters (Dispatch). Bug / Exploit deploy and the Byte's Launch
/// Mines need no target.
pub(crate) fn ability_is_aimed(kind: UnitKind) -> bool {
    (kind.has_command_fire_ability() && kind != UnitKind::Byte) || kind.is_teleporter()
}

/// Launch mines from every Byte in `units`. Upstream's Launch Mines is
/// an instant command (`CMDTYPE.ICON`): the mines go where the launcher
/// tubes point, so the cast's target is unused.
pub(crate) fn launch_mines(
    units: impl IntoIterator<Item = (Entity, UnitKind)>,
    net: &mut Issue,
    commands: &mut Commands,
) {
    let bytes: Vec<Entity> = units
        .into_iter()
        .filter(|(_, kind)| *kind == UnitKind::Byte)
        .map(|(e, _)| e)
        .collect();
    if bytes.is_empty() {
        return;
    }
    crate::audio::ack_order(commands);
    let units = net.units(bytes);
    net.issue(SimCommand::Cast {
        units,
        target: NetPos::quantize(Vec3::ZERO),
        auto: false,
    });
}

/// Cast the aimed abilities at `target` for every unit in `units`:
/// command-fire casters fire (range / approach handled by
/// `process_command_fire`), teleporters dispatch.
///
/// `alt_held` mirrors upstream `network_dispatch.lua`: the dispatch
/// order makes a pass on each of the teleporter's slow updates until
/// the team's Packet Buffer is empty, instead of stopping after one
/// (`DispatchOrder`, see `net::apply`).
pub(crate) fn cast_aimed_abilities(
    units: impl IntoIterator<Item = (Entity, UnitKind)>,
    target: Vec3,
    alt_held: bool,
    net: &mut Issue,
    commands: &mut Commands,
) {
    crate::audio::ack_order(commands);
    let units = net.units(
        units
            .into_iter()
            .filter(|(_, kind)| ability_is_aimed(*kind))
            .map(|(e, _)| e),
    );
    net.issue(SimCommand::Cast {
        units,
        target: NetPos::quantize(target),
        auto: alt_held,
    });
}

fn alt_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight)
}

fn shift_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
}

/// Shared click-handler prologue: `mode` armed *and* the committing
/// left-click, plus the ground point under the cursor. `None` = the
/// handler has nothing to do this frame (mode not armed, no click, or
/// the ray missed the terrain).
fn armed_ground_click(
    modes: &OrderCursorModes,
    mouse: &ButtonInput<MouseButton>,
    mode: Mode,
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    ray_cast: &mut PickRayCast,
) -> Option<Vec3> {
    if !modes.is(mode) || !mouse.just_pressed(MouseButton::Left) {
        return None;
    }
    ground_hit(windows, camera_q, ray_cast)
}

/// A click handler committed its order: the press is used up, so
/// selection (which runs after the handlers) never sees this click and
/// its release can't deselect the units just ordered.
fn consume_click(mouse: &mut ButtonInput<MouseButton>) {
    mouse.clear_just_pressed(MouseButton::Left);
}

/// Click handler for [`Mode::Ability`] (its button, `D` or `A`): the
/// next left-click on the ground casts the armed ability there for the
/// selected units that have it. Shift stays armed.
#[allow(clippy::too_many_arguments)]
fn trigger_ability_click(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
    mut commands: Commands,
) {
    let Some(Mode::Ability(cmd)) = modes.mode else {
        return;
    };
    // Bombard is an order with a target of its own (`trigger_bombard_click`).
    if cmd == CmdId::Bombard || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    let filter = cmd.ability_caster();
    cast_aimed_abilities(
        selected_q
            .iter()
            .map(|(e, u)| (e, u.0))
            .filter(|(_, k)| filter.is_none_or(|f| f(*k))),
        target,
        alt_held(&keys),
        &mut net,
        &mut commands,
    );
    let marker = if cmd == CmdId::Dispatch {
        OrderMarker::Dispatch
    } else {
        OrderMarker::Attack
    };
    pending.markers.push((target, marker));
    modes.committed(&keys);
    consume_click(&mut mouse);
}

/// Attack (`A` / button), `CMDTYPE_ICON_UNIT_OR_AREA` under KP's
/// `newAttackMode`: the press is taken while the mode is armed; a
/// release within [`AREA_DRAG_THRESHOLD_PX`] is a click — an enemy
/// under it is attacked (`AttackUnit`), else the ground there
/// (`AttackGround`; `attack_ground_system` moves the unit into range and
/// fires each reload) — and a dragged release is an area attack on
/// everything hostile in the disc (`AttackArea`, expanded when applied).
/// Every selected unit with the Attack command takes it (an armed,
/// non-command-fire primary weapon: a command only goes to units that
/// list it). Shift queues and stays in mode.
#[allow(clippy::too_many_arguments)]
fn trigger_attack_press_release(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType, &TeamId), With<Selected>>,
    lookups: AreaClickLookups,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut drag: ResMut<AreaDrag>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
) {
    let AreaClickLookups {
        windows,
        camera_q,
        unit_root_q,
        parent_q,
        unit_info_q,
        unit_registry,
        alliances,
    } = lookups;
    let Some(release) = area_press_release(
        Mode::AttackGround,
        &mut mouse,
        &modes,
        &mut drag,
        &windows,
        &camera_q,
        &mut ray_cast,
    ) else {
        return;
    };
    let queue = shift_held(&keys);
    let attackers = || {
        selected_q
            .iter()
            .filter(|(_, unit, _)| can_attack(unit.0))
            .map(|(e, ..)| e)
    };
    if let Some(radius) = release.area {
        let units = net.units(attackers());
        net.issue(SimCommand::Order {
            units,
            order: Order::AttackArea {
                center: NetPos::quantize(release.center),
                radius: NetLen::quantize(radius),
            },
            queue,
        });
        pending.markers.push((
            release.center,
            OrderMarker::AttackArea(radius.round().det_min(f32::from(u16::MAX)) as u16),
        ));
        modes.committed(&keys);
        return;
    }
    // A click: the enemy under it, else the ground (`GetCommand`'s
    // `unit && type != CMDTYPE_ICON_AREA` arm, enemies only here).
    let sel_team = selected_q.iter().next().map(|(_, _, t)| t.0);
    let enemy = unit_hit(&windows, &camera_q, &mut ray_cast, &unit_root_q, &parent_q)
        .and_then(|t| unit_info_q.get(t).ok().map(|info| (t, info)))
        .filter(|(_, (t_team, ..))| {
            sel_team.is_some_and(|mine| !alliances.friendly(mine, t_team.0))
        });
    match enemy {
        Some((target, (_, t_kind, t_gtf))) => {
            let units = net.units(attackers().filter(|e| {
                selected_q
                    .get(*e)
                    .is_ok_and(|(_, u, _)| unit_registry.can_attack(u.0, t_kind.0))
            }));
            if let Some(target) = net.id(target) {
                net.issue(SimCommand::Order {
                    units,
                    order: Order::AttackUnit(target),
                    queue,
                });
            }
            pending
                .markers
                .push((t_gtf.translation(), OrderMarker::Attack));
        }
        None => {
            let units = net.units(attackers());
            net.issue(SimCommand::Order {
                units,
                order: Order::AttackGround(NetPos::quantize(release.at)),
                queue,
            });
            pending.markers.push((release.at, OrderMarker::Attack));
        }
    }
    modes.committed(&keys);
}

/// Clear (`E` / button on a constructor), the builders' area-capable
/// `CMD_RECLAIM`: a click on a clearable feature clears that one, a
/// dragged disc every one inside it, nearest first. A click on nothing
/// clearable keeps the mode armed. Shift queues and stays in mode.
#[allow(clippy::too_many_arguments)]
fn trigger_clear_press_release(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    features: Query<(Entity, &crate::units::features::Feature, &Transform)>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut drag: ResMut<AreaDrag>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
) {
    let Some(release) = area_press_release(
        Mode::Clear,
        &mut mouse,
        &modes,
        &mut drag,
        &windows,
        &camera_q,
        &mut ray_cast,
    ) else {
        return;
    };
    let queue = shift_held(&keys);
    let constructors = net.units(
        selected_q
            .iter()
            .filter(|(_, unit)| unit.0.is_constructor())
            .map(|(e, _)| e),
    );
    if let Some(radius) = release.area {
        net.issue(SimCommand::Order {
            units: constructors,
            order: Order::ClearArea {
                center: NetPos::quantize(release.center),
                radius: NetLen::quantize(radius),
            },
            queue,
        });
        pending.markers.push((
            release.center,
            OrderMarker::Clear(Some(radius.round().det_min(f32::from(u16::MAX)) as u16)),
        ));
        modes.committed(&keys);
        return;
    }
    let Some(feature) = crate::units::features::clearable_feature_at(release.at, features.iter())
    else {
        return;
    };
    if let Some(target) = net.id(feature) {
        net.issue(SimCommand::Order {
            units: constructors,
            order: Order::ClearFeature(target),
            queue,
        });
    }
    let pos = features
        .get(feature)
        .map_or(release.at, |(_, _, tf)| tf.translation);
    pending.markers.push((pos, OrderMarker::Clear(None)));
    modes.committed(&keys);
}

/// Bombard (button on a Bug), `bug_bombard.lua`'s `CMDTYPE_ICON_UNIT_OR_MAP`:
/// the next left-click on an enemy or the ground sends every selected
/// Bug to walk within range, deploy and attack it; with Alt it only
/// deploys there. Shift queues and stays in mode.
#[allow(clippy::too_many_arguments)]
fn trigger_bombard_click(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType, &TeamId), With<Selected>>,
    lookups: AreaClickLookups,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
) {
    if !modes.is(Mode::Ability(CmdId::Bombard)) || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let AreaClickLookups {
        windows,
        camera_q,
        unit_root_q,
        parent_q,
        unit_info_q,
        alliances,
        ..
    } = lookups;
    let sel_team = selected_q.iter().next().map(|(_, _, t)| t.0);
    let enemy = unit_hit(&windows, &camera_q, &mut ray_cast, &unit_root_q, &parent_q)
        .and_then(|t| unit_info_q.get(t).ok().map(|info| (t, info)))
        .filter(|(_, (t_team, ..))| {
            sel_team.is_some_and(|mine| !alliances.friendly(mine, t_team.0))
        });
    let (target, at) = match enemy {
        Some((t, (_, _, gtf))) => match net.id(t) {
            Some(id) => (BombardTarget::Unit(id), gtf.translation()),
            None => return,
        },
        None => match ground_hit(&windows, &camera_q, &mut ray_cast) {
            Some(p) => (BombardTarget::Ground(NetPos::quantize(p)), p),
            None => return,
        },
    };
    let units = net.units(
        selected_q
            .iter()
            .filter(|(_, unit, _)| unit.0 == UnitKind::Bug)
            .map(|(e, ..)| e),
    );
    net.issue(SimCommand::Order {
        units,
        order: Order::Bombard {
            target,
            deploy_only: alt_held(&keys),
        },
        queue: shift_held(&keys),
    });
    pending.markers.push((at, OrderMarker::Bombard));
    modes.committed(&keys);
    consume_click(&mut mouse);
}

/// Click handler for the move mode: the next left-click issues a plain
/// move order. Shift queues it behind the active order and stays armed.
#[allow(clippy::too_many_arguments)]
fn trigger_move_click(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
    unit_registry: Res<UnitRegistry>,
) {
    let Some(target) = armed_ground_click(
        &modes,
        &mouse,
        Mode::Move,
        &windows,
        &camera_q,
        &mut ray_cast,
    ) else {
        return;
    };
    let units = mobile(&selected_q, &unit_registry, &net);
    net.issue(SimCommand::Order {
        units,
        order: Order::Move(NetPos::quantize(target)),
        queue: shift_held(&keys),
    });
    pending.markers.push((target, OrderMarker::Move));
    modes.committed(&keys);
    consume_click(&mut mouse);
}

/// Click handler for the set-target mode: the next left-click on an enemy
/// unit designates it as the selection's manual target. This is an aim
/// designation only — current orders are left untouched. Only units whose
/// weapon may target that unit take it (`OnlyTargetCategory1`: a DOS
/// can't be pointed at a building, as it can't be ordered to attack
/// one). Shift stays armed.
#[allow(clippy::too_many_arguments)]
fn trigger_set_target_click(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType, &TeamId), With<Selected>>,
    unit_root_q: Query<Entity, With<UnitType>>,
    parent_q: Query<&ChildOf>,
    unit_info_q: Query<(&TeamId, &UnitType)>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    target_gtf_q: Query<&GlobalTransform>,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
    unit_registry: Res<UnitRegistry>,
    alliances: Res<Alliances>,
) {
    if !modes.is(Mode::SetTarget) || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = unit_hit(&windows, &camera_q, &mut ray_cast, &unit_root_q, &parent_q) else {
        return;
    };
    // Manual fire must only ever designate hostiles — the forced-target
    // combat path bypasses the auto-targeter's friend filters.
    let Ok((t_team, t_kind)) = unit_info_q.get(target) else {
        return;
    };
    let Some(sel_team) = selected_q.iter().next().map(|(_, _, team)| team.0) else {
        return;
    };
    if alliances.friendly(sel_team, t_team.0) {
        return;
    }
    let units = net.units(
        selected_q
            .iter()
            .filter(|(_, unit, _)| {
                unit_registry.weapon(unit.0).is_some() && unit_registry.can_attack(unit.0, t_kind.0)
            })
            .map(|(e, ..)| e),
    );
    if let Some(target) = net.id(target) {
        net.issue(SimCommand::SetTarget { units, target });
    }
    if let Ok(t_gtf) = target_gtf_q.get(target) {
        pending
            .markers
            .push((t_gtf.translation(), OrderMarker::Target));
    }
    modes.committed(&keys);
    consume_click(&mut mouse);
}

/// Click handler: while [`Mode::Patrol`] is armed, the next left-click
/// issues a patrol order for every selected mobile unit: a route
/// between where the unit starts and the clicked point, walked as fight
/// legs forever (the patrol points cycle through the queue, see
/// [`QueuedCommand::Patrol`]). Shift adds the clicked point behind the
/// unit's orders instead (and stays armed for chain-patrolling).
#[allow(clippy::too_many_arguments)]
fn trigger_patrol_click(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
    unit_registry: Res<UnitRegistry>,
) {
    let Some(target) = armed_ground_click(
        &modes,
        &mouse,
        Mode::Patrol,
        &windows,
        &camera_q,
        &mut ray_cast,
    ) else {
        return;
    };
    // The route (where it starts, Shift appending) is worked out from
    // each unit's orders when the command is applied (`net::apply`).
    let units = mobile(&selected_q, &unit_registry, &net);
    net.issue(SimCommand::Order {
        units,
        order: Order::Patrol(NetPos::quantize(target)),
        queue: shift_held(&keys),
    });
    pending.markers.push((target, OrderMarker::Patrol));
    modes.committed(&keys);
    consume_click(&mut mouse);
}

/// The units of the selection that take moves, fights and patrols: the
/// mobile ones, and factories, which keep them for the units they build
/// (`UnitKind::takes_rally_orders`).
fn mobile(
    selected_q: &Query<(Entity, &UnitType), With<Selected>>,
    unit_registry: &UnitRegistry,
    net: &Issue,
) -> crate::net::Units {
    net.units(
        selected_q
            .iter()
            .filter(|(_, unit)| unit_registry.speed(unit.0) > 0.0 || unit.0.takes_rally_orders())
            .map(|(e, _)| e),
    )
}

/// The commands a patrol click to `target` gives one unit (Spring's
/// `CCommandAI::GiveAllowedCommand` for `CMD_PATROL`). A fresh patrol
/// (`append == false`) is just the leg to `target`; the caller queues
/// the route behind it. Appended behind other orders, the first patrol
/// point pairs with where those orders end, the last queued position,
/// else the active order's goal, else the unit itself, so the route
/// has two ends to shuttle between.
pub(crate) fn patrol_commands(
    append: bool,
    queue: &[QueuedCommand],
    active_goal: Option<Vec3>,
    unit_pos: Vec3,
    target: Vec3,
) -> Vec<QueuedCommand> {
    let has_patrol = queue.iter().any(|c| matches!(c, QueuedCommand::Patrol(_)));
    if !append || has_patrol {
        return vec![QueuedCommand::Patrol(target)];
    }
    let route_start = queue
        .last()
        .map(QueuedCommand::position)
        .or(active_goal)
        .unwrap_or(unit_pos);
    vec![
        QueuedCommand::Patrol(route_start),
        QueuedCommand::Patrol(target),
    ]
}

/// Click handler: while [`Mode::AttackMove`] is active, the
/// next left-click issues an attack-move order (march to the point,
/// engaging hostiles en route) for every selected mobile unit. Shift
/// queues the march behind the active order and stays armed.
#[allow(clippy::too_many_arguments)]
fn trigger_attack_move_click(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
    unit_registry: Res<UnitRegistry>,
) {
    let Some(target) = armed_ground_click(
        &modes,
        &mouse,
        Mode::AttackMove,
        &windows,
        &camera_q,
        &mut ray_cast,
    ) else {
        return;
    };
    let units = mobile(&selected_q, &unit_registry, &net);
    net.issue(SimCommand::Order {
        units,
        order: Order::AttackMove(NetPos::quantize(target)),
        queue: shift_held(&keys),
    });
    pending.markers.push((target, OrderMarker::Attack));
    modes.committed(&keys);
    consume_click(&mut mouse);
}

/// Click handler: while [`Mode::Guard`] is armed, the next
/// left-click on a friendly unit makes every selected mobile unit guard
/// it — trail it at close range while the auto-attack path defends it.
/// Shift queues the guard behind the current orders. Clicking an enemy
/// or bare ground is ignored (the mode stays armed, and selection
/// swallows the click).
#[allow(clippy::too_many_arguments)]
fn trigger_guard_click(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType, &TeamId), With<Selected>>,
    unit_root_q: Query<Entity, With<UnitType>>,
    parent_q: Query<&ChildOf>,
    unit_info_q: Query<&TeamId>,
    target_gtf_q: Query<&GlobalTransform>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
    unit_registry: Res<UnitRegistry>,
    alliances: Res<Alliances>,
) {
    if !modes.is(Mode::Guard) || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(target) = unit_hit(&windows, &camera_q, &mut ray_cast, &unit_root_q, &parent_q) else {
        return;
    };
    let Ok(t_team) = unit_info_q.get(target) else {
        return;
    };
    // Guarding makes sense only on a friendly unit.
    let Some(sel_team) = selected_q.iter().next().map(|(_, _, team)| team.0) else {
        return;
    };
    if !alliances.friendly(sel_team, t_team.0) {
        return;
    }

    let pos = target_gtf_q
        .get(target)
        .map_or(Vec3::ZERO, |t| t.translation());
    // A unit can't guard itself (also checked when applied).
    let units = net.units(
        selected_q
            .iter()
            .filter(|(entity, unit, _)| {
                *entity != target
                    && (unit_registry.speed(unit.0) > 0.0 || unit.0.takes_rally_orders())
            })
            .map(|(e, ..)| e),
    );
    if let Some(target) = net.id(target) {
        net.issue(SimCommand::Order {
            units,
            order: Order::Guard(target),
            queue: shift_held(&keys),
        });
    }
    pending.markers.push((pos, OrderMarker::Guard));
    modes.committed(&keys);
    consume_click(&mut mouse);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh patrol is the leg to the click; queued behind other
    /// orders, the first patrol point pairs with where they end.
    #[test]
    fn patrol_route_ends() {
        let unit = Vec3::new(1.0, 0.0, 1.0);
        let goal = Vec3::new(50.0, 0.0, 0.0);
        let click = Vec3::new(90.0, 0.0, 90.0);
        let patrol = |c: &QueuedCommand| match c {
            QueuedCommand::Patrol(p) => *p,
            other => panic!("not a patrol: {other:?}"),
        };
        let fresh = patrol_commands(false, &[], Some(goal), unit, click);
        assert_eq!(fresh.iter().map(patrol).collect::<Vec<_>>(), [click]);
        let behind_move = patrol_commands(true, &[], Some(goal), unit, click);
        assert_eq!(
            behind_move.iter().map(patrol).collect::<Vec<_>>(),
            [goal, click]
        );
        let queued = [QueuedCommand::Move(Vec3::X * 7.0)];
        let behind_queue = patrol_commands(true, &queued, Some(goal), unit, click);
        assert_eq!(
            behind_queue.iter().map(patrol).collect::<Vec<_>>(),
            [Vec3::X * 7.0, click]
        );
        let chained = [QueuedCommand::Patrol(goal), QueuedCommand::Patrol(unit)];
        let more = patrol_commands(true, &chained, Some(goal), unit, click);
        assert_eq!(more.iter().map(patrol).collect::<Vec<_>>(), [click]);
    }
}
