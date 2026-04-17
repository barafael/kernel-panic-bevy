/// All unit types across the three factions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::IntoStaticStr)]
pub enum UnitKind {
    // --- System ---
    #[strum(serialize = "kernel")]
    Kernel, // homebase + primary factory
    #[strum(serialize = "assembler")]
    Assembler, // mobile constructor, builds Sockets on datavents
    #[strum(serialize = "bit")]
    Bit, // basic swarm unit
    #[strum(serialize = "byte")]
    Byte, // heavy defensive unit, 70% damage reduction when closed
    #[strum(serialize = "pointer")]
    Pointer, // deployable artillery + NX Flag ability
    #[strum(serialize = "socket")]
    Socket, // secondary factory on datavents, auto-produces Bits
    #[strum(serialize = "firewall")]
    Firewall, // defensive structure (Network faction — listed here to preserve index order)

    // --- Hacker ---
    #[strum(serialize = "hole")]
    Hole, // homebase
    #[strum(serialize = "bug")]
    Bug, // basic swarm unit, can morph into Exploit
    #[strum(serialize = "exploit")]
    Exploit, // stationary artillery (increasing damage at range)
    #[strum(serialize = "worm")]
    Worm, // cloaked ambusher, kills convert to Viruses
    #[strum(serialize = "virus")]
    Virus, // spawned from Worm kills, not directly buildable
    #[strum(serialize = "dos")]
    Dos, // stuns/paralyzes enemies
    #[strum(serialize = "window")]
    Window, // secondary factory on datavents
    #[strum(serialize = "logic_bomb")]
    LogicBomb, // suicide unit
    #[strum(serialize = "trojan")]
    Trojan, // mobile builder, places Hacker structures on datavents

    // --- Network ---
    #[strum(serialize = "connection")]
    Connection, // homebase + teleporter
    #[strum(serialize = "port")]
    Port, // factory on datavents, increments Buffer
    #[strum(serialize = "packet")]
    Packet, // main combat unit, materialized from Buffer
    #[strum(serialize = "signal")]
    Signal, // scout unit
    #[strum(serialize = "gateway")]
    Gateway, // mobile builder, places Network structures on datavents
    #[strum(serialize = "flow")]
    Flow, // airborne assault, speed scales with team small-building count
}

use super::components::Faction;

/// All `UnitKind` variants in declaration order.
pub const ALL_UNIT_KINDS: [UnitKind; 22] = [
    UnitKind::Kernel,
    UnitKind::Assembler,
    UnitKind::Bit,
    UnitKind::Byte,
    UnitKind::Pointer,
    UnitKind::Socket,
    UnitKind::Firewall,
    UnitKind::Hole,
    UnitKind::Bug,
    UnitKind::Exploit,
    UnitKind::Worm,
    UnitKind::Virus,
    UnitKind::Dos,
    UnitKind::Window,
    UnitKind::LogicBomb,
    UnitKind::Trojan,
    UnitKind::Connection,
    UnitKind::Port,
    UnitKind::Packet,
    UnitKind::Signal,
    UnitKind::Gateway,
    UnitKind::Flow,
];

impl UnitKind {
    /// The FBI `unitname` key for this kind (used to look up stats in the registry).
    pub fn unitname(self) -> &'static str {
        self.into()
    }

    /// Faction for this unit kind.
    pub fn faction(self) -> Faction {
        match self {
            UnitKind::Kernel
            | UnitKind::Assembler
            | UnitKind::Bit
            | UnitKind::Byte
            | UnitKind::Pointer
            | UnitKind::Socket => Faction::System,

            UnitKind::Hole
            | UnitKind::Bug
            | UnitKind::Exploit
            | UnitKind::Worm
            | UnitKind::Virus
            | UnitKind::Dos
            | UnitKind::Window
            | UnitKind::LogicBomb
            | UnitKind::Trojan => Faction::Hacker,

            UnitKind::Firewall
            | UnitKind::Connection
            | UnitKind::Port
            | UnitKind::Packet
            | UnitKind::Signal
            | UnitKind::Gateway
            | UnitKind::Flow => Faction::Network,
        }
    }

    /// Mesh scale override (not in FBI files — game-specific rendering tweak).
    pub fn mesh_scale(self) -> f32 {
        match self {
            UnitKind::Kernel | UnitKind::Hole | UnitKind::Connection => 3.0,
            UnitKind::Socket | UnitKind::Window | UnitKind::Port => 2.0,
            UnitKind::Byte => 2.0,
            UnitKind::Firewall | UnitKind::Exploit | UnitKind::Pointer | UnitKind::Worm => 1.5,
            UnitKind::Dos => 1.3,
            UnitKind::Assembler | UnitKind::Trojan | UnitKind::Gateway | UnitKind::Flow => 1.2,
            UnitKind::LogicBomb => 0.8,
            UnitKind::Virus | UnitKind::Packet => 0.6,
            UnitKind::Bit | UnitKind::Bug => 0.5,
            UnitKind::Signal => 0.4,
        }
    }

    /// COB animation script filename (derived from unitname).
    pub fn script(self) -> String {
        format!("{}.cob", self.unitname())
    }

    /// Linear constant baked into the .cob bytecode by Scriptor when
    /// the .bos was compiled. Every `[N]` literal in the source gets
    /// multiplied by this constant; the animation system divides by it
    /// to recover N elmos.
    ///
    /// Spring's *engine* default is 65536, but the Kernel Panic project
    /// configured its Scriptor with 163840 — so most KP scripts use
    /// 163840. The exceptions are the units whose .bos has an explicit
    /// "linear constant must be changed to 65536" header comment:
    /// pointer.bos and hole.bos.
    pub fn cob_linear_constant(self) -> f32 {
        match self {
            UnitKind::Pointer | UnitKind::Hole => 65536.0,
            _ => 163840.0,
        }
    }
}
