//! Bit-exactness probe of the f32 math the sim uses, per target.
use glam::{Affine3A, Quat, Vec3, Vec3A};

pub const N: u32 = 200_000;
pub const FUNCS: u32 = 16;

fn xs(mut x: u32) -> u32 {
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    x
}
fn unit(i: u32, salt: u32) -> f32 {
    let mut s = (i.wrapping_mul(2654435761) ^ salt.wrapping_mul(40503)) | 1;
    for _ in 0..3 {
        s = xs(s);
    }
    (s >> 8) as f32 / (1u32 << 24) as f32
}
fn r(i: u32, salt: u32, lo: f32, hi: f32) -> f32 {
    lo + (hi - lo) * unit(i, salt)
}

pub fn eval(f: u32, i: u32) -> u32 {
    let a = r(i, 1, -12.566, 12.566);
    let b = r(i, 2, -2000.0, 2000.0);
    let c = r(i, 3, -2000.0, 2000.0);
    let u = r(i, 4, -1.0, 1.0);
    let v = match f {
        0 => a.sin(),
        1 => a.cos(),
        2 => (a * 0.12).tan(),
        3 => b.atan2(c),
        4 => u.asin(),
        5 => u.acos(),
        6 => r(i, 5, 0.0, 10.0).powf(r(i, 6, -3.0, 3.0)),
        7 => b.hypot(c),
        8 => r(i, 7, 0.0, 1.0e6).cbrt(),
        9 => (b * b + c * c).sqrt(),
        10 => r(i, 8, 0.0, 10.0).powi(3),
        11 => {
            let q = Quat::from_rotation_y(a) * Quat::from_rotation_x(u);
            (q * Vec3::new(b, 1.0, c)).x
        }
        12 => Vec3A::new(b, u, c).normalize().x,
        13 => Vec3A::new(b, u, c).dot(Vec3A::new(c, a, b)),
        14 => {
            let t = Affine3A::from_rotation_translation(
                Quat::from_rotation_y(a),
                Vec3::new(b, 0.0, c),
            );
            t.transform_point3(Vec3::new(c, u, b)).z
        }
        15 => {
            let q1 = Quat::from_rotation_y(a);
            let q2 = Quat::from_rotation_y(a + u);
            q1.slerp(q2, 0.37).y
        }
        _ => 0.0,
    };
    v.to_bits()
}

#[unsafe(no_mangle)]
pub extern "C" fn probe(f: u32, i: u32) -> u32 {
    eval(f, i)
}
