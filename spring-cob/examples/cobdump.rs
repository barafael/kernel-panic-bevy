//! COB disassembler — prints the bytecode of a compiled Spring unit
//! script (`.cob`) in a `.bos`-like readable form.
//!
//! The game's animation drivers
//! (`kernel-panic/src/units/assets/animation/units/<kind>.rs`) are
//! hand-translations of this bytecode, so the dumper is the audit tool:
//! dump a unit's script and check every constant against the driver.
//!
//! Angle values print both raw and in degrees (1 revolution = 65536);
//! linear values print both raw and in elmos (65536 = 1 elmo). Note
//! that `.bos` source brackets are *authoring* units scaled by the
//! Scriptor linear constant at compile time — units compiled at 163840
//! (byte, kernel, bit, assembler, socket, badblock, logic_bomb, …)
//! encode `[N]` as `N × 163840`, so the bytecode value is the only
//! ground truth.
//!
//! Usage (the crate is retired from the workspace; run from its
//! directory):
//!
//! ```sh
//! cd spring-cob
//! cargo run --example cobdump -- ../upstream/Kernel-Panic/scripts/byte.cob
//! ```

use std::collections::VecDeque;

use spring_cob::{parse_cob, Opcode};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: cobdump <file.cob>");
        std::process::exit(2);
    };
    let data = std::fs::read(&path).expect("read script");
    let cob = parse_cob(&data).expect("parse script");

    println!("name={:?} static_vars={}", cob.name, cob.num_static_vars);
    println!("pieces: {:?}", cob.piece_names);

    for (i, name) in cob.script_names.iter().enumerate() {
        let (off, len) = (cob.script_offsets[i], cob.script_lengths[i]);
        println!("\n=== fn {name} (#{i}) offset={off} len={len} ===");
        if cob.lua_scripts.get(i).is_some_and(|s| s.starts_with("lua_")) {
            println!("  (lua-only reference)");
            continue;
        }
        disassemble(&cob, off, off + len);
    }
}

/// Best-effort symbolic stack: the values of recently pushed constants,
/// newest first, so stack-consuming ops (turn/move/spin destinations,
/// speeds, sleep durations) can be annotated inline.
#[derive(Default)]
struct Stack<'a> {
    values: VecDeque<(&'a str, i32)>,
}

impl<'a> Stack<'a> {
    fn push(&mut self, kind: &'a str, v: i32) {
        self.values.push_front((kind, v));
        if self.values.len() > 8 {
            self.values.pop_back();
        }
    }

    /// Show (without consuming) the top `n` values, newest first.
    fn show(&self, n: usize) -> String {
        self.values
            .iter()
            .take(n)
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Pop `n` values, newest first (the operands of a stack-consuming op).
    fn drain(&mut self, n: usize) -> Vec<(&'a str, i32)> {
        (0..n).filter_map(|_| self.values.pop_front()).collect()
    }
}

/// Pretty-print an angle/length raw value with its game-unit reading.
fn val(v: i32) -> String {
    format!("{v} ({:.2})", v as f32 / 65536.0)
}

fn piece_name(cob: &spring_cob::CobFile, piece: i32) -> String {
    cob.piece_names
        .get(piece as usize)
        .map(|s| format!("{s}({piece})"))
        .unwrap_or_else(|| format!("{piece}"))
}

fn axis_name(axis: i32) -> &'static str {
    ["x", "y", "z"].get(axis as usize).copied().unwrap_or("?")
}

fn fn_name(cob: &spring_cob::CobFile, id: i32) -> String {
    cob.script_names
        .get(id as usize)
        .map(|s| format!("{s}({id})"))
        .unwrap_or_else(|| format!("{id}"))
}

fn disassemble(cob: &spring_cob::CobFile, start: usize, end: usize) {
    let code = &cob.code;
    let mut pc = start;
    let mut stack = Stack::default();

    while pc < end {
        let raw = code[pc];
        let Some(op) = Opcode::from_repr(raw) else {
            println!("  {pc:04}: ??? {raw:#x}");
            pc += 1;
            continue;
        };
        let arg = |i: usize| code[pc + 1 + i];

        // Inline operand word count per opcode, mirroring the VM's
        // `read_code` consumption in `CobThread` (stack-popped values
        // don't occupy stream words).
        let words = match op {
            // piece, axis
            Opcode::Move
            | Opcode::Turn
            | Opcode::Spin
            | Opcode::StopSpin
            | Opcode::MoveNow
            | Opcode::TurnNow
            | Opcode::WaitTurn
            | Opcode::WaitMove => 2,
            // a single value or piece
            Opcode::PushConstant
            | Opcode::PushLocalVar
            | Opcode::PushStatic
            | Opcode::PopLocalVar
            | Opcode::PopStatic
            | Opcode::Show
            | Opcode::Hide
            | Opcode::Cache
            | Opcode::DontCache
            | Opcode::Shade
            | Opcode::DontShade
            | Opcode::EmitSfx
            | Opcode::Explode
            | Opcode::WaitScale
            | Opcode::Scale
            | Opcode::ScaleNow
            | Opcode::PlaySound => 1,
            // function id, arg count
            Opcode::Start | Opcode::Call | Opcode::RealCall | Opcode::LuaCall
            | Opcode::BatchLua => 2,
            // a single target address
            Opcode::Jump | Opcode::JumpNotEqual => 1,
            _ => 0,
        };

        // Annotate the stack-consuming ops with their effective operands.
        let note = match op {
            Opcode::Turn | Opcode::Move | Opcode::Spin | Opcode::StopSpin => {
                let args = stack.drain(2);
                if args.len() == 2 {
                    let a = args[1].1;
                    let b = args[0].1;
                    format!("dest={} speed={}", val(a), val(b))
                } else {
                    String::new()
                }
            }
            Opcode::TurnNow | Opcode::MoveNow => {
                let args = stack.drain(1);
                args.first().map(|&(_, v)| format!("dest={}", val(v))).unwrap_or_default()
            }
            Opcode::EmitSfx => {
                let args = stack.drain(1);
                args.first().map(|&(_, v)| format!("sfx={v}")).unwrap_or_default()
            }
            Opcode::Explode => {
                let args = stack.drain(1);
                args.first().map(|&(_, v)| format!("severity={v}")).unwrap_or_default()
            }
            Opcode::Sleep => {
                let args = stack.drain(1);
                args.first().map(|&(_, v)| format!("{}ms", val(v))).unwrap_or_default()
            }
            Opcode::Return | Opcode::Signal | Opcode::SetSignalMask => {
                let args = stack.drain(1);
                args.first().map(|&(_, v)| v.to_string()).unwrap_or_default()
            }
            Opcode::Set => {
                let args = stack.drain(2);
                if args.len() == 2 {
                    format!("key={} value={}", args[1].1, args[0].1)
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        };

        let line = match op {
            Opcode::PushConstant => {
                let v = arg(0);
                stack.push("imm", v);
                format!("push {}", val(v))
            }
            Opcode::PushLocalVar => format!("push local[{}]", arg(0)),
            Opcode::PopLocalVar => format!("local[{}] = pop", arg(0)),
            Opcode::PushStatic => format!("push static[{}]", arg(0)),
            Opcode::PopStatic => format!("static[{}] = pop", arg(0)),
            Opcode::CreateLocalVar => "create-local".into(),
            Opcode::PopStack => "pop".into(),
            Opcode::Jump => format!("jump -> {}", arg(0)),
            Opcode::JumpNotEqual => format!("jump-if-false -> {}", arg(0)),
            Opcode::Start => {
                format!("start-script {} args={}", fn_name(cob, arg(0)), arg(1))
            }
            Opcode::Call | Opcode::RealCall => {
                format!("call {} args={}", fn_name(cob, arg(0)), arg(1))
            }
            Opcode::LuaCall | Opcode::BatchLua => {
                format!("lua-call {} args={}", fn_name(cob, arg(0)), arg(1))
            }
            Opcode::Move | Opcode::Turn | Opcode::Spin | Opcode::StopSpin
            | Opcode::MoveNow | Opcode::TurnNow => format!(
                "{op:?} {} axis={}   {note}",
                piece_name(cob, arg(0)),
                axis_name(arg(1)),
            ),
            Opcode::Show | Opcode::Hide | Opcode::Cache | Opcode::DontCache
            | Opcode::Shade | Opcode::DontShade | Opcode::EmitSfx | Opcode::Explode
            | Opcode::Scale | Opcode::ScaleNow | Opcode::WaitScale => format!(
                "{op:?} {}   {note}",
                piece_name(cob, arg(0)),
            ),
            Opcode::WaitTurn | Opcode::WaitMove => format!(
                "{op:?} {} axis={}",
                piece_name(cob, arg(0)),
                axis_name(arg(1)),
            ),
            Opcode::PlaySound => format!("play-sound id={}", arg(0)),
            Opcode::Sleep => format!("sleep   {note}"),
            Opcode::Set => format!("set   {note}"),
            Opcode::Return => format!("return   {note}"),
            Opcode::Signal => format!("signal   {note}"),
            Opcode::SetSignalMask => format!("set-signal-mask   {note}"),
            Opcode::Get | Opcode::GetUnitValue => format!("get   [{}]", stack.show(5)),
            other => format!("{other:?}"),
        };

        println!("  {pc:04}: {line}");
        pc += 1 + words;
    }
}
