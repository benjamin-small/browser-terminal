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
    /// The probe-deadline `setTimeout` id, if one was armed for this run.
    /// Cleared whenever the run ends, by whichever path gets there first
    /// (normal finish or abort) -- otherwise a late timer could fire after
    /// the run's `ProgressiveConsumer` is gone and force a commit on
    /// whatever the pane happens to be running next.
    probe_timeout: Option<i32>,
}

/// Cancel an armed probe-deadline timer. Never fails loudly: a missing
/// `window` (already torn down) just means there is nothing to clear.
fn clear_probe_timeout(id: i32) {
    if let Some(win) = web_sys::window() {
        win.clear_timeout_with_handle(id);
    }
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
        t.borrow_mut()
            .insert(run_id, TaskEntry { pane, handle, controller, probe_timeout: None });
    });
}

/// Record the probe-deadline timer id armed for this run, so `finish` and
/// the abort paths below can clear it. A run whose entry is already gone
/// (finished or aborted between arming and this call) means the timer was
/// already cleared, so this is a no-op rather than resurrecting the entry.
pub fn set_probe_timeout(run_id: u64, id: i32) {
    TASKS.with(|t| {
        if let Some(entry) = t.borrow_mut().get_mut(&run_id) {
            entry.probe_timeout = Some(id);
        }
    });
}

pub fn finish(run_id: u64) {
    let entry = TASKS.with(|t| t.borrow_mut().remove(&run_id));
    if let Some(id) = entry.and_then(|e| e.probe_timeout) {
        clear_probe_timeout(id);
    }
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
    let victims: Vec<TaskEntry> = TASKS.with(|t| {
        let mut map = t.borrow_mut();
        let ids: Vec<u64> = map
            .iter()
            .filter(|(_, e)| e.pane == pane)
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| map.remove(id)).collect()
    });
    let any = !victims.is_empty();
    for entry in victims {
        if let Some(id) = entry.probe_timeout {
            clear_probe_timeout(id);
        }
        entry.handle.abort();
        entry.controller.abort();
    }
    any
}

/// Abort every run in each listed pane (panes closed by mux mutations).
pub fn abort_panes(panes: &[u32]) {
    for pane in panes {
        abort_pane(*pane);
    }
}

/// Abort everything (dispose).
pub fn abort_all() {
    let victims: Vec<TaskEntry> = TASKS.with(|t| t.borrow_mut().drain().map(|(_, e)| e).collect());
    for entry in victims {
        if let Some(id) = entry.probe_timeout {
            clear_probe_timeout(id);
        }
        entry.handle.abort();
        entry.controller.abort();
    }
}
