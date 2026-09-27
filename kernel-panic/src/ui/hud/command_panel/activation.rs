//! The single command-activation path: a click on a panel button, a
//! click on a Kernel Panic build-bar icon and every order hotkey all
//! become an [`ActivateCommand`] message, executed here the way
//! `CGuiHandler::SetActiveCommand` executes a command description:
//!
//! - `CMDTYPE_ICON` commands (Stop, Deploy, Enter, factory build
//!   options …) run at once;
//! - `CMDTYPE_ICON_MODE` state buttons (Repeat, AutoHold) step to their
//!   next option;
//! - targeted commands (Attack, Move, NX Flag, SIGTERM …) and building
//!   placement *arm* the command; the next map click supplies the
//!   target. Clicking a button arms it (again); pressing an armed
//!   command's hotkey a second time disarms it, as the remake's hotkeys
//!   always did.
//!
//! Factory build options follow `CFactoryCAI::GiveCommandReal`: left
//! click queues one, right click removes one, Shift ×5, Ctrl ×20
//! (both ×100), Alt puts the order at the front of the queue (with a
//! right click: removes the oldest instead of the newest).

use bevy::prelude::*;

use crate::interaction::ability::{Mode, OrderCursorModes};
use crate::interaction::movement::{
    AttackMoveActive, CommandQueue, GuardTarget, MovePath, MoveTarget,
};
use crate::interaction::selection::Selected;
use crate::units::combat::{
    AttackGroundOrder, AttackTargetOrder, ForcedTarget, SELF_DESTRUCT_DELAY, SelfDestructCountdown,
};
use crate::units::components::UnitType;
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::construction::PendingBuild;
use crate::units::lifecycle::production::{Producer, factory_roster};
use crate::units::mechanics::command_fire::PendingCommandFire;
use crate::units::mechanics::deploy::DeployEvent;
use crate::units::mechanics::network_buffer::EnterEvent;
use crate::units::mechanics::worm::AutoHold;

use super::super::placement::PlacementMode;
use super::commands::{CmdId, CmdType};
use super::{ActiveCommand, PanelCommands, PanelPage};

/// Request to run / arm a command, from the panel, the build bar or a
/// hotkey.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActivateCommand {
    pub id: CmdId,
    /// Right mouse button (`RIGHT_MOUSE_KEY` option).
    pub right: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Came from a hotkey: re-pressing an armed command's key disarms it.
    pub hotkey: bool,
}

impl ActivateCommand {
    pub fn click(id: CmdId, right: bool, keys: &ButtonInput<KeyCode>) -> Self {
        Self {
            id,
            right,
            shift: keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]),
            ctrl: keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]),
            alt: keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]),
            hotkey: false,
        }
    }

    fn key(id: CmdId, keys: &ButtonInput<KeyCode>) -> Self {
        Self {
            hotkey: true,
            ..Self::click(id, false, keys)
        }
    }
}

/// `CCommandAI::GetCountMultiplierFromOptions`.
pub(crate) fn count_multiplier(shift: bool, ctrl: bool) -> u32 {
    match (shift, ctrl) {
        (true, true) => 100,
        (false, true) => 20,
        (true, false) => 5,
        (false, false) => 1,
    }
}

/// A factory build-option click on one factory (`CFactoryCAI`): left
/// queues, right removes, Shift / Ctrl multiply, Alt works on the front.
pub(crate) fn edit_queue(producer: &mut Producer, kind: UnitKind, msg: &ActivateCommand) {
    let count = count_multiplier(msg.shift, msg.ctrl);
    if msg.right {
        producer.remove_kind(kind, count, msg.alt);
    } else if msg.alt {
        producer.enqueue_front(kind, count);
    } else {
        for _ in 0..count {
            producer.enqueue(kind);
        }
    }
}

/// The minifac each constructor builds (`kp_hotkeys.lua` binds the
/// keypad keys to `buildunit_socket` / `_window` / `_port`).
fn minifac_of(kind: UnitKind) -> Option<UnitKind> {
    kind.is_constructor().then(|| kind.faction().secondary_factory())
}

/// Order hotkeys → activations. `D` (cast at the cursor / deploy) stays
/// in `interaction::ability`: it fires immediately rather than arming.
pub(super) fn order_hotkeys(
    keys: Res<ButtonInput<KeyCode>>,
    selected: Query<&UnitType, With<Selected>>,
    mut out: MessageWriter<ActivateCommand>,
) {
    let ctrl = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    let mut send = |id| {
        out.write(ActivateCommand::key(id, &keys));
    };
    if ctrl {
        if keys.just_pressed(KeyCode::KeyD) {
            send(CmdId::SelfDestruct);
        }
        return;
    }
    const BINDINGS: [(KeyCode, CmdId); 13] = [
        (KeyCode::KeyS, CmdId::Stop),
        (KeyCode::KeyA, CmdId::Attack),
        (KeyCode::KeyF, CmdId::Fight),
        (KeyCode::KeyG, CmdId::Guard),
        (KeyCode::KeyM, CmdId::Move),
        (KeyCode::KeyP, CmdId::Patrol),
        (KeyCode::KeyT, CmdId::SetTarget),
        (KeyCode::KeyX, CmdId::UnsetTarget),
        (KeyCode::KeyH, CmdId::AutoHold),
        (KeyCode::KeyR, CmdId::Enter),
        (KeyCode::KeyU, CmdId::Undeploy),
        (KeyCode::Comma, CmdId::PrevPage),
        (KeyCode::Period, CmdId::NextPage),
    ];
    for (key, id) in BINDINGS {
        if keys.just_pressed(key) {
            send(id);
        }
    }
    if keys.any_just_pressed([
        KeyCode::Numpad2,
        KeyCode::Numpad4,
        KeyCode::Numpad6,
        KeyCode::Numpad8,
    ]) && let Some(minifac) = selected.iter().find_map(|u| minifac_of(u.0))
    {
        send(CmdId::Build(minifac));
    }
}

/// Stop: strip every order component off `entity`.
fn stop_unit(commands: &mut Commands, entity: Entity) {
    commands
        .entity(entity)
        .remove::<MoveTarget>()
        .remove::<MovePath>()
        .remove::<CommandQueue>()
        .remove::<AttackGroundOrder>()
        .remove::<AttackMoveActive>()
        .remove::<AttackTargetOrder>()
        .remove::<GuardTarget>()
        .remove::<ForcedTarget>()
        .remove::<PendingBuild>()
        .remove::<PendingCommandFire>()
        .remove::<SelfDestructCountdown>();
}

/// Disarm whatever command is armed.
pub(super) fn disarm(
    modes: &mut OrderCursorModes,
    placement: &mut PlacementMode,
    active: &mut ActiveCommand,
) {
    modes.clear();
    placement.kind = None;
    active.0 = None;
}

type SelectedUnits<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static UnitType,
        Option<&'static mut Producer>,
        Option<&'static mut AutoHold>,
    ),
    With<Selected>,
>;

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_activations(
    mut msgs: MessageReader<ActivateCommand>,
    panel: Res<PanelCommands>,
    mut page: ResMut<PanelPage>,
    mut active: ResMut<ActiveCommand>,
    mut modes: ResMut<OrderCursorModes>,
    mut placement: ResMut<PlacementMode>,
    mut units: SelectedUnits,
    mut deploy: MessageWriter<DeployEvent>,
    mut enter: MessageWriter<EnterEvent>,
    mut commands: Commands,
) {
    for msg in msgs.read() {
        let hotkey_only = matches!(
            msg.id,
            CmdId::SetTarget | CmdId::UnsetTarget | CmdId::SelfDestruct
        );
        let desc = panel.list.iter().find(|d| d.id == msg.id);
        match (&desc, msg.id) {
            // Page arrows exist only while the panel pages.
            (_, CmdId::PrevPage | CmdId::NextPage) => {}
            (None, _) if hotkey_only => {
                if units.is_empty() {
                    continue;
                }
            }
            // Not offered by the selection (Spring's SetActiveCommand
            // looks the action up in the current command list).
            (None, _) => continue,
            // `if (cd.disabled) return false;`
            (Some(d), _) if d.disabled => continue,
            _ => {}
        }

        let arm = |mode: Mode,
                   modes: &mut OrderCursorModes,
                   placement: &mut PlacementMode,
                   active: &mut ActiveCommand| {
            if msg.hotkey && active.0 == Some(msg.id) {
                disarm(modes, placement, active);
                return;
            }
            modes.arm(mode);
            modes.ability_for = msg.id.ability_caster();
            placement.kind = None;
            active.0 = Some(msg.id);
        };

        match msg.id {
            CmdId::Stop => {
                for (entity, ..) in &units {
                    stop_unit(&mut commands, entity);
                }
                disarm(&mut modes, &mut placement, &mut active);
            }
            CmdId::Attack => arm(Mode::AttackGround, &mut modes, &mut placement, &mut active),
            CmdId::Move => arm(Mode::Move, &mut modes, &mut placement, &mut active),
            CmdId::Patrol => arm(Mode::Patrol, &mut modes, &mut placement, &mut active),
            CmdId::Fight => arm(Mode::AttackMove, &mut modes, &mut placement, &mut active),
            CmdId::Guard => arm(Mode::Guard, &mut modes, &mut placement, &mut active),
            CmdId::SetTarget => arm(Mode::SetTarget, &mut modes, &mut placement, &mut active),
            CmdId::NxFlag
            | CmdId::Infection
            | CmdId::LaunchMines
            | CmdId::Sigterm
            | CmdId::Firewall
            | CmdId::Dispatch => arm(Mode::Ability, &mut modes, &mut placement, &mut active),
            CmdId::Build(kind) => {
                if msg.hotkey && active.0 == Some(msg.id) {
                    disarm(&mut modes, &mut placement, &mut active);
                } else {
                    modes.clear();
                    placement.kind = Some(kind);
                    active.0 = Some(msg.id);
                }
            }
            CmdId::Produce(kind) => {
                for (_, ut, producer, _) in &mut units {
                    if let Some(mut producer) = producer
                        && factory_roster(ut.0).contains(&kind)
                    {
                        edit_queue(&mut producer, kind, msg);
                    }
                }
            }
            CmdId::Repeat | CmdId::AutoHold => {
                // `newMode = params[0] + 1`, wrapping — from the shown
                // (first unit's) state, applied to the whole selection.
                let Some(d) = desc.filter(|d| d.ty == CmdType::Mode) else {
                    continue;
                };
                let on = (d.state + 1) % d.options.len().max(1) == 1;
                for (_, _, producer, autohold) in &mut units {
                    if msg.id == CmdId::Repeat
                        && let Some(mut p) = producer
                    {
                        p.set_repeat(on);
                    }
                    if msg.id == CmdId::AutoHold
                        && let Some(mut a) = autohold
                    {
                        a.0 = on;
                    }
                }
            }
            CmdId::Deploy | CmdId::Undeploy => {
                let from = if msg.id == CmdId::Deploy {
                    UnitKind::Bug
                } else {
                    UnitKind::Exploit
                };
                for (entity, ut, ..) in &units {
                    if ut.0 == from {
                        deploy.write(DeployEvent { entity });
                    }
                }
            }
            CmdId::Enter => {
                for (entity, ut, ..) in &units {
                    if ut.0 == UnitKind::Packet {
                        enter.write(EnterEvent { packet: entity });
                    }
                }
            }
            CmdId::UnsetTarget => {
                for (entity, ..) in &units {
                    commands.entity(entity).remove::<ForcedTarget>();
                }
                if modes.set_target {
                    disarm(&mut modes, &mut placement, &mut active);
                }
            }
            CmdId::SelfDestruct => {
                for (entity, ..) in &units {
                    commands.entity(entity).insert(SelfDestructCountdown {
                        remaining: SELF_DESTRUCT_DELAY,
                    });
                }
            }
            CmdId::PrevPage | CmdId::NextPage => {
                let pages = panel.pages.len().max(1);
                page.0 = if msg.id == CmdId::NextPage {
                    (page.0 + 1) % pages
                } else {
                    (page.0 + pages - 1) % pages
                };
            }
        }
    }
}

/// Keep [`ActiveCommand`] honest: once the armed mode / placement ends
/// (order committed, cancelled, builder deselected) nothing is active.
pub(super) fn sync_active(
    modes: Res<OrderCursorModes>,
    placement: Res<PlacementMode>,
    mut active: ResMut<ActiveCommand>,
) {
    match placement.kind {
        Some(kind) => {
            if active.0 != Some(CmdId::Build(kind)) {
                active.0 = Some(CmdId::Build(kind));
            }
        }
        None if !modes.any_active() && active.0.is_some() => active.0 = None,
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::commands::{UnitCmdState, available_commands};
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn world() -> World {
        let mut world = World::new();
        world.init_resource::<PanelCommands>();
        world.init_resource::<PanelPage>();
        world.init_resource::<ActiveCommand>();
        world.init_resource::<OrderCursorModes>();
        world.init_resource::<PlacementMode>();
        world.init_resource::<Messages<ActivateCommand>>();
        world.init_resource::<Messages<DeployEvent>>();
        world.init_resource::<Messages<EnterEvent>>();
        world
    }

    fn offer(world: &mut World, units: &[UnitCmdState]) {
        world.resource_mut::<PanelCommands>().list = available_commands(units, &[]);
    }

    fn send(world: &mut World, msg: ActivateCommand) {
        world.write_message(msg);
        world.run_system_once(apply_activations).unwrap();
        // A fresh one-shot system would re-read the old messages.
        world.resource_mut::<Messages<ActivateCommand>>().clear();
    }

    fn msg(id: CmdId) -> ActivateCommand {
        ActivateCommand {
            id,
            right: false,
            shift: false,
            ctrl: false,
            alt: false,
            hotkey: false,
        }
    }

    fn kernel_state() -> UnitCmdState {
        UnitCmdState {
            kind: Some(UnitKind::Kernel),
            repeat: Some(false),
            ..Default::default()
        }
    }

    /// Factory clicks: left +1, Shift +5, Ctrl +20, right −1, Alt to the
    /// front.
    #[test]
    fn factory_queue_clicks() {
        let mut world = world();
        offer(&mut world, &[kernel_state()]);
        let kernel = world
            .spawn((UnitType(UnitKind::Kernel), Producer::new(), Selected))
            .id();
        let produce = |kind| msg(CmdId::Produce(kind));
        send(&mut world, produce(UnitKind::Bit));
        send(
            &mut world,
            ActivateCommand {
                shift: true,
                ..produce(UnitKind::Bit)
            },
        );
        send(
            &mut world,
            ActivateCommand {
                ctrl: true,
                ..produce(UnitKind::Byte)
            },
        );
        let q = world.get::<Producer>(kernel).unwrap().queue().clone();
        assert_eq!(q.iter().filter(|k| **k == UnitKind::Bit).count(), 6);
        assert_eq!(q.iter().filter(|k| **k == UnitKind::Byte).count(), 20);
        send(
            &mut world,
            ActivateCommand {
                right: true,
                ..produce(UnitKind::Byte)
            },
        );
        assert_eq!(world.get::<Producer>(kernel).unwrap().queue().len(), 25);
        send(
            &mut world,
            ActivateCommand {
                alt: true,
                ..produce(UnitKind::Pointer)
            },
        );
        assert_eq!(
            world.get::<Producer>(kernel).unwrap().current_production(),
            Some(UnitKind::Pointer)
        );
    }

    /// Build buttons arm placement (clearing any cursor mode); an order
    /// button arms its mode and clears placement. A hotkey pressed again
    /// disarms; a button click re-arms.
    #[test]
    fn arming_is_exclusive_and_hotkeys_toggle() {
        let mut world = world();
        offer(
            &mut world,
            &[UnitCmdState {
                kind: Some(UnitKind::Assembler),
                ..Default::default()
            }],
        );
        world.spawn((UnitType(UnitKind::Assembler), Selected));
        send(&mut world, msg(CmdId::Move));
        assert!(world.resource::<OrderCursorModes>().move_order);
        send(&mut world, msg(CmdId::Build(UnitKind::Socket)));
        assert_eq!(
            world.resource::<PlacementMode>().kind,
            Some(UnitKind::Socket)
        );
        assert!(!world.resource::<OrderCursorModes>().any_active());
        assert_eq!(
            world.resource::<ActiveCommand>().0,
            Some(CmdId::Build(UnitKind::Socket))
        );
        send(&mut world, msg(CmdId::Build(UnitKind::Socket)));
        assert_eq!(
            world.resource::<PlacementMode>().kind,
            Some(UnitKind::Socket)
        );
        send(
            &mut world,
            ActivateCommand {
                hotkey: true,
                ..msg(CmdId::Build(UnitKind::Socket))
            },
        );
        assert_eq!(world.resource::<PlacementMode>().kind, None);
        // Attack isn't offered for an Assembler (CanAttack=0).
        send(&mut world, msg(CmdId::Attack));
        assert!(!world.resource::<OrderCursorModes>().any_active());
    }

    /// State buttons step their option for the whole selection; disabled
    /// commands don't run.
    #[test]
    fn repeat_toggles_and_disabled_is_inert() {
        let mut world = world();
        offer(&mut world, &[kernel_state()]);
        let kernel = world
            .spawn((UnitType(UnitKind::Kernel), Producer::new(), Selected))
            .id();
        send(&mut world, msg(CmdId::Repeat));
        assert!(world.get::<Producer>(kernel).unwrap().repeat());

        world.resource_mut::<PanelCommands>().list = available_commands(
            &[UnitCmdState {
                kind: Some(UnitKind::Terminal),
                recharge: Some(10.0),
                ..Default::default()
            }],
            &[],
        );
        send(&mut world, msg(CmdId::Sigterm));
        assert!(!world.resource::<OrderCursorModes>().ability);
    }
}
