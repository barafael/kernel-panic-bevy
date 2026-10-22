//! `linkbeam.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/linkbeam.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[linkbeam]` — 1 effect layer, no ground flash.
/// Referenced by 6 `[SFXTypes]` entries.
pub static LINKBEAM: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "DoesThisEvenMatter",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Flame(FlameProperties {
            side_texture: Some(CegTexture::LinkBeam),
            front_texture: None,
            color_map: ColorMap(&[
                [1.0, 0.47, 0.0, 1.0],
                [0.0, 1.0, 0.63, 1.0],
                [1.0, 0.47, 0.0, 1.0],
            ]),
            size: Lit(5.0),
            length: Lit(356.0),
            size_growth: Lit(0.0),
            ttl: Lit(60.0),
            front_offset: Lit(0.0),
            speed: Lit(5.0),
            dir: ZERO,
            pos: ZERO,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};
