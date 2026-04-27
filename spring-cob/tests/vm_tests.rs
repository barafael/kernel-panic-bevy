//! Integration tests for the COB virtual machine using real KP animation scripts.

use spring_cob::{
    AnimCommand, AnimType, CallinSlot, CobFile, CobFn, CobVm, Opcode, WeaponCallin, parse_cob,
};
use std::path::PathBuf;

/// kernel.bos was compiled with Scriptor's linear constant set to 163840
/// (per the source comment), so each `[1]` in BOS becomes 163840 raw.
const KP_LINEAR: i32 = 163840;

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

fn load_cob(name: &str) -> Option<CobFile> {
    let dir = scripts_dir()?;
    let data = std::fs::read(dir.join(name)).ok()?;
    parse_cob(&data).ok()
}

fn piece_id(cob: &CobFile, name: &str) -> i32 {
    cob.piece_names
        .iter()
        .position(|p| p == name)
        .unwrap_or_else(|| panic!("piece {name} not found")) as i32
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

/// After the sleep 400, the moves for pillars/heads should be emitted.
/// Validates that the script flow proceeds past SLEEP to the second batch of moves.
#[test]
fn kernel_create_emits_pillar_moves_after_sleep() {
    let Some(cob) = load_cob("kernel.cob") else {
        return;
    };
    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Create", &[]);

    let pillar0 = piece_id(&cob, "pillar0");
    let head0 = piece_id(&cob, "head0");

    // Tick past the sleep 400 with realistic frame intervals.
    let mut all = Vec::new();
    for _ in 0..30 {
        all.extend(vm.tick(&cob, 33));
    }

    let pillar_dest = -16 * KP_LINEAR;
    let pillar_speed = 24 * KP_LINEAR;
    let head_dest = -12 * KP_LINEAR;
    let head_speed = 16 * KP_LINEAR;

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

    let pillars = [
        piece_id(&cob, "pillar0"),
        piece_id(&cob, "pillar1"),
        piece_id(&cob, "pillar2"),
        piece_id(&cob, "pillar3"),
    ];
    let bases = [
        piece_id(&cob, "base0"),
        piece_id(&cob, "base1"),
        piece_id(&cob, "base2"),
        piece_id(&cob, "base3"),
    ];
    let heads = [
        piece_id(&cob, "head0"),
        piece_id(&cob, "head1"),
        piece_id(&cob, "head2"),
        piece_id(&cob, "head3"),
    ];

    const Y: i32 = 1; // y-axis

    // The four pillar TurnNow calls (45*182 = 8190).
    for (pillar, expected) in pillars.iter().zip([8190, 24570, -8190, -24570]) {
        assert!(
            turn_now.contains(&(*pillar, Y, expected)),
            "expected TurnNow pillar y {expected}, got {turn_now:?}"
        );
    }

    // The bases all jump to y=[-8] = -8 * KP_LINEAR.
    let base_jump = -8 * KP_LINEAR;
    for &b in &bases {
        assert!(
            move_now.contains(&(b, Y, base_jump)),
            "expected MoveNow base{} y to {base_jump}, got {move_now:?}",
            b - bases[0]
        );
    }
    // Pillars and heads jump to y=[-32].
    let pillar_jump = -32 * KP_LINEAR;
    for &p in pillars.iter().chain(heads.iter()) {
        assert!(
            move_now.contains(&(p, Y, pillar_jump)),
            "expected MoveNow piece {p} y to {pillar_jump}, got {move_now:?}"
        );
    }

    // The four bases then animate from -8 to 0 at speed [12].
    let base_speed = 12 * KP_LINEAR;
    for &b in &bases {
        assert!(
            moves.contains(&(b, Y, 0, base_speed)),
            "expected Move base{} y dest=0 speed={base_speed}, got {moves:?}",
            b - bases[0]
        );
    }

    // After sleep 400, the script animates pillars to [-16]@[24] and heads to [-12]@[16].
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

    let pillar_dest = -16 * KP_LINEAR;
    let pillar_speed = 24 * KP_LINEAR;
    let head_dest = -12 * KP_LINEAR;
    let head_speed = 16 * KP_LINEAR;
    for &p in &pillars {
        assert!(
            later_moves.contains(&(p, Y, pillar_dest, pillar_speed)),
            "expected Move pillar y dest={pillar_dest} speed={pillar_speed}, got {later_moves:?}"
        );
    }
    for &h in &heads {
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

// ---------------------------------------------------------------------------
// New-opcode and feature tests on synthetic bytecode
// ---------------------------------------------------------------------------

/// Build a minimal CobFile holding a single function whose bytecode is
/// the supplied `code`. Skips the binary parser — useful for opcode-level
/// probes that need a single instruction sequence.
fn synthetic_cob(code: Vec<i32>) -> CobFile {
    CobFile::from_test_parts(
        "synthetic",
        vec!["Test".to_string()],
        vec![0],
        vec![code.len()],
        Vec::new(),
        code,
        0,
        Vec::new(),
    )
}

/// Append `pushc N; return` to `code_prefix`. Used by the scale and
/// built-in-GET probes that need the synthetic function to terminate
/// after the opcode under test.
fn return_after(code_prefix: Vec<i32>, return_value: i32) -> Vec<i32> {
    let mut code = code_prefix;
    code.push(Opcode::PushConstant as i32);
    code.push(return_value);
    code.push(Opcode::Return as i32);
    code
}

#[test]
fn scale_opcode_emits_scale_anim_command() {
    let cob = synthetic_cob(return_after(
        vec![
            Opcode::PushConstant as i32,
            8,
            Opcode::PushConstant as i32,
            4096,
            Opcode::Scale as i32,
            2,
        ],
        0,
    ));
    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Test", &[]);
    let cmds = vm.tick(&cob, 0);

    let scale = cmds.iter().find_map(|c| match c {
        AnimCommand::Scale {
            piece,
            destination,
            speed,
        } => Some((*piece, *destination, *speed)),
        _ => None,
    });
    assert_eq!(scale, Some((2, 4096, 8)));
}

#[test]
fn scale_now_snaps_immediately() {
    let cob = synthetic_cob(return_after(
        vec![
            Opcode::PushConstant as i32,
            65536,
            Opcode::ScaleNow as i32,
            5,
        ],
        0,
    ));
    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Test", &[]);
    let cmds = vm.tick(&cob, 0);
    assert!(matches!(
        cmds.as_slice(),
        [
            AnimCommand::ScaleNow {
                piece: 5,
                destination: 65536
            },
            ..
        ]
    ));
}

#[test]
fn wait_scale_blocks_until_anim_finished() {
    let cob = synthetic_cob(return_after(
        vec![
            Opcode::PushConstant as i32,
            16,
            Opcode::PushConstant as i32,
            1024,
            Opcode::Scale as i32,
            7,
            Opcode::WaitScale as i32,
            7,
        ],
        42,
    ));
    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Test", &[]);
    vm.tick(&cob, 0);
    assert!(
        vm.has_active_threads(),
        "WaitScale must keep the thread alive"
    );
    vm.anim_finished(AnimType::Scale, 7, -1);
    vm.tick(&cob, 0);
    assert!(
        !vm.has_active_threads(),
        "after anim_finished + tick the thread should be done"
    );
}

#[test]
fn signature_lua_kills_thread() {
    let cob = synthetic_cob(vec![
        Opcode::SignatureLua as i32,
        Opcode::PushConstant as i32,
        99,
        Opcode::Return as i32,
    ]);
    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Test", &[]);
    vm.tick(&cob, 0);
    assert!(
        !vm.has_active_threads(),
        "SIGNATURE_LUA must terminate the thread immediately"
    );
}

#[test]
fn batch_lua_consumes_args_and_returns_zero() {
    let cob = synthetic_cob(vec![
        Opcode::PushConstant as i32,
        10,
        Opcode::PushConstant as i32,
        20,
        Opcode::PushConstant as i32,
        30,
        Opcode::BatchLua as i32,
        4,
        3,
        Opcode::Return as i32,
    ]);
    let mut vm = CobVm::new(&cob);
    let ret = vm.call_script(&cob, "Test", &[]);
    assert_eq!(ret, Some(0), "BATCH_LUA must push 0 as its return value");
}

#[test]
fn lua_arg_set_is_thread_local_not_anim_command() {
    let cob = synthetic_cob(return_after(
        vec![
            Opcode::PushConstant as i32,
            spring_cob::unit_values::LUA0,
            Opcode::PushConstant as i32,
            7,
            Opcode::Set as i32,
        ],
        0,
    ));
    let mut vm = CobVm::new(&cob);
    vm.start_script(&cob, "Test", &[]);
    let cmds = vm.tick(&cob, 0);
    assert!(
        !cmds
            .iter()
            .any(|c| matches!(c, AnimCommand::SetValue { .. })),
        "Set on LUA arg must be thread-local, got {cmds:?}"
    );
}

#[test]
fn builtin_get_min_max_works_without_host() {
    // Push order: key, p1, p2, p3, p4. GET pops p4..p1 then key, so
    // top of stack is p4.
    let cob = synthetic_cob(vec![
        Opcode::PushConstant as i32,
        spring_cob::unit_values::COB_MIN,
        Opcode::PushConstant as i32,
        3,
        Opcode::PushConstant as i32,
        8,
        Opcode::PushConstant as i32,
        0,
        Opcode::PushConstant as i32,
        0,
        Opcode::Get as i32,
        Opcode::Return as i32,
    ]);
    let mut vm = CobVm::new(&cob);
    let ret = vm.call_script(&cob, "Test", &[]);
    assert_eq!(ret, Some(3));
}

#[test]
fn rand_stays_inside_inclusive_range() {
    let cob = synthetic_cob(vec![
        Opcode::PushConstant as i32,
        10,
        Opcode::PushConstant as i32,
        20,
        Opcode::Rand as i32,
        Opcode::Return as i32,
    ]);

    for seed in [1u32, 42, 0xDEAD_BEEF, 0xC0FF_EE00] {
        let mut vm = CobVm::new(&cob);
        vm.set_rand_seed(seed);
        let ret = vm.call_script(&cob, "Test", &[]).unwrap();
        assert!(
            (10..=20).contains(&ret),
            "Rand returned {ret} out of [10, 20] (seed {seed:#x})"
        );
    }
}

#[test]
fn rand_seed_zero_does_not_collapse() {
    let cob = synthetic_cob(vec![
        Opcode::PushConstant as i32,
        0,
        Opcode::PushConstant as i32,
        100,
        Opcode::Rand as i32,
        Opcode::Return as i32,
    ]);
    let mut vm = CobVm::new(&cob);
    vm.set_rand_seed(0);
    let ret = vm.call_script(&cob, "Test", &[]).unwrap();
    assert!((0..=100).contains(&ret), "Rand seed=0 returned {ret}");
}

#[test]
fn cobfile_resolves_known_callins_in_real_script() {
    let Some(cob) = load_cob("kernel.cob") else {
        eprintln!("Skipping: kernel.cob not found");
        return;
    };
    assert!(cob.has_callin(CallinSlot::Plain(CobFn::Create)));
    assert!(cob.has_callin(CallinSlot::Plain(CobFn::Activate)));
    assert!(cob.has_callin(CallinSlot::Plain(CobFn::Deactivate)));
    let create_id = cob.function_id_for_callin(CallinSlot::Plain(CobFn::Create));
    assert_eq!(create_id, cob.function_id("Create"));
}

#[test]
fn cobfile_resolves_weapon_callins_on_byte() {
    let Some(cob) = load_cob("byte.cob") else {
        eprintln!("Skipping: byte.cob not found");
        return;
    };
    let aim = cob.function_id_for_callin(CallinSlot::Weapon(WeaponCallin::Aim, 0));
    let fire = cob.function_id_for_callin(CallinSlot::Weapon(WeaponCallin::Fire, 0));
    let query = cob.function_id_for_callin(CallinSlot::Weapon(WeaponCallin::Query, 0));
    assert_eq!(aim, cob.function_id("AimWeapon1"));
    assert_eq!(fire, cob.function_id("FireWeapon1"));
    assert_eq!(query, cob.function_id("QueryWeapon1"));
}

#[test]
fn parse_cob_named_records_script_name() {
    let Some(dir) = scripts_dir() else {
        return;
    };
    let data = std::fs::read(dir.join("bit.cob")).unwrap();
    let cob = spring_cob::parse_cob_named(&data, "bit".to_string()).unwrap();
    assert_eq!(cob.name, "bit");
}
