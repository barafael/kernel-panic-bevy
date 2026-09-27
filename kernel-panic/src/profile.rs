//! Dev tool: frame and sim-tick time percentiles, printed at exit.
//!
//! `KP_PROFILE=1` (read into [`DevOptions`]). Measures each rendered
//! frame (`First` to `First`) and each `FixedUpdate` tick
//! (`FixedFirst` to `FixedLast`) with the real clock, and prints
//! mean / p50 / p90 / p99 / max plus how many ticks the frames ran,
//! so a change can be compared without a tracing build:
//! `KP_PROFILE=1 KP_EXIT_AFTER=3000 KP_TIME_SCALE=4 KP_DEMO_MAP=… cargo run --release`.

use bevy::platform::time::Instant;
use bevy::prelude::*;

use crate::game_setup::DevOptions;

pub struct ProfilePlugin;

impl Plugin for ProfilePlugin {
    fn build(&self, app: &mut App) {
        let Some(dev) = app.world().get_resource::<DevOptions>() else {
            return;
        };
        if !dev.profile {
            return;
        }
        app.init_resource::<Samples>()
            .add_systems(First, frame_start)
            .add_systems(FixedFirst, tick_start)
            .add_systems(FixedLast, tick_end)
            .add_systems(Last, report_on_exit);
    }
}

#[derive(Resource, Default)]
struct Samples {
    frame_started: Option<Instant>,
    tick_started: Option<Instant>,
    /// Microseconds per rendered frame.
    frames: Vec<u32>,
    /// Microseconds per sim tick.
    ticks: Vec<u32>,
    ticks_this_frame: u32,
    /// Frames that ran more than one sim tick.
    catch_up_frames: u32,
}

fn frame_start(mut s: ResMut<Samples>) {
    let now = Instant::now();
    if let Some(started) = s.frame_started.replace(now) {
        let us = now.duration_since(started).as_micros() as u32;
        s.frames.push(us);
    }
    if s.ticks_this_frame > 1 {
        s.catch_up_frames += 1;
    }
    s.ticks_this_frame = 0;
}

fn tick_start(mut s: ResMut<Samples>) {
    s.tick_started = Some(Instant::now());
    s.ticks_this_frame += 1;
}

fn tick_end(mut s: ResMut<Samples>) {
    if let Some(started) = s.tick_started.take() {
        let us = started.elapsed().as_micros() as u32;
        s.ticks.push(us);
    }
}

fn report_on_exit(mut exit: MessageReader<AppExit>, mut s: ResMut<Samples>) {
    if exit.read().next().is_none() {
        return;
    }
    // Skip the load: the first 5% of frames are start-up.
    let skip = s.frames.len() / 20;
    s.frames.drain(..skip);
    let frames = stats(&mut s.frames);
    let ticks = stats(&mut s.ticks);
    println!("KP_PROFILE frames={} {frames}", s.frames.len());
    println!(
        "KP_PROFILE ticks={} (frames with >1 tick: {}) {ticks}",
        s.ticks.len(),
        s.catch_up_frames
    );
}

fn stats(us: &mut [u32]) -> String {
    if us.is_empty() {
        return "no samples".into();
    }
    us.sort_unstable();
    let n = us.len();
    let pct = |p: f64| us[((n as f64 * p) as usize).min(n - 1)] as f64 / 1000.0;
    let mean = us.iter().map(|&v| v as f64).sum::<f64>() / n as f64 / 1000.0;
    format!(
        "ms mean={mean:.2} p50={:.2} p90={:.2} p99={:.2} max={:.2}",
        pct(0.5),
        pct(0.9),
        pct(0.99),
        us[n - 1] as f64 / 1000.0
    )
}
