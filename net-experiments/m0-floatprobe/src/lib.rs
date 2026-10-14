//! Bit-exactness probe of the float math the sim uses, per target (M0.3).
//!
//! Every probe function maps a pseudo-random input index to the raw bits
//! of one result. Running all of them on two targets and comparing the
//! bit streams shows which operations round differently where.
//!
//! Two groups:
//! - `det:` the routes the game uses since M0.3 — `bevy::math::ops`
//!   (libm), `libm` for f64, glam with `libm` + `scalar-math`, plain
//!   IEEE arithmetic, and since M1 the `spring_detmath::DetFloat`
//!   max / min / clamp that replace `f32::max` & co. These must agree
//!   on every target: 0 differing bits.
//! - `chk:` known target differences (`f32::max` / `min` on signed
//!   zeros differ on aarch64) that the sim must not use.
//! - `std:` the std methods the workspace `clippy.toml` now bans, kept
//!   as the control group (they call the platform's C math library on
//!   native, Rust's bundled libm on wasm32, so they do differ).
//!
//! Inputs cover the ranges the sim feeds these functions: headings and
//! angles in ±4π, map coordinates up to ±2000 elmos (Hex Farm: ±8000 in
//! f64), unit directions in [-1, 1], drag/decay bases in (0, 1].

// The control group calls the banned std methods on purpose.
#![allow(clippy::disallowed_methods)]

use bevy_math::ops;
use spring_detmath::DetFloat;
use glam::{Affine3A, EulerRot, Mat3, Mat4, Quat, Vec2, Vec3, Vec3A};

/// Inputs per function.
pub const N: u32 = 200_000;

fn xs(mut x: u32) -> u32 {
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    x
}

/// Uniform [0, 1) from (index, salt) with 24 random bits.
fn unit(i: u32, salt: u32) -> f32 {
    let mut s = (i.wrapping_mul(2_654_435_761) ^ salt.wrapping_mul(40_503)) | 1;
    for _ in 0..3 {
        s = xs(s);
    }
    (s >> 8) as f32 / (1u32 << 24) as f32
}

fn r(i: u32, salt: u32, lo: f32, hi: f32) -> f32 {
    lo + (hi - lo) * unit(i, salt)
}

fn r64(i: u32, salt: u32, lo: f64, hi: f64) -> f64 {
    // 48 random bits: two 24-bit draws.
    let a = unit(i, salt) as f64;
    let b = unit(i, salt.wrapping_add(7919)) as f64;
    lo + (hi - lo) * (a + b / (1u64 << 24) as f64)
}

/// The inputs one probe sees: an angle, two coordinates, a direction
/// component, a positive base and exponent.
struct In {
    a: f32,
    b: f32,
    c: f32,
    u: f32,
    base: f32,
    e: f32,
}

fn inputs(i: u32) -> In {
    In {
        a: r(i, 1, -12.566, 12.566),
        b: r(i, 2, -2000.0, 2000.0),
        c: r(i, 3, -2000.0, 2000.0),
        u: r(i, 4, -1.0, 1.0),
        base: r(i, 5, 0.0, 1.0),
        e: r(i, 6, 0.0, 60.0),
    }
}

fn b32(v: f32) -> u64 {
    v.to_bits() as u64
}

fn b64(v: f64) -> u64 {
    v.to_bits()
}

/// Fold several f32 results into one u64 without losing a differing bit
/// in practice (each lane rotated differently before xor).
fn fold(vs: &[f32]) -> u64 {
    vs.iter().enumerate().fold(0u64, |acc, (k, v)| {
        acc ^ (v.to_bits() as u64).rotate_left(k as u32 * 13)
    })
}

type Probe = fn(u32) -> u64;

/// `(name, probe)`. Append only: the bit-stream layout is by index.
pub const PROBES: &[(&str, Probe)] = &[
    // ---- det: the game's routes -------------------------------------
    ("det: ops::sin", |i| b32(ops::sin(inputs(i).a))),
    ("det: ops::cos", |i| b32(ops::cos(inputs(i).a))),
    ("det: ops::tan", |i| b32(ops::tan(inputs(i).a * 0.12))),
    ("det: ops::sin_cos", |i| {
        let (s, c) = ops::sin_cos(inputs(i).a);
        fold(&[s, c])
    }),
    ("det: ops::atan2", |i| {
        let x = inputs(i);
        b32(ops::atan2(x.b, x.c))
    }),
    ("det: ops::atan", |i| b32(ops::atan(inputs(i).b * 0.01))),
    ("det: ops::asin", |i| b32(ops::asin(inputs(i).u))),
    ("det: ops::acos", |i| b32(ops::acos(inputs(i).u))),
    ("det: ops::powf (drag^t)", |i| {
        let x = inputs(i);
        b32(ops::powf(x.base, x.e))
    }),
    ("det: ops::hypot", |i| {
        let x = inputs(i);
        b32(ops::hypot(x.b, x.c))
    }),
    ("det: ops::cbrt", |i| b32(ops::cbrt(r(i, 7, 0.0, 1.0e6)))),
    ("det: ops::exp", |i| b32(ops::exp(inputs(i).u * 20.0))),
    ("det: ops::ln", |i| b32(ops::ln(r(i, 8, 1e-6, 1e6)))),
    ("det: sqrt (IEEE)", |i| {
        let x = inputs(i);
        b32((x.b * x.b + x.c * x.c).sqrt())
    }),
    ("det: x*x (was powi(2))", |i| {
        let x = inputs(i);
        b32(x.b * x.b)
    }),
    ("det: rem_euclid(TAU)", |i| {
        b32(inputs(i).b.rem_euclid(std::f32::consts::TAU))
    }),
    ("det: floor/round/trunc", |i| {
        let b = inputs(i).b * 0.37;
        fold(&[b.floor(), b.round(), b.trunc(), b.ceil()])
    }),
    ("det: libm::sin f64", |i| {
        b64(libm::sin(r64(i, 9, -12.566, 12.566)))
    }),
    ("det: libm::cos f64", |i| {
        b64(libm::cos(r64(i, 9, -12.566, 12.566)))
    }),
    ("det: libm::atan2 f64", |i| {
        b64(libm::atan2(
            r64(i, 10, -8000.0, 8000.0),
            r64(i, 11, -8000.0, 8000.0),
        ))
    }),
    ("det: libm::pow f64", |i| {
        b64(libm::pow(r64(i, 12, 0.0, 1.0), 0.4))
    }),
    ("det: libm::tan f64", |i| {
        b64(libm::tan(r64(i, 13, -1.4, 1.4)))
    }),
    ("det: glam Quat*Vec3", |i| {
        let x = inputs(i);
        let q = Quat::from_rotation_y(x.a) * Quat::from_rotation_x(x.u);
        let v = q * Vec3::new(x.b, 1.0, x.c);
        fold(&[v.x, v.y, v.z])
    }),
    ("det: glam Vec3A::normalize", |i| {
        let x = inputs(i);
        let v = Vec3A::new(x.b, x.u, x.c).normalize();
        fold(&[v.x, v.y, v.z])
    }),
    ("det: glam Vec3A::dot", |i| {
        let x = inputs(i);
        b32(Vec3A::new(x.b, x.u, x.c).dot(Vec3A::new(x.c, x.a, x.b)))
    }),
    ("det: glam Affine3A::transform_point3", |i| {
        let x = inputs(i);
        let t = Affine3A::from_rotation_translation(
            Quat::from_rotation_y(x.a),
            Vec3::new(x.b, 0.0, x.c),
        );
        let p = t.transform_point3(Vec3::new(x.c, x.u, x.b));
        fold(&[p.x, p.y, p.z])
    }),
    ("det: glam Mat4 * Mat4", |i| {
        let x = inputs(i);
        let m = Mat4::from_rotation_translation(
            Quat::from_euler(EulerRot::YXZ, x.a, x.u, x.a * 0.5),
            Vec3::new(x.b, x.u, x.c),
        ) * Mat4::from_scale(Vec3::splat(1.0 + x.u.abs()));
        fold(&m.to_cols_array())
    }),
    ("det: glam Quat::slerp", |i| {
        let x = inputs(i);
        let q = Quat::from_rotation_y(x.a).slerp(Quat::from_rotation_y(x.a + x.u), 0.37);
        fold(&[q.x, q.y, q.z, q.w])
    }),
    ("det: glam Quat::from_euler", |i| {
        let x = inputs(i);
        let q = Quat::from_euler(EulerRot::YXZ, x.a, x.u, x.a * 0.3);
        fold(&[q.x, q.y, q.z, q.w])
    }),
    ("det: glam Vec2::from_angle/to_angle", |i| {
        let x = inputs(i);
        let v = Vec2::from_angle(x.a);
        fold(&[v.x, v.y, Vec2::new(x.b, x.c).to_angle()])
    }),
    ("det: glam angle_between", |i| {
        let x = inputs(i);
        b32(Vec3::new(x.b, x.u, x.c).angle_between(Vec3::new(x.c, x.a, x.b)))
    }),
    ("det: glam Mat3::from_axis_angle", |i| {
        let x = inputs(i);
        let axis = Vec3::new(x.u, 1.0, x.b * 0.001).normalize();
        fold(&Mat3::from_axis_angle(axis, x.a).to_cols_array())
    }),
    // ---- std: the banned methods, control group ---------------------
    ("std: f32::sin", |i| b32(inputs(i).a.sin())),
    ("std: f32::cos", |i| b32(inputs(i).a.cos())),
    ("std: f32::tan", |i| b32((inputs(i).a * 0.12).tan())),
    ("std: f32::atan2", |i| {
        let x = inputs(i);
        b32(x.b.atan2(x.c))
    }),
    ("std: f32::asin", |i| b32(inputs(i).u.asin())),
    ("std: f32::acos", |i| b32(inputs(i).u.acos())),
    ("std: f32::powf (drag^t)", |i| {
        let x = inputs(i);
        b32(x.base.powf(x.e))
    }),
    ("std: f32::hypot", |i| {
        let x = inputs(i);
        b32(x.b.hypot(x.c))
    }),
    ("std: f32::cbrt", |i| b32(r(i, 7, 0.0, 1.0e6).cbrt())),
    ("std: f32::powi(2)", |i| b32(inputs(i).b.powi(2))),
    ("std: f64::sin", |i| b64(r64(i, 9, -12.566, 12.566).sin())),
    ("std: f64::atan2", |i| {
        b64(r64(i, 10, -8000.0, 8000.0).atan2(r64(i, 11, -8000.0, 8000.0)))
    }),
    ("std: f64::powf", |i| b64(r64(i, 12, 0.0, 1.0).powf(0.4))),
    // ---- chk: IEEE leaves the sign of max(+0, -0) / min(+0, -0) open --
    ("chk: f32::max/min with signed zeros", |i| {
        const Z: [f32; 4] = [0.0, -0.0, 1e-30, -1e-30];
        let a = Z[(i & 3) as usize];
        let b = Z[((i >> 2) & 3) as usize];
        // Also the sim's spelling `x.max(0.0)` on a negative zero.
        fold(&[
            a.max(b),
            a.min(b),
            a.max(0.0),
            a.min(0.0),
            a.clamp(0.0, 1.0),
        ])
    }),
    ("chk: atan2(max(-0, -0), -1)", |i| {
        let z = if i & 1 == 0 { -0.0f32 } else { 0.0 };
        b32(ops::atan2(z.max(-0.0), -1.0))
    }),
    // ---- det: the sim's replacements for the chk: rows (M1) ----------
    ("det: DetFloat f32 max/min/clamp", |i| {
        let a = EDGE[(i & 7) as usize];
        let b = EDGE[((i >> 3) & 7) as usize];
        let x = inputs(i).u * 1e-30;
        fold(&[
            a.det_max(b),
            a.det_min(b),
            a.det_max(0.0),
            a.det_min(0.0),
            a.det_clamp(0.0, 1.0),
            a.det_clamp(-1.0, -0.0),
            x.det_max(-x),
            x.det_clamp(-0.0, 0.0),
        ])
    }),
    ("det: DetFloat f64 max/min/clamp", |i| {
        let a = EDGE[(i & 7) as usize] as f64;
        let b = EDGE[((i >> 3) & 7) as usize] as f64;
        b64(a.det_max(b))
            ^ b64(a.det_min(b)).rotate_left(13)
            ^ b64(a.det_clamp(0.0, 1.0)).rotate_left(26)
    }),
    // glam's scalar `max`/`min`/`clamp` are compare-and-select, so the
    // sim may keep using them.
    ("det: glam Vec3/Vec3A max/min/clamp", |i| {
        let e = |k: u32| EDGE[((i >> k) & 7) as usize];
        let a = Vec3::new(e(0), e(3), e(6));
        let b = Vec3::new(e(9), e(12), e(15));
        let lo = Vec3::new(0.0, -0.0, -1.0);
        let hi = Vec3::new(0.0, 0.0, 1.0);
        let a4 = Vec3A::from(a);
        let mut out = Vec::with_capacity(20);
        out.extend(a.max(b).to_array());
        out.extend(a.min(b).to_array());
        out.extend(a.clamp(lo, hi).to_array());
        out.extend(a4.max(Vec3A::from(b)).to_array());
        out.extend(a4.min(Vec3A::from(b)).to_array());
        out.push(a.max_element());
        out.push(a.min_element());
        fold(&out)
    }),
];

/// Signed zeros, a NaN and their neighbours, paired up by the max / min
/// probes.
const EDGE: [f32; 8] = [0.0, -0.0, 1e-30, -1e-30, f32::NAN, 1.0, -1.0, f32::MIN_POSITIVE];

/// Number of probe functions (for the wasm runner).
#[unsafe(no_mangle)]
pub extern "C" fn probe_count() -> u32 {
    PROBES.len() as u32
}

/// Inputs per function (for the wasm runner).
#[unsafe(no_mangle)]
pub extern "C" fn probe_n() -> u32 {
    N
}

/// Result bits of probe `f` on input `i`.
#[unsafe(no_mangle)]
pub extern "C" fn probe(f: u32, i: u32) -> u64 {
    (PROBES[f as usize].1)(i)
}

/// All results, probe-major, little-endian u64s.
pub fn run_all() -> Vec<u8> {
    let mut out = Vec::with_capacity(PROBES.len() * N as usize * 8);
    for (_, p) in PROBES {
        for i in 0..N {
            out.extend(p(i).to_le_bytes());
        }
    }
    out
}

/// Per-probe count of inputs whose result bits differ between two runs.
pub fn compare(a: &[u8], b: &[u8]) -> Vec<(&'static str, u32)> {
    let n = N as usize;
    PROBES
        .iter()
        .enumerate()
        .map(|(k, (name, _))| {
            let at = |buf: &[u8], i: usize| {
                let o = (k * n + i) * 8;
                u64::from_le_bytes(buf[o..o + 8].try_into().unwrap())
            };
            let d = (0..n).filter(|&i| at(a, i) != at(b, i)).count() as u32;
            (*name, d)
        })
        .collect()
}
