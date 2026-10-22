//! `none.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/none.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::*;

/// `[none]` — 0 effect layers, no ground flash.
/// Referenced by 4 weapons.
pub static NONE: ExplosionDef = ExplosionDef {
    effects: &[],
    ground_flash: None,
};
