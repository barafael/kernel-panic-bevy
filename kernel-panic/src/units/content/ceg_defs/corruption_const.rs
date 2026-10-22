//! `corruption_const.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/corruption_const.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::CegOp::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[corruption_const]`.
pub static CORRUPTION_CONST: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "stuff",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Flame(FlameProperties {
            side_texture: None,
            front_texture: Some(CegTexture::SquareHollow),
            color_map: ColorMap(&[
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            size: Lit(1.0),
            length: Lit(4.0),
            size_growth: Ops(&[Add(8.0), Rand(2.0)]),
            ttl: Ops(&[Add(12.0), Rand(4.0)]),
            front_offset: Lit(1.0),
            speed: Lit(0.0),
            dir: CegVec3 {
                x: Ops(&[Add(-0.05), Rand(0.1)]),
                y: Lit(-1.0),
                z: Ops(&[Add(-0.05), Rand(0.1)]),
            },
            pos: ZERO,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};
