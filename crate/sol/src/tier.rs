// Ties tier 0 (interp.rs) and native compilation (jit.rs) together: every
// function starts interpreted, promotes to native once hot (`jit::promote`).
// Tier 1/tier 2 are deliberately collapsed into one - `jit.rs` already runs
// the full optimizing pipeline, so there's no separate dumb "baseline" tier.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::bccompile;
use crate::bytecode::BcFunction;
use crate::interp::{self, Slot};
use crate::jit::Jit;
use crate::types::TProgram;

/// Call count at which a function promotes to native code (env-overridable; `=1` forces immediate promotion).
const DEFAULT_PROMOTE_THRESHOLD: u32 = 200;
fn promote_threshold() -> u32 {
    std::env::var("SOL_PROMOTE_THRESHOLD")
        .or_else(|_| std::env::var("SOL_PROMOTE_THRESHOLD"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PROMOTE_THRESHOLD)
}

/// Loop-backedge count that triggers OSR - lower than the promote threshold since a spinning loop is a stronger hot signal.
const DEFAULT_OSR_THRESHOLD: u32 = 50;
fn osr_threshold() -> u32 {
    std::env::var("SOL_OSR_THRESHOLD")
        .or_else(|_| std::env::var("SOL_OSR_THRESHOLD"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_OSR_THRESHOLD)
}

/// Matching-tag call count that triggers speculative specialization - lower still, since a wrong guess just wastes one unused compile.
const DEFAULT_SPECULATIVE_THRESHOLD: u32 = 30;
fn speculative_threshold() -> u32 {
    std::env::var("SOL_SPECULATIVE_THRESHOLD")
        .or_else(|_| std::env::var("SOL_SPECULATIVE_THRESHOLD"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SPECULATIVE_THRESHOLD)
}

/// M7 §23 PGO: which functions got promoted/speculatively-specialized in a
/// prior run - `Engine::new` preloads these immediately, skipping the
/// interpreted warm-up that produced them the first time. Text format,
/// one `promoted <name>` or `speculative <name>` line each - the
/// speculative candidate's type is re-derived from static analysis
/// (`jit::speculative_candidate`), not stored, so the file only ever
/// needs a name.
pub struct Profile {
    pub promoted: Vec<String>,
    pub speculative: Vec<String>,
}

impl Profile {
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read profile '{path}': {e}"))?;
        let mut promoted = Vec::new();
        let mut speculative = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some("promoted"), Some(name)) => promoted.push(name.to_string()),
                (Some("speculative"), Some(name)) => speculative.push(name.to_string()),
                _ => {}
            }
        }
        Ok(Profile {
            promoted,
            speculative,
        })
    }
}

pub struct Engine<H: interp::Hooks = ()> {
    runtime: interp::Runtime<'static, H>,
    name_to_id: HashMap<String, u8>,
    id_to_name: Vec<String>,
}

impl<H: interp::Hooks> Engine<H> {
    /// `hooks` is M8's debug/profile instrumentation point (default `()`,
    /// the no-op `sol run` always uses) - see `interp::Hooks`.
    pub fn new(program: TProgram, profile: Option<&Profile>, hooks: H) -> Result<Self, String> {
        let name_to_id = bccompile::function_index(&program)?;
        let functions = program.functions.clone();
        let jit = Rc::new(RefCell::new(Jit::new(program)?));

        // Too large for tier 0's 8-bit register field: compile straight to
        // native, never interpret. Placeholder slot below is never run.
        let mut slots = Vec::new();
        let mut always_native = Vec::new();
        for f in &functions {
            match bccompile::compile_function(f, &name_to_id) {
                Some(bf) => slots.push(Slot::Bytecode(Rc::new(bf))),
                None => {
                    always_native.push(f.name.clone());
                    slots.push(Slot::Bytecode(Rc::new(BcFunction {
                        metadata: sol_core::PrototypeMetadata::new(
                            &f.name,
                            f.params.len(),
                            false,
                            0,
                        )
                        .expect("typed function metadata fits u32"),
                        source_map: sol_core::SourceMap::default(),
                        source_file: f.source_file.clone(),
                        source_line: f.source_line,
                        source_span: f.source_span,
                        literals: vec![],
                        code: Vec::new(),
                        consts: Vec::new(),
                        local_count: f.local_count,
                        top_level_loops: Vec::new(),
                    })));
                }
            }
        }
        if let Some(p) = profile {
            always_native.extend(p.promoted.iter().cloned());
        }
        for name in &always_native {
            // `Jit::compiled` makes re-promoting an already-pulled-in dependency a harmless no-op.
            let (ptr, others) = jit.borrow_mut().promote(name)?;
            slots[name_to_id[name] as usize] = Slot::Native(ptr);
            for (other_name, other_ptr) in others {
                slots[name_to_id[&other_name] as usize] = Slot::Native(other_ptr);
            }
        }

        // FFI (M7 §25): externs are always native (no bytecode), appended
        // after `functions` in `bccompile::function_index`'s id space -
        // `link_externs` returns them in the same order, so pushing here
        // lines up with that numbering.
        for (_, ptr) in jit.borrow_mut().link_externs()? {
            slots.push(Slot::Native(ptr));
        }

        // Owns an `Rc` clone + owned `HashMap`s, no borrows - genuinely `'static`.
        let promote_jit = Rc::clone(&jit);
        let id_to_name: Vec<String> = {
            let mut v = vec![String::new(); name_to_id.len()];
            for (name, &id) in &name_to_id {
                v[id as usize] = name.clone();
            }
            v
        };
        let name_to_id_for_closure = name_to_id.clone();
        // Cloned before `promote` moves its own copy below.
        let osr_id_to_name = id_to_name.clone();
        let spec_id_to_name = id_to_name.clone();
        let engine_id_to_name = id_to_name.clone();
        let promote = move |id: u8| -> Option<(*const u8, Vec<(u8, *const u8)>)> {
            let (ptr, others) = promote_jit
                .borrow_mut()
                .promote(&id_to_name[id as usize])
                .ok()?;
            let others = others
                .into_iter()
                .map(|(name, p)| (name_to_id_for_closure[&name], p))
                .collect();
            Some((ptr, others))
        };

        // Same shape as `promote`, for OSR's per-loop compilation instead of a whole function.
        let osr_jit = Rc::clone(&jit);
        let osr_name_to_id = name_to_id.clone();
        let osr_promote =
            move |id: u8, stmt_index: usize| -> Option<(*const u8, Vec<(u8, *const u8)>)> {
                let (ptr, others) = osr_jit
                    .borrow_mut()
                    .promote_osr(&osr_id_to_name[id as usize], stmt_index)
                    .ok()?;
                let others = others
                    .into_iter()
                    .map(|(name, p)| (osr_name_to_id[&name], p))
                    .collect();
                Some((ptr, others))
            };

        // Speculative-candidate eligibility is a static property of each function's body, computed once up front.
        let speculative_candidates: HashMap<u8, (usize, i64, bool)> = functions
            .iter()
            .filter_map(|f| {
                let (param_index, ty) = jit.borrow().speculative_candidate(&f.name)?;
                let tag = crate::value::tag_for(&ty)
                    .expect("speculative candidate types are always boxable scalars");
                let proven = jit.borrow().speculative_exhaustive(&f.name);
                Some((name_to_id[&f.name], (param_index, tag, proven)))
            })
            .collect();
        let spec_jit = Rc::clone(&jit);
        let spec_name_to_id = name_to_id.clone();
        let speculative_promote = move |id: u8| -> Option<(*const u8, Vec<(u8, *const u8)>)> {
            let (ptr, others) = spec_jit
                .borrow_mut()
                .promote_speculative(&spec_id_to_name[id as usize])
                .ok()?;
            let others = others
                .into_iter()
                .map(|(name, p)| (spec_name_to_id[&name], p))
                .collect();
            Some((ptr, others))
        };

        // The closures' own `Rc` clones keep `Jit` alive as long as `Engine` does - no field needed here.
        let speculative = interp::SpeculativeConfig {
            candidates: speculative_candidates,
            threshold: speculative_threshold(),
            promote: Box::new(speculative_promote),
        };
        let runtime = interp::Runtime::new(
            slots,
            promote_threshold(),
            promote,
            osr_threshold(),
            osr_promote,
            speculative,
            hooks,
        );

        if let Some(p) = profile {
            for name in &p.speculative {
                if let Ok((ptr, others)) = jit.borrow_mut().promote_speculative(name) {
                    runtime.preload_speculative(name_to_id[name], ptr);
                    for (other_name, other_ptr) in others {
                        runtime.preload_native(name_to_id[&other_name], other_ptr);
                    }
                }
            }
        }

        Ok(Engine {
            runtime,
            name_to_id,
            id_to_name: engine_id_to_name,
        })
    }

    pub fn call(&self, name: &str, args: &[u64]) -> u64 {
        self.runtime.call(self.name_to_id[name], args)
    }

    /// M8: read back what `hooks` recorded during the run (profiling
    /// totals, a debugger's final state, etc.).
    pub fn hooks(&self) -> &H {
        self.runtime.hooks()
    }

    /// Function name for a given id - `debug.rs`/`profile.rs` format
    /// reports/traces in terms of names, not raw ids.
    pub fn name_of(&self, id: u8) -> &str {
        &self.id_to_name[id as usize]
    }

    /// M7 §23 PGO: writes which functions actually got promoted/
    /// speculatively-specialized during this run, for a later run's
    /// `profile` to preload - see `Profile`'s doc comment.
    pub fn dump_profile(&self, path: &str) -> Result<(), String> {
        let mut out = String::new();
        for id in self.runtime.native_function_ids() {
            out.push_str(&format!("promoted {}\n", self.id_to_name[id as usize]));
        }
        for id in self.runtime.specialized_speculative_ids() {
            out.push_str(&format!("speculative {}\n", self.id_to_name[id as usize]));
        }
        std::fs::write(path, out).map_err(|e| format!("failed to write profile '{path}': {e}"))
    }
}
