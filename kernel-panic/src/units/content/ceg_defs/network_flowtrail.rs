//! `network_flowtrail.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/network_flowtrail.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::CegOp::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[network_flowtrail]` — 2 effect layers, no ground flash.
/// Referenced by 1 weapon.
pub static NETWORK_FLOWTRAIL: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "Circle",
            air: true,
            ground: true,
            water: true,
            count: 0,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Circle),
                color_map: ColorMap(&[[1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 0.0]]),
                num_particles: Lit(1.0),
                particle_life: Lit(48.0),
                particle_life_spread: Lit(0.0),
                particle_speed: Lit(1.0),
                particle_speed_spread: Lit(0.0),
                particle_size: Lit(2.0),
                particle_size_spread: Lit(0.0),
                size_growth: Lit(0.05),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(0.0),
                airdrag: Lit(0.9),
                gravity: ZERO,
                pos: ZERO,
                emit_vector: EmitVector::Direction,
                directional: false,
                always_visible: false,
            }),
        },
        EffectLayer {
            name: "spikes",
            air: true,
            ground: true,
            water: true,
            count: 6,
            properties: Spike(SpikeProperties {
                length: Ops(&[Add(10.0), Rand(7.0)]),
                width: Lit(4.0),
                alpha: Lit(0.9),
                alpha_decay: Lit(0.3),
                dir: CegVec3 {
                    x: Ops(&[Add(-1.0), Rand(2.0)]),
                    y: Ops(&[Add(-1.0), Rand(2.0)]),
                    z: Ops(&[Add(-1.0), Rand(2.0)]),
                },
                pos: ZERO,
                color: Some(vec3(1.0, 1.0, 1.0)),
            }),
        },
    ],
    ground_flash: None,
};
