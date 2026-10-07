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
use super::{DeathFx, move_time};
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

/// Which script thread a [`Thread`] runs.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Script {
    /// `Open()`, after `StopMoving`'s sleep (or straight from `Create`).
    Open,
    /// `Close()` then the roll, after `StartMoving`'s sleep.
    Close,
}

/// One script thread. Entry `fired + 1` runs when `t` reaches `next_at`;
/// each entry returns how long its `wait-for-*` takes from the pieces'
/// poses *at that moment* (an interrupted thread's pieces keep moving in
/// the meantime), and the thread ends after its last entry.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Thread {
    script: Script,
    t: f32,
    next_at: f32,
    fired: usize,
}

impl Thread {
    fn new(script: Script, delay: f32) -> Self {
        Self {
            script,
            t: 0.0,
            next_at: delay,
            fired: 0,
        }
    }

    fn entries(script: Script) -> usize {
        match script {
            Script::Open => 3,
            Script::Close => 4,
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
    /// Run `script`'s entry `i` (1-based) and return the wait the script
    /// does before its next line: the issued move's duration plus the
    /// frame `wait-for-move` costs, or nothing after the last entry.
    fn fire(&mut self, rig: &mut AnimRig, script: Script, i: usize) -> f32 {
        let p = self.pieces;
        match (script, i) {
            // Open(): show gun; plates out; wait-for-move left.
            (Script::Open, 1) => {
                rig.show(p.gun);
                rig.move_to(p.left, Axis::X, PLATE_SPREAD, MOVE_SPEED);
                rig.move_to(p.right, Axis::X, -PLATE_SPREAD, MOVE_SPEED);
                move_time(rig, p.left, Axis::X, PLATE_SPREAD, MOVE_SPEED) + WAIT_LATENCY
            }
            // gun out; wait-for-move gun.
            (Script::Open, 2) => {
                rig.move_to(p.gun, Axis::Y, GUN_EXTEND, MOVE_SPEED);
                move_time(rig, p.gun, Axis::Y, GUN_EXTEND, MOVE_SPEED) + WAIT_LATENCY
            }
            // isOpen = 1.
            (Script::Open, 3) => {
                self.is_open = true;
                0.0
            }
            // Close(): isOpen = 0; gunbase back; wait-for-turn gunbase y
            // (y is already 0: one frame).
            (Script::Close, 1) => {
                self.is_open = false;
                rig.turn_deg(p.gunbase, Axis::X, GUNBASE_REST_DEG, GUNBASE_SPEED);
                rig.turn_deg(p.gunbase, Axis::Y, 0.0, GUNBASE_SPEED);
                WAIT_LATENCY
            }
            // gun in; wait-for-move gun.
            (Script::Close, 2) => {
                rig.move_to(p.gun, Axis::Y, 0.0, MOVE_SPEED);
                move_time(rig, p.gun, Axis::Y, 0.0, MOVE_SPEED) + WAIT_LATENCY
            }
            // plates in; wait-for-move left.
            (Script::Close, 3) => {
                rig.move_to(p.left, Axis::X, 0.0, MOVE_SPEED);
                rig.move_to(p.right, Axis::X, 0.0, MOVE_SPEED);
                move_time(rig, p.left, Axis::X, 0.0, MOVE_SPEED) + WAIT_LATENCY
            }
            // hide gun; back in StartMoving(): `spin body around x-axis <180>`.
            (Script::Close, 4) => {
                rig.hide(p.gun);
                rig.spin_dps(p.body, Axis::X, ROLL_DPS);
                self.rolling = true;
                0.0
            }
            _ => 0.0,
        }
    }

    fn tick_thread(&mut self, rig: &mut AnimRig, dt: f32) {
        let Some(mut thread) = self.thread else {
            return;
        };
        thread.t += dt;
        let entries = Thread::entries(thread.script);
        while thread.fired < entries && thread.t >= thread.next_at {
            thread.fired += 1;
            let wait = self.fire(rig, thread.script, thread.fired);
            thread.next_at += wait;
        }
        self.thread = (thread.fired < entries).then_some(thread);
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
            self.thread = Some(Thread::new(Script::Open, 0.0));
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

    fn start_moving(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // StartMoving(): `signal SIG_CHANGE` kills a running Open()/
        // StopMoving thread (pieces keep heading where they were sent),
        // then `sleep 50; call-script Close(); spin body`.
        self.thread = Some(Thread::new(Script::Close, CLOSE_DELAY));
    }

    fn stop_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StopMoving(): kill StartMoving/Close(); `turn body to x-axis 0
        // now; stop-spin body`; `sleep 200; start-script Open()`.
        rig.stop_spin(self.pieces.body, Axis::X);
        rig.turn_deg(self.pieces.body, Axis::X, 0.0, 0.0);
        self.rolling = false;
        self.thread = Some(Thread::new(Script::Open, OPEN_DELAY));
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

    /// StartMoving from open: 2 frames, then the gun retracts (1.0 s)
    /// while still shown, then the plates close (0.5 s), and only then the
    /// gun hides and the roll begins.
    #[test]
    fn closes_before_rolling_and_keeps_the_gun_shown_meanwhile() {
        let (mut anim, mut rig) = pointer(false);
        step(&mut anim, &mut rig, 60);
        assert_eq!(anim.deploy_state(), Some(DeployState::Open));
        anim.start_moving(&mut rig, AnimCtx::minimal());
        step(&mut anim, &mut rig, 10);
        assert_eq!(anim.deploy_state(), Some(DeployState::Closing));
        assert!(!anim.rolling);
        let gun = rig.piece_translations[4][1];
        assert!(
            gun > 0.0 && gun < 20.0,
            "gun retracting, still shown: {gun}"
        );
        assert_eq!(rig.piece_translations[2][0], -10.0, "plates still open");
        // 1.3 s in: gun retracted, plates closing, no roll yet.
        step(&mut anim, &mut rig, 29);
        assert_eq!(rig.piece_translations[4][1], 0.0);
        let plate = rig.piece_translations[2][0];
        assert!(plate < 0.0 && plate > -10.0, "plates closing: {plate}");
        assert!(!anim.rolling);
        // Plates home at ~1.67 s, hide + spin a frame later.
        step(&mut anim, &mut rig, 15);
        assert!(anim.rolling);
        assert_eq!(anim.deploy_state(), Some(DeployState::Closed));
        assert_eq!(rig.piece_translations[2][0], 0.0);
    }

    /// Re-opening a Pointer that stopped mid-close budgets the gun's wait
    /// from where the gun actually is when that line runs, not from where
    /// it was when the stop happened.
    #[test]
    fn interrupted_close_reopens_from_the_live_pose() {
        let (mut anim, mut rig) = pointer(false);
        step(&mut anim, &mut rig, 60);
        anim.start_moving(&mut rig, AnimCtx::minimal());
        // Gun half retracted (0.5 s of its 1.0 s).
        step(&mut anim, &mut rig, 18);
        let gun = rig.piece_translations[4][1];
        assert!(gun > 5.0 && gun < 15.0, "{gun}");
        anim.stop_moving(&mut rig, AnimCtx::minimal());
        // The plates never started closing, so Open() only has to wait
        // out the sleep, a frame for the plates, and the gun's climb back
        // from wherever it has sunk to by then (~15 elmos, 0.73 s).
        let mut frames = 0;
        while anim.deploy_state() != Some(DeployState::Open) && frames < 120 {
            step(&mut anim, &mut rig, 1);
            frames += 1;
        }
        assert!(
            (28..=36).contains(&frames),
            "reopened after {frames} frames"
        );
        assert_eq!(
            rig.piece_translations[4][1], 20.0,
            "gun fully out when isOpen"
        );
    }
}
