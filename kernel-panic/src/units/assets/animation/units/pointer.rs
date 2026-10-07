//! pointer.bos — deployable artillery cube (`cube.s3o`). The script's
//! threads, translated with their `sleep`s and `wait-for-*`s:
//!
//! - `Create()`: hide gun, gunpoint x −90, gunbase x 90 (stowed); a unit
//!   that is already built opens at once, a factory product stays closed
//!   until its first stop.
//! - `StartMoving()`: `sleep 50`, then `Close()` — gunbase returns at
//!   50°/s (only its y is waited for), the gun retracts (1.0 s), the
//!   plates close (0.5 s), the gun is hidden — and only then
//!   `spin body x <180>`. The unit drives the whole time.
//! - `StopMoving()`: `turn body x 0 now; stop-spin; sleep 200`, then
//!   `Open()` — show gun, plates ±10 (0.5 s), gun out (1.0 s), `isOpen`.
//! - `AimWeapon1`: only `if (isOpen)`: gunbase x → 90 − p at 50°/s and the
//!   body steps toward h (host: `aim_weapons_system`).
//!
//! Linear constant 65536: `[20]` = 20 elmos, `[10]` = 10.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};
use super::DeathFx;
use crate::units::combat::DeployState;

/// StartMoving(): `spin body around x-axis speed <180>`.
const ROLL_DPS: f32 = 180.0;
/// Open()/Close(): plates to `[10]`/`[-10]` and gun to `[20]` at `[20]`.
const PLATE_SPREAD: f32 = 10.0;
const GUN_EXTEND: f32 = 20.0;
const MOVE_SPEED: f32 = 20.0;
/// Close(): gunbase x → <90>, y → 0 at <50>.
const GUNBASE_REST_DEG: f32 = 90.0;
const GUNBASE_SPEED: f32 = 50.0;
/// StopMoving(): `sleep 200` = 7 frames before Open(); StartMoving():
/// `sleep 50` = 2 frames before Close().
const OPEN_DELAY: f32 = 7.0 / 30.0;
const CLOSE_DELAY: f32 = 2.0 / 30.0;
/// A `wait-for-move`/`wait-for-turn` resumes on the frame after the
/// animation finishes.
const WAIT_LATENCY: f32 = 1.0 / 30.0;

#[derive(Clone, Copy, Default)]
struct PointerPieces {
    base: usize,
    body: usize,
    left: usize,
    right: usize,
    gun: usize,
    gunbase: usize,
    gunpoint: usize,
}

impl PointerPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            base: rig.bind_piece("base"),
            body: rig.bind_piece("body"),
            left: rig.bind_piece("left"),
            right: rig.bind_piece("right"),
            gun: rig.bind_piece("gun"),
            gunbase: rig.bind_piece("gunbase"),
            gunpoint: rig.bind_piece("gunpoint"),
        }
    }
}

/// Seconds a `move` needs from the piece's current offset on `axis`
/// (the rig stores X mirrored, as `move_to` does).
fn move_time(rig: &AnimRig, piece: usize, axis: Axis, target: f32) -> f32 {
    let target = if axis == Axis::X { -target } else { target };
    let current = rig
        .piece_translations
        .get(piece)
        .map_or(0.0, |t| t[axis as usize]);
    (target - current).abs() / MOVE_SPEED
}

/// Which script thread a [`Thread`] runs.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Script {
    /// `Open()`, after `StopMoving`'s sleep (or straight from `Create`).
    Open,
    /// `Close()` then the roll, after `StartMoving`'s sleep.
    Close,
}

/// One script thread: entries fire when their `sleep`/`wait` elapses.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Thread {
    script: Script,
    t: f32,
    /// Elapsed times at which entries 1..=N fire; the thread ends at the
    /// last one.
    thresholds: [f32; 4],
    entries: usize,
    fired: usize,
}

impl Thread {
    fn open(rig: &AnimRig, p: &PointerPieces, delay: f32) -> Self {
        // sleep; show gun + plates (wait left); gun out (wait); isOpen.
        let plates = delay + move_time(rig, p.left, Axis::X, PLATE_SPREAD) + WAIT_LATENCY;
        let gun = plates + move_time(rig, p.gun, Axis::Y, GUN_EXTEND) + WAIT_LATENCY;
        Self {
            script: Script::Open,
            t: 0.0,
            thresholds: [delay, plates, gun, gun],
            entries: 3,
            fired: 0,
        }
    }

    fn close(rig: &AnimRig, p: &PointerPieces) -> Self {
        // sleep 50; isOpen=0, gunbase back (wait y: already 0, one
        // frame); gun in (wait); plates in (wait); hide gun + spin.
        let gunbase = CLOSE_DELAY + WAIT_LATENCY;
        let gun = gunbase + move_time(rig, p.gun, Axis::Y, 0.0) + WAIT_LATENCY;
        let plates = gun + move_time(rig, p.left, Axis::X, 0.0) + WAIT_LATENCY;
        Self {
            script: Script::Close,
            t: 0.0,
            thresholds: [gunbase, gun, plates, plates],
            entries: 4,
            fired: 0,
        }
    }
}

#[derive(Default)]
pub struct PointerAnim {
    pieces: PointerPieces,
    /// `static-var isOpen`.
    is_open: bool,
    /// The body is rolling (`spin body`).
    rolling: bool,
    thread: Option<Thread>,
    death: DeathFx,
}

impl PointerAnim {
    /// Run `thread`'s entry `i` (1-based; entry `entries` ends it).
    fn fire(&mut self, rig: &mut AnimRig, script: Script, i: usize) {
        let p = self.pieces;
        match (script, i) {
            (Script::Open, 1) => {
                rig.show(p.gun);
                rig.move_to(p.left, Axis::X, PLATE_SPREAD, MOVE_SPEED);
                rig.move_to(p.right, Axis::X, -PLATE_SPREAD, MOVE_SPEED);
            }
            (Script::Open, 2) => {
                rig.move_to(p.gun, Axis::Y, GUN_EXTEND, MOVE_SPEED);
            }
            (Script::Open, 3) => {
                self.is_open = true;
            }
            (Script::Close, 1) => {
                self.is_open = false;
                rig.turn_deg(p.gunbase, Axis::X, GUNBASE_REST_DEG, GUNBASE_SPEED);
                rig.turn_deg(p.gunbase, Axis::Y, 0.0, GUNBASE_SPEED);
            }
            (Script::Close, 2) => {
                rig.move_to(p.gun, Axis::Y, 0.0, MOVE_SPEED);
            }
            (Script::Close, 3) => {
                rig.move_to(p.left, Axis::X, 0.0, MOVE_SPEED);
                rig.move_to(p.right, Axis::X, 0.0, MOVE_SPEED);
            }
            (Script::Close, 4) => {
                rig.hide(p.gun);
                // Back in StartMoving(): `spin body around x-axis <180>`.
                rig.spin_dps(p.body, Axis::X, ROLL_DPS);
                self.rolling = true;
            }
            _ => {}
        }
    }

    fn tick_thread(&mut self, rig: &mut AnimRig, dt: f32) {
        let Some(mut thread) = self.thread else {
            return;
        };
        thread.t += dt;
        while thread.fired < thread.entries && thread.t >= thread.thresholds[thread.fired] {
            thread.fired += 1;
            self.fire(rig, thread.script, thread.fired);
        }
        self.thread = (thread.fired < thread.entries).then_some(thread);
    }
}

impl UnitAnim for PointerAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = PointerPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create(): hide gun; turn gunpoint to x <-90> now; gunbase to
        // x <90> now. `if (!get BUILD_PERCENT_LEFT) start-script Open()`:
        // a unit placed complete opens right away; a factory product
        // stays stowed until its first StopMoving.
        rig.hide(self.pieces.gun);
        rig.turn_deg(self.pieces.gunpoint, Axis::X, -90.0, 0.0);
        rig.turn_deg(self.pieces.gunbase, Axis::X, 90.0, 0.0);
        if !ctx.emerging {
            self.thread = Some(Thread::open(rig, &self.pieces, 0.0));
        }
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge loop: base sinks [-32]·pct/100 while building.
        if ctx.emerging {
            super::emerge_lift(rig, self.pieces.base, 32.0, ctx.build_percent);
        }
        self.death.tick(ctx.dt);
        self.tick_thread(rig, ctx.dt);
    }

    fn start_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StartMoving(): `signal SIG_CHANGE` kills a running Open()/
        // StopMoving thread (pieces keep heading where they were sent),
        // then `sleep 50; call-script Close(); spin body`.
        self.thread = Some(Thread::close(rig, &self.pieces));
    }

    fn stop_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StopMoving(): kill StartMoving/Close(); `turn body to x-axis 0
        // now; stop-spin body`; `sleep 200; start-script Open()`.
        rig.stop_spin(self.pieces.body, Axis::X);
        rig.turn_deg(self.pieces.body, Axis::X, 0.0, 0.0);
        self.rolling = false;
        self.thread = Some(Thread::open(rig, &self.pieces, OPEN_DELAY));
    }

    fn aim(&mut self, rig: &mut AnimRig, _h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1: `if (isOpen)`: gunbase x → <90> − p at <50>; the
        // `set HEADING` loop is the host's body turn. Returns 0 while
        // stowed, so nothing fires until Open() has finished.
        if !self.is_open {
            return false;
        }
        rig.turn_deg(
            self.pieces.gunbase,
            Axis::X,
            GUNBASE_REST_DEG - p.to_degrees(),
            GUNBASE_SPEED,
        );
        true
    }

    fn fire(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): emit-sfx 1024 from gunpoint
        // (Rendered by combat, not here: the `emit-sfx 1024+i` CEG comes
        // out of `fire_weapon_sfx` → `ProjectileFxEvent::muzzle_ceg` at
        // the shot's resolved muzzle. Emitting it from the rig as well
        // drew a second, generic faction puff on every shot.)
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide left/right/gun; explode gun FALL(4), plates
        // SHATTER(1).
        rig.hide(self.pieces.left);
        rig.hide(self.pieces.right);
        rig.hide(self.pieces.gun);
        rig.explode(self.pieces.gun, 4);
        rig.explode(self.pieces.left, 1);
        rig.explode(self.pieces.right, 1);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }

    fn is_open(&self) -> Option<bool> {
        Some(self.is_open)
    }

    fn deploy_state(&self) -> Option<DeployState> {
        Some(match (self.is_open, self.thread.map(|t| t.script)) {
            (true, _) => DeployState::Open,
            (false, Some(Script::Open)) => DeployState::Opening,
            (false, Some(Script::Close)) => DeployState::Closing,
            (false, None) => DeployState::Closed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIECES: &[&str] = &[
        "base", "body", "left", "right", "gun", "gunbase", "gunpoint",
    ];

    fn pointer(emerging: bool) -> (PointerAnim, AnimRig) {
        let mut rig = AnimRig::for_test(PIECES);
        let mut anim = PointerAnim::default();
        anim.bind(&rig);
        anim.create(
            &mut rig,
            AnimCtx {
                emerging,
                ..AnimCtx::minimal()
            },
        );
        (anim, rig)
    }

    fn step(anim: &mut PointerAnim, rig: &mut AnimRig, frames: usize) {
        for _ in 0..frames {
            let ctx = AnimCtx {
                dt: 1.0 / 30.0,
                ..AnimCtx::minimal()
            };
            anim.update(rig, ctx);
            super::super::super::tick_rig(rig, 1.0 / 30.0);
        }
    }

    /// StopMoving → sleep 200 → plates 0.5 s → gun 1.0 s → isOpen at
    /// about 1.73 s. The body never turns and nothing fires before that.
    #[test]
    fn opens_sequentially_after_the_stop_delay() {
        let (mut anim, mut rig) = pointer(true);
        assert_eq!(anim.deploy_state(), Some(DeployState::Closed));
        anim.stop_moving(&mut rig, AnimCtx::minimal());
        assert_eq!(anim.deploy_state(), Some(DeployState::Opening));
        // 0.5 s in: plates moving, gun still retracted.
        step(&mut anim, &mut rig, 15);
        assert!(rig.piece_translations[2][0].abs() > 0.0);
        assert_eq!(rig.piece_translations[4][1], 0.0);
        step(&mut anim, &mut rig, 33);
        assert_eq!(anim.deploy_state(), Some(DeployState::Opening));
        assert!(!anim.aim(&mut rig, 0.0, 0.0, AnimCtx::minimal()));
        step(&mut anim, &mut rig, 7);
        assert_eq!(anim.deploy_state(), Some(DeployState::Open));
        assert!(anim.aim(&mut rig, 0.0, 0.0, AnimCtx::minimal()));
    }

    /// StartMoving from open: the gun stays visible while it retracts and
    /// the plates close; the roll begins only once Close() is through.
    #[test]
    fn closes_before_rolling_and_keeps_the_gun_shown_meanwhile() {
        let (mut anim, mut rig) = pointer(false);
        step(&mut anim, &mut rig, 60);
        assert_eq!(anim.deploy_state(), Some(DeployState::Open));
        anim.start_moving(&mut rig, AnimCtx::minimal());
        step(&mut anim, &mut rig, 10);
        assert_eq!(anim.deploy_state(), Some(DeployState::Closing));
        assert!(!anim.rolling);
        assert!(rig.piece_translations[4][1] > 0.0, "gun still retracting");
        step(&mut anim, &mut rig, 40);
        assert!(!anim.rolling, "plates still closing");
        step(&mut anim, &mut rig, 10);
        assert!(anim.rolling);
        assert_eq!(anim.deploy_state(), Some(DeployState::Closed));
    }
}
