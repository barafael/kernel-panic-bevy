//! `corruption_infection.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/corruption_infection.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::CegOp::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;
use crate::units::content::cegs::CegKind;

/// `[corruption_infection]` — 2 effect layers, no ground flash.
/// Referenced by 1 weapon.
pub static CORRUPTION_INFECTION: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "smoke",
            air: true,
            ground: true,
            water: true,
            count: 40,
            properties: Spawner(SpawnerProperties {
                target: CegKind::CorruptionInfectionSmoke,
                delay: Ops(&[Add(10.0), Index(10.0)]),
                damage: Ops(&[Add(0.0), Index(1.0)]),
                dir: vec3(0.0, 1.0, 0.0),
                pos: ZERO,
                speed: ZERO,
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
                front_texture: Some(CegTexture::Shockwave),
                color_map: ColorMap(&[
                    [0.75, 0.1, 0.3, 0.1],
                    [0.4, 0.1, 0.2, 0.1],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                size: Lit(3.0),
                length: Lit(1.0),
                size_growth: Lit(140.0),
                ttl: Lit(6.0),
                front_offset: Lit(0.0),
                speed: Lit(0.0),
                dir: vec3(0.0, 1.0, 1e-5),
                pos: vec3(0.0, 10.0, 0.0),
                always_visible: false,
            }),
        },
    ],
    ground_flash: None,
};

/// `[corruption_infection_smoke]` — 1 effect layer, no ground flash.
/// Referenced by 1 spawner.
pub static CORRUPTION_INFECTION_SMOKE: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "squarecloud",
        air: true,
        ground: true,
        water: true,
        count: 6,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Square),
            color_map: ColorMap(&[
                [0.0, 0.0, 0.0, 0.0],
                [1.0, 0.5, 0.5, 0.3],
                [1.0, 0.0, 0.0, 0.2],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            num_particles: Lit(4.0),
            particle_life: Lit(50.0),
            particle_life_spread: Lit(20.0),
            particle_speed: Lit(1.0),
            particle_speed_spread: Lit(3.0),
            particle_size: Lit(20.0),
            particle_size_spread: Lit(10.0),
            size_growth: Lit(2.0),
            size_mod: Lit(0.98),
            emit_rot: Lit(85.0),
            emit_rot_spread: Lit(10.0),
            airdrag: Lit(0.95),
            gravity: ZERO,
            pos: CegVec3 {
                x: Ops(&[Add(-300.0), Rand(600.0)]),
                y: Lit(1.0),
                z: Ops(&[Add(-300.0), Rand(600.0)]),
            },
            emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
            directional: false,
            always_visible: true,
        }),
    }],
    ground_flash: None,
};
