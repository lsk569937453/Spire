//! Per-thread instance pools.
//!
//! Why per-thread and not a shared pool: a wasmtime `Store` is `!Sync` and every
//! callback needs `&mut Store`, so *any* sharing scheme ends up serialising
//! calls behind a lock or a channel — a channel hop in the hot path, or a mutex
//! that turns N worker threads into one. Keeping one instance per worker thread
//! and per (plugin, configuration) pair means a request never waits for another
//! request's plugin call, and no synchronisation happens at all.
//!
//! The trade-off is explicit: memory is `threads × plugins × max_memory_bytes`
//! worst case, and an instance is only created when a thread actually serves a
//! route that uses the plugin. Both are documented in `docs/plugin-system.md`
//! together with the sizing formula.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use wasmtime::Store;

use crate::vojo::app_error::AppError;
use crate::wasm::bindings;
use crate::wasm::state::StoreData;

/// Keep at most this many live instances per thread. Reached only by a
/// configuration with many distinct plugin/config pairs; the oldest idle entry
/// is dropped first.
const MAX_SLOTS_PER_THREAD: usize = 64;

/// Drop entries that have not served a request for this long when the pool is
/// over its soft size.
const IDLE_EVICT: std::time::Duration = std::time::Duration::from_secs(300);

pub struct Slot {
    pub store: Store<StoreData>,
    pub plugin: bindings::FilterPlugin,
    last_used: Instant,
}

impl Slot {
    pub fn new(store: Store<StoreData>, plugin: bindings::FilterPlugin) -> Self {
        Self {
            store,
            plugin,
            last_used: Instant::now(),
        }
    }

    /// Calls `lifecycle.stop` best effort. A panic inside the guest hook must
    /// never unwind into the gateway, hence the `catch_unwind`.
    fn shutdown(&mut self) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.plugin
                .spire_filter_lifecycle()
                .call_stop(&mut self.store)
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => debug!(
                "wasm: stop() failed for {}: {e}",
                self.store.data().plugin_id
            ),
            Err(_) => warn!("wasm: stop() panicked for {}", self.store.data().plugin_id),
        }
    }
}

thread_local! {
    static SLOTS: RefCell<HashMap<String, Rc<RefCell<Slot>>>> = RefCell::new(HashMap::new());
}

/// Returns the instance for `key`, creating it with `create` on first use.
///
/// The `Rc` is cloned out and the map borrow is released before the caller runs
/// the guest, so a callback that itself ends up back in this module (it cannot,
/// but the invariant is worth keeping) would not double-borrow.
pub fn acquire(
    key: &str,
    create: impl FnOnce() -> Result<Slot, AppError>,
) -> Result<Rc<RefCell<Slot>>, AppError> {
    SLOTS.with(|slots| {
        let mut map = slots.borrow_mut();
        if let Some(existing) = map.get(key) {
            let existing = existing.clone();
            existing.borrow_mut().last_used = Instant::now();
            return Ok(existing);
        }
        evict_if_needed(&mut map, key);
        let slot = Rc::new(RefCell::new(create()?));
        map.insert(key.to_string(), slot.clone());
        Ok(slot)
    })
}

/// Removes an instance from this thread's pool. Called after a trap: the store
/// is not trustworthy any more.
pub fn discard(key: &str) {
    let removed = SLOTS.with(|slots| slots.borrow_mut().remove(key));
    if let Some(slot) = removed
        && let Ok(mut slot) = slot.try_borrow_mut()
    {
        slot.shutdown();
    }
}

/// Drops instances whose generation is no longer current. Called on the thread
/// that reloaded the configuration; other threads notice through the pool size
/// cap and evict lazily.
pub fn retain_generation(current: &HashMap<String, String>) {
    let removed: Vec<Rc<RefCell<Slot>>> = SLOTS.with(|slots| {
        let mut map = slots.borrow_mut();
        let stale: Vec<String> = map
            .keys()
            .filter(|key| match current.get(plugin_of(key)) {
                Some(generation) => generation_of(key) != generation.as_str(),
                None => true,
            })
            .cloned()
            .collect();
        stale
            .into_iter()
            .filter_map(|key| map.remove(&key))
            .collect()
    });
    for slot in removed {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            slot.shutdown();
        }
    }
}

fn evict_if_needed(map: &mut HashMap<String, Rc<RefCell<Slot>>>, keep: &str) {
    if map.len() < MAX_SLOTS_PER_THREAD {
        return;
    }
    let mut candidates: Vec<(String, Instant)> = map
        .iter()
        .filter(|(key, _)| key.as_str() != keep)
        .filter_map(|(key, slot)| slot.try_borrow().ok().map(|s| (key.clone(), s.last_used)))
        .collect();
    candidates.sort_by_key(|(_, last_used)| *last_used);
    let now = Instant::now();
    let mut dropped = 0usize;
    for (key, last_used) in candidates {
        // Prefer entries that have been idle a while; if everything is hot,
        // still enforce the cap, oldest first.
        if dropped >= MAX_SLOTS_PER_THREAD / 4 && now.duration_since(last_used) < IDLE_EVICT {
            break;
        }
        if let Some(slot) = map.remove(&key) {
            dropped += 1;
            if let Ok(mut slot) = slot.try_borrow_mut() {
                slot.shutdown();
            }
        }
    }
}

/// Slot keys are `<plugin id>#<generation>#<config hash>`; these two helpers
/// peel them apart without a second map.
fn plugin_of(key: &str) -> &str {
    key.split('#').next().unwrap_or(key)
}

fn generation_of(key: &str) -> &str {
    key.split('#').nth(1).unwrap_or("")
}

pub fn slot_key(plugin_id: &str, generation: u64, config_key: &str) -> String {
    format!("{plugin_id}#{generation}#{config_key}")
}

/// Number of live instances on this thread (tests and diagnostics).
#[cfg(test)]
pub fn live_slots() -> usize {
    SLOTS.with(|slots| slots.borrow().len())
}
