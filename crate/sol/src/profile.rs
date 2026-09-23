// M8: `sol run --profile-time <out>` - a real `interp::Hooks` impl
// (not the zero-cost `()` default `sol run` normally uses) that times
// every interpreted-or-native call by wrapping it, and reports inclusive
// wall time (including callees, not self time - a real, honestly-scoped
// simplification) plus call count per function.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::interp::Hooks;

pub struct TimingHooks {
    stack: RefCell<Vec<Instant>>,
    totals: RefCell<HashMap<u8, (Duration, u32)>>,
}

impl TimingHooks {
    pub fn new() -> Self {
        TimingHooks {
            stack: RefCell::new(Vec::new()),
            totals: RefCell::new(HashMap::new()),
        }
    }

    /// Sorted by total time, descending - the usual "where did the time go" order.
    pub fn report(&self, name_of: impl Fn(u8) -> String) -> String {
        let mut rows: Vec<(u8, Duration, u32)> = self
            .totals
            .borrow()
            .iter()
            .map(|(&id, &(d, n))| (id, d, n))
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        let mut out =
            String::from("function                        calls        total (inclusive)\n");
        for (id, total, calls) in rows {
            out.push_str(&format!(
                "{:<32} {:>8} {:>12.3}ms\n",
                name_of(id),
                calls,
                total.as_secs_f64() * 1000.0
            ));
        }
        out
    }
}

impl Default for TimingHooks {
    fn default() -> Self {
        Self::new()
    }
}

impl Hooks for TimingHooks {
    fn on_call_enter(&self, _func_id: u8, _args: &[u64]) {
        self.stack.borrow_mut().push(Instant::now());
    }

    fn on_call_exit(&self, func_id: u8, _result: u64) {
        let start = self
            .stack
            .borrow_mut()
            .pop()
            .expect("on_call_enter/on_call_exit are always paired by Runtime::call");
        let elapsed = start.elapsed();
        let mut totals = self.totals.borrow_mut();
        let entry = totals.entry(func_id).or_insert((Duration::ZERO, 0));
        entry.0 += elapsed;
        entry.1 += 1;
    }
}
