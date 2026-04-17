/// All unit types across the three factions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::IntoStaticStr, strum::VariantArray)]
pub enum UnitKind {
    // --- System ---
    /// Homebase + primary factory.
    #[strum(serialize = "kernel")]
    Kernel,

    /// Mobile constructor, builds Sockets on datavents.
    #[strum(serialize = "assembler")]
    Assembler,

    /// Basic swarm unit.
    #[strum(serialize = "bit")]
    Bit,

    /// Heavy defensive unit with 70% damage reduction when closed.
    #[strum(serialize = "byte")]
    Byte,

    /// Deployable artillery with the NX Flag ability.
    #[strum(serialize = "pointer")]
    Pointer,

    /// Secondary factory on datavents, auto-produces Bits.
    #[strum(serialize = "socket")]
    Socket,

    /// Defensive structure. Listed here (under System) to preserve
    /// declaration order — it's actually a Network-faction building.
    #[strum(serialize = "firewall")]
    Firewall,

    /// System special building — launches SIGTERM air strikes.
    #[strum(serialize = "terminal")]
    Terminal,

    // --- Hacker ---
    /// Homebase.
    #[strum(serialize = "hole")]
    Hole,

    /// Basic swarm unit, can morph into Exploit.
    #[strum(serialize = "bug")]
    Bug,

    /// Stationary artillery whose damage scales with target distance.
    #[strum(serialize = "exploit")]
    Exploit,

    /// Cloaked ambusher; kills convert the victim into a Virus.
    #[strum(serialize = "worm")]
    Worm,

    /// Spawned from Worm kills — not directly buildable.
    #[strum(serialize = "virus")]
    Virus,

    /// Stuns / paralyzes enemies.
    #[strum(serialize = "dos")]
    Dos,

    /// Secondary factory on datavents.
    #[strum(serialize = "window")]
    Window,

    /// Cloaked suicide mine.
    #[strum(serialize = "logic_bomb")]
    LogicBomb,

    /// Mobile builder, places Hacker structures on datavents.
    #[strum(serialize = "trojan")]
    Trojan,

    /// Hacker special building — infection-gas artillery.
    #[strum(serialize = "obelisk")]
    Obelisk,

    // --- Network ---
    /// Homebase + teleporter.
    #[strum(serialize = "connection")]
    Connection,

    /// Factory on datavents — increments the team's packet buffer.
    #[strum(serialize = "port")]
    Port,

    /// Main combat unit, materialized from the packet buffer.
    #[strum(serialize = "packet")]
    Packet,

    /// Scout unit.
    #[strum(serialize = "signal")]
    Signal,

    /// Mobile builder, places Network structures on datavents.
    #[strum(serialize = "gateway")]
    Gateway,

    /// Airborne assault — speed scales with team small-building count.
    #[strum(serialize = "flow")]
    Flow,

    // --- Shared (buildable by every faction's constructor) ---
    /// One-shot mine/wall clearer. FBI `unitname=mineblaster`,
    /// displayed as "Debug".
    #[strum(serialize = "mineblaster")]
    Debug,

    /// Cheap destructible wall. Blocks movement but not projectiles.
    #[strum(serialize = "badblock")]
    BadBlock,
}

use super::components::Faction;
use strum::VariantArray;

/// All `UnitKind` variants in declaration order. Re-exported from
/// `strum::VariantArray::VARIANTS` so that every added variant
/// automatically appears here without a second edit.
pub const ALL_UNIT_KINDS: &[UnitKind] = UnitKind::VARIANTS;

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

    /// Whether this kind should be spawned on the Showcase map. True
    /// for every mobile unit that's directly buildable in normal play;
    /// false for homebases (stationary, would never appear mid-game),
    /// secondary factories, Firewall / Terminal / Obelisk buildings,
    /// BadBlock / LogicBomb (stationary tactical pieces), and Debug
    /// (one-shot mine).
    pub fn is_showcase_candidate(self) -> bool {
        matches!(
            self,
            UnitKind::Assembler
                | UnitKind::Bit
                | UnitKind::Byte
                | UnitKind::Pointer
                | UnitKind::Bug
                | UnitKind::Exploit
                | UnitKind::Worm
                | UnitKind::Virus
                | UnitKind::Dos
                | UnitKind::Trojan
                | UnitKind::Packet
                | UnitKind::Signal
                | UnitKind::Gateway
                | UnitKind::Flow
        )
    }

    /// "Small building" per upstream `kpunittypes.lua`: the on-datavent
    /// factories, Firewall, Terminal, Obelisk. Used by Kernel Boost and
    /// Flow speed scaling (and anything else that rewards controlling
    /// the map's datavents without counting the homebase).
    pub fn is_small_building(self) -> bool {
        matches!(
            self,
            UnitKind::Socket
                | UnitKind::Window
                | UnitKind::Port
                | UnitKind::Terminal
                | UnitKind::Obelisk
                | UnitKind::Firewall
        )
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

    #[test]
    fn small_building_classifier() {
        assert!(UnitKind::Socket.is_small_building());
        assert!(UnitKind::Window.is_small_building());
        assert!(UnitKind::Port.is_small_building());
        assert!(UnitKind::Terminal.is_small_building());
        assert!(UnitKind::Obelisk.is_small_building());
        assert!(UnitKind::Firewall.is_small_building());
        // Homebases and mobile units are not small buildings.
        assert!(!UnitKind::Kernel.is_small_building());
        assert!(!UnitKind::Hole.is_small_building());
        assert!(!UnitKind::Connection.is_small_building());
        assert!(!UnitKind::Bit.is_small_building());
    }
}
