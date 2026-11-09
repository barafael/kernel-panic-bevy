//! What two peers (or a peer and a replay) must share for lockstep to
//! mean anything: the same sim code, as far as [`SIM_VERSION`] tells,
//! the same unit data and the same map bytes.
//!
//! The lobby compares the stamps before a match ([`super::lobby`]) and
//! refuses a mismatch with the reason; a replay refuses to play against
//! another build (`headless::replay`). Spring did the same with its
//! engine version (`CGameServer::BindConnection` refuses another
//! version, `rts/Net/GameServer.cpp:2893-2906`) and the archive
//! checksums in the start script (`CPreGame::GameDataReceived`,
//! `rts/Game/PreGame.cpp:617-657`; Recoil only logs those now — a
//! lockstep sim on different data is a certain desync, so we refuse).

use serde::{Deserialize, Serialize};

use super::state::Fnv;

/// Bumped by hand whenever the simulation changes on purpose in a way
/// that changes outcomes (then the golden logs are re-recorded; see
/// `net-experiments/m0-golden-replay.md`). The unit bundle and map
/// hashes catch data changes on their own.
///
/// 2 (M1, A): per-seat owners and ally teams, shared victory and
/// defeat, the sim no longer stops for the local outcome, `DetFloat`
/// max / min / clamp.
/// 3 (M1, B on top): commands are validated when they apply
/// (`net-experiments/m1-commands.md`): attacks need a weapon that can
/// hit the target, points must lie on the map (the AI's scattered
/// pushes near an edge lose those units' orders, as in Spring).
/// 4 (2026-10-15, the user's leaving rules): a leaver's units wait for
/// an ally's `Take` (an AI ally takes them itself) or, with the last
/// active owner of the ally team gone, blow up within 4 s; unit caps
/// never change.
/// 5 (M2): a `Take` hands over the leaver's packet buffer with its
/// units; a peer dropping out leaves play ([`SimCommand::Dropped`]);
/// pause, speed and cheats follow the setup's [`MatchRules`].
/// 6 (M3): the host's speed control ([`SimCommand::InternalSpeed`]
/// from the server seat; a `SetSpeed` while slowed down takes effect
/// as Spring's `UserSpeedChange`); a dropped player's seat returns to
/// play ([`SimCommand::Returned`]).
/// 7 (2026-10-19): a factory keeps the move, fight, patrol and guard
/// orders it is given for the units it builds (`FactoryOrders`); before,
/// a move reached it and did nothing, the rest were refused.
/// 8 (2026-10-19): straight-firing weapons can't shoot through terrain
/// (an exact ground test for every target and shot; beams stop at the
/// ground; units close in on a target behind a hill); the SIGTERM
/// Signal takes off and flies as an airplane and its bomb falls on a
/// ballistic arc.
/// 9 (2026-10-19): Launch Mines is untargeted: the Byte unfolds and
/// fires five MineLauncher bolts from its tubes, and a Logic Bomb
/// appears where each one explodes; before, five mines appeared in a
/// line at the clicked point.
/// 10 (2026-10-19): the maps' own features stand (the Bad Blocks of
/// Quad Core, Dual Core, Corrupted Core and DigitalDivide, Central Hub's
/// unusable sectors): they block, take damage and can be crushed.
/// 11 (2026-10-19): a finished Bad Block turns into its feature
/// (`IsFeature`): no longer a unit of its team.
/// 12 (2026-10-19): a feature in the way holds a straight shot
/// (`avoidFeature`) and stops a beam; attack orders close in for a line.
/// 13 (2026-10-20): a builder clears a feature off its build site before
/// it builds; a Bad Block ordered onto a Bad Block is already built.
/// 14 (2026-10-20): DigitalDivide's and Speed Balls' water hurts
/// (`map_events::water`); and, unversioned since 66fa258, a unit's
/// midpoint (aim point, collision sphere) lies along its up vector and a
/// mobile builder keeps its slope tilt while it builds.
/// 15 (2026-10-22): five orders appended — area attack (expanded when
/// applied as `SelectAttackNet`), a constructor's clear of one feature
/// or an area (`ExecuteReclaim`), the Bug's Bombard (`bug_bombard.lua`)
/// and a Packet's walk-and-enter — the Security Hole's AutoHold, which
/// its Worms inherit, and a folded Pointer or Byte that chases and stops
/// in range (only its shot waits for `Open`); and, unversioned since
/// `3a94ed2`, `CanAttack=0` disarms a unit whatever its `Weapon1` says
/// (`UnitRegistry::weapon`), so the Trojan no longer auto-fires its
/// `BuildLightning`.
/// 16 (2026-10-25, the parity audit): a Worm's bite detonates its
/// Wormsplash (the 800 splash and the infection); ground weapons shoot
/// Flows in range (`NoChaseCategory` only stops chasing); a dispatched
/// Packet's spawn protection lasts 16 frames, not 5 s; the stun meter is
/// the engine's (capped at `paralyzetime` of drain, drained every slow
/// update, kept when the stun lifts).
/// 17 (2026-10-25): an AI with no enemy left razes itself only while it
/// has more than one unit (upstream `SuicideIfAlone`).
/// 18 (2026-10-26): Dispatch is `network_dispatch.lua`'s: an order run on
/// the teleporter's slow updates, one pass over its 12 grid spots each
/// (a taken spot skipped and paid nothing, the Packets facing out), with
/// Alt a pass per slow update until the buffer is empty; it replaces the
/// teleporter's orders. The buffer is clamped to the room under the unit
/// limit every 8th frame (`network_buffer.lua`).
/// 19 (2026-10-26): every mover class's QTPFS layer is brought up to the
/// nav revision each tick (before, on its class's next request); auto-
/// target rescans land on the unit's slow updates; a path is dirtied by
/// each changed rectangle on its own, not by their bounding box.
/// 20 (2026-10-26, keyboard shortcuts): `SelfDestruct` says start or
/// call off (`Ctrl+D` again); a start leaves a running countdown its
/// time. A `Build` goes in instead of the builder's orders, behind them
/// or (Alt) at the head of its queue.
/// 21 (2026-11-08, parity review): a unit at 0 HP takes no further hit
/// that frame (`DoDamage`'s `isDead`); a paralyzed unit's self-destruct
/// countdown and a paralyzed Logic Bomb's trigger stand still, and its
/// weapons stop reloading (`++reloadStatus` per stunned frame); the
/// Firewall protects for 640 frames (21.3 s) within a 300-elmo plane
/// cylinder; a Flow's starburst range counts `heightmod` twice
/// (`CStarburstLauncher::GetRange2D`); and `Sign(0)` is -1 as in the
/// engine (exact ties in the push, avoidance and strafe signs).
/// 22 (2026-11-08, parity review, second pass): line of sight and radar
/// are rules — weapons and attack orders only take targets their team has
/// in sight or on radar (radar-only ones aimed at a drifting blip, ranked
/// below seen ones), only Assemblers, Trojans and Gateways run radar, and
/// an attack order on a mobile target ends 60 frames after it is lost; a
/// unit under construction is in the spatial index and its health follows
/// its build progress; homebases start on Repeat (the AI turns it off),
/// AI teams run while they own any factory; Worms and Logic Bombs cloak
/// only once built (a Worm 193 frames after); explosion fronts reaching a
/// victim after the attacker is gone have no attacker; an NX shell whose
/// Pointer died leaves no zone; infected Packets entering a Port and
/// morphing Bugs/Exploits release their Virus; the kamikaze scan runs on
/// the unit's slow update; the SIGTERM Signal flies as a bomber (thrust
/// 0.065, turn radius 1000) and drops within 12 frames of the target; the
/// Obelisk's Infection is a high-arc shell with a 200/1500 impact blast
/// that starts the gas where it lands.
/// 23 (2026-11-09, parity review, third pass): constructors repair
/// (`Order::Repair`, `maxHealth · WorkerTime / BuildTime` a second); a
/// Bug or Exploit morphs only when `morphable` / `idle`, then spends 88
/// silent frames pinned before the swap, which keeps heading and queue; a
/// finished infected frame releases its Virus; a melee weapon's range
/// runs to the target's surface; a DOS/Gauss beam is `beamtime · 30`
/// one-frame sub-beams with a 50 % falloff at range, and a salvo waits for
/// its last sub-beam; an east/west-facing building rotates its yardmap; a
/// build order reserves no datavent (only a nanoframe or building does).
/// 24 (2026-11-09, parity review, fourth pass): a Worm's radar and seismic
/// sense start 152 frames after it is built (worm.bos `ACTIVATION`); the
/// result of a Bug/Exploit morph is off the blocking map (`SetUnitBlocking
/// (false)`: nothing collides with, avoids or paths round it; a Bug is
/// blocking again once a slow update finds it on another square, an
/// Exploit never is); a Worm holding fire (cloaked, AutoHold on) neither
/// engages idle nor diverts a Fight.
/// 25 (2026-11-09, parity review, fifth pass): `SimCommand::Cast` carries
/// a `queue` flag (Shift: an aimed ability waits its turn in the unit's
/// queue as `QueuedCommand::Cast`); a builder resumes its own unfinished
/// nanoframe with a Repair order (only it: `CanAssist=0`); Enter is
/// refused on a stunned teleporter.
/// 26 (2026-11-09, CI of 25): the Bug/Exploit morphs that finish on the
/// same tick are spawned in `NetId` order (their pairs took their ids in
/// query order, so the ids, and the sim from there on, depended on the
/// ECS storage order and diverged between a shuffled and a plain run).
/// 27 (2026-11-09, CI of 26): idle and Fight engagement break a tie
/// between two equally distant targets by `NetId`, not by the Bevy
/// `Entity` (which follows the allocation order: wasm and a shuffled run
/// picked another target than native, so the orders hash and from there
/// the sim diverged).
///
/// [`SimCommand::Dropped`]: crate::net::SimCommand::Dropped
/// [`SimCommand::Returned`]: crate::net::SimCommand::Returned
/// [`SimCommand::InternalSpeed`]: crate::net::SimCommand::InternalSpeed
/// [`MatchRules`]: crate::net::MatchRules
pub const SIM_VERSION: u32 = 27;

/// The map-independent half of [`Compat`]: what a peer announces when
/// it joins a room, before any map is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    pub sim_version: u32,
    /// FNV-1a of the embedded unit data: `assets/units.kpu` followed by
    /// every definition file of `assets/defs/` (`units::content::defs`) —
    /// which no longer includes `assets/defs/explosions/`: the CEGs are
    /// code now, and code is [`SIM_VERSION`]'s business, not the data
    /// hash's. Peers may therefore disagree about CEGs, which is sound:
    /// nothing a CEG spawns enters the sim hash (`net::state`).
    pub units_hash: u64,
}

impl Build {
    /// This build's.
    pub fn current() -> Self {
        Self {
            sim_version: SIM_VERSION,
            units_hash: {
                let mut h = Fnv::default();
                h.bytes(crate::units::content::bundle::bundle_bytes());
                for text in crate::units::content::defs::texts() {
                    h.bytes(text.as_bytes());
                }
                h.finish()
            },
        }
    }

    /// Why a peer with `theirs` cannot play with this build, in words.
    pub fn mismatch(&self, theirs: &Build) -> Option<String> {
        if theirs.sim_version != self.sim_version {
            Some(format!(
                "runs game version {} (this one runs {}); update the game",
                theirs.sim_version, self.sim_version
            ))
        } else if theirs.units_hash != self.units_hash {
            Some(format!(
                "has different unit data ({:08x}, here {:08x}); update the game",
                theirs.units_hash as u32, self.units_hash as u32
            ))
        } else {
            None
        }
    }
}

/// What a replay needs to be meaningful, and what every peer of a
/// match must share: the same sim code (as far as [`SIM_VERSION`]
/// tells), the same unit data, the same map bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compat {
    pub sim_version: u32,
    /// FNV-1a of the embedded `assets/units.kpu`.
    pub units_hash: u64,
    /// FNV-1a of the baked `.kpmap`.
    pub map_hash: u64,
}

impl Compat {
    /// This build's stamp for these map bytes.
    #[cfg_attr(not(any(test, feature = "replay")), allow(dead_code))]
    pub fn current(map_bytes: &[u8]) -> Self {
        Self::of(Build::current(), map_hash(map_bytes))
    }

    pub fn of(build: Build, map_hash: u64) -> Self {
        Self {
            sim_version: build.sim_version,
            units_hash: build.units_hash,
            map_hash,
        }
    }

    pub fn build(&self) -> Build {
        Build {
            sim_version: self.sim_version,
            units_hash: self.units_hash,
        }
    }
}

/// The map content hash [`Compat::map_hash`] carries.
pub fn map_hash(map_bytes: &[u8]) -> u64 {
    fnv(map_bytes)
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = Fnv::default();
    h.bytes(bytes);
    h.finish()
}
