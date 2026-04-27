//! Well-known COB value keys and helpers.
//!
//! Spring's engine exposes a fixed set of integer keys that COB scripts
//! read with `get KEY` and write with `set KEY to VAL`. The numeric values
//! come from upstream `rts/Sim/Units/Scripts/CobDefines.h` and are part of
//! the COB ABI — changing them would break every script. Re-using the
//! same names (and snake-cased SFX_*) makes it trivial to cross-reference
//! `.bos` source against this crate.
//!
//! [CobDefines.h]: https://github.com/beyond-all-reason/RecoilEngine/blob/master/rts/Sim/Units/Scripts/CobDefines.h
//!
//! Three categories live here:
//!
//! - **Get/Set keys** ([`ACTIVATION`] .. [`PIECE_PITCH`]): integer keys
//!   the host publishes with [`CobVm::set_unit_value`](crate::CobVm::set_unit_value).
//!   The VM also answers a few of these *itself*, without the host —
//!   see [`builtin_get_value`].
//! - **`emit-sfx` types** ([`SFX_VTOL`] .. [`SFX_GLOBAL`]): the integer
//!   selectors that scripts pass to the `emit-sfx` opcode, plus
//!   weapon-OR offsets ([`SFX_FIRE_WEAPON`] etc.) used by upstream's
//!   FIRE_WEAPON / DETONATE_WEAPON / CEG / GLOBAL banks.
//! - **Lua arg slots** ([`LUA0`] .. [`LUA9`]): the per-thread scratch
//!   the engine reserves for Cob↔Lua argument exchange. The VM stores
//!   these in the calling thread, not the unit-value bag, so they don't
//!   leak across concurrent threads.
//!
//! [`PACKXZ`] / [`UNPACKX`] / [`UNPACKZ`] mirror upstream's pack
//! convention so the host can encode/decode `(x, z)` pairs the same
//! way scripts expect.

// ---------------------------------------------------------------------------
// Get/Set keys (1..)
// ---------------------------------------------------------------------------

pub const ACTIVATION: i32 = 1;
pub const STANDINGMOVEORDERS: i32 = 2;
pub const STANDINGFIREORDERS: i32 = 3;
pub const HEALTH: i32 = 4;
pub const INBUILDSTANCE: i32 = 5;
pub const BUSY: i32 = 6;
pub const PIECE_XZ: i32 = 7;
pub const PIECE_Y: i32 = 8;
pub const UNIT_XZ: i32 = 9;
pub const UNIT_Y: i32 = 10;
pub const UNIT_HEIGHT: i32 = 11;
pub const XZ_ATAN: i32 = 12;
pub const XZ_HYPOT: i32 = 13;
pub const ATAN: i32 = 14;
pub const HYPOT: i32 = 15;
pub const GROUND_HEIGHT: i32 = 16;
/// `get BUILD_PERCENT_LEFT` — 100 = just spawned, 0 = fully built. Read
/// by unit `Create()` scripts to drive their emerge animation
/// (rise-out-of-ground for System units, alpha-fade for Hacker/Network).
pub const BUILD_PERCENT_LEFT: i32 = 17;
pub const YARD_OPEN: i32 = 18;
pub const BUGGER_OFF: i32 = 19;
pub const ARMORED: i32 = 20;
pub const IN_WATER: i32 = 28;
pub const CURRENT_SPEED: i32 = 29;
pub const VETERAN_LEVEL: i32 = 32;
pub const ON_ROAD: i32 = 34;

pub const MAX_ID: i32 = 70;
pub const MY_ID: i32 = 71;
pub const UNIT_TEAM: i32 = 72;
pub const UNIT_BUILD_PERCENT_LEFT: i32 = 73;
pub const UNIT_ALLIED: i32 = 74;
pub const MAX_SPEED: i32 = 75;
pub const CLOAKED: i32 = 76;
pub const WANT_CLOAK: i32 = 77;
pub const GROUND_WATER_HEIGHT: i32 = 78;
pub const UPRIGHT: i32 = 79;
pub const POW: i32 = 80;
pub const PRINT: i32 = 81;
pub const HEADING: i32 = 82;
pub const TARGET_ID: i32 = 83;
pub const LAST_ATTACKER_ID: i32 = 84;
pub const LOS_RADIUS: i32 = 85;
pub const AIR_LOS_RADIUS: i32 = 86;
pub const RADAR_RADIUS: i32 = 87;
pub const JAMMER_RADIUS: i32 = 88;
pub const SONAR_RADIUS: i32 = 89;
pub const SONAR_JAM_RADIUS: i32 = 90;
pub const SEISMIC_RADIUS: i32 = 91;
pub const DO_SEISMIC_PING: i32 = 92;
pub const CURRENT_FUEL: i32 = 93;
pub const TRANSPORT_ID: i32 = 94;
pub const SHIELD_POWER: i32 = 95;
pub const STEALTH: i32 = 96;
pub const CRASHING: i32 = 97;
pub const CHANGE_TARGET: i32 = 98;
pub const CEG_DAMAGE: i32 = 99;
pub const COB_ID: i32 = 100;
pub const PLAY_SOUND: i32 = 101;
pub const KILL_UNIT: i32 = 102;
pub const SET_WEAPON_UNIT_TARGET: i32 = 106;
pub const SET_WEAPON_GROUND_TARGET: i32 = 107;
pub const SONAR_STEALTH: i32 = 108;
pub const REVERSING: i32 = 109;

// ---------------------------------------------------------------------------
// Lua arg slots (110..119)
// ---------------------------------------------------------------------------

/// Lowest Lua argument slot. Reading/writing keys in `[LUA0..=LUA9]`
/// hits the *current thread's* `lua_args[i]` array (see
/// [`vm::CobVm::set_unit_value`](crate::CobVm::set_unit_value) and the
/// `Get`/`Set`/`GetUnitValue` opcode handlers). Upstream's COB→Lua
/// dispatch uses the same slots to marshal arguments and the return
/// value across the boundary; reserving them in the VM keeps any
/// accidentally-Lua-ish bytecode from clobbering host-published unit
/// state.
pub const LUA0: i32 = 110;
pub const LUA1: i32 = 111;
pub const LUA2: i32 = 112;
pub const LUA3: i32 = 113;
pub const LUA4: i32 = 114;
pub const LUA5: i32 = 115;
pub const LUA6: i32 = 116;
pub const LUA7: i32 = 117;
pub const LUA8: i32 = 118;
pub const LUA9: i32 = 119;

/// Number of Lua arg slots reserved per thread.
pub const NUM_LUA_ARGS: usize = 10;

/// Returns `Some(i)` if `key` falls inside the `[LUA0..=LUA9]` range,
/// where `i ∈ 0..NUM_LUA_ARGS` is the index into the per-thread
/// `lua_args` array.
pub const fn lua_arg_index(key: i32) -> Option<usize> {
    if key >= LUA0 && key <= LUA9 {
        Some((key - LUA0) as usize)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Flanking / weapon-tweak / math (120..)
// ---------------------------------------------------------------------------

pub const FLANK_B_MODE: i32 = 120;
pub const FLANK_B_DIR: i32 = 121;
pub const FLANK_B_MOBILITY_ADD: i32 = 122;
pub const FLANK_B_MAX_DAMAGE: i32 = 123;
pub const FLANK_B_MIN_DAMAGE: i32 = 124;
pub const WEAPON_RELOADSTATE: i32 = 125;
pub const WEAPON_RELOADTIME: i32 = 126;
pub const WEAPON_ACCURACY: i32 = 127;
pub const WEAPON_SPRAY: i32 = 128;
pub const WEAPON_RANGE: i32 = 129;
pub const WEAPON_PROJECTILE_SPEED: i32 = 130;
pub const COB_MIN: i32 = 131;
pub const COB_MAX: i32 = 132;
pub const ABS: i32 = 133;
pub const GAME_FRAME: i32 = 134;
pub const KSIN: i32 = 135;
pub const KCOS: i32 = 136;
pub const KTAN: i32 = 137;
pub const SQRT: i32 = 138;
pub const PIECE_HEADING: i32 = 139;
pub const PIECE_PITCH: i32 = 140;

// ---------------------------------------------------------------------------
// emit-sfx selectors
// ---------------------------------------------------------------------------

pub const SFX_VTOL: i32 = 0;
pub const SFX_WAKE: i32 = 2;
pub const SFX_WAKE_2: i32 = 3;
pub const SFX_REVERSE_WAKE: i32 = 4;
pub const SFX_REVERSE_WAKE_2: i32 = 5;
pub const SFX_WHITE_SMOKE: i32 = 257;
pub const SFX_BLACK_SMOKE: i32 = 258;
pub const SFX_BUBBLE: i32 = 259;
pub const SFX_CEG: i32 = 1024;
pub const SFX_FIRE_WEAPON: i32 = 2048;
pub const SFX_DETONATE_WEAPON: i32 = 4096;
pub const SFX_GLOBAL: i32 = 16384;

// ---------------------------------------------------------------------------
// Pack / unpack helpers
// ---------------------------------------------------------------------------

/// Pack `(x, z)` into a single `i32` using upstream's
/// `PACKXZ(x,z) = (x<<16) | (z & 0xffff)` convention. The two
/// components are interpreted as signed 16-bit integers — values
/// outside `[-32768, 32767]` are silently truncated, matching the C
/// macro's `(int) <<` behaviour.
#[inline]
pub const fn packxz(x: i32, z: i32) -> i32 {
    (x << 16) | (z & 0xffff)
}

/// Inverse of [`packxz`] for the X coordinate.
#[inline]
pub const fn unpackx(packed: i32) -> i32 {
    ((packed as u32) >> 16) as i16 as i32
}

/// Inverse of [`packxz`] for the Z coordinate.
#[inline]
pub const fn unpackz(packed: i32) -> i32 {
    ((packed as u32) & 0xffff) as i16 as i32
}

// ---------------------------------------------------------------------------
// Built-in GET handler
// ---------------------------------------------------------------------------

use crate::COBSCALE;

/// Side-effect-free `get` evaluations the VM can answer without touching
/// host-published state. The host should call this *before* falling back
/// to its unit-value table; whatever this returns matches upstream's
/// hard-coded behaviour bit-for-bit (subject to 32-bit truncation).
///
/// Returns `None` if `key` doesn't match a built-in — the host should
/// then look it up in its published unit_values bag.
pub fn builtin_get_value(key: i32, p1: i32, p2: i32, _p3: i32, _p4: i32) -> Option<i32> {
    use std::f32::consts::PI;
    let cobscale_f = COBSCALE as f32;
    let rad2taang = (COBSCALE as f32 / 2.0) / PI;
    let taang2rad = PI / (COBSCALE as f32 / 2.0);

    Some(match key {
        XZ_ATAN => {
            let x = unpackx(p1) as f32;
            let z = unpackz(p1) as f32;
            // Upstream subtracts unit->heading; without a host heading
            // we report the raw angle. Hosts that care should answer
            // XZ_ATAN themselves via `set_unit_value` (which takes
            // priority) or via a future heading-aware overload.
            (rad2taang * x.atan2(z)) as i32 + 32768
        }
        XZ_HYPOT => {
            let x = unpackx(p1) as f32;
            let z = unpackz(p1) as f32;
            (x.hypot(z) * cobscale_f) as i32
        }
        ATAN => {
            let x = p1 as f32;
            let y = p2 as f32;
            (rad2taang * x.atan2(y)) as i32
        }
        HYPOT => {
            let x = p1 as f32;
            let y = p2 as f32;
            x.hypot(y) as i32
        }
        POW => {
            // Upstream divides operands by COBSCALE then re-multiplies.
            let base = p1 as f32 / cobscale_f;
            let exp = p2 as f32 / cobscale_f;
            let res = base.powf(exp);
            if res.is_nan() {
                0
            } else {
                (res * cobscale_f) as i32
            }
        }
        COB_MIN => p1.min(p2),
        COB_MAX => p1.max(p2),
        ABS => p1.abs(),
        KSIN => (1024.0 * (taang2rad * p1 as f32).sin()) as i32,
        KCOS => (1024.0 * (taang2rad * p1 as f32).cos()) as i32,
        KTAN => {
            let res = 1024.0 * (taang2rad * p1 as f32).tan();
            if res.is_nan() { 0 } else { res as i32 }
        }
        SQRT => {
            let res = (p1 as f32).sqrt();
            if res.is_nan() { 0 } else { res as i32 }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_round_trip_positive() {
        let packed = packxz(123, 456);
        assert_eq!(unpackx(packed), 123);
        assert_eq!(unpackz(packed), 456);
    }

    #[test]
    fn pack_unpack_round_trip_negative() {
        // Negative components must survive sign extension on unpack.
        let packed = packxz(-300, -7);
        assert_eq!(unpackx(packed), -300);
        assert_eq!(unpackz(packed), -7);
    }

    #[test]
    fn lua_arg_index_in_range() {
        assert_eq!(lua_arg_index(LUA0), Some(0));
        assert_eq!(lua_arg_index(LUA9), Some(9));
        assert_eq!(lua_arg_index(LUA0 - 1), None);
        assert_eq!(lua_arg_index(LUA9 + 1), None);
    }

    #[test]
    fn builtin_min_max_abs() {
        assert_eq!(builtin_get_value(COB_MIN, 3, 8, 0, 0), Some(3));
        assert_eq!(builtin_get_value(COB_MAX, 3, 8, 0, 0), Some(8));
        assert_eq!(builtin_get_value(ABS, -42, 0, 0, 0), Some(42));
    }

    #[test]
    fn builtin_sqrt_rounds_down() {
        assert_eq!(builtin_get_value(SQRT, 9, 0, 0, 0), Some(3));
        assert_eq!(builtin_get_value(SQRT, 100, 0, 0, 0), Some(10));
    }

    #[test]
    fn builtin_ksin_quarter_circle() {
        // KSIN at 90 deg = 16384 TA-units → sin(π/2)*1024 = 1024.
        let result = builtin_get_value(KSIN, 16384, 0, 0, 0).unwrap();
        // Float rounding may land on 1023.
        assert!((1023..=1024).contains(&result), "KSIN(90°) = {result}");
    }

    #[test]
    fn builtin_returns_none_for_host_keys() {
        // BUILD_PERCENT_LEFT and friends must defer to host state.
        assert_eq!(builtin_get_value(BUILD_PERCENT_LEFT, 0, 0, 0, 0), None);
        assert_eq!(builtin_get_value(ACTIVATION, 0, 0, 0, 0), None);
    }
}
