// M6 tier-0 bytecode interpreter: `match`-based dispatch (measured faster
// than a function-pointer table, see benchmarks/RESULTS.md's M6 section),
// plus hot-counter-driven promotion to native code via `tier.rs`'s
// `promote` callback (this file knows nothing about Cranelift/codegen.rs).
//
// Register file is a plain `Vec<u64>`, one untagged slot per value.
//
// Traps go through `trap()` (`process::abort()`), matching Cranelift's
// native trap behavior - a signal kill, not an unwinding Rust panic.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::bytecode::{BcFunction, Op};
use crate::gc;
use crate::runtime;

/// `None` = keep interpreting (not an error). `Some` = wrapper pointer plus
/// `(id, pointer)` for every dependency also compiled along the way.
type PromoteResult = Option<(*const u8, Vec<(u8, *const u8)>)>;

/// Calls a promoted function's uniform-ABI wrapper.
fn call_native(ptr: *const u8, args: &[u64]) -> u64 {
    let f: extern "C" fn(*const u64, i64) -> u64 = unsafe { std::mem::transmute(ptr) };
    f(args.as_ptr(), args.len() as i64)
}

pub type SemanticCallable = dyn Fn(&[u64]) -> sol_core::CallOutcome<u64, String>;

/// A function's current tier: interpreted bytecode or a promoted native wrapper
/// pointer. `Bytecode` holds an `Rc` (not a bare reference) so a recursive call
/// can clone the handle and drop the `RefCell` borrow before executing -
/// otherwise a nested call promoting the same function would panic on the
/// outstanding borrow.
#[derive(Clone)]
pub enum Slot {
    Bytecode(Rc<BcFunction>),
    /// `extern "C" fn(args: *const u64, argc: i64) -> u64` - the uniform ABI
    /// every promoted wrapper compiles to regardless of its real signature.
    Native(*const u8),
    /// A function implemented by another semantic tier (currently the dynamic
    /// Lua interpreter). Arguments and results use the specialized slot
    /// representation at this boundary; the adapter owns checked
    /// boxing/unboxing into canonical dynamic values.
    Semantic(Rc<SemanticCallable>),
}

#[cold]
fn trap() -> ! {
    std::process::abort();
}

/// M8: zero-cost-when-disabled instrumentation point for `Runtime::call`.
/// The default `()` impl's empty bodies inline away entirely, so `sol
/// run` (no `--profile-time`/`debug`) pays nothing for this existing - see
/// `profile.rs`/`debug.rs` for the real implementations, and
/// `benchmarks/RESULTS.md`'s M8 section for the before/after proof.
pub trait Hooks {
    fn on_call_enter(&self, _func_id: u8, _args: &[u64]) {}
    fn on_call_exit(&self, _func_id: u8, _result: u64) {}
    fn on_frame_state(&self, _frame: &sol_core::FrameHeader) {}
    fn on_tail_call(&self, _from: u8, _to: u8, _args: &[u64]) {}
}

impl Hooks for () {}

/// Speculative-specialization state for one function - see
/// `jit::speculative_candidate`. `ptr` starts `None` and is set once,
/// when `hits` first reaches the threshold.
struct Speculative {
    param_index: usize,
    tag: i64,
    hits: Cell<u32>,
    ptr: Cell<Option<*const u8>>,
}

/// Reads a boxed `any` value's `{tag, payload}` pair (see `value.rs`).
fn unbox_raw(boxed: u64) -> (i64, u64) {
    let ptr = boxed as *const i64;
    unsafe { (*ptr, *(ptr.add(1)) as u64) }
}

/// `Runtime::new`'s speculative-specialization inputs, bundled to avoid
/// clippy's `too_many_arguments`.
pub struct SpeculativeConfig<'a> {
    pub candidates: HashMap<u8, (usize, i64)>,
    pub threshold: u32,
    pub promote: Box<dyn Fn(u8) -> PromoteResult + 'a>,
}

/// Owns every function's current tier and call counters, and drives
/// promotion, shared by the top-level caller and nested `Op::Call`s so a hot
/// function promotes no matter how deep it's called from. Generic over
/// `H: Hooks` (default `()`, the no-op) so `sol run`'s normal path never
/// carries debug/profile instrumentation in its own compiled code.
pub struct Runtime<'a, H: Hooks = ()> {
    hooks: H,
    slots: Vec<RefCell<Slot>>,
    functions: RefCell<sol_core::FunctionRegistry>,
    counts: Vec<Cell<u32>>,
    threshold: u32,
    /// Compiles `func_id` to native, returning its wrapper pointer plus any
    /// dependency functions also compiled. `None` = keep interpreting.
    promote: Box<dyn Fn(u8) -> PromoteResult + 'a>,
    /// Per-`(func_id, stmt_index)` loop-backedge counts, separate from
    /// `counts`: a function called once (e.g. `main`) can still spend its
    /// whole runtime in one hot loop, which needs OSR to ever promote.
    osr_counts: RefCell<HashMap<(u8, usize), u32>>,
    osr_threshold: u32,
    /// Compiles an OSR entry for `(func_id, stmt_index)`; result is used once,
    /// immediately, and never written back into `slots`.
    osr_promote: Box<dyn Fn(u8, usize) -> PromoteResult + 'a>,
    /// Sparse: only functions with a speculative candidate have an entry.
    speculative: HashMap<u8, Speculative>,
    speculative_threshold: u32,
    /// Compiles `func_id`'s speculative variant; a side entry point, never
    /// written into `slots`.
    speculative_promote: Box<dyn Fn(u8) -> PromoteResult + 'a>,
}

impl<'a, H: Hooks> Runtime<'a, H> {
    pub fn new(
        slots: Vec<Slot>,
        threshold: u32,
        promote: impl Fn(u8) -> PromoteResult + 'a,
        osr_threshold: u32,
        osr_promote: impl Fn(u8, usize) -> PromoteResult + 'a,
        speculative: SpeculativeConfig<'a>,
        hooks: H,
    ) -> Self {
        let mut functions = sol_core::FunctionRegistry::default();
        for (index, slot) in slots.iter().enumerate() {
            let (metadata, tier) = match slot {
                Slot::Bytecode(function) => (
                    function.metadata.clone(),
                    sol_core::ExecutionTier::Specialized,
                ),
                Slot::Native(_) => (
                    sol_core::PrototypeMetadata::new(format!("native#{index}"), 0, false, 0)
                        .expect("placeholder native metadata fits u32"),
                    sol_core::ExecutionTier::Native,
                ),
                Slot::Semantic(_) => (
                    sol_core::PrototypeMetadata::new(format!("semantic#{index}"), 0, true, 0)
                        .expect("placeholder semantic metadata fits u32"),
                    sol_core::ExecutionTier::SemanticAdapter,
                ),
            };
            functions
                .insert(sol_core::FunctionId::new(index as u32), metadata, tier)
                .expect("slot indices are unique function IDs");
        }
        let counts = vec![Cell::new(0); slots.len()];
        let speculative_threshold = speculative.threshold;
        let speculative_promote = speculative.promote;
        let speculative = speculative
            .candidates
            .into_iter()
            .map(|(id, (param_index, tag))| {
                (
                    id,
                    Speculative {
                        param_index,
                        tag,
                        hits: Cell::new(0),
                        ptr: Cell::new(None),
                    },
                )
            })
            .collect();
        Runtime {
            hooks,
            slots: slots.into_iter().map(RefCell::new).collect(),
            functions: RefCell::new(functions),
            counts,
            threshold,
            promote: Box::new(promote),
            osr_counts: RefCell::new(HashMap::new()),
            osr_threshold,
            osr_promote: Box::new(osr_promote),
            speculative,
            speculative_threshold,
            speculative_promote,
        }
    }

    /// M7 §23 PGO: function ids currently running as native code - a prior
    /// run's "what got hot" becomes a later run's "promote this
    /// immediately, skip warm-up" (see `tier::Engine::dump_profile`).
    pub fn native_function_ids(&self) -> Vec<u8> {
        (0..self.slots.len() as u8)
            .filter(|&id| matches!(*self.slots[id as usize].borrow(), Slot::Native(_)))
            .collect()
    }

    /// Function ids whose speculative candidate actually got specialized
    /// during this run.
    pub fn specialized_speculative_ids(&self) -> Vec<u8> {
        self.speculative
            .iter()
            .filter(|(_, s)| s.ptr.get().is_some())
            .map(|(&id, _)| id)
            .collect()
    }

    /// Preloads `func_id`'s speculative variant from a prior run's
    /// profile, skipping the hit-count warm-up entirely.
    pub fn preload_speculative(&self, func_id: u8, ptr: *const u8) {
        if let Some(s) = self.speculative.get(&func_id) {
            s.ptr.set(Some(ptr));
        }
    }

    /// Preloads `func_id`'s slot as already-native (a dependency compiled
    /// alongside a preloaded promotion/specialization).
    pub fn preload_native(&self, func_id: u8, ptr: *const u8) {
        *self.slots[func_id as usize].borrow_mut() = Slot::Native(ptr);
        self.mark_native(func_id);
    }

    pub fn function_descriptor(&self, func_id: u8) -> Option<sol_core::FunctionDescriptor> {
        self.functions
            .borrow()
            .get(sol_core::FunctionId::new(func_id as u32))
            .cloned()
    }

    fn mark_native(&self, func_id: u8) {
        self.functions
            .borrow_mut()
            .set_tier(
                sol_core::FunctionId::new(func_id as u32),
                sol_core::ExecutionTier::Native,
            )
            .expect("every runtime slot has a function descriptor");
    }

    pub fn call(&self, func_id: u8, args: &[u64]) -> u64 {
        match self.call_outcome(func_id, args) {
            sol_core::CallOutcome::Returned(values) => values.into_iter().next().unwrap_or(0),
            sol_core::CallOutcome::Yielded(_) => {
                panic!("attempt to yield across a specialized host-call boundary")
            }
            sol_core::CallOutcome::Raised(error) => panic!("{error}"),
            sol_core::CallOutcome::TailCall(_) => {
                unreachable!("the dispatcher consumes tail-call outcomes")
            }
        }
    }

    pub fn call_outcome(&self, func_id: u8, args: &[u64]) -> sol_core::CallOutcome<u64, String> {
        self.hooks.on_call_enter(func_id, args);
        let result = self.dispatch(func_id, args);
        if let sol_core::CallOutcome::Returned(values) = &result {
            self.hooks
                .on_call_exit(func_id, values.first().copied().unwrap_or(0));
        }
        result
    }

    /// Accessor for the hooks instance - `debug.rs`'s REPL and
    /// `profile.rs`'s report both need to read back what they recorded
    /// after the run finishes (or, for the debugger, are the same object
    /// driving the pause/step loop as it happens).
    pub fn hooks(&self) -> &H {
        &self.hooks
    }

    fn dispatch(&self, func_id: u8, args: &[u64]) -> sol_core::CallOutcome<u64, String> {
        let mut current_function = func_id;
        let mut tail_arguments: Option<Vec<u64>> = None;
        loop {
            let current_args = tail_arguments.as_deref().unwrap_or(args);
            let _tail_roots = tail_arguments
                .as_ref()
                .map(|values| crate::gc::RootGuard::new(values.as_ptr(), values.len()));
            if let Some(result) = self.try_speculative(current_function, current_args) {
                return sol_core::CallOutcome::Returned(vec![result]);
            }
            let n = self.counts[current_function as usize].get().wrapping_add(1);
            self.counts[current_function as usize].set(n);
            if n == self.threshold {
                if let Some((ptr, others)) = (self.promote)(current_function) {
                    *self.slots[current_function as usize].borrow_mut() = Slot::Native(ptr);
                    self.mark_native(current_function);
                    for (other_id, other_ptr) in others {
                        *self.slots[other_id as usize].borrow_mut() = Slot::Native(other_ptr);
                        self.mark_native(other_id);
                    }
                }
            }
            // Borrow released immediately after cloning - see `Slot`'s doc comment.
            let slot = self.slots[current_function as usize].borrow().clone();
            let outcome = match slot {
                Slot::Native(ptr) => {
                    sol_core::CallOutcome::Returned(vec![call_native(ptr, current_args)])
                }
                Slot::Bytecode(bf) => self.interpret(current_function, &bf, current_args),
                Slot::Semantic(callable) => callable(current_args),
            };
            match outcome {
                outcome @ sol_core::CallOutcome::Returned(_) => return outcome,
                sol_core::CallOutcome::TailCall(request) => {
                    let function = u8::try_from(request.function.get())
                        .expect("typed function IDs originate as u8 indices");
                    self.hooks
                        .on_tail_call(current_function, function, &request.arguments);
                    current_function = function;
                    tail_arguments = Some(request.arguments);
                }
                outcome @ sol_core::CallOutcome::Yielded(_)
                | outcome @ sol_core::CallOutcome::Raised(_) => return outcome,
            }
        }
    }

    /// The inline-cache guard: `Some(result)` only if `func_id` has a
    /// candidate param and the argument's tag matches. Skipping this check
    /// would let a mismatched boxed value be reinterpreted as a raw scalar
    /// instead of trapping. `None` = no candidate, not hot yet, or a tag
    /// mismatch (falls through to the general path, which traps correctly).
    fn try_speculative(&self, func_id: u8, args: &[u64]) -> Option<u64> {
        let spec = self.speculative.get(&func_id)?;
        let (tag, payload) = unbox_raw(args[spec.param_index]);
        if tag != spec.tag {
            return None;
        }
        if let Some(ptr) = spec.ptr.get() {
            let mut native_args = args.to_vec();
            native_args[spec.param_index] = payload;
            return Some(call_native(ptr, &native_args));
        }
        let n = spec.hits.get().wrapping_add(1);
        spec.hits.set(n);
        if n == self.speculative_threshold {
            if let Some((ptr, others)) = (self.speculative_promote)(func_id) {
                spec.ptr.set(Some(ptr));
                for (other_id, other_ptr) in others {
                    *self.slots[other_id as usize].borrow_mut() = Slot::Native(other_ptr);
                    self.mark_native(other_id);
                }
            }
        }
        None
    }

    /// Counts one loop-backedge; once `osr_threshold` is crossed, compiles
    /// and immediately runs an OSR entry, replacing the rest of this call
    /// with native code. `locals` is the named-locals prefix of the
    /// register file (`bf.local_count`, not the full register count).
    fn on_loop_backedge(&self, func_id: u8, stmt_index: usize, locals: &[u64]) -> Option<u64> {
        let key = (func_id, stmt_index);
        let n = {
            let mut counts = self.osr_counts.borrow_mut();
            let n = counts.entry(key).or_insert(0);
            *n = n.wrapping_add(1);
            *n
        };
        if n != self.osr_threshold {
            return None;
        }
        let (ptr, others) = (self.osr_promote)(func_id, stmt_index)?;
        for (other_id, other_ptr) in others {
            *self.slots[other_id as usize].borrow_mut() = Slot::Native(other_ptr);
            self.mark_native(other_id);
        }
        Some(call_native(ptr, locals))
    }

    fn interpret(
        &self,
        func_id: u8,
        bf: &BcFunction,
        args: &[u64],
    ) -> sol_core::CallOutcome<u64, String> {
        debug_assert_eq!(
            args.len(),
            bf.metadata.arity.parameters as usize,
            "Op::Call's argc must match the callee's own declared parameter count"
        );
        let mut regs = vec![0u64; bf.metadata.registers as usize];
        regs[..args.len()].copy_from_slice(args);
        // Register file lives on the heap, invisible to gc.rs's native-stack
        // scan - must register it as a GC root explicitly.
        let _roots = crate::gc::RootGuard::new(regs.as_ptr(), regs.len());
        let mut pc: usize = 0;
        let mut frame = sol_core::FrameHeader::new(
            sol_core::FunctionId::new(func_id as u32),
            0,
            regs.len() as u32,
        );
        frame.state = sol_core::FrameState::Running;
        self.hooks.on_frame_state(&frame);

        loop {
            frame.pc = pc as u32;
            let instr = bf.code[pc];
            pc += 1;
            let a = instr.a() as usize;
            let b = instr.b() as usize;
            let c = instr.c() as usize;

            macro_rules! ri {
                ($r:expr) => {
                    regs[$r] as i64
                };
            }
            macro_rules! rf {
                ($r:expr) => {
                    f64::from_bits(regs[$r])
                };
            }
            macro_rules! seti {
                ($v:expr) => {
                    regs[a] = ($v) as u64
                };
            }
            macro_rules! setf {
                ($v:expr) => {
                    regs[a] = ($v).to_bits()
                };
            }
            macro_rules! setb {
                ($v:expr) => {
                    regs[a] = ($v) as u64
                };
            }

            match instr.op() {
                Op::DynamicBinary | Op::DynamicCompare => {
                    let op = bf.code[pc].0 as i64;
                    pc += 1;
                    regs[a] = unsafe {
                        if instr.op() == Op::DynamicBinary {
                            crate::dynamic::sol_dynamic_binary(
                                regs[b] as *const u64,
                                regs[c] as *const u64,
                                op,
                            ) as u64
                        } else {
                            crate::dynamic::sol_dynamic_compare(
                                regs[b] as *const u64,
                                regs[c] as *const u64,
                                op,
                            )
                        }
                    };
                }
                Op::DynamicNeg => {
                    regs[a] =
                        unsafe { crate::dynamic::sol_dynamic_neg(regs[b] as *const u64) as u64 }
                }
                Op::DynamicTruth => {
                    regs[a] = unsafe { crate::dynamic::sol_truth(regs[b] as *const u64) }
                }
                Op::StringOrder => {
                    regs[a] = unsafe {
                        crate::strings::sol_string_compare(
                            regs[b] as *const u8,
                            regs[c] as *const u8,
                        ) as u64
                    }
                }
                Op::TrapIfZero => {
                    if regs[a] == 0 {
                        trap();
                    }
                }
                Op::AddNoOverflow => {
                    let (next, overflow) = ri!(b).overflowing_add(ri!(c));
                    regs[a] = next as u64;
                    regs[a + 1] = (!overflow) as u64;
                }
                Op::LoadK => regs[a] = bf.consts[instr.bx() as usize],
                Op::LoadBool => regs[a] = b as u64,
                Op::Move => regs[a] = regs[b],
                Op::NegI => seti!(ri!(b).wrapping_neg()),
                Op::NegF => setf!(-rf!(b)),
                Op::Not => setb!(regs[b] == 0),
                Op::IntToFloat => setf!(ri!(b) as f64),
                Op::AddI => seti!(ri!(b).wrapping_add(ri!(c))),
                Op::SubI => seti!(ri!(b).wrapping_sub(ri!(c))),
                Op::MulI => seti!(ri!(b).wrapping_mul(ri!(c))),
                Op::DivI => {
                    if ri!(c) == 0 {
                        trap();
                    }
                    seti!(crate::numeric::floor_div(ri!(b), ri!(c)))
                }
                Op::ModI => {
                    if ri!(c) == 0 {
                        trap();
                    }
                    seti!(crate::numeric::modulo(ri!(b), ri!(c)))
                }
                Op::FloorDivF => setf!((rf!(b) / rf!(c)).floor()),
                Op::ModF => setf!(crate::numeric::modulo_float(rf!(b), rf!(c))),
                Op::PowF => setf!(rf!(b).powf(rf!(c))),
                Op::BandI => seti!(ri!(b) & ri!(c)),
                Op::BorI => seti!(ri!(b) | ri!(c)),
                Op::BxorI => seti!(ri!(b) ^ ri!(c)),
                Op::ShlI => seti!(crate::numeric::shift(ri!(b), ri!(c), true)),
                Op::ShrI => seti!(crate::numeric::shift(ri!(b), ri!(c), false)),
                Op::AddF => setf!(rf!(b) + rf!(c)),
                Op::SubF => setf!(rf!(b) - rf!(c)),
                Op::MulF => setf!(rf!(b) * rf!(c)),
                Op::DivF => setf!(rf!(b) / rf!(c)), // IEEE754 inf/NaN on zero, matching Cranelift's fdiv - no trap
                Op::EqI => setb!(ri!(b) == ri!(c)),
                Op::NeI => setb!(ri!(b) != ri!(c)),
                Op::LtI => setb!(ri!(b) < ri!(c)),
                Op::LeI => setb!(ri!(b) <= ri!(c)),
                Op::GtI => setb!(ri!(b) > ri!(c)),
                Op::GeI => setb!(ri!(b) >= ri!(c)),
                Op::EqF => setb!(rf!(b) == rf!(c)),
                Op::NeF => setb!(rf!(b) != rf!(c)),
                Op::LtF => setb!(rf!(b) < rf!(c)),
                Op::LeF => setb!(rf!(b) <= rf!(c)),
                Op::GtF => setb!(rf!(b) > rf!(c)),
                Op::GeF => setb!(rf!(b) >= rf!(c)),
                Op::EqB => setb!(regs[b] == regs[c]),
                Op::NeB => setb!(regs[b] != regs[c]),
                Op::And => regs[a] = regs[b] & regs[c],
                Op::Or => regs[a] = regs[b] | regs[c],
                Op::Jump => {
                    let target = (pc as i64 + instr.sbx() as i64) as usize;
                    // Linear scan: `top_level_loops` is typically tiny.
                    if let Some(&(_, stmt_index)) = bf
                        .top_level_loops
                        .iter()
                        .find(|&&(header, _)| header == target)
                    {
                        if let Some(result) =
                            self.on_loop_backedge(func_id, stmt_index, &regs[..bf.local_count])
                        {
                            frame.state = sol_core::FrameState::Returned;
                            self.hooks.on_frame_state(&frame);
                            return sol_core::CallOutcome::Returned(vec![result]);
                        }
                    }
                    pc = target;
                }
                Op::JumpIfFalse => {
                    if regs[a] == 0 {
                        pc = (pc as i64 + instr.sbx() as i64) as usize;
                    }
                }
                Op::Len => {
                    let len = unsafe { *(regs[b] as *const i64) };
                    seti!(len)
                }
                Op::NewArrayI64 => regs[a] = runtime::sol_new_array_i64(ri!(b)) as u64,
                Op::NewArrayF64 => regs[a] = runtime::sol_new_array_f64(ri!(b)) as u64,
                Op::NewArrayPtr => regs[a] = runtime::sol_new_array_ptr(ri!(b)) as u64,
                Op::ArrayMapI64 => {
                    let input = regs[b] as *const runtime::ArrayHeader;
                    let output = runtime::sol_new_array_i64(unsafe { (*input).len });
                    for index in 0..unsafe { (*input).len as usize } {
                        let value = unsafe { *((*input).data.add(index * 8) as *const u64) };
                        let mapped = self.call(regs[c] as u8, &[value]);
                        unsafe { *((*output).data.add(index * 8) as *mut u64) = mapped };
                    }
                    regs[a] = output as u64;
                }
                Op::NewMapI64 => regs[a] = runtime::sol_new_map_i64() as u64,
                Op::MapGetI64 => {
                    regs[a] = unsafe { runtime::sol_map_get_i64(regs[b] as *mut _, ri!(c)) as u64 }
                }
                Op::MapSetI64 => unsafe {
                    runtime::sol_map_set_i64(regs[a] as *mut _, ri!(b), ri!(c));
                },
                Op::MapNextI64 => {
                    regs[a] = unsafe { runtime::sol_map_next_i64(regs[b] as *mut _, ri!(c)) as u64 }
                }
                Op::MapKeyI64 => {
                    regs[a] =
                        unsafe { runtime::sol_map_key_at_i64(regs[b] as *mut _, ri!(c)) as u64 }
                }
                Op::MapValueI64 => {
                    regs[a] =
                        unsafe { runtime::sol_map_value_at_i64(regs[b] as *mut _, ri!(c)) as u64 }
                }
                Op::Index => {
                    let hdr = regs[b] as *const i64;
                    // Unsigned compare also catches a negative index (wraps huge).
                    let (len, data) = unsafe { (*hdr as u64, *(hdr.add(1)) as *const u8) };
                    if ri!(c) as u64 >= len {
                        trap();
                    }
                    regs[a] = unsafe { *(data.add(ri!(c) as usize * 8) as *const u64) };
                }
                Op::SetIndex => {
                    let hdr = regs[a] as *const i64;
                    let (len, data) = unsafe { (*hdr as u64, *(hdr.add(1)) as *mut u8) };
                    if ri!(b) as u64 >= len {
                        trap();
                    }
                    unsafe { *(data.add(ri!(b) as usize * 8) as *mut u64) = regs[c] };
                    gc::sol_gc_write_barrier(data as i64, regs[c] as i64);
                }
                Op::StructAlloc => {
                    let pointer_mask = bf.code[pc].0 as u64 | ((bf.code[pc + 1].0 as u64) << 32);
                    pc += 2;
                    regs[a] = runtime::sol_alloc_layout(instr.bx() as i64, pointer_mask) as u64;
                }
                Op::GetField => regs[a] = unsafe { *((regs[b] as *const u64).add(c)) },
                Op::SetField => {
                    unsafe { *((regs[a] as *mut u64).add(b)) = regs[c] };
                    gc::sol_gc_write_barrier(regs[a] as i64, regs[c] as i64);
                }
                Op::Box => {
                    let expected_tag = bf.code[pc].0 as i64;
                    let pointer_mask = bf.code[pc + 1].0 as u64;
                    pc += 2;
                    let ptr = runtime::sol_alloc_layout(16, pointer_mask) as *mut i64;
                    unsafe {
                        *ptr = expected_tag;
                        *(ptr.add(1)) = regs[b] as i64;
                    }
                    regs[a] = ptr as u64;
                }
                Op::Unbox => {
                    let expected_tag = bf.code[pc].0 as i64;
                    pc += 1;
                    let ptr = regs[b] as *const i64;
                    let tag = unsafe { *ptr };
                    if tag != expected_tag {
                        trap();
                    }
                    regs[a] = unsafe { *(ptr.add(1)) as u64 };
                }
                Op::Call => {
                    let argc = c;
                    match self.call_outcome(b as u8, &regs[a + 1..a + 1 + argc]) {
                        sol_core::CallOutcome::Returned(values) => {
                            regs[a] = values.into_iter().next().unwrap_or(0)
                        }
                        outcome @ sol_core::CallOutcome::Yielded(_)
                        | outcome @ sol_core::CallOutcome::Raised(_) => return outcome,
                        sol_core::CallOutcome::TailCall(_) => {
                            unreachable!("the dispatcher consumes nested tail calls")
                        }
                    }
                }
                Op::LoadFunc => regs[a] = b as u64,
                Op::CallIndirect => {
                    let argc = c;
                    match self.call_outcome(regs[b] as u8, &regs[a + 1..a + 1 + argc]) {
                        sol_core::CallOutcome::Returned(values) => {
                            regs[a] = values.into_iter().next().unwrap_or(0)
                        }
                        outcome @ sol_core::CallOutcome::Yielded(_)
                        | outcome @ sol_core::CallOutcome::Raised(_) => return outcome,
                        sol_core::CallOutcome::TailCall(_) => {
                            unreachable!("the dispatcher consumes nested tail calls")
                        }
                    }
                }
                Op::TailCall => {
                    frame.state = sol_core::FrameState::Returned;
                    self.hooks.on_frame_state(&frame);
                    return sol_core::CallOutcome::TailCall(sol_core::CallRequest::new(
                        sol_core::FunctionId::new(b as u32),
                        regs[a + 1..a + 1 + c].to_vec(),
                        sol_core::ValueCount::ONE,
                        sol_core::CallKind::Tail,
                    ));
                }
                Op::TailCallIndirect => {
                    frame.state = sol_core::FrameState::Returned;
                    self.hooks.on_frame_state(&frame);
                    return sol_core::CallOutcome::TailCall(sol_core::CallRequest::new(
                        sol_core::FunctionId::new(regs[b] as u32),
                        regs[a + 1..a + 1 + c].to_vec(),
                        sol_core::ValueCount::ONE,
                        sol_core::CallKind::Tail,
                    ));
                }
                Op::Return => {
                    frame.state = sol_core::FrameState::Returned;
                    self.hooks.on_frame_state(&frame);
                    return sol_core::CallOutcome::Returned(vec![regs[a]]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytecode(name: &str, registers: usize, code: Vec<crate::bytecode::Instr>) -> BcFunction {
        BcFunction {
            metadata: sol_core::PrototypeMetadata::new(name, 0, false, registers).unwrap(),
            source_map: sol_core::SourceMap::single_line(code.len(), 1),
            source_file: None,
            source_line: 1,
            source_span: crate::diagnostic::SourceSpan::new(0, 0, 1, 1),
            literals: Vec::new(),
            code,
            consts: vec![41],
            local_count: 0,
            top_level_loops: Vec::new(),
        }
    }

    fn dynamic_adapter(
        runtime: Rc<RefCell<crate::lua_runtime::LuaRuntime>>,
        name: &'static str,
    ) -> Slot {
        let parameters = if name == "dynamic_fail" {
            Vec::new()
        } else {
            vec![crate::lua_runtime::BridgeScalar::I64]
        };
        Slot::Semantic(Rc::new(move |arguments| {
            runtime.borrow_mut().call_global_scalar_outcome(
                name,
                &parameters,
                crate::lua_runtime::BridgeScalar::I64,
                arguments,
            )
        }))
    }

    fn yielding_dynamic_adapter(
        runtime: Rc<RefCell<crate::lua_runtime::LuaRuntime>>,
        name: &'static str,
    ) -> Slot {
        let coroutine = Rc::new(RefCell::new(None));
        Slot::Semantic(Rc::new(move |arguments| {
            let mut runtime = runtime.borrow_mut();
            if coroutine.borrow().is_none() {
                let created = match runtime.create_global_coroutine(name) {
                    Ok(created) => created,
                    Err(error) => return sol_core::CallOutcome::Raised(error.to_string()),
                };
                *coroutine.borrow_mut() = Some(created);
            }
            let arguments = arguments
                .iter()
                .map(|value| crate::lua_runtime::LuaValue::Integer(*value as i64))
                .collect();
            let handle = *coroutine.borrow().as_ref().unwrap();
            match runtime.resume_coroutine_outcome(handle, arguments) {
                sol_core::CallOutcome::Returned(values) => {
                    match values
                        .into_iter()
                        .next()
                        .unwrap_or(crate::lua_runtime::LuaValue::Nil)
                    {
                        crate::lua_runtime::LuaValue::Integer(value) => {
                            sol_core::CallOutcome::Returned(vec![value as u64])
                        }
                        value => sol_core::CallOutcome::Raised(format!(
                            "dynamic coroutine returned {}, expected integer",
                            value.type_name()
                        )),
                    }
                }
                sol_core::CallOutcome::Yielded(values) => {
                    let values = values
                        .into_iter()
                        .map(|value| match value {
                            crate::lua_runtime::LuaValue::Integer(value) => Ok(value as u64),
                            value => Err(format!(
                                "dynamic coroutine yielded {}, expected integer",
                                value.type_name()
                            )),
                        })
                        .collect::<Result<Vec<_>, _>>();
                    match values {
                        Ok(values) => sol_core::CallOutcome::Yielded(values),
                        Err(error) => sol_core::CallOutcome::Raised(error),
                    }
                }
                sol_core::CallOutcome::Raised(error) => {
                    sol_core::CallOutcome::Raised(error.to_string())
                }
                sol_core::CallOutcome::TailCall(_) => {
                    unreachable!("the dynamic trampoline consumes tail calls")
                }
            }
        }))
    }

    #[test]
    fn specialized_bytecode_calls_dynamic_lua_through_the_semantic_slot() {
        let source = br#"
            function dynamic_add_one(n)
                local value = { number = n }
                return value.number + 1
            end
            function dynamic_fail()
                error("dynamic boom")
            end
            function dynamic_yielder(value)
                local resumed = coroutine.yield(value)
                return resumed + 1
            end
            function main() return 0 end
        "#;
        let program = crate::parser::parse_lua(crate::lexer::lex_bytes(source).unwrap()).unwrap();
        let mut dynamic = crate::lua_runtime::LuaRuntime::new();
        assert_eq!(
            dynamic.run(&program).unwrap(),
            crate::lua_runtime::LuaValue::Integer(0)
        );
        let dynamic = Rc::new(RefCell::new(dynamic));

        let caller = bytecode(
            "typed_caller",
            2,
            vec![
                crate::bytecode::Instr::iabx(crate::bytecode::Op::LoadK, 1, 0),
                crate::bytecode::Instr::iabc(crate::bytecode::Op::Call, 0, 1, 1),
                crate::bytecode::Instr::iabc(crate::bytecode::Op::Return, 0, 0, 0),
            ],
        );
        let failing_caller = bytecode(
            "typed_failing_caller",
            1,
            vec![
                crate::bytecode::Instr::iabc(crate::bytecode::Op::Call, 0, 2, 0),
                crate::bytecode::Instr::iabc(crate::bytecode::Op::Return, 0, 0, 0),
            ],
        );
        let yielding_tail_caller = bytecode(
            "typed_yielding_tail_caller",
            2,
            vec![
                crate::bytecode::Instr::iabx(crate::bytecode::Op::LoadK, 1, 0),
                crate::bytecode::Instr::iabc(crate::bytecode::Op::TailCall, 0, 5, 1),
            ],
        );
        let runtime = Runtime::new(
            vec![
                Slot::Bytecode(Rc::new(caller)),
                dynamic_adapter(dynamic.clone(), "dynamic_add_one"),
                dynamic_adapter(dynamic.clone(), "dynamic_fail"),
                Slot::Bytecode(Rc::new(failing_caller)),
                Slot::Bytecode(Rc::new(yielding_tail_caller)),
                yielding_dynamic_adapter(dynamic, "dynamic_yielder"),
            ],
            u32::MAX,
            |_| None,
            u32::MAX,
            |_, _| None,
            SpeculativeConfig {
                candidates: HashMap::new(),
                threshold: u32::MAX,
                promote: Box::new(|_| None),
            },
            (),
        );

        assert_eq!(runtime.call(0, &[]), 42);
        assert_eq!(
            runtime.call_outcome(3, &[]),
            sol_core::CallOutcome::Raised(
                "dynamic boom\nstack traceback:\n\tdynamic_fail\n\tline 7".into()
            )
        );
        assert_eq!(
            runtime.call_outcome(4, &[]),
            sol_core::CallOutcome::Yielded(vec![41])
        );
        assert_eq!(
            runtime.call_outcome(5, &[99]),
            sol_core::CallOutcome::Returned(vec![100])
        );
        let descriptor = runtime.function_descriptor(0).unwrap();
        assert_eq!(descriptor.id, sol_core::FunctionId::new(0));
        assert_eq!(descriptor.tier, sol_core::ExecutionTier::Specialized);
    }
}
