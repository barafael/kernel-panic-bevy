//! COB bytecode opcode constants.
//!
//! Values from Spring engine's CobThread.cpp. Each opcode is a 32-bit
//! integer. Some opcodes consume additional inline operands from the
//! code stream (via GET_LONG_PC), others pop values from the data stack.

// Model interaction — inline: piece, axis; stack: destination, speed
pub const MOVE: i32 = 0x10001000_u32 as i32;
pub const TURN: i32 = 0x10002000_u32 as i32;
pub const SPIN: i32 = 0x10003000_u32 as i32;
pub const STOP_SPIN: i32 = 0x10004000_u32 as i32;
pub const SHOW: i32 = 0x10005000_u32 as i32;
pub const HIDE: i32 = 0x10006000_u32 as i32;
pub const CACHE: i32 = 0x10007000_u32 as i32;
pub const DONT_CACHE: i32 = 0x10008000_u32 as i32;
pub const MOVE_NOW: i32 = 0x1000B000_u32 as i32;
pub const TURN_NOW: i32 = 0x1000C000_u32 as i32;
pub const SHADE: i32 = 0x1000D000_u32 as i32;
pub const DONT_SHADE: i32 = 0x1000E000_u32 as i32;
pub const EMIT_SFX: i32 = 0x1000F000_u32 as i32;

// Blocking operations
pub const WAIT_TURN: i32 = 0x10011000_u32 as i32;
pub const WAIT_MOVE: i32 = 0x10012000_u32 as i32;
pub const SLEEP: i32 = 0x10013000_u32 as i32;

// Stack manipulation
pub const PUSH_CONSTANT: i32 = 0x10021001_u32 as i32;
pub const PUSH_LOCAL_VAR: i32 = 0x10021002_u32 as i32;
pub const PUSH_STATIC: i32 = 0x10021004_u32 as i32;
pub const CREATE_LOCAL_VAR: i32 = 0x10022000_u32 as i32;
pub const POP_LOCAL_VAR: i32 = 0x10023002_u32 as i32;
pub const POP_STATIC: i32 = 0x10023004_u32 as i32;
pub const POP_STACK: i32 = 0x10024000_u32 as i32;

// Arithmetic
pub const ADD: i32 = 0x10031000_u32 as i32;
pub const SUB: i32 = 0x10032000_u32 as i32;
pub const MUL: i32 = 0x10033000_u32 as i32;
pub const DIV: i32 = 0x10034000_u32 as i32;
pub const MOD: i32 = 0x10034001_u32 as i32;
pub const BITWISE_AND: i32 = 0x10035000_u32 as i32;
pub const BITWISE_OR: i32 = 0x10036000_u32 as i32;
pub const BITWISE_XOR: i32 = 0x10037000_u32 as i32;
pub const BITWISE_NOT: i32 = 0x10038000_u32 as i32;

// Native function calls
pub const RAND: i32 = 0x10041000_u32 as i32;
pub const GET_UNIT_VALUE: i32 = 0x10042000_u32 as i32;
pub const GET: i32 = 0x10043000_u32 as i32;

// Comparison
pub const SET_LESS: i32 = 0x10051000_u32 as i32;
pub const SET_LESS_OR_EQUAL: i32 = 0x10052000_u32 as i32;
pub const SET_GREATER: i32 = 0x10053000_u32 as i32;
pub const SET_GREATER_OR_EQUAL: i32 = 0x10054000_u32 as i32;
pub const SET_EQUAL: i32 = 0x10055000_u32 as i32;
pub const SET_NOT_EQUAL: i32 = 0x10056000_u32 as i32;
pub const LOGICAL_AND: i32 = 0x10057000_u32 as i32;
pub const LOGICAL_OR: i32 = 0x10058000_u32 as i32;
pub const LOGICAL_XOR: i32 = 0x10059000_u32 as i32;
pub const LOGICAL_NOT: i32 = 0x1005A000_u32 as i32;

// Flow control
pub const START: i32 = 0x10061000_u32 as i32;
pub const CALL: i32 = 0x10062000_u32 as i32;
pub const REAL_CALL: i32 = 0x10062001_u32 as i32;
pub const LUA_CALL: i32 = 0x10062002_u32 as i32;
pub const JUMP: i32 = 0x10064000_u32 as i32;
pub const RETURN: i32 = 0x10065000_u32 as i32;
pub const JUMP_NOT_EQUAL: i32 = 0x10066000_u32 as i32;
pub const SIGNAL: i32 = 0x10067000_u32 as i32;
pub const SET_SIGNAL_MASK: i32 = 0x10068000_u32 as i32;

// Piece destruction / effects
pub const EXPLODE: i32 = 0x10071000_u32 as i32;
pub const PLAY_SOUND: i32 = 0x10072000_u32 as i32;

// Special
pub const SET: i32 = 0x10082000_u32 as i32;
pub const ATTACH: i32 = 0x10083000_u32 as i32;
pub const DROP: i32 = 0x10084000_u32 as i32;

/// Axis indices used in MOVE/TURN/SPIN operands.
pub const AXIS_X: i32 = 0;
pub const AXIS_Y: i32 = 1;
pub const AXIS_Z: i32 = 2;

pub fn opcode_name(opcode: i32) -> &'static str {
    match opcode {
        MOVE => "move",
        TURN => "turn",
        SPIN => "spin",
        STOP_SPIN => "stop-spin",
        SHOW => "show",
        HIDE => "hide",
        CACHE => "cache",
        DONT_CACHE => "dont-cache",
        MOVE_NOW => "move-now",
        TURN_NOW => "turn-now",
        SHADE => "shade",
        DONT_SHADE => "dont-shade",
        EMIT_SFX => "emit-sfx",
        WAIT_TURN => "wait-for-turn",
        WAIT_MOVE => "wait-for-move",
        SLEEP => "sleep",
        PUSH_CONSTANT => "push-constant",
        PUSH_LOCAL_VAR => "push-local",
        PUSH_STATIC => "push-static",
        CREATE_LOCAL_VAR => "create-local",
        POP_LOCAL_VAR => "pop-local",
        POP_STATIC => "pop-static",
        POP_STACK => "pop-stack",
        ADD => "add",
        SUB => "sub",
        MUL => "mul",
        DIV => "div",
        MOD => "mod",
        BITWISE_AND => "bit-and",
        BITWISE_OR => "bit-or",
        BITWISE_XOR => "bit-xor",
        BITWISE_NOT => "bit-not",
        RAND => "rand",
        GET_UNIT_VALUE => "get-unit-value",
        GET => "get",
        SET_LESS => "set-less",
        SET_LESS_OR_EQUAL => "set-less-eq",
        SET_GREATER => "set-greater",
        SET_GREATER_OR_EQUAL => "set-greater-eq",
        SET_EQUAL => "set-equal",
        SET_NOT_EQUAL => "set-not-equal",
        LOGICAL_AND => "logical-and",
        LOGICAL_OR => "logical-or",
        LOGICAL_XOR => "logical-xor",
        LOGICAL_NOT => "logical-not",
        START => "start",
        CALL | REAL_CALL => "call",
        LUA_CALL => "lua-call",
        JUMP => "jump",
        RETURN => "return",
        JUMP_NOT_EQUAL => "jne",
        SIGNAL => "signal",
        SET_SIGNAL_MASK => "set-signal-mask",
        EXPLODE => "explode",
        PLAY_SOUND => "play-sound",
        SET => "set",
        ATTACH => "attach",
        DROP => "drop",
        _ => "unknown",
    }
}
