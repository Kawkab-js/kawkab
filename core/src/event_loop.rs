// QuickJS microtasks + Tokio: drain tasks, flush `run_pending_jobs`, then await the channel.

use std::{sync::Arc, time::Duration};

use quickjs_sys as qjs;
use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot},
    time::timeout,
};
use tracing::{debug, error, instrument, warn};

use crate::{error::JsError, isolate::Isolate, qjs_compat};

/// A task sent from the host (or another isolate) into this EventLoop.
pub enum Task {
    /// Evaluate a JS source snippet.
    Eval {
        src: Arc<[u8]>,
        filename: Arc<str>,
        reply: oneshot::Sender<Result<String, JsError>>,
    },
    /// Execute cached bytecode by key.
    ExecBytecode {
        key: Arc<str>,
        reply: oneshot::Sender<Result<String, JsError>>,
    },
    /// Resolve a pending JS Promise (used by I/O completion callbacks).
    ResolvePromise {
        promise_id: u64,
        payload: Arc<[u8]>, // zero-copy: raw bytes from I/O subsystem
    },
    /// Resolve a pending Promise with `undefined` (e.g. `fs.promises.writeFile`).
    ResolvePromiseVoid { promise_id: u64 },
    /// Resolve with a JSON value (`fs.promises.stat` / `readdir` shapes).
    ResolvePromiseJson {
        promise_id: u64,
        json: String,
    },
    /// Reject a pending JS Promise.
    RejectPromise { promise_id: u64, reason: String },
    /// Accepted TCP connection for the HTTP shim (handled on the isolate thread).
    HttpConnection {
        server_id: u64,
        stream: tokio::net::TcpStream,
    },
    /// Deferred timer (`setTimeout` / `setInterval` shim) — run on the isolate thread.
    TimerCallback { timer_id: u64 },
    /// UDP datagram for a `dgram` socket.
    UdpMessage {
        socket_id: u64,
        payload: Arc<[u8]>,
        host: Arc<str>,
        port: u16,
    },
    /// Graceful shutdown.
    Shutdown,
}

pub struct EventLoop {
    isolate: Isolate,
    task_rx: mpsc::UnboundedReceiver<Task>,
    /// Public handle: clone this to send tasks into the loop.
    task_tx: mpsc::UnboundedSender<Task>,
    /// Tokio runtime handle — reserved for spawning I/O outside the loop.
    _rt_handle: Handle,
}

impl EventLoop {
    /// Construct an EventLoop around an already-initialised Isolate.
    ///
    /// `rt_handle` must be a Tokio multi-thread runtime handle so that I/O
    /// futures can be spawned onto other threads while JS stays pinned here.
    pub fn new(isolate: Isolate, rt_handle: Handle) -> Self {
        let (task_tx, task_rx) = mpsc::unbounded_channel();
        Self {
            isolate,
            task_rx,
            task_tx,
            _rt_handle: rt_handle,
        }
    }

    /// Returns a `TaskSender` — a cheaply-cloneable handle for submitting work.
    pub fn sender(&self) -> TaskSender {
        TaskSender {
            inner: self.task_tx.clone(),
        }
    }

    pub fn ctx_ptr(&self) -> *mut qjs::JSContext {
        self.isolate.ctx_ptr()
    }

    /// Drive the event loop to completion.
    ///
    /// This is a `!Send` future; it must be `.await`ed on the thread that owns
    /// the Isolate. Use `tokio::task::LocalSet` or a pinned task for this.
    ///
    /// ```ignore
    /// let local = tokio::task::LocalSet::new();
    /// local.run_until(event_loop.run()).await;
    /// ```
    #[instrument(name = "event_loop_run", skip(self))]
    pub async fn run(mut self) -> Result<(), JsError> {
        debug!("EventLoop starting");

        loop {
            loop {
                match self.task_rx.try_recv() {
                    Ok(task) => {
                        if self.handle_task(task).await? == LoopSignal::Shutdown {
                            debug!("EventLoop received Shutdown, exiting");
                            return Ok(());
                        }
                    }
                    Err(_) => break,
                }
            }

            match self.isolate.run_pending_jobs() {
                Ok(had_jobs) => {
                    if had_jobs {
                        continue;
                    }
                }
                Err(e) => {
                    error!(error = %e, "Uncaught exception in microtask");
                }
            }

            match timeout(Duration::from_secs(30), self.task_rx.recv()).await {
                Ok(Some(task)) => {
                    if self.handle_task(task).await? == LoopSignal::Shutdown {
                        return Ok(());
                    }
                }
                Ok(None) => {
                    debug!("Task channel closed, EventLoop exiting");
                    return Ok(());
                }
                Err(_elapsed) => {
                    debug!("EventLoop idle 30 s; looping for GC opportunity");
                }
            }
        }
    }

    async fn handle_task(&mut self, task: Task) -> Result<LoopSignal, JsError> {
        match task {
            Task::Eval {
                src,
                filename,
                reply,
            } => {
                let s = String::from_utf8_lossy(&src);
                let transpiled = crate::transpiler::transpile_ts(&s, &filename).unwrap_or_else(|_| s.into_owned());

                let wrapper = format!("(function(exports, require, module, __filename, __dirname) {{\n{}\n}})", transpiled);
                let result = match self.isolate.eval(wrapper.as_bytes(), &filename) {
                    Ok(func_val) => unsafe {
                        let ctx = self.isolate.ctx_ptr();
                        let global = qjs::JS_GetGlobalObject(ctx);
                        let module_obj = qjs::JS_NewObject(ctx);
                        let exports_obj = qjs::JS_NewObject(ctx);
                        qjs::JS_SetPropertyStr(ctx, module_obj, std::ffi::CString::new("exports").unwrap().as_ptr(), exports_obj);

                        let require_fn = qjs::JS_GetPropertyStr(ctx, global, std::ffi::CString::new("require").unwrap().as_ptr());
                        let filename_val = qjs_compat::new_string_from_cstr(ctx, std::ffi::CString::new(filename.as_ref()).unwrap().as_ptr());
                        let dir = std::path::Path::new(&*filename).parent().unwrap_or(std::path::Path::new(".")).to_string_lossy();
                        let dirname_val = qjs_compat::new_string_from_cstr(ctx, std::ffi::CString::new(dir.as_ref()).unwrap().as_ptr());

                        let exports_arg = crate::ffi::js_dup_value(exports_obj);
                        let module_arg = crate::ffi::js_dup_value(module_obj);

                        let mut args = [exports_arg, require_fn, module_arg, filename_val, dirname_val];
                        let ret = qjs::JS_Call(ctx, func_val, global, 5, args.as_mut_ptr());

                        crate::ffi::js_free_value(ctx, func_val);
                        crate::ffi::js_free_value(ctx, exports_arg);
                        crate::ffi::js_free_value(ctx, module_arg);
                        crate::ffi::js_free_value(ctx, require_fn);
                        crate::ffi::js_free_value(ctx, filename_val);
                        crate::ffi::js_free_value(ctx, dirname_val);
                        crate::ffi::js_free_value(ctx, module_obj);
                        crate::ffi::js_free_value(ctx, global);

                        if ret.tag == qjs::JS_TAG_EXCEPTION as i64 {
                            let exc = qjs::JS_GetException(ctx);
                            let msg = self.isolate.stringify_js_value(exc);
                            Err(JsError::Js(format!("Execution failed: {}", msg)))
                        } else {
                            Ok(self.isolate.stringify_js_value(ret))
                        }
                    },
                    Err(e) => Err(e),
                };
                let _ = reply.send(result);
            }

            Task::ExecBytecode { key, reply } => {
                let result = self
                    .isolate
                    .eval_bytecode(&key)
                    .map(|v| self.isolate.stringify_js_value(v));
                let _ = reply.send(result);
            }

            Task::ResolvePromise {
                promise_id,
                payload,
            } => {
                if let Err(e) = self.resolve_promise(promise_id, payload) {
                    warn!(promise_id, error = %e, "Promise resolution failed");
                }
            }

            Task::ResolvePromiseVoid { promise_id } => {
                if let Err(e) = self.resolve_promise_void(promise_id) {
                    warn!(promise_id, error = %e, "Promise void resolution failed");
                }
            }

            Task::ResolvePromiseJson { promise_id, json } => {
                if let Err(e) = self.resolve_promise_json(promise_id, &json) {
                    warn!(promise_id, error = %e, "Promise JSON resolution failed");
                }
            }

            Task::RejectPromise { promise_id, reason } => {
                if let Err(e) = self.reject_promise(promise_id, &reason) {
                    warn!(promise_id, error = %e, "Promise rejection failed");
                }
            }

            Task::HttpConnection { server_id, stream } => unsafe {
                if let Err(e) =
                    crate::node::dispatch_http_connection(self.isolate.ctx_ptr(), server_id, stream)
                        .await
                {
                    warn!(server_id, error = %e, "HTTP connection handling failed");
                }
            },

            Task::TimerCallback { timer_id } => unsafe {
                if let Err(e) =
                    crate::node::dispatch_timer_callback(self.isolate.ctx_ptr(), timer_id)
                {
                    warn!(timer_id, error = %e, "Timer callback failed");
                }
            },
            Task::UdpMessage {
                socket_id,
                payload,
                host,
                port,
            } => unsafe {
                if let Err(e) = crate::node::dispatch_dgram_message(
                    self.isolate.ctx_ptr(),
                    socket_id,
                    payload,
                    &host,
                    port,
                ) {
                    warn!(socket_id, error = %e, "UDP message dispatch failed");
                }
            },

            Task::Shutdown => return Ok(LoopSignal::Shutdown),
        }

        Ok(LoopSignal::Continue)
    }

    fn resolve_promise(&mut self, promise_id: u64, payload: Arc<[u8]>) -> Result<(), JsError> {
        unsafe {
            let ctx = self.isolate.ctx_ptr();
            crate::node::host_resolve_promise(ctx, promise_id, payload)
                .map_err(|e| JsError::Js(e))
        }
    }

    fn resolve_promise_void(&mut self, promise_id: u64) -> Result<(), JsError> {
        unsafe {
            let ctx = self.isolate.ctx_ptr();
            crate::node::host_resolve_capability_void(ctx, promise_id)
                .map_err(|e| JsError::Js(e))
        }
    }

    fn resolve_promise_json(&mut self, promise_id: u64, json: &str) -> Result<(), JsError> {
        unsafe {
            let ctx = self.isolate.ctx_ptr();
            crate::node::host_resolve_promise_json(ctx, promise_id, json)
                .map_err(|e| JsError::Js(e))
        }
    }

    fn reject_promise(&mut self, promise_id: u64, reason: &str) -> Result<(), JsError> {
        unsafe {
            let ctx = self.isolate.ctx_ptr();
            crate::node::host_reject_promise(ctx, promise_id, reason)
                .map_err(|e| JsError::Js(e))
        }
    }
}

#[derive(PartialEq)]
enum LoopSignal {
    Continue,
    Shutdown,
}

#[derive(Clone)]
pub struct TaskSender {
    inner: mpsc::UnboundedSender<Task>,
}

impl TaskSender {
    /// Build a TaskSender directly from a channel sender (used by the CLI runner).
    pub fn from_sender(inner: tokio::sync::mpsc::UnboundedSender<Task>) -> Self {
        Self { inner }
    }

    /// Evaluate a JS string and wait for the result.
    pub async fn eval(
        &self,
        src: impl Into<Arc<[u8]>>,
        filename: impl Into<Arc<str>>,
    ) -> Result<String, JsError> {
        let (reply, rx) = oneshot::channel();
        self.inner
            .send(Task::Eval {
                src: src.into(),
                filename: filename.into(),
                reply,
            })
            .map_err(|_| JsError::ChannelClosed)?;
        rx.await.map_err(|_| JsError::ChannelClosed)?
    }

    /// Execute cached bytecode and wait for the result.
    pub async fn exec_bytecode(&self, key: impl Into<Arc<str>>) -> Result<String, JsError> {
        let (reply, rx) = oneshot::channel();
        self.inner
            .send(Task::ExecBytecode {
                key: key.into(),
                reply,
            })
            .map_err(|_| JsError::ChannelClosed)?;
        rx.await.map_err(|_| JsError::ChannelClosed)?
    }

    /// Resolve a pending promise from an I/O completion callback.
    ///
    /// `payload` is an `Arc<[u8]>` — zero-copy hand-off from the I/O layer.
    pub fn resolve_promise(&self, promise_id: u64, payload: Arc<[u8]>) {
        let _ = self.inner.send(Task::ResolvePromise {
            promise_id,
            payload,
        });
    }

    pub fn resolve_promise_void(&self, promise_id: u64) {
        let _ = self.inner.send(Task::ResolvePromiseVoid { promise_id });
    }

    pub fn resolve_promise_json(&self, promise_id: u64, json: String) {
        let _ = self.inner.send(Task::ResolvePromiseJson { promise_id, json });
    }

    /// Reject a pending promise.
    pub fn reject_promise(&self, promise_id: u64, reason: String) {
        let _ = self.inner.send(Task::RejectPromise { promise_id, reason });
    }

    /// Queue an accepted HTTP connection to be processed on the isolate thread.
    pub fn send_http_connection(&self, server_id: u64, stream: tokio::net::TcpStream) {
        let _ = self.inner.send(Task::HttpConnection { server_id, stream });
    }

    /// Queue a timer tick to run the registered JS callback on the isolate thread.
    pub fn send_timer_callback(&self, timer_id: u64) {
        let _ = self.inner.send(Task::TimerCallback { timer_id });
    }

    /// Queue a UDP datagram to be emitted on the JS isolate thread.
    pub fn send_udp_message(&self, socket_id: u64, payload: Arc<[u8]>, host: String, port: u16) {
        let _ = self.inner.send(Task::UdpMessage {
            socket_id,
            payload,
            host: Arc::<str>::from(host),
            port,
        });
    }

    /// Initiate graceful shutdown.
    pub fn shutdown(&self) {
        let _ = self.inner.send(Task::Shutdown);
    }
}
