//! Float `max` / `min` / `clamp` that give the same bits on every target.
//!
//! `f32::max` and `f32::min` compile to the CPU's own instruction
//! (LLVM's `maxnum` / `minnum`), and IEEE 754-2008 leaves open which of
//! two equal operands they return. For `+0.0` against `-0.0` the
//! platforms disagree: x86_64's `maxss` / `minss` return the second
//! operand, aarch64's `fmaxnm` / `fminnm` order `-0 < +0`. M0.4's float
//! probe measured it (`net-experiments/m0-golden-replay.md`, the `chk:`
//! row: 31 % of its signed-zero pairs differ on both arm64 targets). A
//! lockstep peer that keeps a `-0.0` where another keeps `+0.0` hashes
//! differently, and the sign survives into a later division or
//! `atan2`.
//!
//! [`DetFloat`] decides with comparisons and selects, which IEEE pins
//! down exactly on every target, and fixes the open cases:
//!
//! - **Signed zeros**: `-0.0 < +0.0`, as IEEE 754-2019's
//!   `maximumNumber` / `minimumNumber`: `det_max(+0, -0) = +0`,
//!   `det_min(+0, -0) = -0`, in either operand order.
//! - **NaN**: a NaN operand is ignored, as with `f32::max` (the other
//!   operand is returned); NaN only when both are NaN.
//! - **Clamp** is `det_max(lo)` then `det_min(hi)`, so a NaN input
//!   clamps to `lo` (where `f32::clamp` returns NaN) and `-0.0`
//!   clamped to `[0, 1]` is `+0.0`.
//!
//! The workspace `clippy.toml` bans `f32::{max, min, clamp}` and the
//! f64 versions in favour of these. glam's `Vec*::max / min / clamp`
//! need no wrapper: with `scalar-math` they are already
//! compare-and-select (`if a > b { a } else { b }`), which is exact on
//! every target (the probe's `det: glam Vec3 max/min/clamp` row).

#![no_std]

/// Target-independent `max` / `min` / `clamp` (see the crate docs for
/// the signed-zero and NaN rules).
pub trait DetFloat: Copy {
    /// The larger operand; `+0.0` over `-0.0`; a NaN operand ignored.
    fn det_max(self, other: Self) -> Self;
    /// The smaller operand; `-0.0` under `+0.0`; a NaN operand ignored.
    fn det_min(self, other: Self) -> Self;
    /// `self.det_max(lo).det_min(hi)`: NaN clamps to `lo`. `lo <= hi`
    /// is checked in debug builds only (`f32::clamp` always panics).
    fn det_clamp(self, lo: Self, hi: Self) -> Self;
}

macro_rules! det_float {
    ($t:ty) => {
        impl DetFloat for $t {
            #[inline]
            fn det_max(self, other: Self) -> Self {
                // `self` wins when larger, on a tie against `-0.0`
                // (so `+0` beats `-0` either way round) and against NaN.
                if self > other || (self == other && other.is_sign_negative()) || other.is_nan() {
                    self
                } else {
                    other
                }
            }

            #[inline]
            fn det_min(self, other: Self) -> Self {
                if self < other || (self == other && other.is_sign_positive()) || other.is_nan() {
                    self
                } else {
                    other
                }
            }

            #[inline]
            fn det_clamp(self, lo: Self, hi: Self) -> Self {
                debug_assert!(lo <= hi, "det_clamp: lo > hi or NaN bound");
                self.det_max(lo).det_min(hi)
            }
        }
    };
}

det_float!(f32);
det_float!(f64);

#[cfg(test)]
mod tests {
    use super::DetFloat;

    const P: f32 = 0.0;
    const N: f32 = -0.0;

    fn bits(v: f32) -> u32 {
        v.to_bits()
    }

    #[test]
    fn signed_zeros_are_ordered() {
        for (a, b) in [(P, N), (N, P), (P, P), (N, N)] {
            let max = if a.is_sign_positive() || b.is_sign_positive() {
                P
            } else {
                N
            };
            let min = if a.is_sign_negative() || b.is_sign_negative() {
                N
            } else {
                P
            };
            assert_eq!(bits(a.det_max(b)), bits(max), "max({a:?}, {b:?})");
            assert_eq!(bits(a.det_min(b)), bits(min), "min({a:?}, {b:?})");
        }
        assert_eq!(bits(N.det_clamp(0.0, 1.0)), bits(P));
        assert_eq!(bits(P.det_clamp(-1.0, -0.0)), bits(N));
    }

    #[test]
    fn nan_operand_is_ignored() {
        let nan = f32::NAN;
        assert_eq!(nan.det_max(2.0), 2.0);
        assert_eq!(2.0f32.det_max(nan), 2.0);
        assert_eq!(nan.det_min(-2.0), -2.0);
        assert_eq!((-2.0f32).det_min(nan), -2.0);
        assert!(nan.det_max(nan).is_nan());
        assert!(nan.det_min(nan).is_nan());
        assert_eq!(nan.det_clamp(1.0, 3.0), 1.0);
    }

    // The expected values are spelled out as compare-and-select on purpose.
    #[allow(clippy::manual_clamp)]
    #[test]
    fn ordinary_values_match_std() {
        let xs = [-3.5f32, -1.0, -1e-30, 1e-30, 0.25, 7.0, f32::INFINITY];
        for &a in &xs {
            for &b in &xs {
                assert_eq!(a.det_max(b), if a > b { a } else { b });
                assert_eq!(a.det_min(b), if a < b { a } else { b });
            }
            let c = a.det_clamp(-1.0, 1.0);
            assert_eq!(
                c,
                if a < -1.0 {
                    -1.0
                } else if a > 1.0 {
                    1.0
                } else {
                    a
                }
            );
        }
        assert_eq!(2.0f64.det_max(-1.0), 2.0);
        assert_eq!((-0.0f64).det_max(0.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(5.0f64.det_clamp(0.0, 1.0), 1.0);
    }
}
