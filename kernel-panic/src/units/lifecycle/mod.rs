//! Entity lifecycle: how units enter the world (spawning, construction,
//! production), the COB script hooks that drive their animations, end-of-game
//! detection, and bookkeeping resources that track live entities.

pub mod bookkeeping;
pub mod construction;
pub mod game_over;
pub mod production;
pub mod script_triggers;
pub mod spawning;
