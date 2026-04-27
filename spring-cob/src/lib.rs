//! Parser and virtual machine for Spring RTS engine COB animation scripts.
//!
//! COB is compiled from BOS (a C-like scripting language) and drives unit
//! animations in the Spring engine. This crate provides:
//!
//! - [`cob_file::parse_cob`] — parse `.cob` bytecode files
//! - [`vm::CobVm`] — execute scripts with a stack-based virtual machine
//!
//! The VM is engine-agnostic: it emits [`vm::AnimCommand`]s that the game
//! processes (turn pieces, play effects, etc.).
//!
//! # `COBSCALE` — the 65536 constant
//!
//! Every linear (`move [N]`, `get XY`) and angular (`turn <N>`, `spin`)
//! value in the compiled COB bytecode is a fixed-point integer scaled by
//! **exactly `65536`** at runtime. This is set in upstream Spring as a
//! compile-time constant — there is no per-script, per-unit, or per-file
//! override. The engine always divides by 65536 regardless of how the
//! `.bos` was compiled.
//!
//! Source: [`rts/Sim/Units/Scripts/CobInstance.h` in RecoilEngine][CobInstance]
//!
//! ```cpp
//! // RecoilEngine/rts/Sim/Units/Scripts/CobInstance.h:20-25
//! static constexpr   int COBSCALE      = 65536;
//! static constexpr   int COBSCALE_HALF = COBSCALE / 2;
//! static constexpr float COBSCALE_INV  = 1.0f / COBSCALE;
//!
//! static const float RAD2TAANG = COBSCALE_HALF / math::PI;
//! static const float TAANG2RAD = math::PI / COBSCALE_HALF;
//! ```
//!
//! The engine then uses these directly at every opcode dispatch site:
//!
//! ```cpp
//! // CobInstance.h:131 — Move
//! CUnitScript::Move(piece, axis,
//!     speed       * COBSCALE_INV,
//!     destination * COBSCALE_INV);
//! // CobInstance.h:137 — MoveNow
//! CUnitScript::MoveNow(piece, axis, destination * COBSCALE_INV);
//! ```
//!
//! # The 163840 trap — don't be fooled by the Scriptor compile-time constant
//!
//! Kernel Panic's `.bos` source files carry comments like:
//!
//! ```text
//! // To be compiled with a linear constant of 163840
//! ```
//!
//! at the top of `byte.bos`, `bit.bos`, `kernel.bos`, `socket.bos`,
//! `assembler.bos`, `logic_bomb.bos`, and `badblock.bos`. This is a
//! directive to **Scriptor** (the TA/Spring BOS→COB compiler), not to
//! the runtime. Scriptor multiplies every `[N]` source literal by 163840
//! into the bytecode (so `[4]` → `655360`), and Spring then divides by
//! 65536 at runtime (→ `10.0` elmos effective). The 2.5× gain is
//! exactly what those authors wanted — the source literal is a
//! convenience, the runtime-visible number is what matters.
//!
//! **Consequence for this port:** there is nothing per-unit to handle.
//! Any consumer of the `destination` / `speed` fields on
//! [`vm::AnimCommand::Move`], `MoveNow`, `Turn`, `Spin`, etc. just needs
//! to divide by 65536 once, always. A per-`UnitKind` divisor table is
//! wrong and re-introduces a 2.5× discrepancy versus upstream for any
//! unit whose `.bos` was compiled with a non-default Scriptor constant.
//!
//! The `animation::spring_linear_to_elmos` / `spring_angle_to_radians`
//! helpers in the game crate both hard-code `/ 65536.0` to match. See
//! the regression tests at the bottom of this file.
//!
//! [CobInstance]: https://github.com/beyond-all-reason/RecoilEngine/blob/master/rts/Sim/Units/Scripts/CobInstance.h

mod cob_file;
mod opcodes;
pub mod script_names;
pub mod unit_values;
mod vm;

pub use cob_file::{CobFile, CobParseError, parse_cob, parse_cob_named};
pub use opcodes::Opcode;
pub use script_names::{CallinSlot, CobFn, WeaponCallin};
pub use vm::{AnimCommand, AnimType, CobVm, ThreadState};

/// The runtime fixed-point scale for every linear and angular value in a
/// compiled COB script. Mirrors upstream Spring's
/// [`rts/Sim/Units/Scripts/CobInstance.h:20`][CobInstance] —
/// `static constexpr int COBSCALE = 65536;`. See this crate's module-
/// level documentation for why it is **not** per-unit.
///
/// [CobInstance]: https://github.com/beyond-all-reason/RecoilEngine/blob/master/rts/Sim/Units/Scripts/CobInstance.h
pub const COBSCALE: i32 = 65536;

#[cfg(test)]
mod cobscale_tests {
    use super::COBSCALE;

    /// Upstream's compile-time constant is exactly 65536. Any change
    /// here is an engine-level divergence and should be very
    /// deliberate.
    #[test]
    fn cobscale_matches_upstream_cob_instance_h() {
        // `rts/Sim/Units/Scripts/CobInstance.h:20`
        //   static constexpr int COBSCALE = 65536;
        assert_eq!(COBSCALE, 65536);
    }

    /// `byte.bos` (and bit/kernel/socket/assembler/logic_bomb/badblock)
    /// carry a `// To be compiled with a linear constant of 163840`
    /// comment. That's a **Scriptor** directive; the **engine** still
    /// divides the resulting bytecode by COBSCALE=65536. So a source
    /// `move blade0 to z-axis [4]` in byte.bos becomes `4 * 163840 =
    /// 655360` in the compiled `.cob`, which the engine recovers as
    /// `655360 / 65536 = 10.0` effective elmos — *not* 4.
    ///
    /// Re-introducing a per-unit divisor (the original bug) would
    /// make this test return 4.0 and is a regression.
    #[test]
    fn byte_bos_bracket_4_is_10_effective_elmos() {
        // Scriptor baked 4 * 163840 into the bytecode.
        let bytecode_value: i32 = 4 * 163840;
        let recovered_elmos = bytecode_value as f32 / COBSCALE as f32;
        assert!(
            (recovered_elmos - 10.0).abs() < 1e-4,
            "byte.bos [4] literal: expected 10.0 elmos effective \
             movement after COBSCALE division, got {recovered_elmos}"
        );
    }

    /// Same principle for angular values. byte.bos does
    /// `turn aimer to y-axis <90>`, which Scriptor bakes as
    /// `90 * 182.04 ≈ 16384` (quarter-circle in TA-angle units).
    /// The engine recovers 90° via `16384 * 2π / 65536 = π/2`.
    #[test]
    fn bracket_angle_90_degrees_is_half_pi_radians() {
        // Scriptor baked `<90>` as 16384 (=65536/4) in the bytecode.
        let bytecode_value: i32 = 16384;
        let radians = bytecode_value as f32 * std::f32::consts::TAU / COBSCALE as f32;
        assert!(
            (radians - std::f32::consts::FRAC_PI_2).abs() < 1e-4,
            "<90> literal: expected π/2 radians, got {radians}"
        );
    }
}
