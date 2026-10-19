//! Ability hotkeys.
//!
//! Pressing `D` with a caster selected fires that unit's "ability" at
//! the cursor — whatever the unit kind treats as its ability:
//! - Pointer / Obelisk / Firewall / Byte / Terminal → command-fire weapon
//!   (NX Flag, Infection gas, etc.) routed through `CommandFireEvent`.
//! - Port / Connection → Dispatch packets (with ALT modifier for
//!   "drain the buffer", mirroring upstream `network_dispatch.lua`).
//! - Bug / Exploit → Deploy / Pack Up (the Bug ↔ Exploit morph).
//!
//! The other order hotkeys (Stop, Attack, Move, Fight, Patrol, Guard,
//! Enter, self-destruct …) go through the command panel's single
//! activation path (`ui::hud::command_panel::activation`), exactly like
//! a click on the matching panel button. This module owns the sticky
//! [`OrderCursorModes`] they arm and the map-click handlers that turn an
//! armed mode into an order. The panel's ability buttons (NX Flag,
//! SIGTERM, Dispatch …) arm [`Mode::Ability`] so the next
//! ground click casts the aimed abilities, as `D` over that point would
//! ([`deploy_units`] / [`cast_aimed_abilities`] are shared).

use crate::map_loading::MatchScopeAppExt;
use bevy::prelude::*;

use super::movement::QueuedCommand;
use super::selection::{
    OrderMarker, PendingMoveIndicators, PickRayCast, Selected, SelectionSet, ground_hit, unit_hit,
};
use crate::interaction::cursor::{CursorKind, CursorRequest};
use crate::net::{Issue, NetPos, Order, SimCommand};
use crate::rendering::camera::RtsCamera;
use crate::ui::hud::command_panel::commands::{CmdId, can_attack};
use crate::units::components::{TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::teams::Alliances;

fn ctrl_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
}

pub struct AbilityHotkeyPlugin;

impl Plugin for AbilityHotkeyPlugin {
    fn build(&self, app: &mut App) {
        // An armed order mode doesn't outlive its match.
        app.init_match_resource::<OrderCursorModes>().add_systems(
            Update,
            (
                trigger_aimed_ability_on_hotkey,
                trigger_deploy_on_hotkey,
                // The armed-mode click handlers run before selection and
                // consume the press they commit, so the click never also
                // reaches click-to-select (a plain order click used to
                // deselect everything, a guard click select the guarded
                // unit).
                (
                    trigger_ability_click,
                    trigger_patrol_click,
                    trigger_attack_ground_click,
                    trigger_attack_move_click,
                    trigger_guard_click,
                    trigger_move_click,
                    trigger_set_target_click,
                )
                    .before(SelectionSet::Select),
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
    /// Cast the aimed abilities of the units this panel command lists
    /// (NX Flag, SIGTERM, Dispatch …), like a Spring command goes only
    /// to units that list it.
    Ability(CmdId),
}

impl Mode {
    /// The glyph the armed mode forces.
    fn cursor(self) -> CursorKind {
        match self {
            Mode::Patrol => CursorKind::Patrol,
            Mode::Guard => CursorKind::Defend,
            Mode::Move => CursorKind::Move,
            // Spring renders set-target with the attack crosshair too.
            Mode::AttackGround | Mode::AttackMove | Mode::SetTarget | Mode::Ability(_) => {
                CursorKind::Attack
            }
        }
    }
}

/// Force the armed mode's cursor glyph. The high priority beats the
/// default context resolver; with nothing armed it regains control.
fn update_mode_cursor(modes: Res<OrderCursorModes>, mut request: ResMut<CursorRequest>) {
    if let Some(mode) = modes.mode {
        request.set(mode.cursor(), 10);
    }
}

/// Whether `kind`'s `D` ability is aimed at a map point: command-fire
/// casters (NX Flag, Infection, Protect, Mine Launch, SIGTERM) and the
/// packet teleporters (Dispatch). Bug / Exploit deploy needs no target.
pub(crate) fn ability_is_aimed(kind: UnitKind) -> bool {
    kind.has_command_fire_ability() || kind.is_teleporter()
}

/// Deploy / pack up every Bug / Exploit in `units` — the untargeted half
/// of the `D` ability.
pub(crate) fn deploy_units(
    units: impl IntoIterator<Item = (Entity, UnitKind)>,
    net: &mut Issue,
    commands: &mut Commands,
) {
    crate::audio::ack_order(commands);
    let units = net.units(
        units
            .into_iter()
            .filter(|(_, kind)| kind.deploy_pair().is_some())
            .map(|(e, _)| e),
    );
    net.issue(SimCommand::Deploy { units });
}

/// Cast the aimed half of the `D` ability at `target` for every unit in
/// `units`: command-fire casters fire (range / approach handled by
/// `process_command_fire`), teleporters dispatch.
///
/// `alt_held` mirrors upstream `network_dispatch.lua`: the dispatch
/// command stays active and re-fires every frame until the team's
/// Packet Buffer is empty, instead of stopping after one 12-batch
/// (`AutoDispatch`, see `net::apply`).
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
            .filter(|(_, kind)| kind.has_command_fire_ability() || kind.is_teleporter())
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

/// `D` deploys a selected Bug into an Exploit and packs an Exploit
/// back into a Bug. Co-exists with command-fire / dispatch on the
/// same key because the eligibility sets don't overlap — Bug/Exploit
/// aren't casters, aren't teleporters.
fn trigger_deploy_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    mut net: Issue,
    mut commands: Commands,
) {
    if !keys.just_pressed(KeyCode::KeyD) {
        return;
    }
    if ctrl_held(&keys) {
        return;
    }
    deploy_units(
        selected_q.iter().map(|(e, u)| (e, u.0)),
        &mut net,
        &mut commands,
    );
}

/// `D` casts the selection's aimed abilities (command-fire, Dispatch)
/// at the ground point under the cursor.
#[allow(clippy::too_many_arguments)]
fn trigger_aimed_ability_on_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut net: Issue,
    mut commands: Commands,
) {
    if !keys.just_pressed(KeyCode::KeyD) || ctrl_held(&keys) {
        return;
    }
    if !selected_q.iter().any(|(_, u)| ability_is_aimed(u.0)) {
        return;
    }
    let Some(target) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        return;
    };
    // Why: `D` consumes a click-to-cast the palette button armed — left
    // armed, the next unrelated click would cast again (a second
    // 12-packet Dispatch drains the buffer twice).
    if matches!(modes.mode, Some(Mode::Ability(_))) {
        modes.clear();
    }
    cast_aimed_abilities(
        selected_q.iter().map(|(e, u)| (e, u.0)),
        target,
        alt_held(&keys),
        &mut net,
        &mut commands,
    );
}

/// Click handler for [`Mode::Ability`]: the next left-click
/// on the ground casts the selection's aimed abilities there — exactly
/// what `D` would do with the cursor at that point. Shift stays armed.
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
    if !mouse.just_pressed(MouseButton::Left) {
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
    pending.markers.push((target, OrderMarker::Attack));
    modes.committed(&keys);
    consume_click(&mut mouse);
}

/// Ground-target click: while [`Mode::AttackGround`] is armed, the next
/// left-click issues an attack-ground order to every selected unit that
/// has the Attack command (armed, non-command-fire primary weapon:
/// Spring sends a command only to units that list it, and a
/// command-fire weapon such as the Obelisk's Infection never fires on
/// an attack order). `attack_ground_system` moves the unit into weapon
/// range if needed, then fires each reload cycle at the ground
/// position. Shift queues it behind the current orders. Commits exit
/// the mode (Shift stays in mode).
#[allow(clippy::too_many_arguments)]
fn trigger_attack_ground_click(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    mut modes: ResMut<OrderCursorModes>,
    mut pending: ResMut<PendingMoveIndicators>,
    mut net: Issue,
) {
    let Some(target) = armed_ground_click(
        &modes,
        &mouse,
        Mode::AttackGround,
        &windows,
        &camera_q,
        &mut ray_cast,
    ) else {
        return;
    };
    let units = net.units(
        selected_q
            .iter()
            .filter(|(_, unit)| can_attack(unit.0))
            .map(|(e, _)| e),
    );
    net.issue(SimCommand::Order {
        units,
        order: Order::AttackGround(NetPos::quantize(target)),
        queue: shift_held(&keys),
    });
    pending.markers.push((target, OrderMarker::Attack));
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
                !unit_registry.weapon(unit.0).is_empty()
                    && unit_registry.can_attack(unit.0, t_kind.0)
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
