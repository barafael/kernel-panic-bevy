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
    /// Homebase. The Network's main factory: stationary, builds mobile
    /// units (Packet / Connection / Flow / Gateway). Loaded from
    /// upstream `carrier.fbi`. Separate from the mobile `Connection`
    /// teleporter, which shares no assets with it.
    #[strum(serialize = "carrier")]
    Carrier,

    /// Mobile teleporter. Dispatches Packets from the shared buffer and
    /// absorbs them back on `Enter`. Not a homebase — see `Carrier` for
    /// the Network commander.
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

use crate::units::components::Faction;
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
    /// the caller has no builder context. Shared units (Debug, BadBlock)
    /// default to System but are spawned with the builder's faction in
    /// real gameplay.
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
            | UnitKind::Carrier
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
            UnitKind::Kernel | UnitKind::Hole | UnitKind::Carrier | UnitKind::Connection => 3.0,
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
    /// Per upstream KP's `Kernel_Panic_readme.txt`: almost all BOS were
    /// compiled with 65536 (Spring's engine default). The exceptions —
    /// compiled with 163840 — are Kernel, Socket, Assembler, Bit, Byte,
    /// Logic Bomb, Bad Block (plus ExpScout and Rock from the expansion,
    /// which aren't in our UnitKind).
    pub fn cob_linear_constant(self) -> f32 {
        match self {
            UnitKind::Kernel
            | UnitKind::Socket
            | UnitKind::Assembler
            | UnitKind::Bit
            | UnitKind::Byte
            | UnitKind::LogicBomb
            | UnitKind::BadBlock => 163840.0,
            _ => 65536.0,
        }
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

    /// Mobile constructors that can erect secondary factories on
    /// datavents. Mirrors upstream KP's `SIDEDATA.TDF` builder list.
    pub fn is_constructor(self) -> bool {
        matches!(
            self,
            UnitKind::Assembler | UnitKind::Trojan | UnitKind::Gateway
        )
    }

    /// Network-faction teleporters: units that dispatch packets from
    /// and absorb packets into the shared buffer.
    pub fn is_teleporter(self) -> bool {
        matches!(self, UnitKind::Port | UnitKind::Connection)
    }

    /// Units that carry a `D`-hotkey command-fire ability (NX Flag,
    /// Infection, Protect, Mine Launch, SIGTERM). The UI picks its label
    /// separately; only the eligibility set lives here.
    pub fn has_command_fire_ability(self) -> bool {
        matches!(
            self,
            UnitKind::Pointer
                | UnitKind::Obelisk
                | UnitKind::Firewall
                | UnitKind::Byte
                | UnitKind::Terminal
        )
    }

    /// The paired unit for Bug ↔ Exploit deploy. Returns `None` for
    /// units that can't deploy. Mutual: `X.deploy_pair() == Some(Y)`
    /// implies `Y.deploy_pair() == Some(X)`.
    pub fn deploy_pair(self) -> Option<UnitKind> {
        match self {
            UnitKind::Bug => Some(UnitKind::Exploit),
            UnitKind::Exploit => Some(UnitKind::Bug),
            _ => None,
        }
    }

    /// Units that carry the `Cloaked` marker at spawn time (§3.3).
    pub fn spawns_cloaked(self) -> bool {
        matches!(self, UnitKind::Worm | UnitKind::LogicBomb)
    }

    /// Burrowing units — allowed to sit below the heightmap surface.
    /// Every other ground unit is re-clamped to terrain height each
    /// frame so physics/collision pushes can't slide it into the mesh.
    /// The Worm is the one intentional exception (its ambush animation
    /// sinks it underground).
    pub fn is_subterranean(self) -> bool {
        matches!(self, UnitKind::Worm)
    }

    /// Documented exception to `NoChaseCategory=VTOL`: units whose
    /// projectile homes on air targets despite the FBI filter.
    /// FEATURES.md §12 calls this out specifically for the Pointer —
    /// its `octashot.s3o` round tracks ground *and* air (Flows). Every
    /// other ground unit still respects `NoChaseCategory=VTOL`.
    pub fn homing_targets_air(self) -> bool {
        matches!(self, UnitKind::Pointer)
    }

    /// Units that only auto-fire against mine-class targets. Debug
    /// (`mineblaster.fbi OnlyTargetCategory1=VOID`) is a defensive
    /// Minekiller turret — letting it aggro infantry would waste its
    /// 0.11s reload on pointless 20-HP hits.
    pub fn targets_mines_only(self) -> bool {
        matches!(self, UnitKind::Debug)
    }

    /// Valid targets for a Minekiller. Matches upstream `Debug`'s
    /// declared role: "removes logic bombs and bad blocks in the area".
    pub fn is_minekiller_target(self) -> bool {
        matches!(self, UnitKind::LogicBomb | UnitKind::BadBlock)
    }

    /// Any directly-buildable mobile combat unit (excludes constructors,
    /// Viruses spawned dynamically, LogicBombs, and support like Terminal).
    pub fn is_combat_unit(self) -> bool {
        matches!(
            self,
            UnitKind::Bit
                | UnitKind::Byte
                | UnitKind::Pointer
                | UnitKind::Bug
                | UnitKind::Exploit
                | UnitKind::Worm
                | UnitKind::Dos
                | UnitKind::Packet
                | UnitKind::Signal
                | UnitKind::Flow
        )
    }

    /// Every static structure that claims map territory: homebases,
    /// secondary factories, walls, and special buildings. Broader than
    /// [`Self::is_small_building`] — includes the homebases.
    pub fn is_building(self) -> bool {
        matches!(
            self,
            UnitKind::Kernel
                | UnitKind::Hole
                | UnitKind::Carrier
                | UnitKind::Socket
                | UnitKind::Window
                | UnitKind::Port
                | UnitKind::Firewall
                | UnitKind::Terminal
                | UnitKind::Obelisk
                | UnitKind::BadBlock
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
            | UnitKind::Carrier
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
        assert!(!UnitKind::Carrier.is_small_building());
        assert!(!UnitKind::Connection.is_small_building());
        assert!(!UnitKind::Bit.is_small_building());
    }

    #[test]
    fn constructor_classifier() {
        assert!(UnitKind::Assembler.is_constructor());
        assert!(UnitKind::Trojan.is_constructor());
        assert!(UnitKind::Gateway.is_constructor());
        assert!(!UnitKind::Bit.is_constructor());
        assert!(!UnitKind::Kernel.is_constructor());
    }

    /// Only the Pointer's `octashot.s3o` homes on air targets
    /// (FEATURES.md §12). Every other ground unit respects its FBI
    /// `NoChaseCategory=VTOL` filter. The combat system AND-combines
    /// `no_chase_vtol` with `!homing_targets_air` to decide whether to
    /// skip flying candidates, so widening this list without
    /// intending to would let any unit tag Flows.
    #[test]
    fn only_pointer_homes_on_air_targets() {
        assert!(UnitKind::Pointer.homing_targets_air());
        for kind in [
            UnitKind::Bit,
            UnitKind::Byte,
            UnitKind::Bug,
            UnitKind::Exploit,
            UnitKind::Dos,
            UnitKind::Packet,
            UnitKind::Flow,
            UnitKind::Worm,
            UnitKind::Terminal,
            UnitKind::Obelisk,
        ] {
            assert!(
                !kind.homing_targets_air(),
                "{kind:?} should not bypass NoChaseCategory=VTOL",
            );
        }
    }

    #[test]
    fn teleporter_classifier() {
        assert!(UnitKind::Port.is_teleporter());
        assert!(UnitKind::Connection.is_teleporter());
        assert!(!UnitKind::Packet.is_teleporter());
        assert!(!UnitKind::Kernel.is_teleporter());
    }

    #[test]
    fn spawns_cloaked_classifier() {
        assert!(UnitKind::Worm.spawns_cloaked());
        assert!(UnitKind::LogicBomb.spawns_cloaked());
        assert!(!UnitKind::Bit.spawns_cloaked());
        assert!(!UnitKind::Assembler.spawns_cloaked());
    }

    #[test]
    fn combat_unit_classifier() {
        assert!(UnitKind::Bit.is_combat_unit());
        assert!(UnitKind::Bug.is_combat_unit());
        assert!(UnitKind::Packet.is_combat_unit());
        assert!(UnitKind::Byte.is_combat_unit());
        assert!(!UnitKind::Assembler.is_combat_unit());
        assert!(!UnitKind::Virus.is_combat_unit());
        assert!(!UnitKind::LogicBomb.is_combat_unit());
        assert!(!UnitKind::Kernel.is_combat_unit());
    }

    #[test]
    fn building_classifier() {
        assert!(UnitKind::Kernel.is_building());
        assert!(UnitKind::Socket.is_building());
        assert!(UnitKind::Firewall.is_building());
        assert!(UnitKind::BadBlock.is_building());
        assert!(!UnitKind::Bit.is_building());
        assert!(!UnitKind::Assembler.is_building());
    }

    #[test]
    fn deploy_pair_is_mutual_and_limited_to_bug_exploit() {
        assert_eq!(UnitKind::Bug.deploy_pair(), Some(UnitKind::Exploit));
        assert_eq!(UnitKind::Exploit.deploy_pair(), Some(UnitKind::Bug));
        assert_eq!(UnitKind::Bit.deploy_pair(), None);
        assert_eq!(UnitKind::Kernel.deploy_pair(), None);
        assert_eq!(UnitKind::Packet.deploy_pair(), None);
    }

    #[test]
    fn command_fire_ability_set_does_not_overlap_deploy_set() {
        for kind in ALL_UNIT_KINDS {
            if kind.has_command_fire_ability() {
                assert!(
                    kind.deploy_pair().is_none(),
                    "{kind:?} has both an ability and a deploy pair",
                );
            }
        }
        assert!(UnitKind::Pointer.has_command_fire_ability());
        assert!(UnitKind::Byte.has_command_fire_ability());
        assert!(!UnitKind::Bit.has_command_fire_ability());
        assert!(!UnitKind::Bug.has_command_fire_ability());
    }

    #[test]
    fn debug_only_targets_mines_and_walls() {
        // Attacker side.
        assert!(UnitKind::Debug.targets_mines_only());
        assert!(!UnitKind::Bit.targets_mines_only());
        assert!(!UnitKind::Byte.targets_mines_only());

        // Target side.
        assert!(UnitKind::LogicBomb.is_minekiller_target());
        assert!(UnitKind::BadBlock.is_minekiller_target());
        assert!(!UnitKind::Bit.is_minekiller_target());
        assert!(!UnitKind::Byte.is_minekiller_target());
        assert!(!UnitKind::Worm.is_minekiller_target());
        assert!(!UnitKind::Kernel.is_minekiller_target());
    }
}
