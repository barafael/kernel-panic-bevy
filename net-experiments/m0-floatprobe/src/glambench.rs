//! Cost of the deterministic-math configuration, per operation.
//!
//! `cargo run --release --bin glambench` (scalar glam, the game's
//! configuration) vs `--no-default-features` (glam's SSE2/NEON paths);
//! the transcendental rows compare `bevy::math::ops` (libm) with the
//! std methods in the same build.

#![allow(clippy::disallowed_methods)]

use std::hint::black_box;
use std::time::Instant;

use bevy_math::ops;
use glam::{Affine3A, Mat4, Quat, Vec3, Vec3A, Vec4};

const LEN: usize = 4096;
const REPS: usize = 2000;

fn seq(salt: u32) -> Vec<f32> {
    let mut s = salt | 1;
    (0..LEN)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        })
        .collect()
}

/// Median-of-5 nanoseconds per call of `f` over the input index.
fn bench(name: &str, mut f: impl FnMut(usize) -> f32) {
    let mut runs = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let mut acc = 0.0f32;
        for _ in 0..REPS {
            for i in 0..LEN {
                acc += f(black_box(i));
            }
        }
        black_box(acc);
        runs.push(t.elapsed().as_secs_f64() * 1e9 / (REPS * LEN) as f64);
    }
    runs.sort_by(f64::total_cmp);
    println!("  {name:36} {:7.2} ns", runs[2]);
}

fn main() {
    let simd = if cfg!(feature = "scalar") {
        "scalar-math"
    } else {
        "SIMD"
    };
    println!("glam {simd} on {}", std::env::consts::ARCH);
    let a = seq(1);
    let b = seq(2);
    let c = seq(3);
    let quats: Vec<Quat> = (0..LEN)
        .map(|i| Quat::from_rotation_y(a[i] * 3.0) * Quat::from_rotation_x(b[i]))
        .collect();
    let affs: Vec<Affine3A> = (0..LEN)
        .map(|i| Affine3A::from_rotation_translation(quats[i], Vec3::new(a[i], b[i], c[i]) * 100.0))
        .collect();
    let mats: Vec<Mat4> = affs.iter().map(|&t| Mat4::from(t)).collect();
    let v3: Vec<Vec3> = (0..LEN)
        .map(|i| Vec3::new(a[i], b[i], c[i]) * 500.0)
        .collect();
    let v3a: Vec<Vec3A> = v3.iter().map(|&v| v.into()).collect();
    let v4: Vec<Vec4> = (0..LEN).map(|i| Vec4::new(a[i], b[i], c[i], 1.0)).collect();
    let j = |i: usize| (i + 1) & (LEN - 1);

    println!("glam:");
    bench("Mat4 * Mat4", |i| (mats[i] * mats[j(i)]).x_axis.x);
    bench("Affine3A * Affine3A", |i| {
        (affs[i] * affs[j(i)]).translation.x
    });
    bench("Affine3A::transform_point3", |i| {
        affs[i].transform_point3(v3[j(i)]).x
    });
    bench("Mat4::project_point3", |i| {
        mats[i].project_point3(v3[j(i)]).x
    });
    bench("Mat4 * Vec4", |i| (mats[i] * v4[j(i)]).x);
    bench("Quat * Quat", |i| (quats[i] * quats[j(i)]).x);
    bench("Quat * Vec3", |i| (quats[i] * v3[j(i)]).x);
    bench("Vec3A::normalize", |i| v3a[i].normalize().x);
    bench("Vec4::dot", |i| v4[i].dot(v4[j(i)]));
    bench("Mat4::inverse", |i| mats[i].inverse().x_axis.x);

    println!("transcendentals (ops = libm, std = platform):");
    bench("ops::sin", |i| ops::sin(a[i] * 6.0));
    bench("f32::sin", |i| (a[i] * 6.0).sin());
    bench("ops::sin_cos", |i| {
        let (s, c) = ops::sin_cos(a[i] * 6.0);
        s + c
    });
    bench("f32::sin_cos", |i| {
        let (s, c) = (a[i] * 6.0).sin_cos();
        s + c
    });
    bench("ops::atan2", |i| ops::atan2(a[i], b[i]));
    bench("f32::atan2", |i| a[i].atan2(b[i]));
    bench("ops::acos", |i| ops::acos(a[i]));
    bench("f32::acos", |i| a[i].acos());
    bench("ops::powf", |i| ops::powf(a[i].abs(), 30.0 * b[i].abs()));
    bench("f32::powf", |i| a[i].abs().powf(30.0 * b[i].abs()));
    bench("f32::sqrt", |i| a[i].abs().sqrt());
    bench("libm::sqrtf (glam's sqrt under libm)", |i| {
        libm::sqrtf(a[i].abs())
    });
    bench("libm::floorf (glam's floor under libm)", |i| {
        libm::floorf(a[i] * 100.0)
    });
    bench("f32::floor", |i| (a[i] * 100.0).floor());
}
