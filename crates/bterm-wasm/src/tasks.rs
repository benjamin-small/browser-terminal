//! Registry of in-flight pipeline runs: each submitted line gets a run id,
//! a Rust `AbortHandle` (settles the task future), and a JS
//! `AbortController` (cancels in-flight `fetch`es inside TS commands).

use bterm_core::abort::AbortHandle;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

struct TaskEntry {
    pane: u32,
    handle: AbortHandle,
    controller: web_sys::AbortController,
}

thread_local! {
    static TASKS: RefCell<HashMap<u64, TaskEntry>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

pub fn next_id() -> u64 {
    NEXT_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    })
}

pub fn register(run_id: u64, pane: u32, handle: AbortHandle, controller: web_sys::AbortController) {
    TASKS.with(|t| {
        t.borrow_mut().insert(run_id, TaskEntry { pane, handle, controller });
    });
}

pub fn finish(run_id: u64) {
    TASKS.with(|t| {
        t.borrow_mut().remove(&run_id);
    });
}

/// Any other run still in flight for this pane?
pub fn pane_busy(pane: u32) -> bool {
    TASKS.with(|t| t.borrow().values().any(|e| e.pane == pane))
}

pub fn signal_for(run_id: u64) -> Option<web_sys::AbortSignal> {
    TASKS.with(|t| t.borrow().get(&run_id).map(|e| e.controller.signal()))
}

/// Abort every run in a pane (Ctrl-C). The JS `controller.abort()` calls run
/// after the map borrow is released — a listener may synchronously call back
/// into the engine.
pub fn abort_pane(pane: u32) -> bool {
    let victims: Vec<(AbortHandle, web_sys::AbortController)> = TASKS.with(|t| {
        let mut map = t.borrow_mut();
        let ids: Vec<u64> = map
            .iter()
            .filter(|(_, e)| e.pane == pane)
            .map(|(id, _)| *id)
            .collect();
        ids.iter()
            .filter_map(|id| map.remove(id))
            .map(|e| (e.handle, e.controller))
            .collect()
    });
    let any = !victims.is_empty();
    for (handle, controller) in victims {
        handle.abort();
        controller.abort();
    }
    any
}

/// Abort everything (dispose).
pub fn abort_all() {
    let victims: Vec<(AbortHandle, web_sys::AbortController)> = TASKS.with(|t| {
        t.borrow_mut()
            .drain()
            .map(|(_, e)| (e.handle, e.controller))
            .collect()
    });
    for (handle, controller) in victims {
        handle.abort();
        controller.abort();
    }
}
