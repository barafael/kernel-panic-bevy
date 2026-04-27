//! COB bytecode opcodes.
//!
//! Values come from the upstream Recoil engine
//! ([`rts/Sim/Units/Scripts/CobOpCodes.h`][CobOpCodes]). Each opcode is a
//! 32-bit integer. Some opcodes consume additional inline operands from the
//! code stream (a la `GET_LONG_PC()`), others pop values from the data
//! stack.
//!
//! [CobOpCodes]: https://github.com/beyond-all-reason/RecoilEngine/blob/master/rts/Sim/Units/Scripts/CobOpCodes.h

/// COB opcode. Discriminants are the raw i32 values found in bytecode.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::FromRepr, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub enum Opcode {
    // Model interaction — inline: piece, axis; stack: destination, speed.
    Move = 0x10001000_u32 as i32,
    Turn = 0x10002000_u32 as i32,
    Spin = 0x10003000_u32 as i32,
    StopSpin = 0x10004000_u32 as i32,
    Show = 0x10005000_u32 as i32,
    Hide = 0x10006000_u32 as i32,
    Cache = 0x10007000_u32 as i32,
    DontCache = 0x10008000_u32 as i32,
    MoveNow = 0x1000B000_u32 as i32,
    TurnNow = 0x1000C000_u32 as i32,
    Shade = 0x1000D000_u32 as i32,
    DontShade = 0x1000E000_u32 as i32,
    EmitSfx = 0x1000F000_u32 as i32,

    // Recoil-only piece scaling. Single-axis (uniform) scale that animates
    // toward `destination` at `speed`. `WAIT_SCALE` blocks the thread until
    // the scale animation finishes, like `WAIT_TURN`/`WAIT_MOVE`.
    Scale = 0x100A0000_u32 as i32,
    ScaleNow = 0x100A1000_u32 as i32,
    WaitScale = 0x100A2000_u32 as i32,

    // Blocking operations.
    WaitTurn = 0x10011000_u32 as i32,
    WaitMove = 0x10012000_u32 as i32,
    Sleep = 0x10013000_u32 as i32,

    // Stack manipulation.
    PushConstant = 0x10021001_u32 as i32,
    PushLocalVar = 0x10021002_u32 as i32,
    PushStatic = 0x10021004_u32 as i32,
    CreateLocalVar = 0x10022000_u32 as i32,
    PopLocalVar = 0x10023002_u32 as i32,
    PopStatic = 0x10023004_u32 as i32,
    PopStack = 0x10024000_u32 as i32,

    // Arithmetic.
    Add = 0x10031000_u32 as i32,
    Sub = 0x10032000_u32 as i32,
    Mul = 0x10033000_u32 as i32,
    Div = 0x10034000_u32 as i32,
    Mod = 0x10034001_u32 as i32,
    BitAnd = 0x10035000_u32 as i32,
    BitOr = 0x10036000_u32 as i32,
    BitXor = 0x10037000_u32 as i32,
    BitNot = 0x10038000_u32 as i32,

    // Native function calls.
    Rand = 0x10041000_u32 as i32,
    GetUnitValue = 0x10042000_u32 as i32,
    Get = 0x10043000_u32 as i32,

    // Comparison.
    SetLess = 0x10051000_u32 as i32,
    SetLessOrEqual = 0x10052000_u32 as i32,
    SetGreater = 0x10053000_u32 as i32,
    SetGreaterOrEqual = 0x10054000_u32 as i32,
    SetEqual = 0x10055000_u32 as i32,
    SetNotEqual = 0x10056000_u32 as i32,
    LogicalAnd = 0x10057000_u32 as i32,
    LogicalOr = 0x10058000_u32 as i32,
    LogicalXor = 0x10059000_u32 as i32,
    LogicalNot = 0x1005A000_u32 as i32,

    // Flow control.
    Start = 0x10061000_u32 as i32,
    Call = 0x10062000_u32 as i32,
    RealCall = 0x10062001_u32 as i32,
    LuaCall = 0x10062002_u32 as i32,
    /// Recoil-only "deferred Lua call" — same shape as `LuaCall` but the
    /// engine queues the invocation onto the next Lua sync point. We treat
    /// it identically (it is a no-op without a Lua VM); see
    /// [`vm`](crate::vm) for the full discussion.
    BatchLua = 0x10062004_u32 as i32,
    Jump = 0x10064000_u32 as i32,
    Return = 0x10065000_u32 as i32,
    JumpNotEqual = 0x10066000_u32 as i32,
    Signal = 0x10067000_u32 as i32,
    SetSignalMask = 0x10068000_u32 as i32,

    // Piece destruction / effects.
    Explode = 0x10071000_u32 as i32,
    PlaySound = 0x10072000_u32 as i32,

    // Special.
    Set = 0x10082000_u32 as i32,
    Attach = 0x10083000_u32 as i32,
    Drop = 0x10084000_u32 as i32,

    /// Recoil "this script is a Lua reference, not real bytecode" sentinel.
    /// Hitting this opcode in the dispatch loop is a bug — the engine
    /// should have routed the call to Lua instead. Like upstream
    /// (`CobThread.cpp:314-317`) we kill the offending thread.
    SignatureLua = 0x10090000_u32 as i32,
}

#[cfg(test)]
mod tests {
    use super::Opcode;

    /// Every opcode value here matches the upstream `CobOpCodes.h` table.
    /// If the engine adds a new one, this round-trip ensures the discriminant
    /// is bit-for-bit identical (a one-bit drift would silently dispatch
    /// the wrong handler).
    #[test]
    fn opcode_discriminants_round_trip() {
        for raw in [
            0x10001000_u32,
            0x10002000_u32,
            0x10003000_u32,
            0x10004000_u32,
            0x10005000_u32,
            0x10006000_u32,
            0x10007000_u32,
            0x10008000_u32,
            0x1000B000_u32,
            0x1000C000_u32,
            0x1000D000_u32,
            0x1000E000_u32,
            0x1000F000_u32,
            0x100A0000_u32,
            0x100A1000_u32,
            0x100A2000_u32,
            0x10011000_u32,
            0x10012000_u32,
            0x10013000_u32,
            0x10021001_u32,
            0x10021002_u32,
            0x10021004_u32,
            0x10022000_u32,
            0x10023002_u32,
            0x10023004_u32,
            0x10024000_u32,
            0x10031000_u32,
            0x10032000_u32,
            0x10033000_u32,
            0x10034000_u32,
            0x10034001_u32,
            0x10035000_u32,
            0x10036000_u32,
            0x10037000_u32,
            0x10038000_u32,
            0x10041000_u32,
            0x10042000_u32,
            0x10043000_u32,
            0x10051000_u32,
            0x10052000_u32,
            0x10053000_u32,
            0x10054000_u32,
            0x10055000_u32,
            0x10056000_u32,
            0x10057000_u32,
            0x10058000_u32,
            0x10059000_u32,
            0x1005A000_u32,
            0x10061000_u32,
            0x10062000_u32,
            0x10062001_u32,
            0x10062002_u32,
            0x10062004_u32,
            0x10064000_u32,
            0x10065000_u32,
            0x10066000_u32,
            0x10067000_u32,
            0x10068000_u32,
            0x10071000_u32,
            0x10072000_u32,
            0x10082000_u32,
            0x10083000_u32,
            0x10084000_u32,
            0x10090000_u32,
        ] {
            let op = Opcode::from_repr(raw as i32)
                .unwrap_or_else(|| panic!("opcode {raw:#x} missing from Opcode enum"));
            assert_eq!(op as i32, raw as i32, "{op:?} discriminant drifted");
        }
    }
}
