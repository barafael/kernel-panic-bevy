//! Spring asset runtime: S3O mesh loading/caching and per-unit animation
//! drivers. The glue between the bundled models (`content::model`) and Bevy. The
//! animations are hand-written Rust (see `animation`) — the old
//! `spring-cob` bytecode VM has been retired.

pub mod animation;
pub mod meshes;
pub mod model_fade;
