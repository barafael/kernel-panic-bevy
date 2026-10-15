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
/// 6 (M3, G): the host's speed control ([`SimCommand::InternalSpeed`]
/// from the server seat); a `SetSpeed` while slowed down takes effect
/// as Spring's `UserSpeedChange`.
///
/// [`SimCommand::Dropped`]: crate::net::SimCommand::Dropped
/// [`SimCommand::InternalSpeed`]: crate::net::SimCommand::InternalSpeed
/// [`MatchRules`]: crate::net::MatchRules
pub const SIM_VERSION: u32 = 6;

/// The map-independent half of [`Compat`]: what a peer announces when
/// it joins a room, before any map is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    pub sim_version: u32,
    /// FNV-1a of the embedded `assets/units.kpu`.
    pub units_hash: u64,
}

impl Build {
    /// This build's.
    pub fn current() -> Self {
        Self {
            sim_version: SIM_VERSION,
            units_hash: fnv(crate::units::content::bundle::bundle_bytes()),
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
