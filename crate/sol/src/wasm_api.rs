// U12 item 2, plan goal (a): a throwaway wasm-bindgen harness proving
// `tier0::Engine` runs end-to-end from JS before any `apps/web` wiring. Not a
// product surface - a single `.sol`/`.lua` source string in, its `main()`
// result or error out, exactly mirroring `tests/tier0_conformance.rs`'s
// `run_on_tier0` so the same code path is what's actually being proven here.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn run_sol(source: &str) -> Result<String, JsValue> {
    let (program, return_type) = crate::compile(source).map_err(|e| JsValue::from_str(&e))?;
    let engine: crate::tier0::Engine =
        crate::tier0::Engine::new(program, ()).map_err(|e| JsValue::from_str(&e))?;
    match engine.call_outcome("main", &[]) {
        sol_core::CallOutcome::Returned(values) => {
            let result = values.first().copied().unwrap_or(0);
            Ok(match return_type {
                crate::types::Type::I64 => (result as i64).to_string(),
                crate::types::Type::F64 => f64::from_bits(result).to_string(),
                crate::types::Type::Bool => (result != 0).to_string(),
                other => format!("<unsupported return type for this harness: {other:?}>"),
            })
        }
        sol_core::CallOutcome::Raised(error) => Err(JsValue::from_str(&error)),
        other => Err(JsValue::from_str(&format!("unexpected call outcome: {other:?}"))),
    }
}
