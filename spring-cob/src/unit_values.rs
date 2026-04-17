//! Well-known COB value keys (for `Opcode::Get` / `Opcode::Set`).
//!
//! Spring's engine exposes a fixed set of integer keys that COB scripts
//! read with `get KEY` and write with `set KEY to VAL`. The numeric
//! values come from the engine's `Sim/Units/Scripts/CobInstance.h` and
//! are part of the COB ABI — changing them would break every script.
//!
//! Only the keys our host actively publishes need accurate numbers; the
//! rest are listed for completeness so future wiring has a one-stop
//! reference.

/// `set ACTIVATION` — factory yard open/closed (1/0).
pub const ACTIVATION: i32 = 1;
/// `get/set STANDINGMOVEORDERS` — 0=hold, 1=maneuver, 2=roam.
pub const STANDINGMOVEORDERS: i32 = 2;
/// `get/set STANDINGFIREORDERS` — 0=hold, 1=return, 2=fire-at-will.
pub const STANDINGFIREORDERS: i32 = 3;
/// `get HEALTH` — current hp.
pub const HEALTH: i32 = 4;
/// `set INBUILDSTANCE` — factory has its yard locked open for production.
pub const INBUILDSTANCE: i32 = 5;
/// `get BUSY` — engine-side "is this unit currently doing a script-blocking thing".
pub const BUSY: i32 = 6;
/// `get PIECE_XZ(piece)` — packed (x,z) of a piece.
pub const PIECE_XZ: i32 = 7;
/// `get PIECE_Y(piece)` — y of a piece.
pub const PIECE_Y: i32 = 8;
/// `get UNIT_XZ(unit)` — packed (x,z) of a unit.
pub const UNIT_XZ: i32 = 9;
/// `get UNIT_Y(unit)` — y of a unit.
pub const UNIT_Y: i32 = 10;
/// `get UNIT_HEIGHT(unit)` — height of a unit (0 if unit doesn't exist).
pub const UNIT_HEIGHT: i32 = 11;
/// `get XZ_ATAN(packed_xz)` — atan2 over packed coords.
pub const XZ_ATAN: i32 = 12;
/// `get XZ_HYPOT(packed_xz)` — hypot of packed coords.
pub const XZ_HYPOT: i32 = 13;
/// `get ATAN(x, z)` — atan2.
pub const ATAN: i32 = 14;
/// `get HYPOT(x, z)` — hypot.
pub const HYPOT: i32 = 15;
/// `get GROUND_HEIGHT(packed_xz)` — terrain y at xz.
pub const GROUND_HEIGHT: i32 = 16;
/// `get BUILD_PERCENT_LEFT` — 100 = just spawned, 0 = fully built. Read
/// by unit `Create()` scripts to drive their emerge animation
/// (rise-out-of-ground for System units, alpha-fade for Hacker/Network).
pub const BUILD_PERCENT_LEFT: i32 = 17;
/// `set YARD_OPEN` — factory yardmap open/closed.
pub const YARD_OPEN: i32 = 18;
/// `get/set BUGGER_OFF` — engine-side "shoo nearby units away".
pub const BUGGER_OFF: i32 = 19;
/// `set ARMORED` — toggles damage immunity (used while building).
pub const ARMORED: i32 = 20;
/// `get/set MAX_SPEED` — top speed in elmos/frame.
pub const MAX_SPEED: i32 = 22;
/// `get/set HEADING` — unit heading in 16-bit angular units.
pub const HEADING: i32 = 82;
/// `get TARGET_ID(weapon_num)` — engine-tracked current target.
pub const TARGET_ID: i32 = 83;
