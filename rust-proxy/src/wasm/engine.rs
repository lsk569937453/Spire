//! The wasm engine: one `Engine` (compiled-code cache, epoch clock) and one
//! `Linker` (the *only* set of imports a guest can resolve) for the whole
//! process.
//!
//! Two budgets are armed for every callback, on purpose:
//!
//! * **fuel** — deterministic CPU accounting, so a hot loop is stopped the same
//!   way on an idle and on a saturated machine;
//! * **epoch deadline** — a wall-clock backstop for everything fuel cannot see
//!   (a guest that blocks in a host call, or a machine that is simply slow).
//!
//! Reaching either one traps the instance. The instance is then discarded
//! rather than reused: after a trap the guest's internal state (and its
//! `Store`-side resource table) can be inconsistent, and a gateway must not
//! keep serving from a corrupted sandbox.

use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use wasmtime::component::Component;
use wasmtime::component::Linker;
use wasmtime::Config;
use wasmtime::Engine;
use wasmtime::Store;

use crate::vojo::app_error::AppError;
use crate::wasm::bindings;
use crate::wasm::state::StoreData;

/// How often the epoch clock ticks. Sets the resolution of `timeout_ms`; 1 ms
/// is far below the default 5 ms budget and costs one wake-up per millisecond
/// for the whole process.
pub const EPOCH_TICK: Duration = Duration::from_millis(1);

/// Hard ceiling on the wasm operand stack, independent of the linear memory
/// limit. A recursive guest must hit this before it hits the host's stack.
const MAX_WASM_STACK: usize = 512 * 1024;

/// Budget for *creating* an instance (type-checking the imports, allocating
/// tables, running the component's own start function). Not a request budget:
/// instantiation is host-side work plus a trivial guest prologue, and it
/// happens once per (thread, plugin, configuration). Kept separate from
/// `timeout_ms` because a 5 ms request budget would otherwise kill the
/// instance before the first request callback ever ran.
const INSTANTIATION_BUDGET: Duration = Duration::from_millis(500);

pub struct PluginEngine {
    engine: Engine,
    linker: Linker<StoreData>,
}

impl PluginEngine {
    pub fn new() -> Result<Self, AppError> {
        let mut config = Config::new();
        config
            .wasm_component_model(true)
            .consume_fuel(true)
            // No async, no threads, no reference types beyond what the
            // component model needs: the smaller the feature surface, the
            // smaller the sandbox escape surface.
            .max_wasm_stack(MAX_WASM_STACK)
            .epoch_interruption(true)
            .parallel_compilation(true);
        let engine = Engine::new(&config)
            .map_err(|e| AppError(format!("wasm: cannot create engine: {e}")))?;

        let mut linker: Linker<StoreData> = Linker::new(&engine);
        // WASI preview 2, linked because a component built for
        // `wasm32-wasip2` (the target every Rust guest uses) imports its
        // clocks/randomness from it. The context is the deny-all default: no
        // preopened directories, no environment, no arguments, no sockets, and
        // stdio goes nowhere. A plugin therefore has clocks and randomness and
        // nothing else, which is exactly what a proxy filter needs.
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| AppError(format!("wasm: cannot wire WASI: {e}")))?;
        // The guest's only other imports are the `spire:filter/host` functions
        // implemented by `StoreData`.
        bindings::FilterPlugin::add_to_linker::<_, wasmtime::component::HasSelf<StoreData>>(
            &mut linker,
            |data| data,
        )
        .map_err(|e| AppError(format!("wasm: cannot wire host imports: {e}")))?;

        spawn_epoch_ticker(engine.weak());

        Ok(Self { engine, linker })
    }

    /// Compiles a component. Compilation happens at configuration load, never
    /// on the request path: a slow compile must delay startup, not traffic.
    pub fn compile(&self, path: &Path) -> Result<Component, AppError> {
        let bytes = std::fs::read(path)
            .map_err(|e| AppError(format!("wasm: cannot read {}: {e}", path.display())))?;
        Component::new(&self.engine, &bytes).map_err(|e| {
            AppError(format!(
                "wasm: {} is not a valid component for this host: {e}",
                path.display()
            ))
        })
    }

    /// Instantiates a component with a fresh store whose limits and epoch
    /// callback are already armed.
    pub fn instantiate(
        &self,
        component: &Component,
        data: StoreData,
    ) -> Result<(Store<StoreData>, bindings::FilterPlugin), AppError> {
        let fuel = data.limits.fuel;
        let mut store = Store::new(&self.engine, data);
        store.limiter(|data| &mut data.store_limits);
        store
            .set_fuel(fuel)
            .map_err(|e| AppError(format!("wasm: cannot arm fuel: {e}")))?;
        // The store starts with a deadline in the past (`StoreData::new`), so
        // arm the instantiation budget *before* instantiating, otherwise the
        // first epoch tick would interrupt the instantiation itself.
        store.data().deadline.set(Instant::now() + INSTANTIATION_BUDGET);
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(|hook| {
            if Instant::now() >= hook.data().deadline.get() {
                Err(wasmtime::Error::msg(
                    "plugin callback exceeded its wall-clock budget",
                ))
            } else {
                Ok(wasmtime::UpdateDeadline::Continue(1))
            }
        });

        let plugin = bindings::FilterPlugin::instantiate(&mut store, component, &self.linker)
            .map_err(|e| AppError(format!("wasm: instantiation failed: {e}")))?;

        Ok((store, plugin))
    }
}

/// A process-wide thread that bumps the engine epoch. Every live store's
/// deadline callback runs at least once per tick, which is what turns
/// `timeout_ms` into an actual interruption instead of a wish.
fn spawn_epoch_ticker(engine: wasmtime::EngineWeak) {
    let result = std::thread::Builder::new()
        .name("spire-wasm-epoch".to_string())
        .spawn(move || {
            while let Some(engine) = engine.upgrade() {
                engine.increment_epoch();
                drop(engine);
                std::thread::sleep(EPOCH_TICK);
            }
        });
    if let Err(e) = result {
        // Not fatal: fuel still bounds CPU. Wall-clock timeouts would stop
        // working, so this is loud.
        error!("wasm: cannot start the epoch ticker, timeouts will not fire: {e}");
    }
}

/// Turns a wasmtime error into a low-cardinality reason for metrics and a
/// human-readable message for the log.
pub fn classify_error(error: &wasmtime::Error) -> (&'static str, String) {
    match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => (
            "out_of_fuel",
            "exhausted its CPU budget".to_string(),
        ),
        Some(wasmtime::Trap::Interrupt) => (
            "timeout",
            "was cancelled by the epoch deadline".to_string(),
        ),
        Some(wasmtime::Trap::UnreachableCodeReached) => (
            "panic",
            "crashed (unreachable / panic)".to_string(),
        ),
        Some(trap) => ("trap", format!("trapped: {trap}")),
        None => ("error", format!("{error:#}")),
    }
}

/// Re-arms the budgets before a callback. Called for every call, because a
/// store is reused across requests.
pub fn arm_budget(store: &mut Store<StoreData>) -> Result<(), AppError> {
    let (fuel, timeout) = {
        let data = store.data();
        (data.limits.fuel, data.limits.timeout)
    };
    store
        .set_fuel(fuel)
        .map_err(|e| AppError(format!("wasm: cannot re-arm fuel: {e}")))?;
    store.set_epoch_deadline(1);
    store.data().deadline.set(Instant::now() + timeout);
    store.data().log_budget.set(super::state::LOG_BUDGET);
    Ok(())
}
