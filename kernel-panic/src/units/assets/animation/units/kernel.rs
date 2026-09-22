//! kernel.cob — the System homebase, translated from the compiled
//! bytecode. Kernel compiles with Scriptor linear constant 163840, so
//! every `.bos` bracket is 2.5× the bytecode value: elmos below are
//! raw/65536 straight from the disassembly.
//!
//! Lifecycle, faithful to the script:
//!
//! - `Create()` — pillars fan out (±45°/135°), everything sinks, bases
//!   rise, then pillars/heads rise partway. `canbuild` flips true here.
//! - `Activate()` (production queue becomes non-empty) — wait for
//!   `canbuild`, then unfold the four nano-arm pillars one per 200 ms
//!   (`GetPillarNReady`), each staged with the script's re-speeds: tilt
//!   out to 30°/35° and lift to 0, then pick up the head lift and the
//!   outward turns once the pillar settles.
//! - `Deactivate()` (production stops) — fold the pillars back down one
//!   per **1800 ms** (`GetPillarNRest`): a slow retract that re-speeds
//!   once the head re-seats, exactly like the script's `wait-for-turn`
//!   cascades.
//! - Production is **gated on the unfold**: the base does not start
//!   building until every pillar is fully open (`is_open() == Some(true)`).
//!   Upstream lets production begin mid-rise; the port intentionally
//!   waits so the fold-out is seen in full.
//! - While producing, the tips spray build sparks (`StartBuilding`).

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// Activate(): `sleep 200` between pillar unfolds.
const READY_STAGGER: f32 = 0.2;
/// Deactivate(): `sleep 1800` between pillar folds.
const REST_STAGGER: f32 = 1.8;
/// StartBuilding(): emit burst per `sleep 60` (throttled 2×).
const BUILD_EMIT_INTERVAL: f32 = 0.12;
/// A pillar's choreography is done once its stage passes this.
const LAST_STAGE: u8 = 3;

/// The pillar tree's bound piece indices, resolved once in
/// [`UnitAnim::bind`]. Missing pieces collapse to
/// [`PIECE_MISSING`](super::super::PIECE_MISSING) and every primitive
/// call on them no-ops — same contract as the old name-based path.
#[derive(Clone, Copy, Default)]
struct Pillars {
    pillar: [usize; 4],
    head: [usize; 4],
    base: [usize; 4],
    tip: [usize; 4],
}

impl Pillars {
    fn bind(rig: &AnimRig) -> Self {
        let four = |prefix: &str| {
            [
                rig.bind_piece(&format!("{prefix}0")),
                rig.bind_piece(&format!("{prefix}1")),
                rig.bind_piece(&format!("{prefix}2")),
                rig.bind_piece(&format!("{prefix}3")),
            ]
        };
        Self {
            pillar: four("pillar"),
            head: four("head"),
            base: four("base"),
            tip: four("tip"),
        }
    }
}

/// Which staged choreography is running (mirrors which script the host
/// triggered).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Choreo {
    Ready,
    Rest,
}

impl Choreo {
    /// The stage table for this choreography. Index `n` fires when the
    /// pillar enters stage `n + 1` and completes once its `wait` returns
    /// true — a direct encoding of the script's `GetPillarNReady` /
    /// `GetPillarNRest` command-then-`wait-for-*` cascades.
    fn stages(self) -> &'static [Stage; 3] {
        match self {
            Choreo::Ready => &READY_STAGES,
            Choreo::Rest => &REST_STAGES,
        }
    }

    /// Stagger between pillar launches (Activate `sleep 200`, Deactivate
    /// `sleep 1800`).
    fn stagger(self) -> f32 {
        match self {
            Choreo::Ready => READY_STAGGER,
            Choreo::Rest => REST_STAGGER,
        }
    }
}

/// One choreography stage: the `.bos` commands to fire when entering the
/// stage, and the `wait-for-*` condition that must hold before the next
/// re-speed fires.
struct Stage {
    go: fn(&Pillars, &mut AnimRig, usize),
    done: fn(&Pillars, &AnimRig, usize) -> bool,
}

/// Ready stage 1 — the initial tilt/lift: pillar x → 30° @15°/s, head x
/// → 35° @15°/s, head y → 0 @15, pillar y → 0 @40 elmos/s.
fn pillar_ready_first(p: &Pillars, rig: &mut AnimRig, i: usize) {
    rig.turn_deg(p.pillar[i], Axis::X, 30.0, 15.0);
    rig.turn_deg(p.head[i], Axis::X, 35.0, 15.0);
    rig.move_to(p.head[i], Axis::Y, 0.0, 15.0);
    rig.move_to(p.pillar[i], Axis::Y, 0.0, 40.0);
}

/// Ready stage 2 — after `wait-for-move pillarN along y-axis`: head lift
/// re-speeds to [16]=40 elmos/s and the pillar turn re-speeds to
/// <35>=35°/s.
fn pillar_ready_speed(p: &Pillars, rig: &mut AnimRig, i: usize) {
    rig.move_to(p.head[i], Axis::Y, 0.0, 40.0);
    rig.turn_deg(p.pillar[i], Axis::X, 30.0, 35.0);
}

/// Ready stage 3 — after `wait-for-turn pillarN around x-axis`: the head
/// turn re-speeds to <35>=35°/s.
fn pillar_ready_final(p: &Pillars, rig: &mut AnimRig, i: usize) {
    rig.turn_deg(p.head[i], Axis::X, 35.0, 35.0);
}

/// Rest stage 1 — the initial slow fold: pillar y → −15 @[1]=2.5, head y
/// → −12.5 @2.5 elmos/s, pillar x → 0 @5°/s, head x → 5° @20°/s.
fn pillar_rest_first(p: &Pillars, rig: &mut AnimRig, i: usize) {
    rig.turn_deg(p.pillar[i], Axis::X, 0.0, 5.0);
    rig.turn_deg(p.head[i], Axis::X, 5.0, 20.0);
    rig.move_to(p.pillar[i], Axis::Y, -15.0, 2.5);
    rig.move_to(p.head[i], Axis::Y, -12.5, 2.5);
}

/// Rest stage 2 — after `wait-for-turn headN around x-axis`: the pillar
/// turn re-speeds to <20>=20°/s.
fn pillar_rest_speed(p: &Pillars, rig: &mut AnimRig, i: usize) {
    rig.turn_deg(p.pillar[i], Axis::X, 0.0, 20.0);
}

/// Rest stage 3 — after `wait-for-turn pillarN around x-axis`: both
/// lifts re-speed, pillar y to [8]=20 and head y to [6]=15 elmos/s.
fn pillar_rest_final(p: &Pillars, rig: &mut AnimRig, i: usize) {
    rig.move_to(p.pillar[i], Axis::Y, -15.0, 20.0);
    rig.move_to(p.head[i], Axis::Y, -12.5, 15.0);
}

/// Ready wait conditions, stage by stage: pillar y settled, then pillar x
/// settled, then head x settled.
fn ready_wait_1(p: &Pillars, rig: &AnimRig, i: usize) -> bool {
    rig.at_target(p.pillar[i], Axis::Y)
}
fn ready_wait_2(p: &Pillars, rig: &AnimRig, i: usize) -> bool {
    rig.at_target(p.pillar[i], Axis::X)
}
fn ready_wait_3(p: &Pillars, rig: &AnimRig, i: usize) -> bool {
    rig.at_target(p.head[i], Axis::X)
}

/// Rest wait conditions: head x seated, then pillar x seated, then both
/// lifts settled.
fn rest_wait_1(p: &Pillars, rig: &AnimRig, i: usize) -> bool {
    rig.at_target(p.head[i], Axis::X)
}
fn rest_wait_2(p: &Pillars, rig: &AnimRig, i: usize) -> bool {
    rig.at_target(p.pillar[i], Axis::X)
}
fn rest_wait_3(p: &Pillars, rig: &AnimRig, i: usize) -> bool {
    rig.at_target(p.pillar[i], Axis::Y) && rig.at_target(p.head[i], Axis::Y)
}

const READY_STAGES: [Stage; 3] = [
    Stage {
        go: pillar_ready_first,
        done: ready_wait_1,
    },
    Stage {
        go: pillar_ready_speed,
        done: ready_wait_2,
    },
    Stage {
        go: pillar_ready_final,
        done: ready_wait_3,
    },
];

const REST_STAGES: [Stage; 3] = [
    Stage {
        go: pillar_rest_first,
        done: rest_wait_1,
    },
    Stage {
        go: pillar_rest_speed,
        done: rest_wait_2,
    },
    Stage {
        go: pillar_rest_final,
        done: rest_wait_3,
    },
];

/// One pillar's Activate/Deactivate choreography. Each pillar advances
/// through stages 1→2→3 independently, mirroring the script's
/// `wait-for-*`/re-speed cascades.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ChoreoState {
    kind: Choreo,
    /// Next pillar index to begin (0..4). Pillars `< next` have started.
    next: usize,
    /// Stagger countdown before `next` begins.
    timer: f32,
    /// Highest stage reached per pillar (0 = not yet started).
    stage: [u8; 4],
}

pub struct KernelAnim {
    /// The pillar tree's bound piece indices.
    pieces: Pillars,
    /// Create()'s staged rise: `Some(seconds_until_pillar_rise)` while
    /// the bases are still coming up, `None` once `canbuild` is set.
    rise_wait: Option<f32>,
    /// Active Activate/Deactivate choreography, if any.
    choreo: Option<ChoreoState>,
    /// Fully unfolded (`All` pillars through `LAST_STAGE` of a Ready).
    /// Mirrored by `is_open()` so production can gate on the unfold.
    open: bool,
    build_emit_timer: f32,
}

impl Default for KernelAnim {
    fn default() -> Self {
        Self {
            pieces: Pillars::default(),
            rise_wait: Some(0.4),
            choreo: None,
            open: false,
            build_emit_timer: 0.0,
        }
    }
}

impl UnitAnim for KernelAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = Pillars::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): fan the pillars out (8190=45°, 24570=135°), sink
        // bases −20 / pillars −80 / heads −80 (bytecode −1310720 /
        // −5242880), rise the bases @30.
        let p = &self.pieces;
        for (i, yaw) in [45.0, 135.0, -45.0, -135.0].iter().enumerate() {
            rig.turn_deg(p.pillar[i], Axis::Y, *yaw, 0.0);
        }
        for i in 0..4 {
            rig.move_to(p.base[i], Axis::Y, -20.0, 0.0);
            rig.move_to(p.pillar[i], Axis::Y, -80.0, 0.0);
            rig.move_to(p.head[i], Axis::Y, -80.0, 0.0);
        }
        for i in 0..4 {
            rig.move_to(p.base[i], Axis::Y, 0.0, 30.0);
        }
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s staged pillar rise after the bases come up.
        if let Some(wait) = &mut self.rise_wait {
            *wait -= ctx.dt;
            if *wait <= 0.0 {
                self.rise_wait = None;
                for i in 0..4 {
                    rig.move_to(self.pieces.pillar[i], Axis::Y, -40.0, 60.0);
                    rig.move_to(self.pieces.head[i], Axis::Y, -30.0, 40.0);
                }
            }
        }

        // Activate()/Deactivate() staged choreography. Activate waits
        // for the rise (`while(!canbuild) sleep 100`) before the unfold
        // starts.
        if let Some(choreo) = &mut self.choreo
            && !(choreo.kind == Choreo::Ready && self.rise_wait.is_some())
        {
            let stages = choreo.kind.stages();
            // Cascade stages across every started pillar until no
            // further `wait-for-*` is satisfied this frame (a
            // re-speed can land a piece instantly after an
            // interrupt).
            loop {
                let mut advanced = false;
                for i in 0..4 {
                    let s = choreo.stage[i];
                    if s == 0 || s > LAST_STAGE {
                        continue;
                    }
                    if (stages[(s - 1) as usize].done)(&self.pieces, rig, i) {
                        choreo.stage[i] += 1;
                        advanced = true;
                        if choreo.stage[i] <= LAST_STAGE {
                            (stages[(choreo.stage[i] - 1) as usize].go)(&self.pieces, rig, i);
                        }
                    }
                }
                if !advanced {
                    break;
                }
            }

            if choreo.next < 4 {
                // Stagger-launch the next pillar (one per
                // `sleep 200` / `sleep 1800`).
                choreo.timer -= ctx.dt;
                if choreo.timer <= 0.0 {
                    let i = choreo.next;
                    (stages[0].go)(&self.pieces, rig, i);
                    choreo.stage[i] = 1;
                    choreo.next += 1;
                    choreo.timer = choreo.kind.stagger();
                }
            } else if choreo.stage.iter().all(|&s| s > LAST_STAGE) {
                // Every pillar finished: Ready leaves the base open,
                // Rest leaves it folded.
                self.open = choreo.kind == Choreo::Ready;
                self.choreo = None;
            }
        }

        // StartBuilding(): spray from all four tips while producing.
        if ctx.producing {
            self.build_emit_timer -= ctx.dt;
            if self.build_emit_timer <= 0.0 {
                self.build_emit_timer = BUILD_EMIT_INTERVAL;
                for i in 0..4 {
                    rig.emit(self.pieces.tip[i], super::super::SfxKind::FireFlash);
                }
            }
        }
    }

    fn activate(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): staged pillar-unfold (one per 200 ms across the 4).
        self.open = false;
        self.choreo = Some(ChoreoState {
            kind: Choreo::Ready,
            next: 0,
            timer: 0.0,
            stage: [0; 4],
        });
    }

    fn deactivate(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): staged pillar-fold (one per 1800 ms across the 4).
        self.open = false;
        self.choreo = Some(ChoreoState {
            kind: Choreo::Rest,
            next: 0,
            timer: 0.0,
            stage: [0; 4],
        });
    }

    fn is_open(&self) -> Option<bool> {
        Some(self.open)
    }
}

#[cfg(test)]
mod tests {
    //! Regression guards for the staged Ready/Rest choreography: each
    //! pillar must complete its fold via the script's re-speed cascades
    //! (not crawl at the initial slow speeds forever), and `is_open()`
    //! must only go true once every pillar is fully unfolded.

    use super::KernelAnim;
    use super::super::super::{AnimCtx, AnimRig, Axis, UnitAnim, tick_rig};

    /// The eight animated kernel pieces, one rig array each.
    fn kernel_rig() -> AnimRig {
        static NAMES: [&str; 8] = [
            "pillar0", "pillar1", "pillar2", "pillar3", "head0", "head1", "head2", "head3",
        ];
        let n = NAMES.len();
        AnimRig {
            piece_names: &NAMES,
            piece_entities: Vec::new(),
            piece_base_offsets: vec![[0.0; 3]; n],
            piece_rotations: vec![[0.0; 3]; n],
            piece_translations: vec![[0.0; 3]; n],
            target_rotations: vec![[0.0; 3]; n],
            turn_speeds: vec![[0.0; 3]; n],
            target_translations: vec![[0.0; 3]; n],
            move_speeds: vec![[0.0; 3]; n],
            spin_speeds: vec![[0.0; 3]; n],
            muzzle: 0,
            move_gate: 1.0,
            outbox: Vec::new(),
            dirty: true,
        }
    }

    fn ctx(dt: f32) -> AnimCtx {
        AnimCtx {
            dt,
            build_percent: 0,
            moving: false,
            producing: false,
            deploy: None,
            aim_active: false,
            attack_ordering: false,
            emerging: false,
        }
    }

    fn step(anim: &mut KernelAnim, rig: &mut AnimRig, dt: f32) {
        anim.update(rig, ctx(dt));
        tick_rig(rig, dt);
    }

    /// Advance a rig past the Create rise (`while(!canbuild)`) so the
    /// Ready choreography can proceed.
    fn finish_rise(anim: &mut KernelAnim, rig: &mut AnimRig) {
        for _ in 0..(3 * 60) {
            step(anim, rig, 1.0 / 60.0);
        }
        assert!(anim.rise_wait.is_none());
    }

    fn deg(r: f32) -> f32 {
        r.to_radians()
    }

    fn piece_idx(rig: &AnimRig, name: &str) -> usize {
        rig.piece(name).expect("kernel piece exists")
    }

    #[test]
    fn unfold_completes_and_opens_gate() {
        let mut anim = KernelAnim::default();
        let mut rig = kernel_rig();
        anim.bind(&rig);
        anim.create(&mut rig, ctx(0.0));
        assert_eq!(anim.is_open(), Some(false));
        finish_rise(&mut anim, &mut rig);

        anim.activate(&mut rig, ctx(0.0));
        let mut t = 0.0;
        while anim.choreo.is_some() && t < 8.0 {
            step(&mut anim, &mut rig, 1.0 / 60.0);
            t += 1.0 / 60.0;
        }
        assert!(t < 8.0, "unfold did not complete in time");
        assert_eq!(anim.is_open(), Some(true));
        assert_eq!(anim.choreo, None);

        for i in 0..4 {
            let r = &rig.piece_rotations;
            let tr = &rig.piece_translations;
            assert!(
                (r[piece_idx(&rig, &format!("pillar{i}"))][0] - deg(30.0)).abs() < 1e-3,
                "pillar{i} x not unfolded"
            );
            assert!(
                (r[piece_idx(&rig, &format!("head{i}"))][0] - deg(35.0)).abs() < 1e-3,
                "head{i} x not unfolded"
            );
            assert!(
                tr[piece_idx(&rig, &format!("pillar{i}"))][1].abs() < 1e-2,
                "pillar{i} not lifted"
            );
            assert!(
                tr[piece_idx(&rig, &format!("head{i}"))][1].abs() < 1e-2,
                "head{i} not lifted"
            );
        }
    }

    #[test]
    fn fold_uses_respeed_cascades_not_slow_crawl() {
        // Key regression: upstream GetPillarNRest re-speeds the pillar
        // turn and both lifts after the head seats. Without it the fold
        // crawls at 2.5 elmos/s / 5°/s and pillar0 alone takes ~6s;
        // with the cascades it settles in ~3s. Assert the slow tail is
        // gone on the first pillar (which starts immediately on
        // deactivate).
        let mut anim = KernelAnim::default();
        let mut rig = kernel_rig();
        anim.bind(&rig);
        anim.create(&mut rig, ctx(0.0));
        finish_rise(&mut anim, &mut rig);
        anim.activate(&mut rig, ctx(0.0));
        while anim.choreo.is_some() {
            step(&mut anim, &mut rig, 1.0 / 60.0);
        }
        assert_eq!(anim.is_open(), Some(true));

        anim.deactivate(&mut rig, ctx(0.0));
        let pillar0 = piece_idx(&rig, "pillar0");
        let head0 = piece_idx(&rig, "head0");
        let mut t = 0.0;
        while !(rig.at_target(pillar0, Axis::Y) && rig.at_target(head0, Axis::Y)) && t < 5.0 {
            step(&mut anim, &mut rig, 1.0 / 60.0);
            t += 1.0 / 60.0;
        }
        assert!(t < 5.0, "pillar0 fold still crawling at {t:.2}s");

        while anim.choreo.is_some() && t < 15.0 {
            step(&mut anim, &mut rig, 1.0 / 60.0);
            t += 1.0 / 60.0;
        }
        assert!(t < 15.0, "fold did not complete in time");
        assert_eq!(anim.is_open(), Some(false));

        for i in 0..4 {
            let r = &rig.piece_rotations;
            let tr = &rig.piece_translations;
            assert!(
                (r[piece_idx(&rig, &format!("pillar{i}"))][0] - deg(0.0)).abs() < 1e-3,
                "pillar{i} x not seated"
            );
            assert!(
                (r[piece_idx(&rig, &format!("head{i}"))][0] - deg(5.0)).abs() < 1e-3,
                "head{i} x not seated"
            );
            assert!(
                (tr[piece_idx(&rig, &format!("pillar{i}"))][1] + 15.0).abs() < 1e-2,
                "pillar{i} not retracted"
            );
            assert!(
                (tr[piece_idx(&rig, &format!("head{i}"))][1] + 12.5).abs() < 1e-2,
                "head{i} not retracted"
            );
        }
    }

    #[test]
    fn interrupt_and_resume_reunfolds_from_partial() {
        // Deactivate mid-unfold must cut the Ready choreography short;
        // a re-activate folds back out to full open from wherever the
        // pieces are (no state left holding pillar1..3 at stage 0).
        let mut anim = KernelAnim::default();
        let mut rig = kernel_rig();
        anim.bind(&rig);
        anim.create(&mut rig, ctx(0.0));
        finish_rise(&mut anim, &mut rig);
        anim.activate(&mut rig, ctx(0.0));
        // Cut the unfold half a second in: pillar0 raising, rest not
        // started yet.
        for _ in 0..30 {
            step(&mut anim, &mut rig, 1.0 / 60.0);
        }
        assert!(anim.choreo.is_some());
        anim.deactivate(&mut rig, ctx(0.0));
        // Let the fold run a moment, then resume producing.
        for _ in 0..60 {
            step(&mut anim, &mut rig, 1.0 / 60.0);
        }
        anim.activate(&mut rig, ctx(0.0));
        let mut t = 0.0;
        while anim.choreo.is_some() && t < 8.0 {
            step(&mut anim, &mut rig, 1.0 / 60.0);
            t += 1.0 / 60.0;
        }
        assert_eq!(anim.is_open(), Some(true));
        // All four pillars must re-open regardless of interrupt state.
        for i in 0..4 {
            let r = &rig.piece_rotations;
            assert!(
                (r[piece_idx(&rig, &format!("pillar{i}"))][0] - deg(30.0)).abs() < 1e-3,
                "pillar{i} not re-unfolded"
            );
            assert!(
                (r[piece_idx(&rig, &format!("head{i}"))][0] - deg(35.0)).abs() < 1e-3,
                "head{i} not re-unfolded"
            );
        }
    }

    #[test]
    fn ready_stages_form_expected_transition_sequence() {
        // Spot-check the stage machine: Ready stage 2 re-speeds both the
        // head lift and the pillar turn, keyed to the pillar y-move; and
        // Rest stage 3 re-speeds both lifts, keyed to the pillar x-turn.
        let mut rig = kernel_rig();
        let p = super::Pillars::bind(&rig);
        (super::pillar_ready_first)(&p, &mut rig, 0);
        assert_eq!(rig.move_speeds[piece_idx(&rig, "head0")][1], 15.0);
        assert_eq!(rig.turn_speeds[piece_idx(&rig, "pillar0")][0], deg(15.0));
        (super::pillar_ready_speed)(&p, &mut rig, 0);
        assert_eq!(rig.move_speeds[piece_idx(&rig, "head0")][1], 40.0);
        assert_eq!(rig.turn_speeds[piece_idx(&rig, "pillar0")][0], deg(35.0));
        (super::pillar_ready_final)(&p, &mut rig, 0);
        assert_eq!(rig.turn_speeds[piece_idx(&rig, "head0")][0], deg(35.0));

        (super::pillar_rest_first)(&p, &mut rig, 0);
        assert_eq!(rig.move_speeds[piece_idx(&rig, "pillar0")][1], 2.5);
        assert_eq!(rig.turn_speeds[piece_idx(&rig, "pillar0")][0], deg(5.0));
        (super::pillar_rest_speed)(&p, &mut rig, 0);
        assert_eq!(rig.turn_speeds[piece_idx(&rig, "pillar0")][0], deg(20.0));
        (super::pillar_rest_final)(&p, &mut rig, 0);
        assert_eq!(rig.move_speeds[piece_idx(&rig, "pillar0")][1], 20.0);
        assert_eq!(rig.move_speeds[piece_idx(&rig, "head0")][1], 15.0);
    }
}
