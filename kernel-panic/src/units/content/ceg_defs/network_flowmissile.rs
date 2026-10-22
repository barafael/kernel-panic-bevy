//! `network_flowmissile.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/network_flowmissile.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::CegOp::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[network_flowmissile]` — 2 effect layers, no ground flash.
/// Referenced by 1 weapon.
pub static NETWORK_FLOWMISSILE: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "Circle",
            air: true,
            ground: true,
            water: true,
            count: 1,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Circle),
                color_map: ColorMap(&[[1.0, 1.0, 1.0, 0.1], [0.0, 0.0, 0.0, 0.0]]),
                num_particles: Lit(1.0),
                particle_life: Lit(16.0),
                particle_life_spread: Lit(0.0),
                particle_speed: Lit(0.0),
                particle_speed_spread: Lit(0.0),
                particle_size: Lit(2.0),
                particle_size_spread: Lit(0.0),
                size_growth: Lit(1.0),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(0.0),
                airdrag: Lit(0.9),
                gravity: ZERO,
                pos: vec3(0.0, 5.0, 0.0),
                emit_vector: EmitVector::Direction,
                directional: false,
                always_visible: false,
            }),
        },
        EffectLayer {
            name: "shockwave",
            air: true,
            ground: true,
            water: true,
            count: 1,
            properties: Flame(FlameProperties {
                side_texture: None,
                front_texture: Some(CegTexture::Hcircle),
                color_map: ColorMap(&[[1.0, 1.0, 1.0, 0.1], [0.0, 0.0, 0.0, 0.0]]),
                size: Lit(1.0),
                length: Lit(1.0),
                size_growth: Lit(30.0),
                ttl: Lit(16.0),
                front_offset: Lit(0.0),
                speed: Lit(0.0),
                dir: CegVec3 {
                    x: Ops(&[Add(-0.3), Rand(0.6)]),
                    y: Lit(1.0),
                    z: Ops(&[Add(-0.3), Rand(0.6)]),
                },
                pos: vec3(0.0, 5.0, 0.0),
                always_visible: false,
            }),
        },
    ],
    ground_flash: None,
};
