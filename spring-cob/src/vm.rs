//! COB virtual machine: executes compiled COB bytecode.
//!
//! The VM manages multiple concurrent threads per unit instance, each with
//! its own program counter, data stack, and call stack. Threads can sleep,
//! wait for animations, signal each other, and spawn child threads.

use crate::cob_file::CobFile;
use crate::opcodes::*;

/// Maximum data stack depth per thread.
const MAX_STACK: usize = 64;
/// Maximum call stack depth per thread.
const MAX_CALL_STACK: usize = 16;
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

    data_stack: Vec<i32>,
    call_stack: Vec<CallFrame>,
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
            data_stack: Vec::with_capacity(16),
            call_stack: Vec::with_capacity(4),
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
            let opcode = thread.read_code(&cob.code);

            match opcode {
                PUSH_CONSTANT => {
                    let val = thread.read_code(&cob.code);
                    thread.push(val);
                }
                PUSH_LOCAL_VAR => {
                    let idx = thread.read_code(&cob.code) as usize;
                    let frame = thread.local_stack_frame();
                    let val = thread.data_stack.get(frame + idx).copied().unwrap_or(0);
                    thread.push(val);
                }
                PUSH_STATIC => {
                    let idx = thread.read_code(&cob.code) as usize;
                    let val = self.static_vars.get(idx).copied().unwrap_or(0);
                    thread.push(val);
                }
                POP_LOCAL_VAR => {
                    let idx = thread.read_code(&cob.code) as usize;
                    let val = thread.pop();
                    let frame = thread.local_stack_frame();
                    if frame + idx < thread.data_stack.len() {
                        thread.data_stack[frame + idx] = val;
                    }
                }
                POP_STATIC => {
                    let idx = thread.read_code(&cob.code) as usize;
                    let val = thread.pop();
                    if idx < self.static_vars.len() {
                        self.static_vars[idx] = val;
                    }
                }
                POP_STACK => {
                    thread.pop();
                }
                CREATE_LOCAL_VAR => {
                    if thread.param_count == 0 {
                        thread.push(0);
                    } else {
                        thread.param_count -= 1;
                    }
                }

                // Arithmetic
                ADD => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a.wrapping_add(b));
                }
                SUB => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a.wrapping_sub(b));
                }
                MUL => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a.wrapping_mul(b));
                }
                DIV => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(if b != 0 { a / b } else { 1000 });
                }
                MOD => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(if b != 0 { a % b } else { 0 });
                }
                BITWISE_AND => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a & b);
                }
                BITWISE_OR => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a | b);
                }
                BITWISE_XOR => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(a ^ b);
                }
                BITWISE_NOT => {
                    let a = thread.pop();
                    thread.push(!a);
                }

                // Comparison
                SET_LESS => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(i32::from(a < b));
                }
                SET_LESS_OR_EQUAL => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(i32::from(a <= b));
                }
                SET_GREATER => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(i32::from(a > b));
                }
                SET_GREATER_OR_EQUAL => {
                    let b = thread.pop();
                    let a = thread.pop();
                    thread.push(i32::from(a >= b));
                }
                SET_EQUAL => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from(a == b));
                }
                SET_NOT_EQUAL => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from(a != b));
                }
                LOGICAL_AND => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from(a != 0 && b != 0));
                }
                LOGICAL_OR => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from(a != 0 || b != 0));
                }
                LOGICAL_XOR => {
                    let a = thread.pop();
                    let b = thread.pop();
                    thread.push(i32::from((a != 0) ^ (b != 0)));
                }
                LOGICAL_NOT => {
                    let a = thread.pop();
                    thread.push(i32::from(a == 0));
                }

                RAND => {
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
                JUMP => {
                    let addr = thread.read_code(&cob.code) as usize;
                    thread.pc = addr;
                }
                JUMP_NOT_EQUAL => {
                    let addr = thread.read_code(&cob.code) as usize;
                    let val = thread.pop();
                    if val == 0 {
                        thread.pc = addr;
                    }
                }
                CALL | REAL_CALL => {
                    let func_id = thread.read_code(&cob.code) as usize;
                    let arg_count = thread.read_code(&cob.code) as usize;

                    if func_id < cob.script_lengths.len()
                        && cob.script_lengths[func_id] > 0
                        && thread.call_stack.len() < MAX_CALL_STACK
                    {
                        thread.call_stack.push(CallFrame {
                            function_id: func_id,
                            return_addr: thread.pc as i32,
                            stack_top: thread.data_stack.len() - arg_count,
                        });
                        thread.param_count = arg_count;
                        thread.pc = cob.script_offsets[func_id];
                    }
                }
                LUA_CALL => {
                    // Skip lua calls — read and discard the args.
                    let _func_id = thread.read_code(&cob.code);
                    let arg_count = thread.read_code(&cob.code) as usize;
                    for _ in 0..arg_count {
                        thread.pop();
                    }
                    // Push 0 as lua return value (lua_* calls return 0 by default).
                    thread.push(0);
                }
                RETURN => {
                    let ret = thread.pop();
                    thread.ret_code = ret;

                    if thread.local_return_addr() == -1 {
                        thread.state = ThreadState::Dead;
                        break;
                    }

                    let return_addr = thread.local_return_addr() as usize;
                    let stack_frame = thread.local_stack_frame();
                    thread
                        .data_stack
                        .truncate(stack_frame.min(thread.data_stack.len()));
                    thread.call_stack.pop();
                    thread.pc = return_addr;
                }
                START => {
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
                SIGNAL => {
                    let sig = thread.pop();
                    // Kill all threads whose signal_mask overlaps with sig.
                    for t in &mut self.threads {
                        if t.id != thread_id && (t.signal_mask & sig) != 0 {
                            t.state = ThreadState::Dead;
                        }
                    }
                }
                SET_SIGNAL_MASK => {
                    let mask = thread.pop();
                    thread.signal_mask = mask;
                }

                // Sleep
                SLEEP => {
                    let ms = thread.pop();
                    thread.wake_time = self.current_time + ms;
                    thread.state = ThreadState::Sleep;
                    break;
                }

                // Animation commands.
                // Spring stack convention for MOVE/TURN: compiler pushes speed
                // first then destination, so top of stack is destination.
                TURN => {
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
                TURN_NOW => {
                    let dest = thread.pop();
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    commands.push(AnimCommand::TurnNow {
                        piece,
                        axis,
                        destination: dest,
                    });
                }
                MOVE => {
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
                MOVE_NOW => {
                    let dest = thread.pop();
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    commands.push(AnimCommand::MoveNow {
                        piece,
                        axis,
                        destination: dest,
                    });
                }
                SPIN => {
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
                STOP_SPIN => {
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    let decel = thread.pop();
                    commands.push(AnimCommand::StopSpin { piece, axis, decel });
                }

                WAIT_TURN => {
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    // In the real engine, this checks NeedsWait. For simplicity,
                    // always wait — the game will call anim_finished when done.
                    thread.wait_piece = piece;
                    thread.wait_axis = axis;
                    thread.state = ThreadState::WaitTurn;
                    break;
                }
                WAIT_MOVE => {
                    let piece = thread.read_code(&cob.code);
                    let axis = thread.read_code(&cob.code);
                    thread.wait_piece = piece;
                    thread.wait_axis = axis;
                    thread.state = ThreadState::WaitMove;
                    break;
                }

                SHOW => {
                    let piece = thread.read_code(&cob.code);
                    commands.push(AnimCommand::Show { piece });
                }
                HIDE => {
                    let piece = thread.read_code(&cob.code);
                    commands.push(AnimCommand::Hide { piece });
                }
                EMIT_SFX => {
                    let sfx_type = thread.pop();
                    let piece = thread.read_code(&cob.code);
                    commands.push(AnimCommand::EmitSfx { sfx_type, piece });
                }
                EXPLODE => {
                    let severity = thread.pop();
                    let piece = thread.read_code(&cob.code);
                    commands.push(AnimCommand::Explode { piece, severity });
                }

                // Get/Set unit values — return 0 for now (game integration point).
                GET_UNIT_VALUE => {
                    let _key = thread.pop();
                    thread.push(0);
                }
                GET => {
                    let _p5 = thread.pop();
                    let _p4 = thread.pop();
                    let _p3 = thread.pop();
                    let _p2 = thread.pop();
                    let _key = thread.pop();
                    thread.push(0);
                }
                SET => {
                    let value = thread.pop();
                    let key = thread.pop();
                    commands.push(AnimCommand::SetValue { key, value });
                }

                // No-ops for visual hints we don't implement.
                SHADE | DONT_SHADE | CACHE | DONT_CACHE => {
                    let _piece = thread.read_code(&cob.code);
                }

                PLAY_SOUND => {
                    let _volume = thread.pop();
                    let _sound_id = thread.read_code(&cob.code);
                }

                ATTACH => {
                    let _p3 = thread.pop();
                    let _p2 = thread.pop();
                    let _p1 = thread.pop();
                }
                DROP => {
                    let _p1 = thread.pop();
                }

                _ => {
                    // Unknown opcode — kill the thread to avoid infinite loops.
                    thread.state = ThreadState::Dead;
                    break;
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
