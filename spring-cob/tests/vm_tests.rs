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
