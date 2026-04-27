//! COB virtual machine: executes compiled COB bytecode.
//!
//! The VM manages multiple concurrent threads per unit instance, each with
//! its own program counter, data stack, and call stack. Threads can sleep,
//! wait for animations, signal each other, and spawn child threads.

use smallvec::SmallVec;

use crate::cob_file::CobFile;
use crate::opcodes::Opcode;
use crate::unit_values::{NUM_LUA_ARGS, builtin_get_value, lua_arg_index};

/// Maximum data stack depth per thread.
const MAX_STACK: usize = 64;
/// Maximum call stack depth per thread.
const MAX_CALL_STACK: usize = 16;
/// Game-tick rate the engine runs at. The Spring engine ships at 30
/// frames/sec; `GAME_FRAME` and a few helpers depend on this exact
/// value. Mirrors `rts/Sim/Misc/GlobalConstants.h:GAME_SPEED`.
const GAME_SPEED_FPS: i32 = 30;

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
    /// Suspended waiting on a piece-scale animation (Recoil's
    /// `WAIT_SCALE`). Wakes on `anim_finished(AnimType::Scale, ...)`.
    WaitScale,
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
    /// Per-thread Lua argument slots `[LUA0..LUA9]` (keys 110..119).
    /// Mirrors upstream `CCobThread::luaArgs`. `Set(LUA_n)` writes here
    /// instead of generating a host-visible `AnimCommand::SetValue`,
    /// and `Get(LUA_n)` reads from here. Without a Lua VM these slots
    /// behave as a tiny per-thread scratch register file — useful for
    /// scripts that hand off intermediate values across LuaCalls that
    /// we stub out.
    lua_args: [i32; NUM_LUA_ARGS],
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
            lua_args: [0; NUM_LUA_ARGS],
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
///
/// Hosts pattern-match this enum and apply each variant to their
/// rendering / sim layer. New variants may be added in additive minor
/// releases — match it with a `_ => {}` catch-all so future opcodes
/// don't break compilation.
#[derive(Debug, Clone)]
#[non_exhaustive]
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
    /// Recoil-only piece scale animation. `destination` and `speed` are
    /// in COBSCALE-fixed-point (divide by 65536 for the host-side float).
    /// Single-axis (uniform) — there is no `axis` because the engine
    /// scales the whole piece.
    Scale {
        piece: i32,
        destination: i32,
        speed: i32,
    },
    /// Snap a piece's scale (uniform) without animating.
    ScaleNow {
        piece: i32,
        destination: i32,
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
    /// Play a sound by index into `CobFile::sound_names` (TA:K v6 sound
    /// table). `volume` matches upstream's `attr` arg — most scripts
    /// pass 0 and let the engine pick a default.
    PlaySound {
        sound_id: i32,
        volume: i32,
    },
    /// COB transports a unit by attaching it to one of its pieces.
    /// `unit_id` and `piece` come from the script; `attach_type` is
    /// the third arg (upstream calls it the asPieceNum/extra slot).
    Attach {
        unit_id: i32,
        piece: i32,
        attach_type: i32,
    },
    /// Drop the previously-attached unit.
    Drop {
        unit_id: i32,
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
    /// xorshift32 state for `Opcode::Rand`. Seeded deterministically per
    /// VM so two units sharing the same script get reproducible
    /// animations across runs (required for replay/desync-free play).
    /// Use [`CobVm::set_rand_seed`] to pin.
    rand_state: u32,
    /// Host-supplied state the VM exposes through `Opcode::Get`. Keys are
    /// the well-known Spring COB value indices (see `cob_values`); the
    /// host updates them each tick (e.g. `BUILD_PERCENT_LEFT`,
    /// `INBUILDSTANCE`). Anything not set reads as 0.
    unit_values: smallvec::SmallVec<[(i32, i32); 8]>,
    /// `(thread_id, ret_code)` pairs for threads that died on the most
    /// recent `tick()`, captured before pruning so the host can still
    /// correlate a previously-spawned thread (e.g. `AimWeapon1`) to its
    /// return value. Drained by [`CobVm::take_ended_threads`].
    ended_threads: Vec<(u32, i32)>,
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
            // Non-zero default — xorshift32 collapses to 0 if seeded
            // with 0. `0xC0BEFE` is arbitrary but stable.
            rand_state: 0x00C0_BEFE,
            unit_values: smallvec::SmallVec::new(),
            ended_threads: Vec::new(),
        }
    }

    /// Seed the per-VM `Opcode::Rand` PRNG. Pass any 32-bit value other
    /// than zero (xorshift32 collapses on 0; the VM substitutes a
    /// non-zero default in that case). Determinism is per-VM, not
    /// per-unit-kind, so seed each spawn with its unit id (or any
    /// other replay-stable value) to avoid lockstep desync.
    pub fn set_rand_seed(&mut self, seed: u32) {
        self.rand_state = if seed == 0 { 0x00C0_BEFE } else { seed };
    }

    /// Update (or insert) the value the VM should return for COB
    /// `Opcode::Get(key)`. Used by the host to publish state like
    /// `BUILD_PERCENT_LEFT` so unit `Create()` scripts that animate the
    /// build emerge actually do something.
    pub fn set_unit_value(&mut self, key: i32, value: i32) {
        if let Some(slot) = self.unit_values.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.unit_values.push((key, value));
        }
    }

    /// Read a host-published unit value, or 0 if none was set.
    pub fn get_unit_value(&self, key: i32) -> i32 {
        self.lookup_unit_value(key).unwrap_or(0)
    }

    /// Internal lookup that returns `None` when the host hasn't
    /// published the key — distinguishes "explicitly set to zero" from
    /// "not in the bag", which the dispatch loop uses to fall through
    /// to built-in math handlers.
    fn lookup_unit_value(&self, key: i32) -> Option<i32> {
        self.unit_values
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
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

    /// Call a script function by name and read back local[0] — the
    /// BOS-idiomatic out-parameter slot used by every `Query*`
    /// function (e.g. `QueryWeapon1(piecenum) { piecenum = gunpoint; }`).
    /// Mirrors upstream's [`CCobInstance::Call`][CobInstance], which
    /// copies the locals back into the caller's `args` vector after
    /// the function exits.
    ///
    /// Returns `None` if the function is missing, empty, or yielded
    /// — `Query*` should never yield.
    ///
    /// [CobInstance]: https://github.com/beyond-all-reason/RecoilEngine/blob/master/rts/Sim/Units/Scripts/CobInstance.cpp
    pub fn call_script_out_param(&mut self, cob: &CobFile, name: &str) -> Option<i32> {
        let func_id = cob.function_id(name)?;
        if cob.script_lengths[func_id] == 0 {
            return None;
        }
        // Seed local[0] with 0 — matches upstream's empty out-param.
        let thread_id = self.start_function(cob, func_id, &[0], 0);
        let mut commands = Vec::new();
        self.tick_thread(cob, thread_id, &mut commands);

        // Why: at root-frame Return the VM breaks out without
        // truncating the data stack, so locals live on at
        // `[0, param_count)`. Read local[0] before pruning.
        let result = self
            .threads
            .iter()
            .find(|t| t.id == thread_id)
            .filter(|t| t.state == ThreadState::Dead)
            .and_then(|t| t.data_stack.first().copied());

        // Prune the just-finished thread directly. Without this,
        // dead `Query*` threads accumulate until the next `tick()`
        // both bloats `self.threads` and leaks into the
        // [`take_thread_return_code`] lookup as bogus entries.
        self.threads.retain(|t| t.id != thread_id);

        result
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

        // Snapshot (id, ret_code) of threads dying this tick before
        // we prune, so the host can still correlate previously-
        // spawned threads (e.g. `AimWeapon1`) via
        // [`take_thread_return_code`]. Overwritten each tick.
        self.ended_threads.clear();
        for t in &self.threads {
            if t.state == ThreadState::Dead {
                self.ended_threads.push((t.id, t.ret_code));
            }
        }
        self.threads.retain(|t| t.state != ThreadState::Dead);

        commands
    }

    /// Take the return code of `thread_id` if that thread ended
    /// during the last [`tick`](Self::tick). Drains the entry so
    /// repeated calls return `None`; lets a host correlate a
    /// previously-spawned thread to its final return value even
    /// though the thread itself has been pruned from the live list.
    pub fn take_thread_return_code(&mut self, thread_id: u32) -> Option<i32> {
        let pos = self
            .ended_threads
            .iter()
            .position(|(id, _)| *id == thread_id)?;
        Some(self.ended_threads.swap_remove(pos).1)
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
                (ThreadState::WaitTurn, AnimType::Turn)
                    | (ThreadState::WaitMove, AnimType::Move)
                    | (ThreadState::WaitScale, AnimType::Scale)
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
                    // Stack order matches upstream: top = high, below = low.
                    // `gsRNG.NextInt(high - low + 1) + low`.
                    let b = thread.pop();
                    let a = thread.pop();
                    // xorshift32; seeded once per VM and advances per Rand.
                    let mut s = self.rand_state;
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    self.rand_state = if s == 0 { 0x00C0_BEFE } else { s };
                    // Inclusive range: count = b - a + 1, but guard `b < a`
                    // (upstream Spring's NextInt would assert there; we
                    // collapse to a so the script doesn't loop).
                    let count = (b - a + 1).max(1) as u32;
                    let val = a + (self.rand_state % count) as i32;
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

                    // start-script spawns a fresh thread that listens to
                    // *no* signals until it sets its own mask. If we
                    // inherited the parent's mask, the parent's next
                    // `signal` would kill this thread immediately. Notably
                    // byte's AimWeapon1 sets SIG_AIM, then start-scripts
                    // Close() — if Close() inherited SIG_AIM the next
                    // AimWeapon1 would terminate it before it could turn
                    // the aimer back to rest, leaving the byte stuck
                    // tilted at the previous target.
                    self.pending_spawns.push(SpawnRequest {
                        function_id: func_id,
                        args,
                        signal_mask: 0,
                    });
                }
                Opcode::Signal => {
                    let sig = thread.pop();
                    // Kill all threads whose signal_mask overlaps with sig.
                    // We exclude the current thread to match the surrounding
                    // workaround for inherited signal masks (see the
                    // `Opcode::Start` arm); upstream's `CCobInstance::Signal`
                    // would also kill self, but that interacts badly with
                    // the kernel-panic-specific signal_mask=0 spawn rule.
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

                // Get/Set unit values — three priority layers, in order:
                //   1. LUA0..LUA9   → per-thread `lua_args` slots
                //   2. host-published `unit_values`
                //   3. built-in math handlers (ATAN/HYPOT/POW/...)
                // Anything still unresolved reads as 0; this matches
                // upstream's "missing key returns 0" leniency.
                Opcode::GetUnitValue => {
                    let key = thread.pop();
                    let lua_value = lua_arg_index(key).map(|i| thread.lua_args[i]);
                    let value = match lua_value {
                        Some(v) => v,
                        None => self
                            .lookup_unit_value(key)
                            .or_else(|| builtin_get_value(key, 0, 0, 0, 0))
                            .unwrap_or(0),
                    };
                    self.threads[thread_idx].push(value);
                }
                Opcode::Get => {
                    let p4 = thread.pop();
                    let p3 = thread.pop();
                    let p2 = thread.pop();
                    let p1 = thread.pop();
                    let key = thread.pop();
                    let lua_value = lua_arg_index(key).map(|i| thread.lua_args[i]);
                    let value = match lua_value {
                        Some(v) => v,
                        None => self
                            .lookup_unit_value(key)
                            .or_else(|| builtin_get_value(key, p1, p2, p3, p4))
                            .or_else(|| {
                                // GAME_FRAME counts engine ticks. We
                                // don't have the global game clock, so
                                // map our `current_time` (ms) onto
                                // frames at the engine's 30 fps.
                                (key == crate::unit_values::GAME_FRAME)
                                    .then(|| self.current_time / (1000 / GAME_SPEED_FPS))
                            })
                            .unwrap_or(0),
                    };
                    self.threads[thread_idx].push(value);
                }
                Opcode::Set => {
                    let value = thread.pop();
                    let key = thread.pop();
                    if let Some(idx) = lua_arg_index(key) {
                        // LUA-arg writes stay thread-local; don't bubble.
                        thread.lua_args[idx] = value;
                    } else {
                        commands.push(AnimCommand::SetValue { key, value });
                    }
                }

                // No-ops for visual hints we don't implement.
                Opcode::Shade | Opcode::DontShade | Opcode::Cache | Opcode::DontCache => {
                    let _piece = thread.read_code(&cob.code);
                }

                Opcode::PlaySound => {
                    let volume = thread.pop();
                    let sound_id = thread.read_code(&cob.code);
                    commands.push(AnimCommand::PlaySound { sound_id, volume });
                }

                Opcode::Attach => {
                    // Upstream: `r3 = pop; r2 = pop; r1 = pop;
                    // cobInst->AttachUnit(r2, r1)`. So the topmost arg is
                    // unused (recoil naming: piece-num for the asPiece
                    // slot), the next is the piece, the bottom is the
                    // unit id. We forward all three so the host can
                    // implement the full attach call if it ever wires
                    // transports up.
                    let attach_type = thread.pop();
                    let piece = thread.pop();
                    let unit_id = thread.pop();
                    commands.push(AnimCommand::Attach {
                        unit_id,
                        piece,
                        attach_type,
                    });
                }
                Opcode::Drop => {
                    let unit_id = thread.pop();
                    commands.push(AnimCommand::Drop { unit_id });
                }

                // Recoil scale animations — single-axis (uniform) scaling
                // of a piece. Stack layout matches MOVE: top=destination,
                // below=speed; piece comes from inline operand.
                Opcode::Scale => {
                    let piece = thread.read_code(&cob.code);
                    let dest = thread.pop();
                    let speed = thread.pop();
                    commands.push(AnimCommand::Scale {
                        piece,
                        destination: dest,
                        speed,
                    });
                }
                Opcode::ScaleNow => {
                    let piece = thread.read_code(&cob.code);
                    let dest = thread.pop();
                    commands.push(AnimCommand::ScaleNow {
                        piece,
                        destination: dest,
                    });
                }
                Opcode::WaitScale => {
                    let piece = thread.read_code(&cob.code);
                    thread.wait_piece = piece;
                    thread.wait_axis = -1;
                    thread.state = ThreadState::WaitScale;
                    break;
                }

                // Recoil "deferred Lua call" — same wire format as
                // LuaCall (script id, arg count, then `arg_count`
                // values popped). Without a Lua VM the call is a
                // no-op; consume the args so we don't desync the
                // stack and push a 0 return.
                Opcode::BatchLua => {
                    let _func_id = thread.read_code(&cob.code);
                    let arg_count = thread.read_code(&cob.code) as usize;
                    for _ in 0..arg_count {
                        thread.pop();
                    }
                    thread.push(0);
                }

                // Hitting SIGNATURE_LUA in the dispatch loop means the
                // caller routed a Lua-only script through the bytecode
                // VM by mistake. Upstream logs an error and kills the
                // thread (`CobThread.cpp:314-317`); same here.
                Opcode::SignatureLua => {
                    thread.state = ThreadState::Dead;
                    break;
                }
            }
        }
    }
}

/// Animation type for `anim_finished` callbacks. Mirrors upstream's
/// `CUnitScript::AnimType { ATurn, ASpin, AMove, AScale }` (Spin runs
/// forever and never raises a finished event, so it isn't represented
/// here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimType {
    Turn,
    Move,
    /// Recoil-only scale animation completion.
    Scale,
}

#[cfg(test)]
mod out_param_tests {
    use super::*;
    use crate::cob_file::CobFile;
    use crate::opcodes::Opcode;

    /// Build a minimal CobFile with a single function that models
    /// BOS-style `Query(piecenum) { piecenum = expected; }`:
    ///
    /// - `CreateLocalVar`    — claims local[0] (pops the param slot)
    /// - `PushConstant N`    — pushes the out-value
    /// - `PopLocalVar 0`     — writes local[0] = N
    /// - `PushConstant 0`    — (Scriptor-style implicit return 0)
    /// - `Return`            — pops → ret_code = 0
    fn single_fn_cob(name: &str, piecenum: i32) -> CobFile {
        let code: Vec<i32> = vec![
            Opcode::CreateLocalVar as i32,
            Opcode::PushConstant as i32,
            piecenum,
            Opcode::PopLocalVar as i32,
            0,
            Opcode::PushConstant as i32,
            0,
            Opcode::Return as i32,
        ];
        CobFile::from_test_parts(
            "test",
            vec![name.to_string()],
            vec![0],
            vec![code.len()],
            Vec::new(),
            code,
            0,
            Vec::new(),
        )
    }

    #[test]
    fn out_param_returns_written_local0() {
        let cob = single_fn_cob("QueryWeapon1", 17);
        let mut vm = CobVm::new(&cob);
        assert_eq!(vm.call_script_out_param(&cob, "QueryWeapon1"), Some(17));
    }

    #[test]
    fn out_param_missing_function_returns_none() {
        let cob = single_fn_cob("QueryWeapon1", 17);
        let mut vm = CobVm::new(&cob);
        assert_eq!(vm.call_script_out_param(&cob, "QueryWeapon42"), None);
    }
}
