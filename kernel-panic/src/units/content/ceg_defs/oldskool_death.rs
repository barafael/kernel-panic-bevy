//! `oldskool_death.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/oldskool_death.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[oldskool_death]` — 3 effect layers, ground flash `ttl = 0`.
/// Referenced by 2 weapons.
pub static OLDSKOOL_DEATH: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "circle",
            air: true,
            ground: true,
            water: true,
            count: 1,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Circle),
                color_map: ColorMap(&[
                    [1.0, 1.0, 0.0, 0.3],
                    [1.0, 0.0, 0.0, 0.2],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                num_particles: Lit(1.0),
                particle_life: Lit(10.0),
                particle_life_spread: Lit(4.0),
                particle_speed: Lit(0.0),
                particle_speed_spread: Lit(0.0),
                particle_size: Lit(6.0),
                particle_size_spread: Lit(5.0),
                size_growth: Lit(4.0),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(80.0),
                airdrag: Lit(1.0),
                gravity: ZERO,
                pos: vec3(0.0, 1.0, 0.0),
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
                directional: false,
                always_visible: false,
            }),
        },
        EffectLayer {
            name: "squarecloud",
            air: true,
            ground: true,
            water: true,
            count: 1,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Square),
                color_map: ColorMap(&[[1.0, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0]]),
                num_particles: Lit(32.0),
                particle_life: Lit(48.0),
                particle_life_spread: Lit(16.0),
                particle_speed: Lit(0.0),
                particle_speed_spread: Lit(1.5),
                particle_size: Lit(0.6),
                particle_size_spread: Lit(0.3),
                size_growth: Lit(0.0),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(80.0),
                airdrag: Lit(1.0),
                gravity: ZERO,
                pos: vec3(0.0, 1.0, 0.0),
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
                directional: false,
                always_visible: false,
            }),
        },
        EffectLayer {
            name: "tracers",
            air: true,
            ground: true,
            water: true,
            count: 1,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::VLine),
                color_map: ColorMap(&[[1.0, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0]]),
                num_particles: Lit(15.0),
                particle_life: Lit(32.0),
                particle_life_spread: Lit(16.0),
                particle_speed: Lit(4.0),
                particle_speed_spread: Lit(0.6),
                particle_size: Lit(6.0),
                particle_size_spread: Lit(0.0),
                size_growth: Lit(0.0),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(80.0),
                airdrag: Lit(0.9),
                gravity: ZERO,
                pos: vec3(0.0, 1.0, 0.0),
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
                directional: true,
                always_visible: false,
            }),
        },
    ],
    // The `[groundflash]` block is `ttl = 0.0`: the engine ages it
    // by nothing per frame, so it draws nothing (§3.4).
    ground_flash: None,
};
