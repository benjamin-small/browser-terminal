//! Registry of in-flight pipeline runs: each submitted line gets a run id,
//! a Rust `AbortHandle` (settles the task future), and a JS
//! `AbortController` (cancels in-flight `fetch`es inside TS commands).
//!
//! A run also owns whatever its TS stages have buffered but not yet emitted
//! (`register_buffer` below). That lives here, and not in `js_command`,
//! because the buffer has to be drained on paths where no code inside the
//! command body ever runs again — see `flush_buffers`.

use bterm_core::abort::AbortHandle;
use bterm_core::outbuf::OutputBuffer;
use bterm_core::sink::{Channel, Record, Sink};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

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

/// One TS stage's `OutputBuffer` for one channel, plus where its tail goes.
struct PendingOutput {
    buf: Rc<RefCell<OutputBuffer>>,
    sink: Rc<dyn Sink>,
    channel: Channel,
}

thread_local! {
    static TASKS: RefCell<HashMap<u64, TaskEntry>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    /// Per run id, because one pipeline can hold several TS stages and each
    /// has its own pair of buffers.
    static BUFFERS: RefCell<HashMap<u64, Vec<PendingOutput>>> = RefCell::new(HashMap::new());
}

/// Hand a TS stage's output buffer to the run that owns it, so the run's end
/// — however it gets there — drains it.
pub fn register_buffer(
    run_id: u64,
    buf: Rc<RefCell<OutputBuffer>>,
    sink: Rc<dyn Sink>,
    channel: Channel,
) {
    BUFFERS.with(|b| {
        b.borrow_mut()
            .entry(run_id)
            .or_default()
            .push(PendingOutput { buf, sink, channel });
    });
}

/// Emit whatever this run's TS stages still hold buffered, and forget them.
///
/// This is the *only* drain that covers every way a run can end. The flushes
/// at the bottom of `js_command`'s two success paths run when the body
/// returns normally; they cannot run when it doesn't. Every `?` in
/// `js_command` returns early past them, and Ctrl-C is worse than early —
/// `Abortable::poll` returns `Ready(Err(Aborted))` *before* polling the
/// inner future (`abort.rs`), so a suspended command body never resumes at
/// all. In the default `line` mode a partial write with no delimiter in it
/// is exactly what is sitting in the buffer at that moment, so without this
/// the text a command wrote on its way to failing is silently dropped —
/// contradicting `RunError`'s promise to carry what the pipeline wrote
/// before it failed, "including on Ctrl-C".
///
/// Callers are `finish` (every non-abort path, from a single call site in
/// both `spawn_pipeline` and `run`) and the abort paths below. Calling it
/// twice for one run is harmless: the entries are removed here, and
/// `OutputBuffer::finish` on an empty buffer yields nothing anyway.
///
/// Two things this must get right:
///
/// - **Never touch a sink after `dispose()`.** `PaneSink::write` goes
///   through `WasmAccess::with`, which panics when the engine is gone; under
///   `panic = "abort"` that takes the module with it. The entries are still
///   removed in that case, so nothing is leaked.
/// - **Never hold a `RefCell` borrow across `sink.write`.** A page can stash
///   `ctx.log` and call `.write()` from an `on_event` handler that our own
///   flush fires, which would re-enter the same buffer. The `let tail = …;`
///   split drops the `RefMut` at the semicolon; folding it into the `if let`
///   scrutinee would hold it for the whole block and panic. Same reason the
///   map borrow is released before the loop rather than held across it.
pub fn flush_buffers(run_id: u64) {
    let pending = BUFFERS.with(|b| b.borrow_mut().remove(&run_id));
    let Some(pending) = pending else {
        return;
    };
    if !crate::engine_alive() {
        return;
    }
    for entry in pending {
        let tail = entry.buf.borrow_mut().finish();
        let Some(text) = tail else {
            continue;
        };
        entry.sink.write(match entry.channel {
            Channel::Log => Record::raw_log(text),
            Channel::Err => Record::raw_err(text),
        });
    }
}

/// Whether `run_id` is still a live, in-flight run (not finished, not
/// aborted). The probe-deadline reschedule loop checks this before touching
/// the engine, so a stale timer from a run that already ended can't force a
/// commit against whatever new run has since reused its pane.
pub fn is_active(run_id: u64) -> bool {
    TASKS.with(|t| t.borrow().contains_key(&run_id))
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
    // Before the caller inspects the run's result: `run()` builds its
    // rejection from the sink immediately after this returns, so the tail
    // has to be in the sink by then.
    flush_buffers(run_id);
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
///
/// The buffered tail is flushed here rather than being left to `finish`, and
/// that is about *ordering*, not only about coverage. This runs inside
/// `feed()`, before its `flush_events()`, and the editor's `^C\r\n` plus the
/// fresh prompt travel back to the host in `Effects::echo` — written to
/// xterm only after `feed()` returns. So flushing here puts the interrupted
/// command's last partial line on screen above the `^C`, the way a real
/// shell does it. Waiting for the aborted task to settle would print it
/// underneath the next prompt.
pub fn abort_pane(pane: u32) -> bool {
    let victims: Vec<(u64, TaskEntry)> = TASKS.with(|t| {
        let mut map = t.borrow_mut();
        let ids: Vec<u64> = map
            .iter()
            .filter(|(_, e)| e.pane == pane)
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| map.remove(&id).map(|e| (id, e)))
            .collect()
    });
    let any = !victims.is_empty();
    for (run_id, entry) in victims {
        if let Some(id) = entry.probe_timeout {
            clear_probe_timeout(id);
        }
        entry.handle.abort();
        // After `controller.abort()`, not before: an abort listener runs
        // synchronously and may write a parting line through `ctx.log`, and
        // that write belongs in the flush too.
        entry.controller.abort();
        flush_buffers(run_id);
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
    let victims: Vec<(u64, TaskEntry)> = TASKS.with(|t| t.borrow_mut().drain().collect());
    for (run_id, entry) in victims {
        if let Some(id) = entry.probe_timeout {
            clear_probe_timeout(id);
        }
        entry.handle.abort();
        entry.controller.abort();
        // `dispose_engine` clears `ENGINE` only after this returns, so the
        // engine is still alive here and the flush is a real one. Its main
        // job is releasing the buffer and sink `Rc`s: a registry keyed by
        // run id would otherwise keep them for the life of the page.
        flush_buffers(run_id);
    }
    // A run that registered buffers but never made it into TASKS (or was
    // removed by an earlier path) would be missed by the loop above.
    // Dispose is the one place that can say "nothing is in flight", so it
    // is also the place to make that unconditional.
    BUFFERS.with(|b| b.borrow_mut().clear());
}

// Native-only: these exercise the buffer registry directly, with no JS
// involved. `#[test]` functions are invisible to wasm-bindgen-test-runner,
// so gating them keeps the wasm test binary honest about what it runs.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use bterm_core::engine::Engine;
    use bterm_core::sink::CollectingSink;

    /// `flush_buffers` refuses to touch a sink with no engine behind it, so
    /// a test that wants a real flush has to look alive. Each `#[test]` gets
    /// its own thread, hence its own `ENGINE`.
    fn engine_alive() {
        crate::ENGINE.with(|c| *c.borrow_mut() = Some(Engine::new()));
    }

    /// A default-mode buffer holding a partial line: written, emitted
    /// nothing, and therefore lost unless the run's end drains it.
    fn partial(text: &str) -> Rc<RefCell<OutputBuffer>> {
        let buf = Rc::new(RefCell::new(OutputBuffer::new()));
        assert_eq!(buf.borrow_mut().write(text), None, "a partial line must stay buffered");
        buf
    }

    #[test]
    fn a_runs_tail_reaches_its_sink_and_the_entry_is_dropped() {
        engine_alive();
        let sink = Rc::new(CollectingSink::new());
        let buf = partial("starting fetch");
        register_buffer(7, buf.clone(), sink.clone(), Channel::Log);

        flush_buffers(7);
        assert_eq!(sink.log_lines(), vec!["starting fetch"]);
        assert_eq!(
            Rc::strong_count(&buf),
            1,
            "the registry still holds the buffer, so every run leaks one"
        );
    }

    #[test]
    fn flushing_a_run_twice_neither_duplicates_nor_resurrects() {
        // Ctrl-C flushes from `abort_pane`, and the aborted task then
        // settles and calls `finish`, which flushes again. Both must be
        // safe to call for the same run.
        engine_alive();
        let sink = Rc::new(CollectingSink::new());
        register_buffer(7, partial("half a line"), sink.clone(), Channel::Log);

        flush_buffers(7);
        flush_buffers(7);
        flush_buffers(99); // a run that never registered anything
        assert_eq!(sink.log_lines(), vec!["half a line"]);
    }

    #[test]
    fn every_stage_and_both_channels_of_one_run_are_drained() {
        // A pipeline can hold several TS stages, each with its own pair of
        // buffers -- hence a Vec per run id rather than one entry.
        engine_alive();
        let sink = Rc::new(CollectingSink::new());
        register_buffer(7, partial("stage one"), sink.clone(), Channel::Log);
        register_buffer(7, partial("stage one warned"), sink.clone(), Channel::Err);
        register_buffer(7, partial("stage two"), sink.clone(), Channel::Log);
        // A different run must not be swept up with it.
        register_buffer(8, partial("other run"), sink.clone(), Channel::Log);

        flush_buffers(7);
        assert_eq!(sink.log_lines(), vec!["stage one", "stage two"]);
        assert_eq!(sink.err_lines(), vec!["stage one warned"]);
    }

    #[test]
    fn a_disposed_engine_is_never_written_to_and_still_does_not_leak() {
        // `PaneSink::write` reaches `WasmAccess::with`, which panics when
        // the engine is gone -- and under `panic = "abort"` that would take
        // the whole module down. Dropping the text is the only safe answer;
        // dropping the entry with it is what keeps this from leaking.
        crate::ENGINE.with(|c| *c.borrow_mut() = None);
        let sink = Rc::new(CollectingSink::new());
        let buf = partial("never emitted");
        register_buffer(7, buf.clone(), sink.clone(), Channel::Log);

        flush_buffers(7);
        assert!(sink.log_lines().is_empty(), "wrote to a sink after dispose()");
        assert_eq!(Rc::strong_count(&buf), 1, "the entry outlived the engine");
    }
}
