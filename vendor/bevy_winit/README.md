# bevy_winit 0.20.0, patched for kernel-panic

This is the crates.io release of `bevy_winit` 0.20.0 (MIT / Apache-2.0,
<https://github.com/bevyengine/bevy>), used through `[patch.crates-io]` in
the workspace `Cargo.toml`, with one change in `src/state.rs`
(`WinitAppRunnerState::redraw_requested`, marked "kernel-panic patch"):

**A hidden browser page counts as all windows invisible.** Bevy runs an
app update once per `RedrawRequested` (`ran_update_since_last_redraw`),
except when every window is invisible ("redraw calls will never
succeed"). On the web a redraw is an animation frame, and a hidden page
(a background tab) gets none, so the game stopped dead in a hidden tab:
no frames, no lockstep ticks, no packets. With the patch a wake-up of
the event loop (`EventLoopProxy`, which `net::web_tab` sends from a
worker every 33 ms while the page is hidden) runs the update as it would
for an invisible native window. Nothing changes while the page is shown,
or natively. Measured in `net-experiments/m3-rooms.md` §1. Drop this
patch once Bevy treats a hidden document as invisible itself.

Also: `web-sys` lists the `Document` and `Window` features it uses.

---

# Bevy Winit

[![License](https://img.shields.io/badge/license-MIT%2FApache-blue.svg)](https://github.com/bevyengine/bevy#license)
[![Crates.io](https://img.shields.io/crates/v/bevy_winit.svg)](https://crates.io/crates/bevy_winit)
[![Downloads](https://img.shields.io/crates/d/bevy_winit.svg)](https://crates.io/crates/bevy_winit)
[![Docs](https://docs.rs/bevy_winit/badge.svg)](https://docs.rs/bevy_winit/latest/bevy_winit/)
[![Discord](https://img.shields.io/discord/691052431525675048.svg?label=&logo=discord&logoColor=ffffff&color=7389D8&labelColor=6A7EC2)](https://discord.gg/bevy)
