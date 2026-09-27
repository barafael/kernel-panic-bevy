//! The command descriptions the panel shows — a port of what Spring's
//! `CCommandAI` / `CMobileCAI` / `CBuilderCAI` / `CFactoryCAI` put in a
//! unit's `possibleCommands`, after Kernel Panic's gadgets and widgets
//! edited them:
//!
//! - `hide_commands.lua` hides Fire state, Move state, Cloak, Repair
//!   level and Land mode; SelfD and the Wait variants are engine-hidden.
//! - `airstrike.lua` removes Stop / Repeat / Fire state from the
//!   Terminal and appends SIGTERM (`"42s"` + disabled while recharging).
//! - `network_reflectorshield.lua` appends Firewall (same countdown).
//! - `specialattack.lua` appends NX Flag (Pointer), Deploy (Bug),
//!   Undeploy (Exploit) and Launch Mines (Byte).
//! - `network_dispatch.lua` / `network_enter.lua` append Dispatch
//!   (Port, Connection) and Enter (Packet).
//! - `autohold.lua` inserts AutoHold right after the last state button.
//!
//! Only commands the remake implements are listed — Spring's Wait, the
//! mobile units' Repeat, the builders' Repair / Reclaim, the factories'
//! rally orders (Move / Patrol / Fight / Guard for new units) and the
//! Bug's Bombard have no simulation behind them yet, and a dead button
//! would lie to the player. Everything else keeps Spring's order.
//!
//! [`available_commands`] merges a selection like
//! `CSelectedUnitsHandler::GetAvailableCommands`: every non-build command
//! in unit order (first occurrence wins), then every build command.

use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::construction::buildings_for;
use crate::units::lifecycle::production::factory_roster;

/// Identity of a panel command (Spring's `SCommandDescription::id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CmdId {
    Stop,
    Attack,
    Repeat,
    AutoHold,
    Move,
    Patrol,
    Fight,
    Guard,
    NxFlag,
    /// The Obelisk's Infection gas. Upstream it is the Obelisk's only
    /// weapon, fired through the ordinary Attack button; the remake casts
    /// it as an aimed ability, so the button reads "Attack" but arms the
    /// Obelisk's cast.
    Infection,
    LaunchMines,
    Deploy,
    Undeploy,
    Sigterm,
    Firewall,
    Dispatch,
    Enter,
    /// Constructor build option (`CMDTYPE_ICON_BUILDING`): arms placement.
    Build(UnitKind),
    /// Factory build option (`CMDTYPE_ICON`): edits the build queue.
    Produce(UnitKind),
    PrevPage,
    NextPage,
    /// Hotkey-only commands: KP's panel has no button for them.
    SetTarget,
    UnsetTarget,
    SelfDestruct,
}

impl CmdId {
    /// Spring build options have negative ids; the panel lists them after
    /// every other command.
    pub fn is_build_option(self) -> bool {
        matches!(self, CmdId::Build(_) | CmdId::Produce(_))
    }

    /// Unit kinds whose aimed ability this command casts, for
    /// [`crate::interaction::ability::OrderCursorModes::ability_for`].
    pub fn ability_caster(self) -> Option<fn(UnitKind) -> bool> {
        Some(match self {
            CmdId::NxFlag => |k| k == UnitKind::Pointer,
            CmdId::Infection => |k| k == UnitKind::Obelisk,
            CmdId::LaunchMines => |k| k == UnitKind::Byte,
            CmdId::Sigterm => |k| k == UnitKind::Terminal,
            CmdId::Firewall => |k| k == UnitKind::Firewall,
            CmdId::Dispatch => |k: UnitKind| k.is_teleporter(),
            _ => return None,
        })
    }
}

/// How activating the command behaves (Spring's `CMDTYPE_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CmdType {
    /// Executes at once (`CMDTYPE_ICON`).
    Icon,
    /// A state button cycling its options (`CMDTYPE_ICON_MODE`).
    Mode,
    /// Arms a cursor mode; the next map click supplies the target.
    Targeted,
    /// Arms building placement (`CMDTYPE_ICON_BUILDING`).
    Building,
    Prev,
    Next,
}

/// Picture drawn on the button instead of (or under) its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Texture {
    /// The unit's buildpic (`unitpics/`).
    Buildpic(UnitKind),
    /// A named picture under `unitpics/` (gadget `texture = "&…&unitpics/x.png…"`).
    Pic(&'static str),
}

/// One button's worth of command description.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CmdDesc {
    pub id: CmdId,
    pub ty: CmdType,
    /// Text drawn on the button (unless `only_texture`).
    pub name: String,
    /// Spring `tooltip` (for build options the tooltip module renders the
    /// unit's stats instead).
    pub tooltip: &'static str,
    /// The keys bound to the command's action, Spring-style
    /// (`"Hotkeys: s"` in the tooltip). Empty when unbound.
    pub hotkeys: &'static str,
    pub disabled: bool,
    pub texture: Option<Texture>,
    pub only_texture: bool,
    /// Queue count drawn in the bottom-left corner (factory build options).
    pub count: Option<u32>,
    /// State buttons: the option labels and the current option index.
    pub options: &'static [&'static str],
    pub state: usize,
}

impl CmdDesc {
    fn new(
        id: CmdId,
        ty: CmdType,
        name: &str,
        tooltip: &'static str,
        hotkeys: &'static str,
    ) -> Self {
        Self {
            id,
            ty,
            name: name.to_string(),
            tooltip,
            hotkeys,
            disabled: false,
            texture: None,
            only_texture: false,
            count: None,
            options: &[],
            state: 0,
        }
    }

    /// The text `DrawButtons` prints: a state button shows its current
    /// option, anything else its name.
    pub fn label(&self) -> &str {
        if self.ty == CmdType::Mode
            && let Some(opt) = self.options.get(self.state)
        {
            return opt;
        }
        &self.name
    }

    /// Internal page-flip command (`CMDTYPE_PREV` / `CMDTYPE_NEXT`).
    pub fn page(next: bool) -> Self {
        if next {
            Self::new(CmdId::NextPage, CmdType::Next, "", "Next menu", ".")
        } else {
            Self::new(CmdId::PrevPage, CmdType::Prev, "", "Previous menu", ",")
        }
    }
}

/// What the panel needs to know about one selected unit.
#[derive(Clone, Debug, Default)]
pub(crate) struct UnitCmdState {
    pub kind: Option<UnitKind>,
    /// Factory: its repeat state and queued-count per kind.
    pub repeat: Option<bool>,
    pub queued: Vec<(UnitKind, u32)>,
    /// Worm: the AutoHold toggle.
    pub autohold: Option<bool>,
    /// Seconds until a Terminal / Firewall recharges (> 0 = recharging).
    pub recharge: Option<f32>,
}

/// Units that fire their weapons on an Attack order (Spring's
/// `UnitDef::CanAttack`: `canAttack` and at least one weapon). Factories
/// also qualify upstream, as an order passed to their new units — the
/// remake has no such rally orders, so they are left out.
fn can_attack(kind: UnitKind) -> bool {
    matches!(
        kind,
        UnitKind::Bit
            | UnitKind::Byte
            | UnitKind::Pointer
            | UnitKind::Bug
            | UnitKind::Exploit
            | UnitKind::Worm
            | UnitKind::Virus
            | UnitKind::Dos
            | UnitKind::Packet
            | UnitKind::Connection
            | UnitKind::Flow
            | UnitKind::Gateway
            | UnitKind::Debug
    )
}

/// Units with a `CMobileCAI` (`canMove` and a non-zero speed): they get
/// Move / Patrol / Fight / Guard.
fn is_mobile(kind: UnitKind) -> bool {
    matches!(
        kind,
        UnitKind::Bit
            | UnitKind::Byte
            | UnitKind::Pointer
            | UnitKind::Assembler
            | UnitKind::Bug
            | UnitKind::Worm
            | UnitKind::Virus
            | UnitKind::Dos
            | UnitKind::Trojan
            | UnitKind::Packet
            | UnitKind::Connection
            | UnitKind::Flow
            | UnitKind::Gateway
    )
}

/// Upstream label while a SIGTERM / Firewall recharges:
/// `math.ceil((readyFrame - now) / 32) .. "s"`.
pub(crate) fn recharge_label(seconds: f32) -> String {
    format!("{}s", seconds.ceil().max(1.0) as u32)
}

/// The keypad keys `kp_hotkeys.lua` binds to `buildunit_<minifac>`.
const MINIFAC_KEYS: &str = "numpad2 numpad4 numpad6 numpad8";

/// Spring's `possibleCommands` for one unit, in order, after KP's edits
/// (see the module docs). `capped` lists kinds whose team limit is
/// reached (Logic Bomb): their build buttons are disabled.
pub(crate) fn unit_commands(unit: &UnitCmdState, capped: &[UnitKind]) -> Vec<CmdDesc> {
    use CmdType::*;
    let Some(kind) = unit.kind else {
        return Vec::new();
    };
    let mut out = Vec::new();

    // --- CCommandAI ---
    if kind != UnitKind::Terminal {
        out.push(CmdDesc::new(
            CmdId::Stop,
            Icon,
            "Stop",
            "Stop: Cancel the units current actions",
            "s",
        ));
    }
    if can_attack(kind) {
        out.push(CmdDesc::new(
            CmdId::Attack,
            Targeted,
            "Attack",
            "Attack: Attacks a unit or a position on the ground",
            "a",
        ));
    }
    if let Some(on) = unit.repeat {
        let mut d = CmdDesc::new(
            CmdId::Repeat,
            Mode,
            "Repeat",
            "Repeat: If on, the unit will continuously\npush finished orders to the end of its\norder queue",
            "",
        );
        d.options = &["Repeat off", "Repeat on"];
        d.state = usize::from(on);
        out.push(d);
    }
    // autohold.lua: inserted right after the last state button.
    if let Some(on) = unit.autohold {
        let mut d = CmdDesc::new(
            CmdId::AutoHold,
            Mode,
            "AutoHold",
            "Automatically change agressivity according to cloakedness",
            "h",
        );
        d.options = &["AutoHold Off", "AutoHold On"];
        d.state = usize::from(on);
        out.push(d);
    }

    // --- CMobileCAI ---
    if is_mobile(kind) {
        out.push(CmdDesc::new(
            CmdId::Move,
            Targeted,
            "Move",
            "Move: Order the unit to move to a position",
            "m",
        ));
        out.push(CmdDesc::new(
            CmdId::Patrol,
            Targeted,
            "Patrol",
            "Patrol: Order the unit to patrol to one or more waypoints",
            "p",
        ));
        out.push(CmdDesc::new(
            CmdId::Fight,
            Targeted,
            "Fight",
            "Fight: Order the unit to take action while moving to a position",
            "f",
        ));
        out.push(CmdDesc::new(
            CmdId::Guard,
            Targeted,
            "Guard",
            "Guard: Order a unit to guard another unit and attack units attacking it",
            "g",
        ));
    }

    // --- Build options (CBuilderCAI / CFactoryCAI) ---
    if kind.is_constructor() {
        for &b in buildings_for(kind) {
            let mut d = CmdDesc::new(CmdId::Build(b), Building, b.unitname(), "", "");
            if b.is_minifac() {
                d.hotkeys = MINIFAC_KEYS;
            }
            d.texture = Some(Texture::Buildpic(b));
            d.only_texture = true;
            d.disabled = capped.contains(&b);
            out.push(d);
        }
    }
    if unit.repeat.is_some() {
        for &b in factory_roster(kind) {
            let mut d = CmdDesc::new(CmdId::Produce(b), Icon, b.unitname(), "", "");
            d.texture = Some(Texture::Buildpic(b));
            d.only_texture = true;
            d.count = unit
                .queued
                .iter()
                .find(|(k, _)| *k == b)
                .map(|(_, n)| *n)
                .filter(|n| *n > 0);
            out.push(d);
        }
    }

    // --- Gadget commands (InsertUnitCmdDesc appends) ---
    let recharging = unit.recharge.filter(|r| *r > 0.0);
    match kind {
        UnitKind::Pointer => {
            let mut d = CmdDesc::new(
                CmdId::NxFlag,
                Targeted,
                "NX Flag",
                "Set the target area on fire",
                "d",
            );
            d.texture = Some(Texture::Pic("nxflag"));
            d.only_texture = true;
            out.push(d);
        }
        UnitKind::Obelisk => out.push(CmdDesc::new(
            CmdId::Infection,
            Targeted,
            "Attack",
            "Attack: Attacks a unit or a position on the ground",
            "d",
        )),
        UnitKind::Bug => out.push(CmdDesc::new(
            CmdId::Deploy,
            Icon,
            "Deploy",
            "Transform into artillery emplacement\nTip: The button is queuable, the hotkey is not.",
            "d",
        )),
        UnitKind::Exploit => out.push(CmdDesc::new(
            CmdId::Undeploy,
            Icon,
            "Undeploy",
            "Transform into bug",
            "d u",
        )),
        UnitKind::Byte => {
            let mut d = CmdDesc::new(
                CmdId::LaunchMines,
                Targeted,
                "Launch Mines",
                "Launches several mines in a forward arc,\nat the cost of 6000 hitpoints. 10s reload.",
                "d",
            );
            d.texture = Some(Texture::Buildpic(UnitKind::LogicBomb));
            d.only_texture = true;
            out.push(d);
        }
        UnitKind::Terminal => {
            let mut d = CmdDesc::new(
                CmdId::Sigterm,
                Targeted,
                "SIGTERM",
                "Send a signal that terminates anything in the target area.",
                "d",
            );
            d.texture = Some(Texture::Pic("sigterm"));
            d.only_texture = true;
            if let Some(r) = recharging {
                d.name = recharge_label(r);
                d.disabled = true;
                d.only_texture = false;
            }
            out.push(d);
        }
        UnitKind::Firewall => {
            let mut d = CmdDesc::new(
                CmdId::Firewall,
                Targeted,
                "Firewall",
                "Protect all units in the target area with a firewall",
                "d",
            );
            if let Some(r) = recharging {
                d.name = recharge_label(r);
                d.disabled = true;
            }
            out.push(d);
        }
        UnitKind::Port | UnitKind::Connection => out.push(CmdDesc::new(
            CmdId::Dispatch,
            Targeted,
            "Dispatch",
            "Dispatch Packets from the Buffer (hold alt to dispatch until the buffer is empty)",
            "d",
        )),
        UnitKind::Packet => out.push(CmdDesc::new(
            CmdId::Enter,
            Icon,
            "Enter",
            "Return this Packet to the Buffer",
            "r",
        )),
        _ => {}
    }
    out
}

/// `CSelectedUnitsHandler::GetAvailableCommands`: non-build commands of
/// every selected unit in order (duplicates dropped, first unit's
/// description wins), then the build options.
pub(crate) fn available_commands(units: &[UnitCmdState], capped: &[UnitKind]) -> Vec<CmdDesc> {
    let per_unit: Vec<Vec<CmdDesc>> = units.iter().map(|u| unit_commands(u, capped)).collect();
    let mut out: Vec<CmdDesc> = Vec::new();
    for build_pass in [false, true] {
        for cmds in &per_unit {
            for d in cmds {
                if d.id.is_build_option() == build_pass && !out.iter().any(|o| o.id == d.id) {
                    out.push(d.clone());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(kind: UnitKind) -> UnitCmdState {
        UnitCmdState {
            kind: Some(kind),
            ..Default::default()
        }
    }

    fn ids(cmds: &[CmdDesc]) -> Vec<CmdId> {
        cmds.iter().map(|c| c.id).collect()
    }

    /// A Bit: CCommandAI's Stop / Attack, then CMobileCAI's orders.
    #[test]
    fn mobile_unit_commands_in_spring_order() {
        assert_eq!(
            ids(&unit_commands(&unit(UnitKind::Bit), &[])),
            vec![
                CmdId::Stop,
                CmdId::Attack,
                CmdId::Move,
                CmdId::Patrol,
                CmdId::Fight,
                CmdId::Guard
            ]
        );
    }

    /// The Kernel: Stop, the Repeat state button, then the build options
    /// in `[CANBUILD]` order with their queue counts.
    #[test]
    fn factory_lists_repeat_and_queue_counts() {
        let k = UnitCmdState {
            kind: Some(UnitKind::Kernel),
            repeat: Some(true),
            queued: vec![(UnitKind::Byte, 3)],
            ..Default::default()
        };
        let cmds = unit_commands(&k, &[]);
        assert_eq!(
            ids(&cmds),
            vec![
                CmdId::Stop,
                CmdId::Repeat,
                CmdId::Produce(UnitKind::Bit),
                CmdId::Produce(UnitKind::Pointer),
                CmdId::Produce(UnitKind::Byte),
                CmdId::Produce(UnitKind::Assembler),
            ]
        );
        assert_eq!(cmds[1].label(), "Repeat on");
        assert_eq!(cmds[4].count, Some(3));
        assert_eq!(cmds[2].count, None);
    }

    /// The Assembler lists its buildings (Logic Bomb greyed at the cap),
    /// the Worm gets AutoHold right after its state buttons.
    #[test]
    fn builder_and_worm_commands() {
        let cmds = unit_commands(&unit(UnitKind::Assembler), &[UnitKind::LogicBomb]);
        assert_eq!(
            ids(&cmds)[..5],
            [
                CmdId::Stop,
                CmdId::Move,
                CmdId::Patrol,
                CmdId::Fight,
                CmdId::Guard
            ]
        );
        let bomb = cmds
            .iter()
            .find(|c| c.id == CmdId::Build(UnitKind::LogicBomb))
            .unwrap();
        assert!(bomb.disabled);
        assert!(
            !cmds
                .iter()
                .find(|c| c.id == CmdId::Build(UnitKind::Socket))
                .unwrap()
                .disabled
        );
        let worm = UnitCmdState {
            kind: Some(UnitKind::Worm),
            autohold: Some(false),
            ..Default::default()
        };
        assert_eq!(
            ids(&unit_commands(&worm, &[]))[..3],
            [CmdId::Stop, CmdId::Attack, CmdId::AutoHold]
        );
    }

    /// airstrike.lua strips Stop from the Terminal; while recharging the
    /// SIGTERM button is disabled and reads the countdown.
    #[test]
    fn terminal_sigterm_countdown() {
        let t = UnitCmdState {
            kind: Some(UnitKind::Terminal),
            recharge: Some(41.2),
            ..Default::default()
        };
        let cmds = unit_commands(&t, &[]);
        assert_eq!(ids(&cmds), vec![CmdId::Sigterm]);
        assert!(cmds[0].disabled);
        assert_eq!(cmds[0].label(), "42s");
        assert!(!cmds[0].only_texture);
        let ready = UnitCmdState {
            recharge: None,
            ..t
        };
        let cmds = unit_commands(&ready, &[]);
        assert!(!cmds[0].disabled);
        assert!(cmds[0].only_texture);
    }

    /// Mixed selection: non-build commands first (deduplicated, in unit
    /// order), build options after.
    #[test]
    fn merged_selection_puts_build_options_last() {
        let cmds = available_commands(
            &[
                UnitCmdState {
                    kind: Some(UnitKind::Kernel),
                    repeat: Some(false),
                    ..Default::default()
                },
                unit(UnitKind::Pointer),
                unit(UnitKind::Bit),
            ],
            &[],
        );
        assert_eq!(
            ids(&cmds),
            vec![
                CmdId::Stop,
                CmdId::Repeat,
                CmdId::Attack,
                CmdId::Move,
                CmdId::Patrol,
                CmdId::Fight,
                CmdId::Guard,
                CmdId::NxFlag,
                CmdId::Produce(UnitKind::Bit),
                CmdId::Produce(UnitKind::Pointer),
                CmdId::Produce(UnitKind::Byte),
                CmdId::Produce(UnitKind::Assembler),
            ]
        );
    }
}
