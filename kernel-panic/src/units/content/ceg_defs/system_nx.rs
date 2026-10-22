//! `system_nx.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/system_nx.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::CegOp::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;
use crate::units::content::cegs::CegKind;

/// `[system_nx]` — 3 effect layers, no ground flash.
/// Referenced by 1 weapon.
pub static SYSTEM_NX: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "fire",
            air: true,
            ground: true,
            water: true,
            count: 240,
            properties: Spawner(SpawnerProperties {
                target: CegKind::SystemNxFire,
                delay: Ops(&[Add(8.0), Index(8.0)]),
                damage: Ops(&[Add(0.0), Index(1.0)]),
                dir: vec3(0.0, 1.0, 0.0),
                pos: ZERO,
                speed: ZERO,
                always_visible: false,
            }),
        },
        EffectLayer {
            name: "explosion",
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
                particle_speed: Lit(3.0),
                particle_speed_spread: Lit(10.0),
                particle_size: Lit(12.0),
                particle_size_spread: Lit(8.0),
                size_growth: Lit(0.2),
                size_mod: Lit(0.99),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(90.0),
                airdrag: Lit(0.9),
                gravity: vec3(0.0, 0.05, 0.0),
                pos: vec3(0.0, 30.0, 0.0),
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
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
                front_texture: Some(CegTexture::Shockwave),
                color_map: ColorMap(&[
                    [0.75, 0.5, 0.3, 0.1],
                    [0.4, 0.3, 0.1, 0.1],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                size: Lit(1.0),
                length: Lit(1.0),
                size_growth: Lit(120.0),
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

/// `[system_nx_fire]` — 2 effect layers, no ground flash.
/// Referenced by 1 spawner.
pub static SYSTEM_NX_FIRE: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "squarecloud",
            air: true,
            ground: true,
            water: true,
            count: 2,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Square),
                color_map: ColorMap(&[
                    [1.0, 1.0, 0.0, 0.3],
                    [1.0, 0.0, 0.0, 0.2],
                    [0.0, 0.0, 0.0, 0.8],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                num_particles: Lit(4.0),
                particle_life: Lit(50.0),
                particle_life_spread: Lit(20.0),
                particle_speed: Lit(3.0),
                particle_speed_spread: Lit(5.0),
                particle_size: Lit(7.0),
                particle_size_spread: Lit(4.0),
                size_growth: Lit(0.0),
                size_mod: Lit(0.98),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(20.0),
                airdrag: Lit(0.95),
                gravity: vec3(0.0, 0.1, 0.0),
                pos: CegVec3 {
                    x: Ops(&[Add(-30.0), Rand(60.0)]),
                    y: Lit(1.0),
                    z: Ops(&[Add(-30.0), Rand(60.0)]),
                },
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
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
                front_texture: Some(CegTexture::Circle),
                color_map: ColorMap(&[
                    [0.3, 0.2, 0.0, 0.1],
                    [0.1, 0.1, 0.0, 0.1],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                size: Lit(1.0),
                length: Lit(1.0),
                size_growth: Lit(120.0),
                ttl: Lit(24.0),
                front_offset: Lit(-0.1),
                speed: Lit(0.0),
                dir: vec3(0.0, 1.0, 1e-5),
                pos: vec3(0.0, 30.0, 0.0),
                always_visible: false,
            }),
        },
    ],
    ground_flash: None,
};
