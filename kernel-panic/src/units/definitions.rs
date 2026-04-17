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
    #[strum(serialize = "terminal")]
    Terminal, // System special building — launches SIGTERM air strikes

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
    #[strum(serialize = "obelisk")]
    Obelisk, // Hacker special building — infection gas artillery

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

    // --- Shared (buildable by every faction's constructor) ---
    #[strum(serialize = "mineblaster")]
    Debug, // one-shot mine/wall clearer (FBI unitname=mineblaster, display "Debug")
    #[strum(serialize = "badblock")]
    BadBlock, // cheap destructible wall, blocks movement not projectiles
}

use super::components::Faction;

/// All `UnitKind` variants in declaration order.
pub const ALL_UNIT_KINDS: [UnitKind; 26] = [
    UnitKind::Kernel,
    UnitKind::Assembler,
    UnitKind::Bit,
    UnitKind::Byte,
    UnitKind::Pointer,
    UnitKind::Socket,
    UnitKind::Firewall,
    UnitKind::Terminal,
    UnitKind::Hole,
    UnitKind::Bug,
    UnitKind::Exploit,
    UnitKind::Worm,
    UnitKind::Virus,
    UnitKind::Dos,
    UnitKind::Window,
    UnitKind::LogicBomb,
    UnitKind::Trojan,
    UnitKind::Obelisk,
    UnitKind::Connection,
    UnitKind::Port,
    UnitKind::Packet,
    UnitKind::Signal,
    UnitKind::Gateway,
    UnitKind::Flow,
    UnitKind::Debug,
    UnitKind::BadBlock,
];

impl UnitKind {
    /// The FBI `unitname` key for this kind (used to look up stats in the registry).
    pub fn unitname(self) -> &'static str {
        self.into()
    }

    /// Default faction for this unit kind. Used as the spawn-faction when
    /// the caller has no builder context (e.g. the showcase map). Shared
    /// units (Debug, BadBlock) default to System but are spawned with the
    /// builder's faction in real gameplay.
    pub fn faction(self) -> Faction {
        match self {
            UnitKind::Kernel
            | UnitKind::Assembler
            | UnitKind::Bit
            | UnitKind::Byte
            | UnitKind::Pointer
            | UnitKind::Socket
            | UnitKind::Terminal
            | UnitKind::Debug
            | UnitKind::BadBlock => Faction::System,

            UnitKind::Hole
            | UnitKind::Bug
            | UnitKind::Exploit
            | UnitKind::Worm
            | UnitKind::Virus
            | UnitKind::Dos
            | UnitKind::Window
            | UnitKind::LogicBomb
            | UnitKind::Trojan
            | UnitKind::Obelisk => Faction::Hacker,

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
            UnitKind::Terminal | UnitKind::Obelisk => 2.5,
            UnitKind::Socket | UnitKind::Window | UnitKind::Port => 2.0,
            UnitKind::Byte => 2.0,
            UnitKind::Firewall | UnitKind::Exploit | UnitKind::Pointer | UnitKind::Worm => 1.5,
            UnitKind::Dos => 1.3,
            UnitKind::Assembler | UnitKind::Trojan | UnitKind::Gateway | UnitKind::Flow => 1.2,
            UnitKind::BadBlock => 1.0,
            UnitKind::LogicBomb => 0.8,
            UnitKind::Debug => 0.8,
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

    /// Armor class used to look up this unit's entry in a weapon's
    /// `[DAMAGE]` table. Mirrors upstream `armor.txt`.
    pub fn armor_class(self) -> ArmorClass {
        match self {
            UnitKind::Bit | UnitKind::Bug | UnitKind::Exploit | UnitKind::Packet => {
                ArmorClass::Spam
            }
            UnitKind::Pointer | UnitKind::Dos => ArmorClass::Arty,
            UnitKind::Flow => ArmorClass::Flyer,
            UnitKind::Byte | UnitKind::Connection => ArmorClass::Heavy,
            UnitKind::Worm => ArmorClass::Subterranean,
            UnitKind::Assembler | UnitKind::Trojan | UnitKind::Gateway => ArmorClass::Constructor,
            UnitKind::Kernel
            | UnitKind::Socket
            | UnitKind::Hole
            | UnitKind::Window
            | UnitKind::Port
            | UnitKind::Firewall
            | UnitKind::Terminal
            | UnitKind::Obelisk
            | UnitKind::BadBlock => ArmorClass::Building,
            UnitKind::LogicBomb | UnitKind::Debug => ArmorClass::Mine,
            UnitKind::Virus => ArmorClass::Infectious,
            UnitKind::Signal => ArmorClass::Spam,
        }
    }
}

/// Armor classes used to look up damage multipliers in weapon damage
/// tables. Each variant serializes to the lowercase key found in
/// upstream `armor.txt` and in `[DAMAGE]` blocks of `.tdf` weapons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::IntoStaticStr)]
pub enum ArmorClass {
    #[strum(serialize = "spam")]
    Spam,
    #[strum(serialize = "arty")]
    Arty,
    #[strum(serialize = "flyer")]
    Flyer,
    #[strum(serialize = "heavy")]
    Heavy,
    #[strum(serialize = "subterranean")]
    Subterranean,
    #[strum(serialize = "constructor")]
    Constructor,
    #[strum(serialize = "building")]
    Building,
    #[strum(serialize = "mine")]
    Mine,
    #[strum(serialize = "infectious")]
    Infectious,
}

impl ArmorClass {
    /// Lowercase key matching the `[DAMAGE]` entries in weapon TDFs.
    pub fn key(self) -> &'static str {
        self.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armor_class_matches_upstream_armor_txt() {
        assert_eq!(UnitKind::Worm.armor_class(), ArmorClass::Subterranean);
        assert_eq!(UnitKind::LogicBomb.armor_class(), ArmorClass::Mine);
        assert_eq!(UnitKind::Byte.armor_class(), ArmorClass::Heavy);
        assert_eq!(UnitKind::Connection.armor_class(), ArmorClass::Heavy);
        assert_eq!(UnitKind::Pointer.armor_class(), ArmorClass::Arty);
        assert_eq!(UnitKind::Dos.armor_class(), ArmorClass::Arty);
        assert_eq!(UnitKind::Flow.armor_class(), ArmorClass::Flyer);
        assert_eq!(UnitKind::Bit.armor_class(), ArmorClass::Spam);
        assert_eq!(UnitKind::Virus.armor_class(), ArmorClass::Infectious);
        assert_eq!(UnitKind::Assembler.armor_class(), ArmorClass::Constructor);
        assert_eq!(UnitKind::Kernel.armor_class(), ArmorClass::Building);
    }

    #[test]
    fn armor_class_key_matches_damage_table_keys() {
        assert_eq!(ArmorClass::Spam.key(), "spam");
        assert_eq!(ArmorClass::Subterranean.key(), "subterranean");
        assert_eq!(ArmorClass::Mine.key(), "mine");
        assert_eq!(ArmorClass::Infectious.key(), "infectious");
    }
}
