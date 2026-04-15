/// All unit types across the three factions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitKind {
    // --- System ---
    Kernel,    // homebase + primary factory
    Assembler, // mobile constructor, builds Sockets on datavents
    Bit,       // basic swarm unit
    Byte,      // heavy defensive unit (15k HP, 70% damage reduction when closed)
    Pointer,   // deployable artillery + NX Flag ability
    Socket,    // secondary factory on datavents, auto-produces Bits
    Firewall,  // defensive structure

    // --- Hacker ---
    Hole,      // homebase
    Bug,       // basic swarm unit, can morph into Exploit
    Exploit,   // stationary artillery (increasing damage at range)
    Worm,      // cloaked ambusher, kills convert to Viruses
    Virus,     // spawned from Worm kills, not directly buildable
    Dos,       // stuns/paralyzes enemies
    Window,    // secondary factory on datavents
    LogicBomb, // suicide unit

    // --- Network ---
    Connection, // homebase + teleporter
    Port,       // factory on datavents, increments Buffer
    Packet,     // main combat unit, materialized from Buffer
    Signal,     // scout unit
}

/// Static stats for a unit type.
#[allow(dead_code)]
pub struct UnitStats {
    pub kind: UnitKind,
    pub name: &'static str,
    pub max_health: f32,
    pub speed: f32,
    pub build_time: f32,
    pub is_building: bool,
    /// Mesh scale relative to default (1.0 = normal).
    pub mesh_scale: f32,
    /// s3o model filename from upstream (e.g. "kernel.s3o").
    pub model: &'static str,
    /// TDF weapon section name (e.g. "Rock", "BugShot"), or `""` for unarmed units.
    pub weapon: &'static str,
    /// Weapon range in elmos (0 = no weapon). Fallback when TDF is unavailable.
    pub attack_range: f32,
    /// Damage per hit. Fallback when TDF is unavailable.
    pub attack_damage: f32,
    /// Seconds between attacks. Fallback when TDF is unavailable.
    pub attack_cooldown: f32,
    /// COB animation script filename (e.g. "kernel.cob").
    pub script: &'static str,
}

pub fn stats(kind: UnitKind) -> &'static UnitStats {
    UNIT_STATS
        .iter()
        .find(|s| s.kind == kind)
        .expect("missing unit stats")
}

pub static UNIT_STATS: &[UnitStats] = &[
    // --- System ---
    UnitStats {
        kind: UnitKind::Kernel,
        name: "Kernel",
        max_health: 10000.0,
        speed: 0.0,
        build_time: 0.0,
        is_building: true,
        mesh_scale: 3.0,
        model: "kernel.s3o",
        weapon: "",
        attack_range: 0.0,
        attack_damage: 0.0,
        attack_cooldown: 0.0,
        script: "kernel.cob",
    },
    UnitStats {
        kind: UnitKind::Assembler,
        name: "Assembler",
        max_health: 1000.0,
        speed: 60.0,
        build_time: 15.0,
        is_building: false,
        mesh_scale: 1.2,
        model: "assembler.s3o",
        weapon: "",
        attack_range: 0.0,
        attack_damage: 0.0,
        attack_cooldown: 0.0,
        script: "assembler.cob",
    },
    UnitStats {
        kind: UnitKind::Bit,
        name: "Bit",
        max_health: 150.0,
        speed: 90.0,
        build_time: 3.0,
        is_building: false,
        mesh_scale: 0.5,
        model: "ball.s3o",
        weapon: "Line",
        attack_range: 256.0,
        attack_damage: 80.0,
        attack_cooldown: 0.5,
        script: "bit.cob",
    },
    UnitStats {
        kind: UnitKind::Byte,
        name: "Byte",
        max_health: 15000.0,
        speed: 30.0,
        build_time: 30.0,
        is_building: false,
        mesh_scale: 2.0,
        model: "octaeder.s3o",
        weapon: "MegaBeam",
        attack_range: 512.0,
        attack_damage: 200.0,
        attack_cooldown: 2.0,
        script: "byte.cob",
    },
    UnitStats {
        kind: UnitKind::Pointer,
        name: "Pointer",
        max_health: 2000.0,
        speed: 40.0,
        build_time: 25.0,
        is_building: false,
        mesh_scale: 1.5,
        model: "cube.s3o",
        weapon: "Geometric",
        attack_range: 1400.0,
        attack_damage: 4000.0,
        attack_cooldown: 4.0,
        script: "pointer.cob",
    },
    UnitStats {
        kind: UnitKind::Socket,
        name: "Socket",
        max_health: 5000.0,
        speed: 0.0,
        build_time: 20.0,
        is_building: true,
        mesh_scale: 2.0,
        model: "socket.s3o",
        weapon: "",
        attack_range: 0.0,
        attack_damage: 0.0,
        attack_cooldown: 0.0,
        script: "socket.cob",
    },
    UnitStats {
        kind: UnitKind::Firewall,
        name: "Firewall",
        max_health: 8000.0,
        speed: 0.0,
        build_time: 15.0,
        is_building: true,
        mesh_scale: 1.5,
        model: "network_super.s3o",
        weapon: "",
        attack_range: 0.0,
        attack_damage: 0.0,
        attack_cooldown: 0.0,
        script: "firewall.cob",
    },
    // --- Hacker ---
    UnitStats {
        kind: UnitKind::Hole,
        name: "Hole",
        max_health: 10000.0,
        speed: 0.0,
        build_time: 0.0,
        is_building: true,
        mesh_scale: 3.0,
        model: "holeNEW.s3o",
        weapon: "",
        attack_range: 0.0,
        attack_damage: 0.0,
        attack_cooldown: 0.0,
        script: "hole.cob",
    },
    UnitStats {
        kind: UnitKind::Bug,
        name: "Bug",
        max_health: 150.0,
        speed: 90.0,
        build_time: 3.0,
        is_building: false,
        mesh_scale: 0.5,
        model: "bugNEW.s3o",
        weapon: "BugShot",
        attack_range: 256.0,
        attack_damage: 80.0,
        attack_cooldown: 0.5,
        script: "bug.cob",
    },
    UnitStats {
        kind: UnitKind::Exploit,
        name: "Exploit",
        max_health: 3000.0,
        speed: 0.0,
        build_time: 0.0,
        is_building: true,
        mesh_scale: 1.5,
        model: "bugNEW.s3o",
        weapon: "BugCannon",
        attack_range: 512.0,
        attack_damage: 200.0,
        attack_cooldown: 2.0,
        script: "exploit.cob",
    },
    UnitStats {
        kind: UnitKind::Worm,
        name: "Worm",
        max_health: 2500.0,
        speed: 70.0,
        build_time: 20.0,
        is_building: false,
        mesh_scale: 1.5,
        model: "wormNEW.s3o",
        weapon: "Wormbite",
        attack_range: 5.0,
        attack_damage: 1500.0,
        attack_cooldown: 2.0,
        script: "worm.cob",
    },
    UnitStats {
        kind: UnitKind::Virus,
        name: "Virus",
        max_health: 200.0,
        speed: 80.0,
        build_time: 0.0,
        is_building: false,
        mesh_scale: 0.6,
        model: "virus.s3o",
        weapon: "VirusDeath",
        attack_range: 256.0,
        attack_damage: 80.0,
        attack_cooldown: 0.5,
        script: "virus.cob",
    },
    UnitStats {
        kind: UnitKind::Dos,
        name: "DOS",
        max_health: 1500.0,
        speed: 50.0,
        build_time: 15.0,
        is_building: false,
        mesh_scale: 1.3,
        model: "dos.s3o",
        weapon: "DOS_Beam",
        attack_range: 256.0,
        attack_damage: 50.0,
        attack_cooldown: 1.0,
        script: "dos.cob",
    },
    UnitStats {
        kind: UnitKind::Window,
        name: "Window",
        max_health: 5000.0,
        speed: 0.0,
        build_time: 20.0,
        is_building: true,
        mesh_scale: 2.0,
        model: "window.s3o",
        weapon: "",
        attack_range: 0.0,
        attack_damage: 0.0,
        attack_cooldown: 0.0,
        script: "window.cob",
    },
    UnitStats {
        kind: UnitKind::LogicBomb,
        name: "Logic Bomb",
        max_health: 500.0,
        speed: 100.0,
        build_time: 10.0,
        is_building: false,
        mesh_scale: 0.8,
        model: "logic_bomb.s3o",
        weapon: "logic_bomb",
        attack_range: 5.0,
        attack_damage: 2000.0,
        attack_cooldown: 0.0, // suicide unit — single use
        script: "logic_bomb.cob",
    },
    // --- Network ---
    UnitStats {
        kind: UnitKind::Connection,
        name: "Connection",
        max_health: 10000.0,
        speed: 0.0,
        build_time: 0.0,
        is_building: true,
        mesh_scale: 3.0,
        model: "network_big.s3o",
        weapon: "",
        attack_range: 0.0,
        attack_damage: 0.0,
        attack_cooldown: 0.0,
        script: "connection.cob",
    },
    UnitStats {
        kind: UnitKind::Port,
        name: "Port",
        max_health: 5000.0,
        speed: 0.0,
        build_time: 20.0,
        is_building: true,
        mesh_scale: 2.0,
        model: "network_minifac.s3o",
        weapon: "",
        attack_range: 0.0,
        attack_damage: 0.0,
        attack_cooldown: 0.0,
        script: "port.cob",
    },
    UnitStats {
        kind: UnitKind::Packet,
        name: "Packet",
        max_health: 300.0,
        speed: 80.0,
        build_time: 4.0,
        is_building: false,
        mesh_scale: 0.6,
        model: "network_spam.s3o",
        weapon: "PacketBeam",
        attack_range: 250.0,
        attack_damage: 130.0,
        attack_cooldown: 0.75,
        script: "packet.cob",
    },
    UnitStats {
        kind: UnitKind::Signal,
        name: "Signal",
        max_health: 100.0,
        speed: 120.0,
        build_time: 5.0,
        is_building: false,
        mesh_scale: 0.4,
        model: "signal.s3o",
        weapon: "",
        attack_range: 0.0,
        attack_damage: 0.0,
        attack_cooldown: 0.0, // scout, no weapon
        script: "signal.cob",
    },
];
