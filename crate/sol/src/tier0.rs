// U12 item 2: a `jit`-feature-independent engine for running a typed `.sol`
// program on Tier-0 bytecode forever, with no promotion path. `tier.rs`
// (`Engine`) always constructs a `jit::Jit` even for a single interpreted
// call - that's a hard dependency on `cranelift-jit`, which has no wasm32
// branch (see `crate/sol/Cargo.toml`'s `jit` feature doc comment). This
// module is what makes a wasm32/`--no-default-features` build able to
// actually *run* a `.sol` program, not just compile the crate - `tier.rs`
// itself is `#[cfg(feature = "jit")]`-gated and was never reachable there.
//
// `interp::Runtime::new`'s `promote`/`osr_promote`/`speculative.promote`
// closures all return `Option` - `None` means "keep interpreting," which is
// exactly what every one of them does here, forever.

use std::collections::HashMap;

use crate::bccompile;
use crate::interp::{self, Slot, SpeculativeConfig};
use crate::types::TProgram;

/// Matches `crate/lua-vm`'s documented `MAX_INSTRUCTIONS` convention (see its
/// `AGENTS.md`) so a sandboxed host gets the same runaway-script cap
/// regardless of which tier/language a project uses. Pass a different value
/// to `Engine::new_with_budget` to override.
pub const DEFAULT_INSTRUCTION_BUDGET: u64 = 10_000_000;

pub struct Engine<H: interp::Hooks = ()> {
    runtime: std::rc::Rc<interp::Runtime<'static, H>>,
    name_to_id: HashMap<String, u8>,
}

/// Adapts the uniform `extern "C" fn(*const u64, i64) -> u64` slot ABI
/// (`interp::call_native`'s calling convention, shared by every
/// promoted/native slot) to `strings::__sol_string_concat`'s raw two-pointer
/// signature.
unsafe extern "C" fn tier0_string_concat_shim(args: *const u64, argc: i64) -> u64 {
    debug_assert_eq!(argc, 2, "typeck.rs always injects this extern as binary");
    let a = *args as *const u8;
    let b = *args.add(1) as *const u8;
    crate::strings::__sol_string_concat(a, b) as u64
}

/// Same adapter role as above, for `dynamic::__sol_any_is`'s
/// `(*const u64, i64) -> u8` signature.
unsafe extern "C" fn tier0_any_is_shim(args: *const u64, argc: i64) -> u64 {
    debug_assert_eq!(argc, 2, "typeck.rs always injects this extern as binary");
    let p = *args as *const u64;
    let expected_tag = *args.add(1) as i64;
    crate::dynamic::__sol_any_is(p, expected_tag) as u64
}

/// Internal runtime helpers `typeck.rs` injects as externs for string
/// concatenation (`__sol_string_concat`) and `any`-type tests
/// (`__sol_any_is`) - fixed, compiler-known signatures, unlike a user's own
/// `extern function` declaration (e.g. `math.sol`'s `sqrt`/`pow`, which link
/// arbitrary native libm symbols through `jit.rs::compile_wrapper`'s
/// generated ABI-marshaling code and have no Tier-0 equivalent - that's a
/// real architectural limit, not a gap to close here: Tier-0 has no code
/// generator to build a wrapper for an arbitrary foreign signature, and a
/// WASM sandbox host has no native libm to link against regardless). Returns
/// `None` for anything else, which the caller treats as unsupported.
fn known_runtime_extern_shim(name: &str) -> Option<unsafe extern "C" fn(*const u64, i64) -> u64> {
    match name {
        "__sol_string_concat" => Some(tier0_string_concat_shim),
        "__sol_any_is" => Some(tier0_any_is_shim),
        _ => None,
    }
}

impl<H: interp::Hooks> Engine<H> {
    pub fn new(program: TProgram, hooks: H) -> Result<Self, String> {
        Self::new_with_budget(program, hooks, DEFAULT_INSTRUCTION_BUDGET)
    }

    pub fn new_with_budget(
        program: TProgram,
        hooks: H,
        instruction_budget: u64,
    ) -> Result<Self, String> {
        let mut unsupported_externs = Vec::new();
        for extern_fn in &program.externs {
            if known_runtime_extern_shim(&extern_fn.name).is_none() {
                unsupported_externs.push(extern_fn.name.clone());
            }
        }
        if !unsupported_externs.is_empty() {
            return Err(format!(
                "Tier-0-only execution cannot link extern function(s) {}: arbitrary FFI linking is native-compilation-only (see jit.rs::link_externs), unavailable without the 'jit' feature",
                unsupported_externs.join(", ")
            ));
        }

        let name_to_id = bccompile::function_index(&program)?;
        let mut slots = Vec::new();
        for f in &program.functions {
            match bccompile::compile_function(f, &name_to_id) {
                Some(bf) => slots.push(Slot::Bytecode(std::rc::Rc::new(bf))),
                None => {
                    return Err(format!(
                        "function '{}' exceeds Tier-0's 8-bit register budget and requires native compilation, which is unavailable without the 'jit' feature",
                        f.name
                    ));
                }
            }
        }
        for extern_fn in &program.externs {
            let shim = known_runtime_extern_shim(&extern_fn.name)
                .expect("already validated above - every extern here has a known shim");
            slots.push(Slot::Native(shim as *const u8));
        }

        let speculative = SpeculativeConfig {
            candidates: HashMap::new(),
            threshold: u32::MAX,
            promote: Box::new(|_: u8| None),
        };
        let runtime = interp::Runtime::new(
            slots,
            u32::MAX,
            |_: u8| None,
            u32::MAX,
            |_: u8, _: usize| None,
            speculative,
            hooks,
            instruction_budget,
        );

        Ok(Engine {
            runtime: std::rc::Rc::new(runtime),
            name_to_id,
        })
    }

    pub fn call(&self, name: &str, args: &[u64]) -> u64 {
        self.runtime.call(self.name_to_id[name], args)
    }

    pub fn call_outcome(&self, name: &str, args: &[u64]) -> sol_core::CallOutcome<u64, String> {
        self.runtime.call_outcome(self.name_to_id[name], args)
    }

    pub fn hooks(&self) -> &H {
        self.runtime.hooks()
    }

    /// A live, bounded Tier-0 execution over the same bytecode and opcode
    /// semantics as ordinary calls. The execution owns its suspended roots.
    pub fn start_live(&self, name: &str, args: &[u64]) -> Result<interp::live::Execution<H>, String>
    where
        H: 'static,
    {
        let id = self
            .function_id(name)
            .ok_or_else(|| format!("unknown function '{name}'"))?;
        interp::live::Execution::new(self.runtime.clone(), id, args)
    }

    /// U12 item 3: `name`'s numeric function id, as assigned by
    /// `bccompile::function_index` - `debugger.rs` needs this to translate a
    /// breakpoint/trace lookup keyed by function name into the `func_id` the
    /// interpreter and its hooks actually use.
    pub fn function_id(&self, name: &str) -> Option<u8> {
        self.name_to_id.get(name).copied()
    }

    /// U12 item 3: `name`'s compiled bytecode (source map included), if it's
    /// still interpreted. See `interp::Runtime::bytecode_function`'s doc
    /// comment for why the debugger reads this instead of recompiling.
    pub fn function_bytecode(
        &self,
        name: &str,
    ) -> Option<std::rc::Rc<crate::bytecode::BcFunction>> {
        self.name_to_id
            .get(name)
            .and_then(|&id| self.runtime.bytecode_function(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(source: &str) -> TProgram {
        let (program, _return_type) = crate::compile(source).expect("fixture compiles");
        program
    }

    #[test]
    fn runs_a_simple_program_on_bytecode_alone() {
        let program = compile("function main(): i64 return 1 + 2 end");
        let engine: Engine = Engine::new(program, ()).expect("tier0 engine builds");
        assert_eq!(engine.call("main", &[]), 3);
    }

    #[test]
    fn enforces_its_instruction_budget() {
        let program = compile(
            "function main(): i64
                 local total: i64 = 0
                 local i: i64 = 0
                 while i < 100000000 do
                     total = total + i
                     i = i + 1
                 end
                 return total
             end",
        );
        let engine: Engine =
            Engine::new_with_budget(program, (), 1000).expect("tier0 engine builds");
        match engine.call_outcome("main", &[]) {
            sol_core::CallOutcome::Raised(message) => {
                assert_eq!(message, "instruction budget exceeded");
            }
            other => panic!("expected the instruction budget to trip, got {other:?}"),
        }
    }

    #[test]
    fn rejects_programs_with_externs() {
        let source = "extern fn host_fn(x: i64): i64
             function main(): i64 return host_fn(1) end";
        let program = compile(source);
        let error = match Engine::<()>::new(program, ()) {
            Err(error) => error,
            Ok(_) => panic!("externs need native linking and should be rejected"),
        };
        assert!(
            error.contains("host_fn"),
            "error should name the extern: {error}"
        );
    }
}
