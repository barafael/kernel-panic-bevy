//! COB virtual machine: executes compiled COB bytecode.
//!
//! The VM manages multiple concurrent threads per unit instance, each with
//! its own program counter, data stack, and call stack. Threads can sleep,
//! wait for animations, signal each other, and spawn child threads.

use smallvec::SmallVec;

use crate::cob_file::CobFile;
use crate::opcodes::Opcode;

/// Maximum data stack depth per thread.
const MAX_STACK: usize = 64;
/// Maximum call stack depth per thread.
const MAX_CALL_STACK: usize = 16;

/// Data stack: capped at `MAX_STACK`, so inline the whole thing.
type DataStack = SmallVec<[i32; MAX_STACK]>;
/// Call stack: capped at `MAX_CALL_STACK`, so inline the whole thing.
type CallStack = SmallVec<[CallFrame; MAX_CALL_STACK]>;
/// Maximum instructions per tick before forced yield (runaway protection).
const MAX_INSTRUCTIONS_PER_TICK: usize = 5000;

/// State of a COB thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Run,
    Sleep,
    WaitTurn,
    WaitMove,
    Dead,
}

/// A single call stack frame.
#[derive(Debug, Clone, Copy, Default)]
struct CallFrame {
    /// Stored for future VM introspection (e.g. GET_UNIT_VALUE).
    #[allow(dead_code)]
    function_id: usize,
    return_addr: i32,
    stack_top: usize,
}

/// A single concurrent thread of execution.
#[derive(Debug, Clone)]
pub struct CobThread {
    pub id: u32,
    pub state: ThreadState,
    pc: usize,
    signal_mask: i32,
    wake_time: i32,
    wait_piece: i32,
    wait_axis: i32,
    param_count: usize,
    ret_code: i32,

    data_stack: DataStack,
    call_stack: CallStack,
}

impl CobThread {
    fn new(id: u32) -> Self {
        Self {
            id,
            state: ThreadState::Run,
            pc: 0,
            signal_mask: 0,
            wake_time: 0,
            wait_piece: -1,
            wait_axis: -1,
            param_count: 0,
            ret_code: 0,
            data_stack: DataStack::new(),
            call_stack: CallStack::new(),
        }
    }

    fn push(&mut self, val: i32) {
        if self.data_stack.len() < MAX_STACK {
            self.data_stack.push(val);
        }
    }

    fn pop(&mut self) -> i32 {
        self.data_stack.pop().unwrap_or(0)
    }

    fn local_stack_frame(&self) -> usize {
        self.call_stack.last().map_or(0, |f| f.stack_top)
    }

    fn local_return_addr(&self) -> i32 {
        self.call_stack.last().map_or(-1, |f| f.return_addr)
    }

    fn read_code(&mut self, code: &[i32]) -> i32 {
        let val = code.get(self.pc).copied().unwrap_or(0);
        self.pc += 1;
        val
    }
}

/// Animation command emitted by the VM for the game to process.
#[derive(Debug, Clone)]
pub enum AnimCommand {
    Turn {
        piece: i32,
        axis: i32,
        destination: i32,
        speed: i32,
    },
    TurnNow {
        piece: i32,
        axis: i32,
        destination: i32,
    },
    Move {
        piece: i32,
        axis: i32,
        destination: i32,
        speed: i32,
    },
    MoveNow {
        piece: i32,
        axis: i32,
        destination: i32,
    },
    Spin {
        piece: i32,
        axis: i32,
        speed: i32,
        accel: i32,
    },
    StopSpin {
        piece: i32,
        axis: i32,
        decel: i32,
    },
    Show {
        piece: i32,
    },
    Hide {
        piece: i32,
    },
    EmitSfx {
        sfx_type: i32,
        piece: i32,
    },
    Explode {
        piece: i32,
        severity: i32,
    },
    SetValue {
        key: i32,
        value: i32,
    },
}

/// A request from the VM to start a new thread.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub function_id: usize,
    pub args: Vec<i32>,
    pub signal_mask: i32,
}

/// The COB virtual machine instance for one unit.
#[derive(Debug, Clone)]
pub struct CobVm {
    threads: Vec<CobThread>,
    pending_spawns: Vec<SpawnRequest>,
    static_vars: Vec<i32>,
    next_thread_id: u32,
    current_time: i32,
}

impl CobVm {
    /// Create a new VM instance for a COB file.
    pub fn new(cob: &CobFile) -> Self {
        Self {
            threads: Vec::new(),
            pending_spawns: Vec::new(),
            static_vars: vec![0; cob.num_static_vars],
            next_thread_id: 1,
            current_time: 0,
        }
    }

    /// Start a script function by name. Returns the thread ID, or None if
    /// the function doesn't exist or has zero length.
    pub fn start_script(&mut self, cob: &CobFile, name: &str, args: &[i32]) -> Option<u32> {
        let func_id = cob.function_id(name)?;
        if cob.script_lengths[func_id] == 0 {
            return None;
        }
        Some(self.start_function(cob, func_id, args, 0))
    }

    /// Call a script function by name, run it to completion (or yield),
    /// and return its return code.
    pub fn call_script(&mut self, cob: &CobFile, name: &str, args: &[i32]) -> Option<i32> {
        let func_id = cob.function_id(name)?;
        if cob.script_lengths[func_id] == 0 {
            return Some(0);
        }
        let thread_id = self.start_function(cob, func_id, args, 0);
        // Tick until this thread finishes or yields.
        let mut commands = Vec::new();
        self.tick_thread(cob, thread_id, &mut commands);

        self.threads
            .iter()
            .find(|t| t.id == thread_id)
            .map(|t| t.ret_code)
    }

    /// Advance time by `dt_ms` milliseconds and tick all runnable threads.
    /// Returns animation commands emitted during execution.
    pub fn tick(&mut self, cob: &CobFile, dt_ms: i32) -> Vec<AnimCommand> {
        self.current_time += dt_ms;
        let mut commands = Vec::new();

        // Wake sleeping threads.
        for thread in &mut self.threads {
            if thread.state == ThreadState::Sleep && self.current_time >= thread.wake_time {
                thread.state = ThreadState::Run;
            }
        }

        // Execute all runnable threads.
        let thread_ids: Vec<u32> = self
            .threads
            .iter()
            .filter(|t| t.state == ThreadState::Run)
            .map(|t| t.id)
            .collect();

        for tid in thread_ids {
            self.tick_thread(cob, tid, &mut commands);
        }

        // Process pending thread spawns.
        let spawns: Vec<SpawnRequest> = self.pending_spawns.drain(..).collect();
        for spawn in spawns {
            self.start_function(cob, spawn.function_id, &spawn.args, spawn.signal_mask);
        }

        // Remove dead threads.
        self.threads.retain(|t| t.state != ThreadState::Dead);

        commands
    }

    /// Notify the VM that a turn/move animation has completed on a piece+axis.
    pub fn anim_finished(&mut self, anim_type: AnimType, piece: i32, axis: i32) {
        for thread in &mut self.threads {
            let matches = thread.wait_piece == piece && thread.wait_axis == axis;
            if !matches {
                continue;
            }
            let wake = matches!(
                (thread.state, anim_type),
                (ThreadState::WaitTurn, AnimType::Turn) | (ThreadState::WaitMove, AnimType::Move)
            );
            if wake {
                thread.state = ThreadState::Run;
                thread.wait_piece = -1;
                thread.wait_axis = -1;
            }
        }
    }

    /// Get the current state of all threads (for debugging).
    pub fn thread_states(&self) -> Vec<(u32, ThreadState, &str)> {
        // Can't return &str without the CobFile, so return basic info.
        self.threads.iter().map(|t| (t.id, t.state, "")).collect()
    }

    /// Check if any threads are alive.
    pub fn has_active_threads(&self) -> bool {
        self.threads.iter().any(|t| t.state != ThreadState::Dead)
    }

    // --- Internal ---

    fn start_function(
        &mut self,
        cob: &CobFile,
        function_id: usize,
        args: &[i32],
        signal_mask: i32,
    ) -> u32 {
        let id = self.next_thread_id;
        self.next_thread_id += 1;

        let mut thread = CobThread::new(id);
        thread.pc = cob.script_offsets[function_id];
        thread.signal_mask = signal_mask;
        thread.param_count = args.len();

        thread.call_stack.push(CallFrame {
            function_id,
            return_addr: -1,
            stack_top: 0,
        });

        for &arg in args {
            thread.push(arg);
        }

        self.threads.push(thread);
        id
    }

    fn tick_thread(&mut self, cob: &CobFile, thread_id: u32, commands: &mut Vec<AnimCommand>) {
        let thread_idx = match self.threads.iter().position(|t| t.id == thread_id) {
            Some(idx) => idx,
            None => return,
        };

        let mut instructions = 0;

        loop {
            if instructions >= MAX_INSTRUCTIONS_PER_TICK {
                break;
            }

            let thread = &mut self.threads[thread_idx];
            if thread.state != ThreadState::Run {
                break;
            }

            instructions += 1;
            let raw_opcode = thread.read_code(&cob.code);
            let Some(opcode) = Opcode::from_repr(raw_opcode) else {
                // Unknown opcode — kill the thread to avoid infinite loops.
                thread.state = ThreadState::Dead;
                break;
            };

            match opcode {
                Opcode::PushConstant => {
                    let val = thread.read_code(&cob.code);
                    thread.push(val);
                }
                Opcode::PushLocalVar => {
                    let idx = thread.read_code(&cob.code) as usize;
                    let frame = thread.local_stack_frame();
                    let val = thread.data_stack.get(frame + idx).copied().unwrap_or(0);
                    thread.push(val);
                }
                Opcode::PushStatic => {
                    let idx = thread.read_code(&cob.code) as usize;
                    let val = self.static_vars.get(idx).copied().unwrap_or(0);
                    thread.push(val);
                }
                Opcode::PopLocalVar => {
                    let idx = thread.read_code(&cob.code) as usize;
                    let val = thread.pop();
                    let frame = thread.local_stack_frame();
                    if frame + idx < thread.data_stack.len() {
                        thread.data_stack[frame + idx] = val;
                    }
                }
                Opcode::PopStatic => {
                    let idx = thread.read_code(&cob.code) as usize;
                    let val = thread.pop();
                    if idx < self.static_vars.len() {
                        self.static_vars[idx] = val;
                    }
                }
                Opcode::PopStack => {
                    thread.pop();
                }
                Opcode::CreateLocalVar => {
                    if thread.param_count == 0 {
                        thread.push(0);
                    } else {
                        thread.param_count -= 1;
                    }
                }

                // Arithmetic
                Opcode::Add => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a.wrapping_add(b));
                }
                Opcode::Sub => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a.wrapping_sub(b));
                }
                Opcode::Mul => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a.wrapping_mul(b));
                }
                Opcode::Div => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(if b != 0 { a / b } else { 1000 });
                }
                Opcode::Mod => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(if b != 0 { a % b } else { 0 });
                }
                Opcode::BitAnd => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a & b);
                }
                Opcode::BitOr => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a | b);
                }
                Opcode::BitXor => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a ^ b);
                }
                Opcode::BitNot => {
                    let a = thread.pop();
                    thread.push(!a);
                }

                // Comparison
                Opcode::SetLess => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(i32::from(a < b));
                }
                Opcode::SetLessOrEqual => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(i32::from(a <= b));
                }
                Opcode::SetGreater => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(i32::from(a > b));
                }
                Opcode::SetGreaterOrEqual => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(i32::from(a >= b));
                }
                Opcode::SetEqual => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from(a == b));
                }
                Opcode::SetNotEqual => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from(a != b));
                }
                Opcode::LogicalAnd => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from(a != 0 && b != 0));
                }
                Opcode::LogicalOr => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from(a != 0 || b != 0));
                }
                Opcode::LogicalXor => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from((a != 0) ^ (b != 0)));
                }
                Opcode::LogicalNot => {
                    let a = thread.pop();
                    thread.push(i32::from(a == 0));
                }

                Opcode::Rand => {
                    let b = thread.pop();
                    let a = thread.pop();
                    // Simple deterministic "random" — good enough for animations.
                    let range = (b - a + 1).max(1);
                    let val = a
                        + (self
                            .current_time
                            .wrapping_mul(1103515245)
                            .wrapping_add(12345)
                            % range)
                            .abs();
                    thread.push(val);
                }

                // Flow control
                Opcode::Jump => {
                    let addr = thread.read_code(&cob.code) as usize;
                    thread.pc = addr;
                }
                Opcode::JumpNotEqual => {
                    let addr = thread.read_code(&cob.code) as usize;
                    let val = thread.pop();
                    if val == 0 {
                        thread.pc = addr;
                    }
                }
                Opcode::Call | Opcode::RealCall => {
                    let func_id = thread.read_code(&cob.code) as usize;
                    let arg_count = thread.read_code(&cob.code) as usize;

                    let Some(stack_top) = thread.data_stack.len().checked_sub(arg_count) else {
                        // Malformed bytecode: more args requested than stack holds.
                        thread.state = ThreadState::Dead;
                        break;
                    };

                    if func_id < cob.script_lengths.len()
                        && cob.script_lengths[func_id] > 0
                        && thread.call_stack.len() < MAX_CALL_STACK
                    {
                        thread.call_stack.push(CallFrame {
                            function_id: func_id,
                            return_addr: thread.pc as i32,
                            stack_top,
                        });
                        thread.param_count = arg_count;
                        thread.pc = cob.script_offsets[func_id];
                    }
                }
                Opcode::LuaCall => {
                    // Skip lua calls — read and discard the args.
                    let _func_id = thread.read_code(&cob.code);
                    let arg_count = thread.read_code(&cob.code) as usize;
                    for _ in 0..arg_count {
                        thread.pop();
                    }
                    // Push 0 as lua return value (lua_* calls return 0 by default).
                    thread.push(0);
                }
                Opcode::Return => {
                    let ret = thread.pop();
                    thread.ret_code = ret;

                    let raw_addr = thread.local_return_addr();
                    let Ok(return_addr) = usize::try_from(raw_addr) else {
                        // -1 marks the root frame; any other negative is malformed bytecode.
                        thread.state = ThreadState::Dead;
                        break;
                    };
                    let stack_frame = thread.local_stack_frame();
                    thread
                        .data_stack
                        .truncate(stack_frame.min(thread.data_stack.len()));
                    thread.call_stack.pop();
                    thread.pc = return_addr;
                }
                Opcode::Start => {
                    let func_id = thread.read_code(&cob.code) as usize;
                    let arg_count = thread.read_code(&cob.code) as usize;

                    let mut args = Vec::with_capacity(arg_count);
                    for _ in 0..arg_count {
                        args.push(thread.pop());
                    }

                    self.pending_spawns.push(SpawnRequest {
                        function_id: func_id,
                        args,
                        signal_mask: thread.signal_mask,
                    });
                }
                Opcode::Signal => {
                    let sig = thread.pop();
                    // Kill all threads whose signal_mask overlaps with sig.
                    for t in &mut self.threads {
                        if t.id != thread_id && (t.signal_mask & sig) != 0 {
                            t.state = ThreadState::Dead;
                        }
                    }
                }
                Opcode::SetSignalMask => {
                    let mask = thread.pop();
                    thread.signal_mask = mask;
                }

                // Sleep
                Opcode::Sleep => {
                    let ms = thread.pop();
                    thread.wake_time = self.current_time + ms;
                    thread.state = ThreadState::Sleep;
                    break;
                }

                // Animation commands.
                // Spring stack convention for MOVE/TURN: compiler pushes speed
                // first then destination, so top of stack is destination.
                Opcode::Turn => {
                    let dest = thread.pop();
                    let speed = thread.pop();
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    commands.push(AnimCommand::Turn {
                        piece,
                        axis,
                        destination: dest,
                        speed,
                    });
                }
                Opcode::TurnNow => {
                    let dest = thread.pop();
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    commands.push(AnimCommand::TurnNow {
                        piece,
                        axis,
                        destination: dest,
                    });
                }
                Opcode::Move => {
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    let dest = thread.pop();
                    let speed = thread.pop();
                    commands.push(AnimCommand::Move {
                        piece,
                        axis,
                        destination: dest,
                        speed,
                    });
                }
                Opcode::MoveNow => {
                    let dest = thread.pop();
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    commands.push(AnimCommand::MoveNow {
                        piece,
                        axis,
                        destination: dest,
                    });
                }
                Opcode::Spin => {
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    let speed = thread.pop();
                    let accel = thread.pop();
                    commands.push(AnimCommand::Spin {
                        piece,
                        axis,
                        speed,
                        accel,
                    });
                }
                Opcode::StopSpin => {
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    let decel = thread.pop();
                    commands.push(AnimCommand::StopSpin { piece, axis, decel });
                }

                Opcode::WaitTurn => {
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    // In the real engine, this checks NeedsWait. For simplicity,
                    // always wait — the game will call anim_finished when done.
                    thread.wait_piece = piece;
                    thread.wait_axis = axis;
                    thread.state = ThreadState::WaitTurn;
                    break;
                }
                Opcode::WaitMove => {
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    thread.wait_piece = piece;
                    thread.wait_axis = axis;
                    thread.state = ThreadState::WaitMove;
                    break;
                }

                Opcode::Show => {
                    let piece = thread.read_code(&cob.code);
                    commands.push(AnimCommand::Show { piece });
                }
                Opcode::Hide => {
                    let piece = thread.read_code(&cob.code);
                    commands.push(AnimCommand::Hide { piece });
                }
                Opcode::EmitSfx => {
                    let sfx_type = thread.pop();
                    let piece = thread.read_code(&cob.code);
                    commands.push(AnimCommand::EmitSfx { sfx_type, piece });
                }
                Opcode::Explode => {
                    let severity = thread.pop();
                    let piece = thread.read_code(&cob.code);
                    commands.push(AnimCommand::Explode { piece, severity });
                }

                // Get/Set unit values — return 0 for now (game integration point).
                Opcode::GetUnitValue => {
                    let _key = thread.pop();
                    thread.push(0);
                }
                Opcode::Get => {
                    let _p5 = thread.pop();
                    let _p4 = thread.pop();
                    let _p3 = thread.pop();
                    let _p2 = thread.pop();
                    let _key = thread.pop();
                    thread.push(0);
                }
                Opcode::Set => {
                    let value = thread.pop();
                    let key = thread.pop();
                    commands.push(AnimCommand::SetValue { key, value });
                }

                // No-ops for visual hints we don't implement.
                Opcode::Shade | Opcode::DontShade | Opcode::Cache | Opcode::DontCache => {
                    let _piece = thread.read_code(&cob.code);
                }

                Opcode::PlaySound => {
                    let _volume = thread.pop();
                    let _sound_id = thread.read_code(&cob.code);
                }

                Opcode::Attach => {
                    let _p3 = thread.pop();
                    let _p2 = thread.pop();
                    let _p1 = thread.pop();
                }
                Opcode::Drop => {
                    let _p1 = thread.pop();
                }
            }
        }
    }
}

/// Animation type for `anim_finished` callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimType {
    Turn,
    Move,
}
