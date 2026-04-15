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

pub mod cob_file;
pub mod opcodes;
pub mod vm;

pub use cob_file::{CobFile, CobParseError, parse_cob};
pub use vm::{AnimCommand, AnimType, CobVm, ThreadState};
