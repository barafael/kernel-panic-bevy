//! Deterministic xorshift32 PRNG. Cheap, seedable, replayable — used by
//! combat spread jitter, geovent puffs, CEG particles, aircraft wobble
//! and anywhere else the sim needs a few random-looking f32s without
//! pulling in a full RNG crate. States are plain `u32`s owned by the
//! caller (a component field or a `Local`).
//!
//! [`clock_f64`] is the one *non*-deterministic source: a clock-seeded
//! stream for choices that should differ per launch (match setup, the
//! menu's attract demo). Gameplay never draws from it.

use bevy::prelude::Vec3;

/// Advance the state and return a uniform `u32`. State must not be zero;
/// callers seed with `... | 1` to guarantee this.
pub fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Uniform `f32` in `[0.0, 1.0)`.
pub fn next_f32(state: &mut u32) -> f32 {
    // Upper 24 bits fill the mantissa range of `[0, 1)` without bias.
    (xorshift32(state) >> 8) as f32 / (1u32 << 24) as f32
}

/// Uniform `f32` in `[-1.0, 1.0)`.
pub fn next_signed(state: &mut u32) -> f32 {
    next_f32(state) * 2.0 - 1.0
}

/// Uniform point inside the unit sphere via cube rejection; falls back
/// to `Vec3::Y` after 8 rejections so a pathological state can't spin.
pub fn random_unit_sphere(state: &mut u32) -> Vec3 {
    for _ in 0..8 {
        let v = Vec3::new(next_signed(state), next_signed(state), next_signed(state));
        let len_sq = v.length_squared();
        if len_sq > 0.0 && len_sq <= 1.0 {
            return v;
        }
    }
    Vec3::Y
}

/// Uniform `f64` in `[0.0, 1.0)` from a thread-local xorshift64 seeded
/// from the clock on first use — differs every launch.
///
/// Uses Bevy's `Instant`, not `std::time`: `SystemTime`/`Instant` panic
/// with "time not supported on this platform" on wasm32, where Bevy's
/// is `performance.now()`-backed instead.
pub fn clock_f64() -> f64 {
    use bevy::platform::time::Instant;
    thread_local! {
        static STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        static ANCHOR: Instant = Instant::now();
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            // No epoch on Instant — seed from nanos elapsed since this
            // thread's first call, mixed with a fixed odd constant.
            let elapsed = ANCHOR.with(|a| a.elapsed());
            x = ((elapsed.subsec_nanos() as u64) ^ (elapsed.as_secs() << 20) ^ 0x9E3779B97F4A7C15)
                | 1;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_signed_stays_in_unit_range() {
        let mut s = 0xDEADBEEFu32;
        for _ in 0..10_000 {
            let v = next_signed(&mut s);
            assert!((-1.0..1.0).contains(&v), "out-of-range draw: {v}");
        }
    }

    #[test]
    fn clock_f64_stays_in_unit_range() {
        for _ in 0..1000 {
            let v = clock_f64();
            assert!((0.0..1.0).contains(&v), "out-of-range draw: {v}");
        }
    }

    #[test]
    fn xorshift32_is_deterministic() {
        let mut a = 0xABCDEF01u32;
        let mut b = 0xABCDEF01u32;
        for _ in 0..100 {
            assert_eq!(xorshift32(&mut a), xorshift32(&mut b));
        }
    }
}
