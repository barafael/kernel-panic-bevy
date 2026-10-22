//! `system_sigterm.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/system_sigterm.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::CegOp::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;
use crate::units::content::cegs::CegKind;

/// `[system_sigterm]`.
pub static SYSTEM_SIGTERM: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "risingfire",
            air: true,
            ground: true,
            water: true,
            count: 20,
            properties: Spawner(SpawnerProperties {
                target: CegKind::SystemSigtermFire,
                delay: Ops(&[Add(2.0), Index(3.0)]),
                damage: Ops(&[Add(0.0), Index(1.0)]),
                dir: vec3(0.0, 1.0, 0.0),
                pos: CegVec3 {
                    x: Lit(0.0),
                    y: Ops(&[Add(0.0), Index(23.0)]),
                    z: Lit(0.0),
                },
                speed: ZERO,
                always_visible: true,
            }),
        },
        EffectLayer {
            name: "downblast",
            air: true,
            ground: true,
            water: true,
            count: 1,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Square),
                color_map: ColorMap(&[
                    [1.0, 1.0, 0.0, 0.3],
                    [1.0, 0.0, 0.0, 0.2],
                    [0.4, 0.0, 0.0, 0.8],
                    [0.2, 0.0, 0.0, 0.8],
                    [0.0, 0.0, 0.0, 0.8],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                num_particles: Lit(30.0),
                particle_life: Lit(90.0),
                particle_life_spread: Lit(70.0),
                particle_speed: Lit(10.0),
                particle_speed_spread: Lit(40.0),
                particle_size: Lit(12.0),
                particle_size_spread: Lit(8.0),
                size_growth: Lit(1.0),
                size_mod: Lit(0.99),
                emit_rot: Lit(80.0),
                emit_rot_spread: Lit(30.0),
                airdrag: Lit(0.85),
                gravity: ZERO,
                pos: vec3(0.0, 30.0, 0.0),
                emit_vector: EmitVector::Literal(vec3(0.0, -1.0, 0.0)),
                directional: false,
                always_visible: true,
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
                    [0.75, 0.7, 0.7, 0.1],
                    [0.4, 0.4, 0.4, 0.1],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                size: Lit(3.0),
                length: Lit(1.0),
                size_growth: Lit(140.0),
                ttl: Lit(6.0),
                front_offset: Lit(0.0),
                speed: Lit(0.0),
                dir: vec3(0.0, 1.0, 1e-5),
                pos: vec3(0.0, 60.0, 0.0),
                always_visible: true,
            }),
        },
    ],
    ground_flash: None,
};

/// `[system_sigterm_fire]`.
pub static SYSTEM_SIGTERM_FIRE: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "squarecloud",
            air: true,
            ground: true,
            water: true,
            count: 1,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Square),
                color_map: ColorMap(&[
                    [1.0, 1.0, 0.0, 0.3],
                    [1.0, 0.0, 0.0, 0.2],
                    [0.4, 0.0, 0.0, 0.8],
                    [0.2, 0.0, 0.0, 0.8],
                    [0.0, 0.0, 0.0, 0.8],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                num_particles: Lit(12.0),
                particle_life: Lit(50.0),
                particle_life_spread: Lit(20.0),
                particle_speed: Ops(&[Add(12.0), Damage(-0.5)]),
                particle_speed_spread: Lit(5.0),
                particle_size: Lit(12.0),
                particle_size_spread: Lit(8.0),
                size_growth: Lit(1.0),
                size_mod: Lit(0.98),
                emit_rot: Lit(20.0),
                emit_rot_spread: Lit(80.0),
                airdrag: Lit(0.95),
                gravity: CegVec3 {
                    x: Lit(0.0),
                    y: Ops(&[Add(-0.1), Damage(0.01)]),
                    z: Lit(0.0),
                },
                pos: vec3(0.0, 1.0, 0.0),
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
                directional: false,
                always_visible: true,
            }),
        },
        EffectLayer {
            name: "pillar",
            air: true,
            ground: true,
            water: true,
            count: 4,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Square),
                color_map: ColorMap(&[
                    [0.4, 0.0, 0.0, 0.01],
                    [0.2, 0.0, 0.0, 0.2],
                    [0.0, 0.0, 0.0, 0.8],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                num_particles: Lit(1.0),
                particle_life: Ops(&[Add(110.0), Damage(-3.0)]),
                particle_life_spread: Lit(48.0),
                particle_speed: Lit(0.5),
                particle_speed_spread: Lit(0.7),
                particle_size: Lit(30.0),
                particle_size_spread: Lit(20.0),
                size_growth: Lit(0.05),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(5.0),
                airdrag: Lit(1.0),
                gravity: ZERO,
                pos: vec3(30.0, -35.0, 30.0),
                emit_vector: EmitVector::Literal(vec3(0.0, -1.0, 0.0)),
                directional: false,
                always_visible: true,
            }),
        },
    ],
    ground_flash: None,
};
