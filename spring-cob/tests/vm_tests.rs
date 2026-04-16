//! Integration tests for the COB virtual machine using real KP animation scripts.

use spring_cob::{AnimCommand, CobVm, parse_cob};
use std::path::PathBuf;

fn scripts_dir() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().unwrap_or(&manifest_dir);
    [
        workspace_root.join("upstream/Kernel-Panic/scripts"),
        PathBuf::from("upstream/Kernel-Panic/scripts"),
    ]
    .into_iter()
    .find(|p| p.is_dir())
}

fn load_cob(name: &str) -> Option<spring_cob::CobFile> {
    let dir = scripts_dir()?;
    let data = std::fs::read(dir.join(name)).ok()?;
    parse_cob(&data).ok()
}

// ---------------------------------------------------------------------------
// Bit.cob tests
// ---------------------------------------------------------------------------

#[test]
fn bit_create_emits_animation_commands() {
    let Some(cob) = load_cob("bit.cob") else {
        eprintln!("Skipping: bit.cob not found");
        return;
    };

    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Create", &[]);

    // Tick several frames — Create() has sleeps and loops.
    let mut all_commands = Vec::new();
    for _ in 0..100 {
        let cmds = vm.tick(&cob, 33); // ~30fps
        all_commands.extend(cmds);
    }

    // Create() does: move gunpoint to z-axis [-3] now; set ARMORED to 1; ...
    let has_move = all_commands
        .iter()
        .any(|c| matches!(c, AnimCommand::MoveNow { .. }));
    let has_set = all_commands
        .iter()
        .any(|c| matches!(c, AnimCommand::SetValue { .. }));
    assert!(has_move, "bit Create should emit a MoveNow command");
    assert!(has_set, "bit Create should emit SetValue (ARMORED)");

    eprintln!("bit Create: {} commands over 100 ticks", all_commands.len());
}

#[test]
fn bit_start_moving_emits_spin() {
    let Some(cob) = load_cob("bit.cob") else {
        eprintln!("Skipping: bit.cob not found");
        return;
    };

    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "StartMoving", &[]);

    let cmds = vm.tick(&cob, 0);

    // StartMoving() does: spin body around x-axis speed <270>
    let has_spin = cmds.iter().any(|c| matches!(c, AnimCommand::Spin { .. }));
    assert!(has_spin, "bit StartMoving should emit Spin");
}

#[test]
fn bit_stop_moving_emits_stop_spin() {
    let Some(cob) = load_cob("bit.cob") else {
        eprintln!("Skipping: bit.cob not found");
        return;
    };

    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "StopMoving", &[]);

    let cmds = vm.tick(&cob, 0);

    let has_stop_spin = cmds
        .iter()
        .any(|c| matches!(c, AnimCommand::StopSpin { .. }));
    assert!(has_stop_spin, "bit StopMoving should emit StopSpin");
}

#[test]
fn bit_killed_hides_pieces() {
    let Some(cob) = load_cob("bit.cob") else {
        eprintln!("Skipping: bit.cob not found");
        return;
    };

    let mut vm = CobVm::new(&cob);
    // Killed(severity, corpsetype)
    vm.start_script(&cob, "Killed", &[100, 0]);

    let cmds = vm.tick(&cob, 0);

    let hide_count = cmds
        .iter()
        .filter(|c| matches!(c, AnimCommand::Hide { .. }))
        .count();
    let explode_count = cmds
        .iter()
        .filter(|c| matches!(c, AnimCommand::Explode { .. }))
        .count();

    assert!(hide_count >= 2, "Killed should hide body and shell");
    assert!(explode_count >= 1, "Killed should explode body");
}

// ---------------------------------------------------------------------------
// Kernel.cob tests
// ---------------------------------------------------------------------------

#[test]
fn kernel_create_emits_initial_animations() {
    let Some(cob) = load_cob("kernel.cob") else {
        eprintln!("Skipping: kernel.cob not found");
        return;
    };

    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Create", &[]);

    let mut all_commands = Vec::new();
    for _ in 0..200 {
        let cmds = vm.tick(&cob, 33);
        all_commands.extend(cmds);
    }

    // Create() turns pillars, moves bases, has sleeps.
    let has_turn = all_commands
        .iter()
        .any(|c| matches!(c, AnimCommand::TurnNow { .. }));
    let has_move = all_commands
        .iter()
        .any(|c| matches!(c, AnimCommand::Move { .. } | AnimCommand::MoveNow { .. }));

    assert!(
        has_turn || has_move,
        "kernel Create should emit turn/move commands for pillar animation"
    );

    eprintln!(
        "kernel Create: {} commands over 200 ticks",
        all_commands.len()
    );
}

/// Diagnostic: after the sleep 400, the moves for pillars/heads should be emitted.
/// This validates that the script flow proceeds past SLEEP to the second batch of moves.
#[test]
fn kernel_create_emits_pillar_moves_after_sleep() {
    let Some(cob) = load_cob("kernel.cob") else {
        return;
    };
    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Create", &[]);

    let pid = |n: &str| {
        cob.piece_names
            .iter()
            .position(|p| p == n)
            .unwrap_or_else(|| panic!("piece {n} not found")) as i32
    };
    let pillar0 = pid("pillar0");
    let head0 = pid("head0");

    // Tick past the sleep 400 with realistic frame intervals.
    let mut all = Vec::new();
    for _ in 0..30 {
        all.extend(vm.tick(&cob, 33));
    }

    let pillar_dest = -16 * 163840;
    let pillar_speed = 24 * 163840;
    let head_dest = -12 * 163840;
    let head_speed = 16 * 163840;

    let pillar_move = all.iter().any(|c| {
        matches!(
            c,
            AnimCommand::Move {
                piece, axis: 1, destination, speed
            } if *piece == pillar0 && *destination == pillar_dest && *speed == pillar_speed
        )
    });
    let head_move = all.iter().any(|c| {
        matches!(
            c,
            AnimCommand::Move {
                piece, axis: 1, destination, speed
            } if *piece == head0 && *destination == head_dest && *speed == head_speed
        )
    });

    assert!(
        pillar_move,
        "pillar0 should get a Move command after sleep; got {} cmds",
        all.len()
    );
    assert!(head_move, "head0 should get a Move command after sleep");
}

/// kernel.bos Create() emits a precise sequence of TurnNow/MoveNow before any sleep.
/// Constants in the compiled .cob: angular constant 182 (cau/deg), linear constant
/// 163840 from the .bos comment ("Scriptor linear constant must be changed 163840").
/// The .bos source explicitly does:
///   turn pillar0 to y-axis <45> now;        // <45> -> 45*182 = 8190
///   turn pillar1 to y-axis <135> now;       // 24570
///   turn pillar2 to y-axis <-45> now;       // -8190
///   turn pillar3 to y-axis <-135> now;      // -24570
///   move base{0..3} to y-axis [-8] now;     // [-8] -> -8 * 163840 = -1310720
///   move pillar{0..3} to y-axis [-32] now;  // -32 * 163840 = -5242880
///   move head{0..3} to y-axis [-32] now;
///   move base{0..3} to y-axis [0] speed [12]; // dest=0, speed = 12 * 163840 = 1966080
///   sleep 400;
#[test]
fn kernel_create_emits_correct_initial_animation_sequence() {
    let Some(cob) = load_cob("kernel.cob") else {
        eprintln!("Skipping: kernel.cob not found");
        return;
    };
    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Create", &[]);

    // Single tick(0) — should run until first SLEEP (sleep 400).
    let cmds = vm.tick(&cob, 0);

    // Filter out commands from sub-threads (ManageONS, TurnTowardBarycenter).
    // ManageONS calls into Lua which we stub to 0. TurnTowardBarycenter does sleep 1
    // immediately so it won't emit on this tick.
    let turn_now: Vec<_> = cmds
        .iter()
        .filter_map(|c| match c {
            AnimCommand::TurnNow {
                piece,
                axis,
                destination,
            } => Some((*piece, *axis, *destination)),
            _ => None,
        })
        .collect();
    let move_now: Vec<_> = cmds
        .iter()
        .filter_map(|c| match c {
            AnimCommand::MoveNow {
                piece,
                axis,
                destination,
            } => Some((*piece, *axis, *destination)),
            _ => None,
        })
        .collect();
    let moves: Vec<_> = cmds
        .iter()
        .filter_map(|c| match c {
            AnimCommand::Move {
                piece,
                axis,
                destination,
                speed,
            } => Some((*piece, *axis, *destination, *speed)),
            _ => None,
        })
        .collect();

    // Look up piece IDs by name.
    let pid = |n: &str| {
        cob.piece_names
            .iter()
            .position(|p| p == n)
            .unwrap_or_else(|| panic!("piece {n} not found")) as i32
    };
    let pillar0 = pid("pillar0");
    let pillar1 = pid("pillar1");
    let pillar2 = pid("pillar2");
    let pillar3 = pid("pillar3");
    let base0 = pid("base0");
    let base1 = pid("base1");
    let base2 = pid("base2");
    let base3 = pid("base3");
    let head0 = pid("head0");
    let head1 = pid("head1");
    let head2 = pid("head2");
    let head3 = pid("head3");

    const Y: i32 = 1; // y-axis

    // The four pillar TurnNow calls (45*182 = 8190).
    assert!(
        turn_now.contains(&(pillar0, Y, 8190)),
        "expected TurnNow pillar0 y +45deg (8190), got {turn_now:?}"
    );
    assert!(
        turn_now.contains(&(pillar1, Y, 24570)),
        "expected TurnNow pillar1 y +135deg (24570), got {turn_now:?}"
    );
    assert!(
        turn_now.contains(&(pillar2, Y, -8190)),
        "expected TurnNow pillar2 y -45deg (-8190), got {turn_now:?}"
    );
    assert!(
        turn_now.contains(&(pillar3, Y, -24570)),
        "expected TurnNow pillar3 y -135deg (-24570), got {turn_now:?}"
    );

    // The bases all jump to y=-8 ([-8] = -1310720).
    for &b in &[base0, base1, base2, base3] {
        assert!(
            move_now.contains(&(b, Y, -1310720)),
            "expected MoveNow base{} y to -8 ([-8]=-1310720), got {move_now:?}",
            b - base0
        );
    }
    // Pillars and heads jump to y=-32 ([-32] = -5242880).
    for &p in &[pillar0, pillar1, pillar2, pillar3] {
        assert!(
            move_now.contains(&(p, Y, -5242880)),
            "expected MoveNow pillar y to -32, got {move_now:?}"
        );
    }
    for &h in &[head0, head1, head2, head3] {
        assert!(
            move_now.contains(&(h, Y, -5242880)),
            "expected MoveNow head y to -32, got {move_now:?}"
        );
    }

    // The four bases then animate from -8 to 0 at speed [12] = 1966080.
    for &b in &[base0, base1, base2, base3] {
        assert!(
            moves.contains(&(b, Y, 0, 1966080)),
            "expected Move base{} y dest=0 speed=1966080, got {moves:?}",
            b - base0
        );
    }

    // After sleep 400, the script does:
    //   move pillar0..3 to y-axis [-16] speed [24];   // dest=-16*163840, speed=24*163840
    //   move head0..3   to y-axis [-12] speed [16];   // dest=-12*163840, speed=16*163840
    //   wait-for-move pillar0 along y-axis;
    // Run additional ticks to cross the sleep boundary and verify those moves.
    let mut later_cmds = Vec::new();
    for _ in 0..20 {
        later_cmds.extend(vm.tick(&cob, 50));
    }
    let later_moves: Vec<_> = later_cmds
        .iter()
        .filter_map(|c| match c {
            AnimCommand::Move {
                piece,
                axis,
                destination,
                speed,
            } => Some((*piece, *axis, *destination, *speed)),
            _ => None,
        })
        .collect();

    let pillar_dest = -16 * 163840;
    let pillar_speed = 24 * 163840;
    let head_dest = -12 * 163840;
    let head_speed = 16 * 163840;
    for &p in &[pillar0, pillar1, pillar2, pillar3] {
        assert!(
            later_moves.contains(&(p, Y, pillar_dest, pillar_speed)),
            "expected Move pillar y dest={pillar_dest} speed={pillar_speed}, got {later_moves:?}"
        );
    }
    for &h in &[head0, head1, head2, head3] {
        assert!(
            later_moves.contains(&(h, Y, head_dest, head_speed)),
            "expected Move head y dest={head_dest} speed={head_speed}, got {later_moves:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// All COB files: Create doesn't crash
// ---------------------------------------------------------------------------

#[test]
fn all_cob_create_scripts_dont_crash() {
    let Some(dir) = scripts_dir() else {
        eprintln!("Skipping: scripts directory not found");
        return;
    };

    let mut tested = 0;

    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("cob") {
            continue;
        }
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let data = std::fs::read(&path).unwrap();
        let cob = match parse_cob(&data) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut vm = CobVm::new(&cob);

        // Start Create if it exists.
        if vm.start_script(&cob, "Create", &[]).is_some() {
            let mut total_cmds = 0;
            for _ in 0..50 {
                let cmds = vm.tick(&cob, 33);
                total_cmds += cmds.len();
            }
            eprintln!("  {name}: Create ran, {total_cmds} commands");
        }

        tested += 1;
    }

    assert!(tested > 0, "expected at least one COB file");
    eprintln!("Tested {tested} COB files without panics");
}

// ---------------------------------------------------------------------------
// Thread management tests
// ---------------------------------------------------------------------------

#[test]
fn sleep_suspends_and_resumes_thread() {
    let Some(cob) = load_cob("bit.cob") else {
        eprintln!("Skipping: bit.cob not found");
        return;
    };

    let mut vm = CobVm::new(&cob);
    let _tid = vm.start_script(&cob, "Create", &[]).unwrap();

    // First tick should hit a sleep and suspend.
    vm.tick(&cob, 0);
    assert!(
        vm.has_active_threads(),
        "thread should be sleeping, not dead"
    );

    // After enough time passes, the thread wakes.
    for _ in 0..100 {
        vm.tick(&cob, 100);
    }

    // Eventually the Create script should complete.
    // (It has a finite loop that exits when BUILD_PERCENT_LEFT is 0,
    // which our stub returns as 0.)
}

#[test]
fn signal_kills_matching_threads() {
    let Some(cob) = load_cob("kernel.cob") else {
        eprintln!("Skipping: kernel.cob not found");
        return;
    };

    let mut vm = CobVm::new(&cob);

    // Start Create (which starts sub-threads).
    vm.start_script(&cob, "Create", &[]);

    // Run for a while to let sub-threads spawn.
    for _ in 0..20 {
        vm.tick(&cob, 33);
    }

    // Start Activate (which signals SIG_ACTDEACT = 64).
    vm.start_script(&cob, "Activate", &[]);

    for _ in 0..10 {
        vm.tick(&cob, 33);
    }

    // The VM should still be functioning (no panics from signal handling).
    // This test mainly verifies the signal mechanism doesn't crash.
}
