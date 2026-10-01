// U7 item 4: match-dispatch vs. function-pointer-table dispatch, measured
// against real compiled `Instr`/`Proto` bytecode - the actual numeric
// for-loop body the production compiler emits for `benchmarks/loop_sum.lua`
// (`ForPrep`/`Binary(Add)`/`Move`/`ForLoop`, real register indices and a
// real constant pool), not the hand-authored 6-opcode toy program in
// `dispatch_bench.rs`. The register file and handler set below only cover
// the opcodes this one proto actually emits (confirmed via
// `cargo run --example dump_proto -- benchmarks/loop_sum.lua`); this is a
// throwaway dispatch-strategy measurement, not a general interpreter, and
// is not wired into the production `dispatch_step`.
//
// Run: cargo run --manifest-path crate/sol/Cargo.toml --release --example dispatch_bench_real

use std::time::Instant;

use sol::ast::BinaryOp;
use sol::lexer;
use sol::lua_bytecode::{Compiler, Const, Instr};
use sol::parser::{self, SourceMode};

const NUM_REGS: usize = 16;

fn load_real_instrs(path: &str) -> (Vec<Instr>, Vec<Const>) {
    let source = std::fs::read(path).expect("read benchmark source");
    let tokens = lexer::lex_bytes(&source).expect("lex");
    let program = parser::parse_with_mode(tokens, SourceMode::Lua).expect("parse");
    let function = program
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main function");
    let proto = Compiler::compile_top_level(function).expect("compile");
    (proto.instrs.clone(), proto.consts.clone())
}

fn const_as_i64(consts: &[Const], k: u32) -> i64 {
    match &consts[k as usize] {
        Const::Integer(n) => *n,
        Const::Float(f) => *f as i64,
        _ => 0,
    }
}

/// Mirrors `dispatch.rs::dispatch_step`'s `match &proto.instrs[pc] { ... }`
/// shape directly: one `match` over a reference to the real `Instr`, each
/// arm inlined into the same loop.
fn run_match(instrs: &[Instr], consts: &[Const]) -> i64 {
    let mut regs = [0i64; NUM_REGS];
    let mut pc = 0usize;
    loop {
        match &instrs[pc] {
            Instr::DetachCell(_) => {}
            Instr::LoadNil(dst) => regs[*dst as usize] = 0,
            Instr::LoadConst(dst, k) => regs[*dst as usize] = const_as_i64(consts, *k),
            Instr::Move(dst, src) | Instr::NewLocal(dst, src, _) => {
                regs[*dst as usize] = regs[*src as usize]
            }
            Instr::Binary(BinaryOp::Add, dst, a, b) => {
                regs[*dst as usize] = regs[*a as usize] + regs[*b as usize]
            }
            Instr::ForPrep(base, delta) => {
                let base = *base as usize;
                let (start, stop, step) = (regs[base], regs[base + 1], regs[base + 2]);
                regs[base + 3] = start;
                let runs_at_least_once = if step > 0 { start <= stop } else { start >= stop };
                if !runs_at_least_once {
                    pc = (pc as i32 + delta) as usize;
                    continue;
                }
            }
            Instr::ForLoop(base, delta) => {
                let base = *base as usize;
                let step = regs[base + 2];
                let var = regs[base] + step;
                let stop = regs[base + 1];
                let continues = if step > 0 { var <= stop } else { var >= stop };
                if continues {
                    regs[base] = var;
                    regs[base + 3] = var;
                    pc = (pc as i32 + delta) as usize;
                    continue;
                }
            }
            Instr::GetGlobal(dst, _) => regs[*dst as usize] = 0,
            Instr::Call(..) => return regs[0],
            other => panic!("unhandled instr in dispatch benchmark: {other:?}"),
        }
        pc += 1;
    }
}

/// `true` = halt (dispatched instruction was `Call`).
type Handler = fn(&mut [i64; NUM_REGS], &Instr, &[Const], &mut usize) -> bool;

fn h_detach_cell(_r: &mut [i64; NUM_REGS], _i: &Instr, _c: &[Const], _pc: &mut usize) -> bool {
    false
}
fn h_load_nil(regs: &mut [i64; NUM_REGS], i: &Instr, _c: &[Const], _pc: &mut usize) -> bool {
    let Instr::LoadNil(dst) = i else { unreachable!() };
    regs[*dst as usize] = 0;
    false
}
fn h_load_const(regs: &mut [i64; NUM_REGS], i: &Instr, consts: &[Const], _pc: &mut usize) -> bool {
    let Instr::LoadConst(dst, k) = i else { unreachable!() };
    regs[*dst as usize] = const_as_i64(consts, *k);
    false
}
fn h_move(regs: &mut [i64; NUM_REGS], i: &Instr, _c: &[Const], _pc: &mut usize) -> bool {
    let (dst, src) = match i {
        Instr::Move(dst, src) => (*dst, *src),
        Instr::NewLocal(dst, src, _) => (*dst, *src),
        _ => unreachable!(),
    };
    regs[dst as usize] = regs[src as usize];
    false
}
fn h_binary_add(regs: &mut [i64; NUM_REGS], i: &Instr, _c: &[Const], _pc: &mut usize) -> bool {
    let Instr::Binary(BinaryOp::Add, dst, a, b) = i else {
        unreachable!()
    };
    regs[*dst as usize] = regs[*a as usize] + regs[*b as usize];
    false
}
fn h_for_prep(regs: &mut [i64; NUM_REGS], i: &Instr, _c: &[Const], pc: &mut usize) -> bool {
    let Instr::ForPrep(base, delta) = i else {
        unreachable!()
    };
    let base = *base as usize;
    let (start, stop, step) = (regs[base], regs[base + 1], regs[base + 2]);
    regs[base + 3] = start;
    let runs_at_least_once = if step > 0 { start <= stop } else { start >= stop };
    if !runs_at_least_once {
        *pc = (*pc as i32 + delta) as usize;
    }
    false
}
fn h_for_loop(regs: &mut [i64; NUM_REGS], i: &Instr, _c: &[Const], pc: &mut usize) -> bool {
    let Instr::ForLoop(base, delta) = i else {
        unreachable!()
    };
    let base = *base as usize;
    let step = regs[base + 2];
    let var = regs[base] + step;
    let stop = regs[base + 1];
    let continues = if step > 0 { var <= stop } else { var >= stop };
    if continues {
        regs[base] = var;
        regs[base + 3] = var;
        *pc = (*pc as i32 + delta) as usize;
    }
    false
}
fn h_get_global(regs: &mut [i64; NUM_REGS], i: &Instr, _c: &[Const], _pc: &mut usize) -> bool {
    let Instr::GetGlobal(dst, _) = i else {
        unreachable!()
    };
    regs[*dst as usize] = 0;
    false
}
fn h_call(_r: &mut [i64; NUM_REGS], _i: &Instr, _c: &[Const], _pc: &mut usize) -> bool {
    true
}

fn tag(instr: &Instr) -> u8 {
    match instr {
        Instr::DetachCell(_) => 0,
        Instr::LoadNil(_) => 1,
        Instr::LoadConst(..) => 2,
        Instr::Move(..) | Instr::NewLocal(..) => 3,
        Instr::Binary(BinaryOp::Add, ..) => 4,
        Instr::ForPrep(..) => 5,
        Instr::ForLoop(..) => 6,
        Instr::GetGlobal(..) => 7,
        Instr::Call(..) => 8,
        other => panic!("unhandled instr in dispatch benchmark: {other:?}"),
    }
}

const HANDLERS: [Handler; 9] = [
    h_detach_cell,
    h_load_nil,
    h_load_const,
    h_move,
    h_binary_add,
    h_for_prep,
    h_for_loop,
    h_get_global,
    h_call,
];

/// Same real `Instr` stream, dispatched through a precomputed-tag function
/// pointer table instead of a direct `match`. Each handler still has to
/// `if let`/`match` on the `Instr` payload to extract operands (Rust's enum
/// layout gives no cheaper way to do this without redesigning `Instr` as a
/// flat opcode+operand byte encoding) - this is the realistic cost a
/// function-pointer-table prototype would actually pay against the current
/// `Instr` representation, not an idealized direct-threaded dispatch.
fn run_table(instrs: &[Instr], consts: &[Const]) -> i64 {
    let tags: Vec<u8> = instrs.iter().map(tag).collect();
    let mut regs = [0i64; NUM_REGS];
    let mut pc = 0usize;
    loop {
        let before = pc;
        let halt = HANDLERS[tags[pc] as usize](&mut regs, &instrs[pc], consts, &mut pc);
        if halt {
            return regs[0];
        }
        // A handler only writes `pc` itself when it branches (`ForPrep`/
        // `ForLoop`); every other handler leaves it untouched, same as a
        // `match` arm that falls through to the loop's own `pc += 1`
        // instead of an explicit `continue`.
        if pc == before {
            pc += 1;
        }
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "benchmarks/loop_sum.lua".to_string());
    let (mut instrs, consts) = load_real_instrs(&path);
    // Both dispatch loops halt at the first `Call` (the trailing `print`),
    // so nothing after it ever executes; truncate here instead of teaching
    // `tag()`/the handler table about the teardown-only tail
    // (`DetachCell`/`LoadNil`/`Return`) that follows it.
    let call_index = instrs
        .iter()
        .position(|i| matches!(i, Instr::Call(..)))
        .expect("benchmark source must contain a call (e.g. the trailing `print`)");
    instrs.truncate(call_index + 1);
    println!("loaded {} real instructions from {path}", instrs.len());

    let t0 = Instant::now();
    let r1 = run_match(&instrs, &consts);
    let match_time = t0.elapsed();

    let t0 = Instant::now();
    let r2 = run_table(&instrs, &consts);
    let table_time = t0.elapsed();

    assert_eq!(
        r1, r2,
        "both dispatch strategies must compute the same result"
    );
    println!("match dispatch: {match_time:?} (result {r1})");
    println!("table dispatch: {table_time:?} (result {r2})");
    let ratio = match_time.as_secs_f64() / table_time.as_secs_f64();
    println!("match/table ratio: {ratio:.3} (>1.0 means table dispatch was faster)");
}
