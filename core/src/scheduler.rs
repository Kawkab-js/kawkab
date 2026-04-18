// core isolate scheduler
//
// ISOLATE SCHEDULER
// ═════════════════
// Manages a pool of Isolates, each pinned to a dedicated OS thread.
// Incoming requests are dispatched to the least-loaded Isolate.
//
// Threading model:
//   Main thread       → Tokio reactor (handles accept, timers, signals)
//   Worker thread[N]  → One Isolate + one LocalSet + one EventLoop each
//
// Dispatch heuristic:
//   We use a work-stealing variant: track a per-Isolate pending_count
//   AtomicU32 and route to the isolate with the lowest count.
//   This is O(N) but N is typically 4–16 (CPU count); acceptable.
//
// Alternative (not implemented): consistent hashing by session ID for
// stateful workloads (ensures affinity so JS globals accumulate correctly).

use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    thread,
};

use tokio::runtime::Handle;
use tracing::{debug, info};

use crate::{
    error::JsError,
    event_loop::{EventLoop, TaskSender},
    isolate::{Isolate, IsolateConfig},
};

// ── WorkerHandle ─────────────────────────────────────────────────────────────

/// A lightweight handle to a single Worker thread.
struct WorkerHandle {
    sender: TaskSender,
    pending_count: Arc<AtomicU32>,
    thread: thread::JoinHandle<()>,
}

// ── Scheduler ────────────────────────────────────────────────────────────────

pub struct Scheduler {
    workers: Vec<WorkerHandle>,
}

impl Scheduler {
    /// Spawn `count` worker threads, each with its own Isolate.
    ///
    /// `rt_handle` is the Tokio runtime handle that workers will use to spawn
    /// I/O futures.
    pub fn spawn(count: usize, config: IsolateConfig, rt_handle: Handle) -> Result<Self, JsError> {
        let mut workers = Vec::with_capacity(count);

        for id in 0..count {
            let config = config.clone();
            let rt = rt_handle.clone();
            let pending = Arc::new(AtomicU32::new(0));
            let pending2 = Arc::clone(&pending);

            // Channel: main → worker (oneshot for Sender extraction).
            let (tx, rx) = std::sync::mpsc::channel::<TaskSender>();

            let thread = thread::Builder::new()
                .name(format!("kawkab-worker-{id}"))
                .stack_size(2 * 1024 * 1024) // 2 MiB thread stack
                .spawn(move || {
                    // ── Worker thread body ─────────────────────────────────────
                    // Pin this thread to its own LocalSet so the !Send Isolate
                    // never crosses a thread boundary.

                    let isolate = Isolate::new(config).expect("Isolate init failed");
                    let event_loop = EventLoop::new(isolate, rt.clone());
                    let sender = event_loop.sender();

                    unsafe {
                        if let Err(e) = crate::node::install_runtime(
                            event_loop.ctx_ptr(),
                            "<kawkab>",
                            Some(sender.clone()),
                        ) {
                            tracing::error!(worker = id, error = %e, "install_runtime failed");
                        }
                    }

                    // Send the sender back to the scheduler so it can dispatch work.
                    tx.send(sender)
                        .expect("Scheduler vanished before worker ready");

                    // Run the event loop on this thread forever.
                    // `block_on` + LocalSet is the standard pattern for !Send futures.
                    let local = tokio::task::LocalSet::new();
                    rt.block_on(local.run_until(async move {
                        if let Err(e) = event_loop.run().await {
                            tracing::error!(worker = id, error = %e, "EventLoop exited with error");
                        }
                    }));

                    info!(worker = id, "Worker thread exiting");
                })
                .map_err(|e| JsError::Io(e))?;

            // Wait for the worker to initialise and send back its TaskSender.
            let sender = rx.recv().map_err(|_| JsError::ChannelClosed)?;

            workers.push(WorkerHandle {
                sender,
                pending_count: pending2,
                thread,
            });

            debug!(worker = id, "Worker ready");
        }

        info!(count, "All workers initialised");
        Ok(Self { workers })
    }

    /// Dispatch a JS evaluation to the least-loaded Isolate.
    ///
    /// Returns a `TaskSender` for the chosen worker so the caller can await
    /// the result.
    pub fn dispatch(&self) -> &TaskSender {
        // Find the worker with the minimum pending count.
        // Relaxed ordering is fine: this is a heuristic, not a guarantee.
        let best = self
            .workers
            .iter()
            .min_by_key(|w| w.pending_count.load(Ordering::Relaxed))
            .expect("No workers in pool");

        best.pending_count.fetch_add(1, Ordering::Relaxed);
        &best.sender
    }

    /// Gracefully shut down all workers.
    pub fn shutdown(self) {
        for worker in &self.workers {
            worker.sender.shutdown();
        }
        for worker in self.workers {
            let _ = worker.thread.join();
        }
    }
}
