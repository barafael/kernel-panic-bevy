//! Deterministic xorshift32 PRNG. Cheap, seedable, replayable — used by
//! combat spread jitter, geovent puffs, CEG particles, aircraft wobble
//! and anywhere else the sim needs a few random-looking f32s without
//! pulling in a full RNG crate. States are plain `u32`s owned by the
//! caller (a component field or a `Local`).
//!
//! [`clock_f64`] is the one *non*-deterministic source: a stream seeded
//! from launch entropy (wall clock, OS randomness) for choices that should differ per launch (match setup, the
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
/// from launch entropy on first use — differs every launch.
pub fn clock_f64() -> f64 {
    thread_local! {
        static STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            x = splitmix64(launch_entropy()) | 1;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}

/// Bits that differ between launches. The seed used to be the time
/// elapsed between two adjacent reads of a just-created `Instant` — a
/// few dozen nanoseconds, nearly the same every launch, so launches
/// rolled (almost) the same demo seats, maps and Hex Farm layouts.
///
/// Native: the wall clock plus the OS-random keys std seeds every
/// `RandomState` with. Web: `Math.random()` and the wall clock
/// (`std::time` panics on wasm32).
fn launch_entropy() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::hash::{BuildHasher, Hasher};
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(wall);
        hasher.finish() ^ wall
    }
    #[cfg(target_arch = "wasm32")]
    {
        let random = (js_sys::Math::random() * (1u64 << 53) as f64) as u64;
        random ^ (js_sys::Date::now() as u64).rotate_left(29)
    }
}

/// SplitMix64's step: what its state advances by per draw.
pub(crate) const SPLITMIX64_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64's finaliser: spreads a seed's entropy over all 64 bits
/// (xorshift needs many rounds to diffuse a seed that differs in only
/// a few low bits). A SplitMix64 stream is this of its state, which
/// then advances by [`SPLITMIX64_GAMMA`].
pub(crate) fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(SPLITMIX64_GAMMA);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
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

    /// Two seedings a moment apart give unrelated streams (the old seed
    /// was nearly constant per launch).
    #[test]
    fn launch_seeds_differ() {
        let a = splitmix64(launch_entropy());
        let b = splitmix64(launch_entropy());
        assert_ne!(a, b);
        assert!((a ^ b).count_ones() > 8, "{a:x} vs {b:x}");
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
