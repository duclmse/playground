//! Internal diagnostic timer. Browser hosts do not supply a clock capability
//! to the runtime; GC still records collection counts, with zero timing.

pub(super) struct Timer {
    #[cfg(not(target_arch = "wasm32"))]
    started: std::time::Instant,
}

impl Timer {
    pub(super) fn now() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            started: std::time::Instant::now(),
        }
    }

    pub(super) fn elapsed(&self) -> std::time::Duration {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.started.elapsed()
        }
        #[cfg(target_arch = "wasm32")]
        {
            std::time::Duration::ZERO
        }
    }
}
