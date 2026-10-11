//! Order-funnel probe for the headless network experiments
//! (`headless`, `net-experiments/x-sim.md`): every decision the AI
//! turns into an order — a unit order, a factory queue change, an
//! ability cast, a deploy, a dispatch, a self-destruct — is reported
//! here as the compact command a lockstep peer would have to send.
//!
//! Outside `cfg(test)` [`record`] compiles to nothing (the closure is
//! never called), so the game build is unchanged.

// The payload is recorded whole; the encoders only size it.
#![allow(dead_code)]

use bevy::prelude::*;

use crate::interaction::movement::QueuedCommand;
use crate::units::content::definitions::UnitKind;

/// What kind of command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProbeKind {
    Move,
    AttackMove,
    BuildAt,
    /// Other queued-command kinds (attack unit / ground, guard, patrol).
    OtherOrder,
    /// Units appended to a factory queue (`count` of them).
    FactoryEnqueue,
    /// A factory's repeat flag flipped.
    FactoryRepeat,
    /// Command-fire ability cast at a position.
    CommandFire,
    Deploy,
    Dispatch,
    SelfDestruct,
}

/// The command's argument.
#[derive(Clone, Copy, Debug)]
pub enum ProbeTarget {
    None,
    Pos(Vec3),
    Unit(Entity),
    /// A unit kind (and count) — factory orders; a build site.
    Kind(UnitKind, u32),
    KindAt(UnitKind, Vec3),
    Flag(bool),
}

/// One command, as issued.
#[derive(Clone, Copy, Debug)]
pub struct ProbeCmd {
    pub team: u8,
    /// The unit (or factory) the command is given to.
    pub unit: Entity,
    pub kind: ProbeKind,
    pub target: ProbeTarget,
}

impl ProbeCmd {
    /// A queued-command order (`replace_order`).
    pub fn order(team: u8, unit: Entity, cmd: &QueuedCommand) -> Self {
        let (kind, target) = match *cmd {
            QueuedCommand::Move(p) => (ProbeKind::Move, ProbeTarget::Pos(p)),
            QueuedCommand::AttackMove(p) => (ProbeKind::AttackMove, ProbeTarget::Pos(p)),
            QueuedCommand::BuildAt { kind, site } => {
                (ProbeKind::BuildAt, ProbeTarget::KindAt(kind, site))
            }
            QueuedCommand::Patrol(p) | QueuedCommand::AttackGround(p) => {
                (ProbeKind::OtherOrder, ProbeTarget::Pos(p))
            }
            QueuedCommand::AttackUnit { target, .. } | QueuedCommand::Guard { target, .. } => {
                (ProbeKind::OtherOrder, ProbeTarget::Unit(target))
            }
        };
        Self {
            team,
            unit,
            kind,
            target,
        }
    }
}

/// Report a command. The closure only runs in test builds while a
/// harness has [`imp::start`]ed recording.
#[inline(always)]
pub fn record(cmd: impl FnOnce() -> ProbeCmd) {
    #[cfg(test)]
    imp::record(cmd());
    #[cfg(not(test))]
    let _ = cmd;
}

#[cfg(test)]
pub mod imp {
    use std::sync::Mutex;

    use super::ProbeCmd;

    static LOG: Mutex<Option<Vec<ProbeCmd>>> = Mutex::new(None);

    pub(super) fn record(cmd: ProbeCmd) {
        if let Some(log) = LOG.lock().unwrap().as_mut() {
            log.push(cmd);
        }
    }

    /// Start (or restart) recording.
    pub fn start() {
        *LOG.lock().unwrap() = Some(Vec::new());
    }

    /// Commands recorded since the last drain.
    pub fn drain() -> Vec<ProbeCmd> {
        LOG.lock()
            .unwrap()
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    pub fn stop() {
        *LOG.lock().unwrap() = None;
    }
}
