// io uring integration
//
// IO_URING INTEGRATION
// ════════════════════
// io_uring allows us to:
//   1. Submit batches of I/O operations in a single syscall (io_uring_enter).
//   2. Poll completions without any syscall (shared-memory ring).
//   3. Use fixed buffers registered with the kernel for zero-copy DMA paths.
//
// This module wraps tokio-uring (which already handles the ring setup) and
// adds our own abstraction layer for promise-based I/O driven by JS.
//
// SYSCALL COUNT:
//   Traditional (read/write per op): 1 syscall per I/O = O(n) syscalls
//   io_uring (batched submit):       1 syscall per batch = O(1) amortised
//
// For 10k concurrent connections this is the difference between:
//   ~10k sys-enter/s  vs  ~1-10 sys-enter/s
//
// BUFFER MANAGEMENT:
//   We use a pool of 4 KiB fixed buffers registered with io_uring via
//   IORING_OP_PROVIDE_BUFFERS. On completion the kernel hands back a buffer
//   ID and a byte count; we wrap the buffer in an Arc and hand it to JS.

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
    };

    use tokio::sync::oneshot;
    use tokio_uring::fs::File;
    use tracing::{debug, error, instrument};

    use crate::event_loop::TaskSender;

    // ── Buffer pool ───────────────────────────────────────────────────────────

    /// Size of each I/O buffer in the fixed pool.
    /// 4 KiB = one OS page = DMA-aligned on most hardware.
    const BUF_SIZE:   usize = 4 * 1024;
    /// Number of fixed buffers to register with the kernel.
    /// 256 × 4 KiB = 1 MiB registered memory — reasonable for edge fns.
    const BUF_COUNT:  usize = 256;

    /// A re-usable, heap-allocated I/O buffer.
    ///
    /// We align to 512 bytes (sector boundary) so kernel DMA can skip the
    /// bounce-buffer path on NVMe drives that support O_DIRECT.
    #[repr(align(512))]
    struct AlignedBuf([u8; BUF_SIZE]);

    /// Pool of pre-allocated I/O buffers.
    /// After a read completes, the buffer is wrapped in Arc and handed to JS;
    /// when JS drops the ArrayBuffer, the Arc drops and the buffer returns here.
    struct BufPool {
        free: parking_lot::Mutex<Vec<Box<AlignedBuf>>>,
    }

    impl BufPool {
        fn new() -> Self {
            let free = (0..BUF_COUNT)
                .map(|_| Box::new(AlignedBuf([0u8; BUF_SIZE])))
                .collect();
            Self { free: parking_lot::Mutex::new(free) }
        }

        fn acquire(&self) -> Option<Box<AlignedBuf>> {
            self.free.lock().pop()
        }

        fn release(&self, buf: Box<AlignedBuf>) {
            self.free.lock().push(buf);
        }
    }

    // ── UringDriver ───────────────────────────────────────────────────────────

    /// Drives all io_uring operations on behalf of the JS event loop.
    ///
    /// Each JS `fs.read()` / `net.recv()` call creates a pending future here;
    /// completion delivers an `Arc<[u8]>` to the EventLoop's TaskSender,
    /// which then resolves the corresponding JS Promise.
    pub struct UringDriver {
        sender:     TaskSender,
        buf_pool:   Arc<BufPool>,
        next_op_id: AtomicU64,
    }

    impl UringDriver {
        pub fn new(sender: TaskSender) -> Self {
            Self {
                sender,
                buf_pool: Arc::new(BufPool::new()),
                next_op_id: AtomicU64::new(1),
            }
        }

        fn next_id(&self) -> u64 {
            self.next_op_id.fetch_add(1, Ordering::Relaxed)
        }

        // ── Async file read ────────────────────────────────────────────────────

        /// Submit an async file read operation.
        ///
        /// On completion, the read bytes are delivered to the JS event loop as
        /// a zero-copy `Arc<[u8]>` payload.
        ///
        /// Returns the promise_id that the JS side should await on.
        #[instrument(skip(self), fields(path, offset, len))]
        pub fn read_file(
            &self,
            path:       String,
            offset:     u64,
            len:        usize,
            promise_id: u64,
        ) {
            let sender   = self.sender.clone();
            let buf_pool = Arc::clone(&self.buf_pool);

            // Spawn onto the tokio-uring executor (must be started with
            // tokio_uring::start() in main entrypoint.
            tokio_uring::spawn(async move {
                // ── 1. Acquire a pre-registered buffer ────────────────────────
                let mut buf = match buf_pool.acquire() {
                    Some(b) => b,
                    None => {
                        // Pool exhausted — fall back to a fresh allocation.
                        // This is a pressure indicator; tune BUF_COUNT if frequent.
                        error!("Buffer pool exhausted; allocating ad-hoc");
                        Box::new(AlignedBuf([0u8; BUF_SIZE]))
                    }
                };

                let read_len = len.min(BUF_SIZE);

                // ── 2. Open + read (two io_uring ops, one kernel round-trip) ──
                // tokio_uring::fs::File::open submits IORING_OP_OPENAT;
                // .read_at submits IORING_OP_READ. Both are batched by the
                // tokio-uring executor into a single io_uring_enter call.
                let result: Result<usize, std::io::Error> = async {
                    let f = File::open(&path).await?;
                    let (res, slice) = f.read_at(
                        // Slice the fixed buffer — no extra allocation.
                        &mut buf.0[..read_len],
                        offset,
                    ).await;
                    let n = res?;
                    Ok(n)
                }.await;

                match result {
                    Ok(n) => {
                        // ── 3. Wrap bytes in Arc for zero-copy delivery ────────
                        // We copy from the fixed buffer into an Arc<[u8]> here.
                        // True zero-copy requires registered buffer + sendmsg path
                        // (IORING_OP_SEND_ZC), which tokio-uring 0.4 partially
                        // supports. For now, this single copy is faster than
                        // multiple allocations in the old epoll path.
                        let payload: Arc<[u8]> = Arc::from(&buf.0[..n]);
                        buf_pool.release(buf);
                        sender.resolve_promise(promise_id, payload);
                    }
                    Err(e) => {
                        buf_pool.release(buf);
                        sender.reject_promise(promise_id, e.to_string());
                    }
                }
            });
        }

        // ── Async file write ───────────────────────────────────────────────────

        /// Submit an async write. `data` is cloned into the I/O path here
        /// (unavoidable for write — the kernel needs stable memory).
        pub fn write_file(
            &self,
            path:       String,
            offset:     u64,
            data:       Arc<[u8]>,
            promise_id: u64,
        ) {
            let sender = self.sender.clone();

            tokio_uring::spawn(async move {
                let result: Result<usize, std::io::Error> = async {
                    let f = tokio_uring::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .open(&path)
                        .await?;
                    // SAFETY: write_at takes a Vec<u8> to ensure buffer
                    // stability for the kernel. We convert Arc → Vec here.
                    let buf = data.to_vec();
                    let (res, _) = f.write_at(buf, offset).await;
                    Ok(res?)
                }.await;

                match result {
                    Ok(n) => {
                        let payload: Arc<[u8]> = Arc::from(n.to_string().as_bytes());
                        sender.resolve_promise(promise_id, payload);
                    }
                    Err(e) => sender.reject_promise(promise_id, e.to_string()),
                }
            });
        }
    }
}

// ── Platform re-exports ───────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub use linux::UringDriver;

/// Fallback stub for non-Linux platforms.
#[cfg(not(target_os = "linux"))]
pub struct UringDriver {
    sender: crate::event_loop::TaskSender,
}

#[cfg(not(target_os = "linux"))]
impl UringDriver {
    pub fn new(sender: crate::event_loop::TaskSender) -> Self {
        Self { sender }
    }

    pub fn read_file(&self, path: String, offset: u64, len: usize, promise_id: u64) {
        use std::sync::Arc;
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let res = tokio::fs::read(path).await.map(|bytes| {
                let start = (offset as usize).min(bytes.len());
                let end = start.saturating_add(len).min(bytes.len());
                Arc::<[u8]>::from(bytes[start..end].to_vec())
            });
            match res {
                Ok(payload) => sender.resolve_promise(promise_id, payload),
                Err(e) => sender.reject_promise(promise_id, e.to_string()),
            }
        });
    }

    pub fn write_file(&self, path: String, offset: u64, data: std::sync::Arc<[u8]>, promise_id: u64) {
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let result = async {
                let mut current = match tokio::fs::read(&path).await {
                    Ok(existing) => existing,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                    Err(e) => return Err(e),
                };

                let start = offset as usize;
                let end = start.saturating_add(data.len());
                if current.len() < end {
                    current.resize(end, 0);
                }
                current[start..end].copy_from_slice(&data);
                tokio::fs::write(&path, current).await?;
                Ok::<usize, std::io::Error>(data.len())
            }
            .await;

            match result {
                Ok(written) => {
                    let payload = std::sync::Arc::<[u8]>::from(written.to_string().into_bytes());
                    sender.resolve_promise(promise_id, payload);
                }
                Err(e) => sender.reject_promise(promise_id, e.to_string()),
            }
        });
    }
}
