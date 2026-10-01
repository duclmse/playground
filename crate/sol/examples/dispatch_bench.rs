// §30: match-dispatch vs. function-pointer-table dispatch, measured on a
// synthetic tight loop (arithmetic + backward jump) rather than assumed.
// Superseded as U7's authoritative dispatch-strategy measurement by
// `dispatch_bench_real.rs`, which runs the same comparison against the real
// `Instr`/`Proto` bytecode the compiler emits for `benchmarks/loop_sum.lua`
// instead of this hand-authored 6-opcode program; kept for its own
// before/after value as the original synthetic baseline.
// Run: cargo run --manifest-path crates/sol/Cargo.toml --release --example dispatch_bench

use std::time::Instant;

#[derive(Clone, Copy)]
#[repr(u8)]
enum Op {
    AddI = 0,
    SubI = 1,
    MulI = 2,
    Jump = 3,
    JumpIfFalse = 4,
    Halt = 5,
}

const OP_TABLE: [Op; 6] = [
    Op::AddI,
    Op::SubI,
    Op::MulI,
    Op::Jump,
    Op::JumpIfFalse,
    Op::Halt,
];

#[derive(Clone, Copy)]
struct Instr {
    op: u8,
    a: u8,
    b: u8,
    c: u8,
    sbx: i32,
}

/// Large enough that dispatch overhead, not setup, dominates.
const ITERS: i64 = 200_000_000;

fn build_program() -> Vec<Instr> {
    vec![
        Instr {
            op: Op::MulI as u8,
            a: 2,
            b: 0,
            c: 0,
            sbx: 0,
        }, // r2 = r0 * r0
        Instr {
            op: Op::AddI as u8,
            a: 1,
            b: 1,
            c: 2,
            sbx: 0,
        }, // r1 += r2
        Instr {
            op: Op::SubI as u8,
            a: 0,
            b: 0,
            c: 3,
            sbx: 0,
        }, // r0 -= r3(=1)
        // r4 = (r0 == 0); jump back to instr 0 (offset -4 from pc=4) while
        // r4 is still false (r0 != 0) - falls through to Halt once r0 hits 0.
        Instr {
            op: Op::JumpIfFalse as u8,
            a: 4,
            b: 0,
            c: 0,
            sbx: -4,
        },
        Instr {
            op: Op::Halt as u8,
            a: 0,
            b: 0,
            c: 0,
            sbx: 0,
        },
    ]
}

fn run_match(code: &[Instr]) -> i64 {
    let mut regs = [ITERS, 0, 0, 1, 0];
    let mut pc = 0usize;
    loop {
        let i = code[pc];
        pc += 1;
        match OP_TABLE[i.op as usize] {
            Op::AddI => regs[i.a as usize] = regs[i.b as usize] + regs[i.c as usize],
            Op::SubI => regs[i.a as usize] = regs[i.b as usize] - regs[i.c as usize],
            Op::MulI => regs[i.a as usize] = regs[i.b as usize] * regs[i.c as usize],
            Op::Jump => pc = (pc as i32 + i.sbx) as usize,
            Op::JumpIfFalse => {
                regs[i.a as usize] = (regs[0] == 0) as i64;
                if regs[i.a as usize] == 0 {
                    pc = (pc as i32 + i.sbx) as usize;
                }
            }
            Op::Halt => return regs[1],
        }
    }
}

type Handler = fn(&mut [i64; 5], Instr, &mut usize) -> bool; // returns true to halt

fn h_addi(regs: &mut [i64; 5], i: Instr, _pc: &mut usize) -> bool {
    regs[i.a as usize] = regs[i.b as usize] + regs[i.c as usize];
    false
}
fn h_subi(regs: &mut [i64; 5], i: Instr, _pc: &mut usize) -> bool {
    regs[i.a as usize] = regs[i.b as usize] - regs[i.c as usize];
    false
}
fn h_muli(regs: &mut [i64; 5], i: Instr, _pc: &mut usize) -> bool {
    regs[i.a as usize] = regs[i.b as usize] * regs[i.c as usize];
    false
}
fn h_jump(_regs: &mut [i64; 5], i: Instr, pc: &mut usize) -> bool {
    *pc = (*pc as i32 + i.sbx) as usize;
    false
}
fn h_jumpiffalse(regs: &mut [i64; 5], i: Instr, pc: &mut usize) -> bool {
    regs[i.a as usize] = (regs[0] == 0) as i64;
    if regs[i.a as usize] == 0 {
        *pc = (*pc as i32 + i.sbx) as usize;
    }
    false
}
fn h_halt(_regs: &mut [i64; 5], _i: Instr, _pc: &mut usize) -> bool {
    true
}

const HANDLERS: [Handler; 6] = [h_addi, h_subi, h_muli, h_jump, h_jumpiffalse, h_halt];

fn run_table(code: &[Instr]) -> i64 {
    let mut regs = [ITERS, 0, 0, 1, 0];
    let mut pc = 0usize;
    loop {
        let i = code[pc];
        pc += 1;
        if HANDLERS[i.op as usize](&mut regs, i, &mut pc) {
            return regs[1];
        }
    }
}

fn main() {
    let code = build_program();

    let t0 = Instant::now();
    let r1 = run_match(&code);
    let match_time = t0.elapsed();

    let t0 = Instant::now();
    let r2 = run_table(&code);
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
