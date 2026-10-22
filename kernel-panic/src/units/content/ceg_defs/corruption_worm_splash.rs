//! `corruption_worm_splash.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/corruption_worm_splash.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[corruption_worm_splash]` — 1 effect layer, no ground flash.
/// Referenced by 1 weapon.
pub static CORRUPTION_WORM_SPLASH: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "shockwave",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Flame(FlameProperties {
            side_texture: None,
            front_texture: Some(CegTexture::Shockwave),
            color_map: ColorMap(&[
                [0.11, 0.97, 1.0, 0.1],
                [0.12, 0.36, 1.0, 0.1],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            size: Lit(5.0),
            length: Lit(1.0),
            size_growth: Lit(23.0),
            ttl: Lit(5.0),
            front_offset: Lit(0.0),
            speed: Lit(0.0),
            dir: vec3(0.0, 1.0, 1e-5),
            pos: vec3(0.0, 10.0, 0.0),
            always_visible: false,
        }),
    }],
    ground_flash: None,
};
