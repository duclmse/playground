// Tier-0 bytecode format: fixed-width 32-bit iABC/iABx/iAsBx instructions,
// Lua-style. Registers are exactly sol's `LocalId`s (no separate allocator) so
// a register file lines up with what OSR hands to freshly-compiled native code.
// Every value is a plain untagged `u64` slot, same convention as types.rs.
//
// 8-bit fields cap functions/locals at 256 each; anything past that skips
// bytecode entirely and runs through the M1-M5 AOT path (bccompile.rs).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    LoadK, // iABx:  R(A) = K(Bx) - constant pool load (i64/f64 bits, or a small bool/i64 that didn't fit a fast-path immediate)
    LoadBool, // iABC:  R(A) = B (0 or 1)
    Move,  // iABC:  R(A) = R(B)
    NegI,  // iABC:  R(A) = -R(B), i64
    NegF,  // iABC:  R(A) = -R(B), f64
    Not,   // iABC:  R(A) = R(B) == 0
    IntToFloat, // iABC: R(A) = (f64)R(B)
    AddI,
    SubI,
    MulI,
    DivI,
    ModI, // iABC: R(A) = R(B) op R(C), i64
    AddF,
    SubF,
    MulF,
    DivF, // iABC: R(A) = R(B) op R(C), f64
    EqI,
    NeI,
    LtI,
    LeI,
    GtI,
    GeI, // iABC: R(A) = R(B) op R(C), i64 -> bool
    EqF,
    NeF,
    LtF,
    LeF,
    GtF,
    GeF, // iABC: R(A) = R(B) op R(C), f64 -> bool
    EqB,
    NeB, // iABC: R(A) = R(B) op R(C), bool -> bool
    And,
    Or,   // iABC:  R(A) = R(B) op R(C), bool (bitwise - not short-circuiting, matching codegen.rs)
    Jump, // iAsBx: pc += sBx (A unused)
    JumpIfFalse, // iAsBx: pc += sBx if R(A) == 0 (A is the test register)
    Len,  // iABC:  R(A) = length of array R(B)
    NewArrayI64,
    NewArrayF64, // iABC: R(A) = new array of length R(B)
    Index, // iABC:  R(A) = array R(B)[R(C)] - bounds-checked, traps like codegen.rs's array_elem_addr
    SetIndex, // iABC:  array R(A)[R(B)] = R(C)
    StructAlloc, // iABx + 2 mask words: R(A) = a fresh `Bx`-byte struct instance
    GetField, // iABC:  R(A) = struct R(B)'s field C
    SetField, // iABC:  struct R(A)'s field B = R(C)
    Box,   // iABC + tag + layout words: R(A) = box(R(B), tag) - M12's full any
    Unbox, // iABC + tag word: R(A) = unbox(R(B), expect tag) - traps on mismatch
    Call, // iABC:  R(A) = call function-table[B](R(A+1) ..= R(A+C)) - see interp.rs's calling convention doc
    FloorDivF,
    ModF,
    PowF,
    BandI,
    BorI,
    BxorI,
    ShlI,
    ShrI,
    TrapIfZero,
    AddNoOverflow,
    DynamicBinary,
    DynamicCompare,
    DynamicNeg,
    DynamicTruth,
    StringOrder,
    Return,           // iABC:  return R(A)
    LoadFunc,         // iABC: R(A) = function-table[B]
    CallIndirect,     // iABC: R(A) = call R(B)(R(A+1) ..= R(A+C))
    NewMapI64,        // iABC: R(A) = new Map<i64, scalar>
    MapGetI64,        // iABC: R(A) = raw scalar bits from map R(B)[R(C)], or zero when absent
    MapSetI64,        // iABC: map R(A)[R(B)] = raw scalar bits R(C)
    MapNextI64,       // iABC: R(A) = next cursor for map R(B), after R(C)
    MapKeyI64,        // iABC: R(A) = key at cursor R(C) in map R(B)
    MapValueI64,      // iABC: R(A) = value at cursor R(C) in map R(B)
    ArrayMapI64,      // iABC: R(A) = map array R(B) through function-table id R(C)
    NewArrayPtr, // iABC: R(A) = new array of length R(B), pointer-bearing element (non-atomic data buffer, traced by the GC)
    TailCall,    // iABC: return function-table[B](R(A+1) ..= R(A+C))
    TailCallIndirect, // iABC: return R(B)(R(A+1) ..= R(A+C))
}

impl Op {
    /// Table lookup, not `transmute` - safe against a malformed instruction word.
    fn from_u8(b: u8) -> Op {
        OPS[b as usize]
    }
}

/// Exhaustive match so a missing variant is a compile error, not a silent gap.
const fn op_from_u8(b: u8) -> Op {
    match b {
        0 => Op::LoadK,
        1 => Op::LoadBool,
        2 => Op::Move,
        3 => Op::NegI,
        4 => Op::NegF,
        5 => Op::Not,
        6 => Op::IntToFloat,
        7 => Op::AddI,
        8 => Op::SubI,
        9 => Op::MulI,
        10 => Op::DivI,
        11 => Op::ModI,
        12 => Op::AddF,
        13 => Op::SubF,
        14 => Op::MulF,
        15 => Op::DivF,
        16 => Op::EqI,
        17 => Op::NeI,
        18 => Op::LtI,
        19 => Op::LeI,
        20 => Op::GtI,
        21 => Op::GeI,
        22 => Op::EqF,
        23 => Op::NeF,
        24 => Op::LtF,
        25 => Op::LeF,
        26 => Op::GtF,
        27 => Op::GeF,
        28 => Op::EqB,
        29 => Op::NeB,
        30 => Op::And,
        31 => Op::Or,
        32 => Op::Jump,
        33 => Op::JumpIfFalse,
        34 => Op::Len,
        35 => Op::NewArrayI64,
        36 => Op::NewArrayF64,
        37 => Op::Index,
        38 => Op::SetIndex,
        39 => Op::StructAlloc,
        40 => Op::GetField,
        41 => Op::SetField,
        42 => Op::Box,
        43 => Op::Unbox,
        44 => Op::Call,
        45 => Op::FloorDivF,
        46 => Op::ModF,
        47 => Op::PowF,
        48 => Op::BandI,
        49 => Op::BorI,
        50 => Op::BxorI,
        51 => Op::ShlI,
        52 => Op::ShrI,
        53 => Op::TrapIfZero,
        54 => Op::AddNoOverflow,
        55 => Op::DynamicBinary,
        56 => Op::DynamicCompare,
        57 => Op::DynamicNeg,
        58 => Op::DynamicTruth,
        59 => Op::StringOrder,
        60 => Op::Return,
        61 => Op::LoadFunc,
        62 => Op::CallIndirect,
        63 => Op::NewMapI64,
        64 => Op::MapGetI64,
        65 => Op::MapSetI64,
        66 => Op::MapNextI64,
        67 => Op::MapKeyI64,
        68 => Op::MapValueI64,
        69 => Op::ArrayMapI64,
        70 => Op::NewArrayPtr,
        71 => Op::TailCall,
        72 => Op::TailCallIndirect,
        _ => panic!("bytecode.rs: corrupt opcode byte"),
    }
}

const OPS: [Op; 256] = {
    let mut table = [Op::Return; 256];
    let mut i = 0;
    while i < 73 {
        table[i] = op_from_u8(i as u8);
        i += 1;
    }
    table
};

/// A 32-bit instruction word; field layout depends on the opcode's format (see `Op` above).
#[derive(Debug, Clone, Copy)]
pub struct Instr(pub u32);

impl Instr {
    pub fn iabc(op: Op, a: u8, b: u8, c: u8) -> Instr {
        Instr(op as u32 | (a as u32) << 8 | (b as u32) << 16 | (c as u32) << 24)
    }

    pub fn iabx(op: Op, a: u8, bx: u16) -> Instr {
        Instr(op as u32 | (a as u32) << 8 | (bx as u32) << 16)
    }

    pub fn iasbx(op: Op, a: u8, sbx: i16) -> Instr {
        Instr(op as u32 | (a as u32) << 8 | ((sbx as u16 as u32) << 16))
    }

    pub fn op(self) -> Op {
        Op::from_u8(self.0 as u8)
    }

    pub fn a(self) -> u8 {
        (self.0 >> 8) as u8
    }

    pub fn b(self) -> u8 {
        (self.0 >> 16) as u8
    }

    pub fn c(self) -> u8 {
        (self.0 >> 24) as u8
    }

    pub fn bx(self) -> u16 {
        (self.0 >> 16) as u16
    }

    pub fn sbx(self) -> i16 {
        (self.0 >> 16) as u16 as i16
    }

    /// Projects either typed call encoding onto the shared semantic ABI.
    /// Direct-call function identity remains in `B`; indirect calls read it
    /// from register `B`, but their register window and value cardinality are
    /// identical at the dispatcher boundary.
    pub fn call_site(self) -> Option<sol_core::CallSite> {
        match self.op() {
            Op::Call | Op::CallIndirect => Some(sol_core::CallSite::new(
                self.a() as u32,
                sol_core::ValueCount::Fixed(self.c() as u32),
                sol_core::ValueCount::ONE,
                sol_core::CallKind::Normal,
            )),
            Op::TailCall | Op::TailCallIndirect => Some(sol_core::CallSite::new(
                self.a() as u32,
                sol_core::ValueCount::Fixed(self.c() as u32),
                sol_core::ValueCount::ONE,
                sol_core::CallKind::Tail,
            )),
            _ => None,
        }
    }
}

/// A compiled function's bytecode - `bccompile.rs`'s output, `interp.rs`'s input.
pub struct BcFunction {
    pub metadata: sol_core::PrototypeMetadata,
    pub source_map: sol_core::SourceMap,
    pub source_file: Option<String>,
    /// Source line of the declaration, retained for existing diagnostics.
    pub source_line: u32,
    /// Declaration range retained for debuggers and future instruction maps.
    pub source_span: crate::diagnostic::SourceSpan,
    /// Owns aligned string literal storage referenced by the constant pool.
    pub literals: Vec<Box<[u64]>>,
    pub code: Vec<Instr>,
    /// Constant pool for values too big for an inline operand.
    pub consts: Vec<u64>,
    /// `0..local_count` are named locals; the rest are temporaries. OSR hands only the named-locals prefix to a fresh native entry.
    pub local_count: usize,
    /// `(header_pc, stmt_index)` per top-level `While`/`NumericFor` (not nested), for OSR's loop-backedge tracking.
    pub top_level_loops: Vec<(usize, usize)>,
}

impl sol_core::ExecutablePrototype for BcFunction {
    fn metadata(&self) -> &sol_core::PrototypeMetadata {
        &self.metadata
    }

    fn source_map(&self) -> &sol_core::SourceMap {
        &self.source_map
    }

    fn instruction_count(&self) -> usize {
        self.code.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_calls_project_to_the_semantic_call_abi() {
        let call = Instr::iabc(Op::Call, 4, 9, 3).call_site().unwrap();
        assert_eq!(call.base, 4);
        assert_eq!(call.arguments, sol_core::ValueCount::Fixed(3));
        assert_eq!(call.results, sol_core::ValueCount::ONE);
        assert_eq!(call.kind, sol_core::CallKind::Normal);
        let tail = Instr::iabc(Op::TailCallIndirect, 7, 2, 4)
            .call_site()
            .unwrap();
        assert_eq!(tail.base, 7);
        assert_eq!(tail.arguments, sol_core::ValueCount::Fixed(4));
        assert_eq!(tail.kind, sol_core::CallKind::Tail);
        assert!(Instr::iabc(Op::AddI, 0, 1, 2).call_site().is_none());
    }
}
