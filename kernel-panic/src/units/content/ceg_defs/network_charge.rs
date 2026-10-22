//! `network_charge.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/network_charge.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::CegOp::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[network_charge]` — 2 effect layers, no ground flash.
/// Referenced by 1 `[SFXTypes]` entry.
pub static NETWORK_CHARGE: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "hcross",
            air: true,
            ground: true,
            water: true,
            count: 3,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Square),
                color_map: ColorMap(&[
                    [0.0, 1.0, 0.0, 1.0],
                    [0.0, 1.0, 0.0, 1.0],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                num_particles: Lit(1.0),
                particle_life: Lit(32.0),
                particle_life_spread: Lit(4.0),
                particle_speed: Lit(2.0),
                particle_speed_spread: Lit(0.0),
                particle_size: Lit(4.0),
                particle_size_spread: Lit(0.0),
                size_growth: Lit(0.0),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(0.0),
                airdrag: Lit(1.0),
                gravity: ZERO,
                pos: CegVec3 {
                    x: Ops(&[Add(-32.0), Rand(64.0)]),
                    y: Lit(0.0),
                    z: Lit(0.0),
                },
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
                directional: false,
                always_visible: false,
            }),
        },
        EffectLayer {
            name: "vcross",
            air: true,
            ground: true,
            water: true,
            count: 3,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Square),
                color_map: ColorMap(&[
                    [0.0, 1.0, 0.0, 1.0],
                    [0.0, 1.0, 0.0, 1.0],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                num_particles: Lit(1.0),
                particle_life: Lit(32.0),
                particle_life_spread: Lit(4.0),
                particle_speed: Lit(2.0),
                particle_speed_spread: Lit(0.0),
                particle_size: Lit(4.0),
                particle_size_spread: Lit(0.0),
                size_growth: Lit(0.0),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(0.0),
                airdrag: Lit(1.0),
                gravity: ZERO,
                pos: CegVec3 {
                    x: Lit(0.0),
                    y: Lit(0.0),
                    z: Ops(&[Add(-32.0), Rand(64.0)]),
                },
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
                directional: false,
                always_visible: false,
            }),
        },
    ],
    ground_flash: None,
};
