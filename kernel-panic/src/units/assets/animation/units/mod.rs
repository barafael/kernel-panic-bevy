//! Per-unit animation drivers — one module per unit kind.
//!
//! Each driver is a plain Rust struct implementing [`UnitAnim`]. The
//! methods map 1:1 onto the `.bos` script entry points the old bytecode
//! VM used to execute (`Create`, `StartMoving`, `AimWeapon1`, `FireWeapon1`,
//! `Activate`, `Killed`, ...), so translating or tweaking a unit means
//! reading its file here next to `upstream/Kernel-Panic/scripts/*.bos`.
//!
//! Angles are Spring degrees (`<n>` in the scripts), translations are
//! elmos (`[n]`) — see the parent module's angle-convention docs for how
//! they map onto Bevy piece transforms.

mod assembler;
mod badblock;
mod bit;
mod bug;
mod byte;
mod carrier;
mod connection;
mod dos;
mod flow;
mod gateway;
mod hole;
mod kernel;
mod logic_bomb;
mod obelisk;
mod packet;
mod pointer;
mod signal;
mod socket;
mod terminal;
mod trojan;
mod window;
mod worm;

use super::{AnimRig, Axis, UnitAnim};
use crate::units::content::definitions::UnitKind;

/// A unit whose script drives nothing we render (Port, Firewall, Virus,
/// the mineblaster, ...). Kept as an explicit variant so the registry
/// documents the omission instead of silently falling through.
#[derive(Default)]
pub struct NoAnim;

impl UnitAnim for NoAnim {}

/// Build the animation driver for `kind`.
pub fn driver_for(kind: UnitKind) -> Box<dyn UnitAnim> {
    use UnitKind::*;
    match kind {
        Kernel => Box::new(kernel::KernelAnim::default()),
        Assembler => Box::new(assembler::AssemblerAnim::default()),
        Bit => Box::new(bit::BitAnim),
        Byte => Box::new(byte::ByteAnim::default()),
        Pointer => Box::new(pointer::PointerAnim::default()),
        Socket => Box::new(socket::SocketAnim::default()),
        Terminal => Box::new(terminal::TerminalAnim::default()),
        BadBlock => Box::new(badblock::BadBlockAnim::default()),
        Hole => Box::new(hole::HoleAnim::default()),
        Bug => Box::new(bug::BugAnim),
        Exploit => Box::new(bug::ExploitAnim),
        Worm => Box::new(worm::WormAnim::default()),
        Virus => Box::new(NoAnim),
        Dos => Box::new(dos::DosAnim::default()),
        Window => Box::new(window::WindowAnim::default()),
        LogicBomb => Box::new(logic_bomb::LogicBombAnim),
        Trojan => Box::new(trojan::TrojanAnim::default()),
        Obelisk => Box::new(obelisk::ObeliskAnim::default()),
        Carrier => Box::new(carrier::CarrierAnim),
        Connection => Box::new(connection::ConnectionAnim),
        Port | Firewall | Debug => Box::new(NoAnim),
        Packet => Box::new(packet::PacketAnim),
        Signal => Box::new(signal::SignalAnim::default()),
        Gateway => Box::new(gateway::GatewayAnim::default()),
        Flow => Box::new(flow::FlowAnim::default()),
    }
}

/// The shared `.bos` build-emerge idiom: `move base to y-axis
/// ([-depth]*(get BUILD_PERCENT_LEFT)/100) now`. Sinks `piece` by
/// `depth` elmos proportionally to the build percentage so freshly-built
/// units rise out of the ground / factory pad.
pub fn emerge_lift(rig: &mut AnimRig, piece: &str, depth: f32, build_percent: i32) {
    let offset = depth * (build_percent.clamp(0, 100) as f32) / 100.0;
    rig.move_to(piece, Axis::Y, -offset, 0.0);
}
