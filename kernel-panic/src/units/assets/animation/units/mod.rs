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
        Bit => Box::new(bit::BitAnim::default()),
        Byte => Box::new(byte::ByteAnim::default()),
        Pointer => Box::new(pointer::PointerAnim::default()),
        Socket => Box::new(socket::SocketAnim::default()),
        Terminal => Box::new(terminal::TerminalAnim::default()),
        BadBlock => Box::new(badblock::BadBlockAnim::default()),
        Hole => Box::new(hole::HoleAnim::default()),
        Bug => Box::new(bug::BugAnim::default()),
        Exploit => Box::new(bug::ExploitAnim::default()),
        Worm => Box::new(worm::WormAnim::default()),
        Virus => Box::new(NoAnim),
        Dos => Box::new(dos::DosAnim::default()),
        Window => Box::new(window::WindowAnim::default()),
        LogicBomb => Box::new(logic_bomb::LogicBombAnim::default()),
        Trojan => Box::new(trojan::TrojanAnim::default()),
        Obelisk => Box::new(obelisk::ObeliskAnim::default()),
        Carrier => Box::new(carrier::CarrierAnim::default()),
        Connection => Box::new(connection::ConnectionAnim::default()),
        Port | Firewall | Debug => Box::new(NoAnim),
        Packet => Box::new(packet::PacketAnim::default()),
        Signal => Box::new(signal::SignalAnim::default()),
        Gateway => Box::new(gateway::GatewayAnim::default()),
        Flow => Box::new(flow::FlowAnim::default()),
    }
}

// ---------------------------------------------------------------------------
// Shared driver idioms
// ---------------------------------------------------------------------------

/// Death-choreography tracker shared by drivers whose `killed()` plays a
/// visible burst: `killed()` calls [`DeathFx::start`], `update()` ticks
/// it, and `busy()` reports it so `cleanup_dying` holds the corpse until
/// the death fx has played instead of despawning on the death frame.
#[derive(Default)]
pub struct DeathFx {
    remaining: Option<f32>,
}

/// How long a driver stays `busy()` after its `killed()` fired. Covers
/// the death-particle burst (~0.5 s lifetime) plus a beat of lingering.
const DEATH_FX_WINDOW: f32 = 0.6;

impl DeathFx {
    pub fn start(&mut self) {
        self.remaining = Some(DEATH_FX_WINDOW);
    }

    pub fn tick(&mut self, dt: f32) {
        if let Some(t) = &mut self.remaining {
            *t -= dt;
            if *t <= 0.0 {
                self.remaining = None;
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.remaining.is_some()
    }
}

/// The two-laser square sweep shared by Socket and BadBlock
/// (BuildLasers()): both lasers trace a square forever — x out on
/// opposite sides, z out on opposite sides, then snap home and hold a
/// leg. Terminal's variant sweeps a rectangle and re-homes at speed, so
/// it keeps its own leg fn.
#[derive(Default)]
pub struct SquareSweep {
    /// Current leg 0..4 (x-out → z-out → snap home ×2).
    leg: usize,
    timer: f32,
}

impl SquareSweep {
    /// Time one leg takes: half-width ÷ speed.
    pub fn new(half_width: f32, speed: f32) -> Self {
        Self {
            leg: 0,
            timer: half_width / speed,
        }
    }

    /// Advance the sweep. `a`/`b` are the two laser pieces; `half_width`
    /// and `speed` are the script constants.
    pub fn update(
        &mut self,
        rig: &mut AnimRig,
        a: usize,
        b: usize,
        half_width: f32,
        speed: f32,
        dt: f32,
    ) {
        self.timer -= dt;
        if self.timer > 0.0 {
            return;
        }
        self.timer = half_width / speed;
        self.leg = (self.leg + 1) % 4;
        match self.leg {
            0 => {
                rig.move_to(a, Axis::X, -half_width, speed);
                rig.move_to(b, Axis::X, half_width, speed);
            }
            1 => {
                rig.move_to(a, Axis::Z, half_width, speed);
                rig.move_to(b, Axis::Z, -half_width, speed);
            }
            _ => {
                // snap home (x 0 now; z 0 now) and hold one leg
                rig.move_to(a, Axis::X, 0.0, 0.0);
                rig.move_to(b, Axis::X, 0.0, 0.0);
                rig.move_to(a, Axis::Z, 0.0, 0.0);
                rig.move_to(b, Axis::Z, 0.0, 0.0);
            }
        }
    }
}

/// The shared `.bos` build-emerge idiom: `move base to y-axis
/// ([-depth]*(get BUILD_PERCENT_LEFT)/100) now`. Sinks `piece` by
/// `depth` elmos proportionally to the build percentage so freshly-built
/// units rise out of the ground / factory pad.
pub fn emerge_lift(rig: &mut AnimRig, piece: usize, depth: f32, build_percent: i32) {
    let offset = depth * (build_percent.clamp(0, 100) as f32) / 100.0;
    rig.move_to(piece, Axis::Y, -offset, 0.0);
}

/// Static piece tables per unit kind, in each script's declaration
/// order. These replace the parsed `.cob` piece-name tables — the game
/// no longer reads script bytecode at all. Extracted from the compiled
/// scripts in `upstream/Kernel-Panic/scripts/*.cob` (which are what
/// `UnitKind::unitname()` points at); the declaration order is kept for
/// documentation value, since drivers address pieces by name.
///
/// Pieces listed here that don't exist in the unit's s3o resolve to
/// stub entities at spawn, so animating them is a harmless no-op.
pub fn piece_names(kind: UnitKind) -> &'static [&'static str] {
    use UnitKind::*;
    match kind {
        Kernel => &[
            "root", "whole", "base0", "pillar0", "head0", "tip0", "base1", "pillar1", "head1",
            "tip1", "base2", "pillar2", "head2", "tip2", "base3", "pillar3", "head3", "tip3",
            "pad", "shoulder", "arm", "hand", "finger",
        ],
        Assembler => &["base", "body", "rotor", "nozzle", "tip"],
        Bit => &["base", "body", "shell", "gunbase", "gunpoint"],
        Byte => &[
            "base", "aimer", "rotor", "blade0", "blade1", "blade2", "blade3", "bp0", "bp1",
            "bp2", "bp3", "gunpoint", "launcher_arm", "launcher1", "launcher2", "launcher3",
            "launcher4", "launcher5",
        ],
        Pointer => &["base", "body", "left", "right", "gun", "gunbase", "gunpoint"],
        Socket => &["base", "body", "blaser0", "blaser1", "claser0", "claser1"],
        Firewall => &["base", "body"],
        Terminal => &["base", "body", "bl0", "bl1"],
        Hole => &[
            "whole", "body", "nanoarm", "nanomover", "nanoemitter", "pad", "shoulder", "arm",
            "hand", "finger",
        ],
        Bug | Exploit => &[
            "base", "center", "body", "nose", "feet0", "feet1", "feet2", "clamps", "turret",
            "muzzle",
        ],
        Worm => &[
            "base", "body", "head", "ring0", "ring1", "ring2", "ring3", "ring4", "ring5", "end",
        ],
        Virus => &["base", "body", "turret", "gun0", "gun1"],
        Dos => &["base", "center", "slash", "dot", "gunpoint", "ground"],
        Window => &[
            "base", "bar", "flap", "hourglass", "hglass0", "hglass1", "sand0", "sand1", "sand2",
        ],
        LogicBomb => &["base", "mine"],
        Trojan => &["base", "center", "piece0", "piece1", "piece2", "piece3"],
        Obelisk => &["base", "segf", "segb", "segl", "segr", "tip"],
        Carrier => &["root", "mover", "pad", "shoulder", "arm", "hand", "finger"],
        Connection => &["base", "body", "correcter", "gp", "gp2"],
        Port => &["base", "body"],
        Packet => &["base", "body", "turret", "gp"],
        Signal => &["base", "bomb", "wingul", "wingur", "wingbl", "wingbr"],
        Gateway => &["base", "body", "center"],
        Flow => &["base", "monolith", "wing1", "wing2", "gp0", "gp1", "gp2", "gp3"],
        Debug => &["base", "sky"],
        BadBlock => &["base", "blaser0", "blaser1"],
    }
}

/// Whether the unit's script declares `AimWeapon1` — gates the host
/// aim-before-fire component ([`AimScript`](crate::units::combat::AimScript))
/// at spawn. Replaces the old `function_id("AimWeapon1")` lookup in the
/// parsed script.
pub fn has_aim_weapon(kind: UnitKind) -> bool {
    use UnitKind::*;
    matches!(
        kind,
        Assembler
            | Bit
            | Byte
            | Pointer
            | Exploit
            | Worm
            | Virus
            | Dos
            | LogicBomb
            | Obelisk
            | Connection
            | Packet
            | Signal
            | Gateway
            | Flow
            | Debug
    )
}
