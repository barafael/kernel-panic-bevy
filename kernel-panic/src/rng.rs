//! Deterministic xorshift32 PRNG. Cheap, seedable, replayable — used by
//! combat spread jitter, geovent puffs, and anywhere else the sim needs
//! a few random-looking f32s without pulling in a full RNG crate.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_signed_stays_in_unit_range() {
        let mut s = 0xDEADBEEFu32;
        for _ in 0..10_000 {
            let v = next_signed(&mut s);
            assert!(v >= -1.0 && v < 1.0, "out-of-range draw: {v}");
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
