//! Per-unit special behaviors: cloak/fog-of-war, shields, Bug↔Exploit
//! deploy, command-fire abilities (SIGTERM bombs, mines, area denial),
//! the Worm's AutoHold / surfacing cycle, and the Network faction's
//! packet-buffer plumbing.

pub mod cloak;
pub mod command_fire;
pub mod deploy;
pub mod network_buffer;
pub mod shield;
pub mod worm;
