//! OS-thread `worker_threads`: one QuickJS isolate per worker, JSON `postMessage` over channels.

use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

use crate::console;
use crate::event_loop::{Task, TaskSender};
use crate::ffi::{js_dup_value, js_free_value, js_string_to_owned};
use crate::isolate::IsolateConfig;
use crate::node::{install_runtime_with_embed, is_exception, js_undefined};
use crate::qjs_compat;
use quickjs_sys as qjs;
use serde_json;

/// How the runtime was embedded: main program vs `worker_threads` worker isolate.
#[derive(Clone)]
pub enum RuntimeEmbed {
    Main,
    Worker {
        worker_id: u64,
        main_task_tx: TaskSender,
    },
}

struct WorkerParentMessageListener {
    id: u64,
    cb: qjs::JSValue,
    once: bool,
}

static NEXT_WORKER_PARENT_LISTENER_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static RUNTIME_EMBED: std::cell::RefCell<Option<RuntimeEmbed>> = const { std::cell::RefCell::new(None) };
    /// `postMessage` listeners on a worker isolate (`parentPort.on` / `once` / prepend variants).
    static WORKER_PARENT_MESSAGE_LISTENERS: std::cell::RefCell<Vec<WorkerParentMessageListener>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Worker-isolate thread only: main -> worker payloads queued until `parentPort` listener exists.
    static WORKER_PARENT_MAILBOX: std::cell::RefCell<VecDeque<String>> =
        std::cell::RefCell::new(VecDeque::new());
}

static NEXT_WORKER_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Main-isolate thread only: `Worker#on('message')` callbacks (not `Send` — holds `JSValue`).
    static MAIN_WORKER_MSG_LISTENERS: std::cell::RefCell<HashMap<u64, qjs::JSValue>> =
        std::cell::RefCell::new(HashMap::new());
    static MAIN_WORKER_MSG_ONCE: std::cell::RefCell<HashMap<u64, bool>> =
        std::cell::RefCell::new(HashMap::new());
    /// Main-isolate thread only: `Worker#on('exit')` callbacks.
    static MAIN_WORKER_EXIT_LISTENERS: std::cell::RefCell<HashMap<u64, ExitListener>> =
        std::cell::RefCell::new(HashMap::new());
    /// Main-isolate thread only: `Worker#once('exit')` callbacks.
    static MAIN_WORKER_EXIT_ONCE_LISTENERS: std::cell::RefCell<HashMap<u64, qjs::JSValue>> =
        std::cell::RefCell::new(HashMap::new());
    /// Main thread: worker → main payloads drained by a QuickJS job (avoids re-entrant `JS_Call`).
    static MAIN_WORKER_MAILBOX: std::cell::RefCell<VecDeque<(u64, String)>> =
        std::cell::RefCell::new(VecDeque::new());
    /// Main-isolate thread only: worker -> main messages that arrived before a `message` listener was attached.
    static MAIN_WORKER_PENDING_MSGS: std::cell::RefCell<HashMap<u64, VecDeque<String>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Drop main-thread `MessageChannel` port routing state (no `JSValue`); safe across isolates.
fn clear_local_message_port_registries() {
    if let Ok(mut g) = port_peers_registry().lock() {
        g.clear();
    }
    if let Ok(mut g) = port_queues_registry().lock() {
        g.clear();
    }
    if let Ok(mut g) = port_ref_registry().lock() {
        g.clear();
    }
}

/// Clear `JSValue` handles and worker mail state on this OS thread before binding a new runtime.
pub(crate) unsafe fn clear_install_thread_js_handles(ctx: *mut qjs::JSContext) {
    clear_local_message_port_registries();
    WORKER_PARENT_MESSAGE_LISTENERS.with(|v| {
        let mut g = v.borrow_mut();
        for e in g.drain(..) {
            js_free_value(ctx, e.cb);
        }
    });
    WORKER_PARENT_MAILBOX.with(|c| c.borrow_mut().clear());
    MAIN_WORKER_MSG_LISTENERS.with(|c| c.borrow_mut().clear());
    MAIN_WORKER_MSG_ONCE.with(|c| c.borrow_mut().clear());
    MAIN_WORKER_EXIT_LISTENERS.with(|c| c.borrow_mut().clear());
    MAIN_WORKER_EXIT_ONCE_LISTENERS.with(|c| c.borrow_mut().clear());
    MAIN_WORKER_MAILBOX.with(|c| c.borrow_mut().clear());
    MAIN_WORKER_PENDING_MSGS.with(|c| c.borrow_mut().clear());
    // `MessagePort` listeners hold `JSValue`s from the *previous* isolate; that runtime is
    // intentionally not freed in `Isolate::Drop`, so do not `js_free_value` them with `ctx`
    // from the new runtime (undefined behavior). Drop stale map entries without freeing.
    WT_PORT_LISTENERS.with(|c| c.borrow_mut().clear());
}

struct WorkerEntry {
    to_worker: mpsc::Sender<Task>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct ExitListener {
    cb: qjs::JSValue,
}

static WORKER_REGISTRY: OnceLock<Mutex<HashMap<u64, WorkerEntry>>> = OnceLock::new();
static WORKER_ENV_DATA: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static NEXT_PORT_ID: AtomicU64 = AtomicU64::new(1);
static WT_PORT_PEERS: OnceLock<Mutex<HashMap<u64, u64>>> = OnceLock::new();
static WT_PORT_QUEUES: OnceLock<Mutex<HashMap<u64, VecDeque<String>>>> = OnceLock::new();
static WT_PORT_HAS_REF: OnceLock<Mutex<HashMap<u64, bool>>> = OnceLock::new();
/// Main-thread `Worker` `ref`/`unref`/`hasRef` baseline: missing entry means `hasRef === true`.
static MAIN_WORKER_HAS_REF: OnceLock<Mutex<HashMap<u64, bool>>> = OnceLock::new();

thread_local! {
    static WT_PORT_LISTENERS: std::cell::RefCell<HashMap<u64, qjs::JSValue>> =
        std::cell::RefCell::new(HashMap::new());
}

fn registry() -> &'static Mutex<HashMap<u64, WorkerEntry>> {
    WORKER_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn env_data_registry() -> &'static Mutex<HashMap<String, String>> {
    WORKER_ENV_DATA.get_or_init(|| Mutex::new(HashMap::new()))
}

fn port_peers_registry() -> &'static Mutex<HashMap<u64, u64>> {
    WT_PORT_PEERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn port_queues_registry() -> &'static Mutex<HashMap<u64, VecDeque<String>>> {
    WT_PORT_QUEUES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn port_ref_registry() -> &'static Mutex<HashMap<u64, bool>> {
    WT_PORT_HAS_REF.get_or_init(|| Mutex::new(HashMap::new()))
}

fn main_worker_ref_registry() -> &'static Mutex<HashMap<u64, bool>> {
    MAIN_WORKER_HAS_REF.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Baseline: Node defaults `hasRef` to true; `unref()` records `false`, `ref()` clears back to default.
pub fn main_worker_ref_set(worker_id: u64, has_ref: bool) {
    let Ok(mut g) = main_worker_ref_registry().lock() else {
        return;
    };
    if has_ref {
        g.remove(&worker_id);
    } else {
        g.insert(worker_id, false);
    }
}

pub fn main_worker_ref_get(worker_id: u64) -> bool {
    main_worker_ref_registry()
        .lock()
        .ok()
        .and_then(|g| g.get(&worker_id).copied())
        .unwrap_or(true)
}

pub fn main_worker_ref_clear(worker_id: u64) {
    if let Ok(mut g) = main_worker_ref_registry().lock() {
        g.remove(&worker_id);
    }
}

pub fn set_runtime_embed(embed: RuntimeEmbed) {
    RUNTIME_EMBED.with(|e| *e.borrow_mut() = Some(embed));
}

pub fn runtime_embed() -> Option<RuntimeEmbed> {
    RUNTIME_EMBED.with(|e| e.borrow().clone())
}

pub fn is_main_embed() -> bool {
    matches!(runtime_embed(), Some(RuntimeEmbed::Main) | None)
}

/// Spawn an OS thread running a worker isolate; registers the worker's std `mpsc::Sender<Task>` before running user script.
pub fn spawn_worker_thread(main_tx: TaskSender, script_path: PathBuf) -> Result<u64, String> {
    let worker_id = NEXT_WORKER_ID.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let path = script_path
        .canonicalize()
        .unwrap_or_else(|_| script_path.clone());
    let path_str = path.to_string_lossy().into_owned();
    let (reg_tx, reg_rx) = mpsc::channel::<Result<mpsc::Sender<Task>, String>>();
    let main_tx_clone = main_tx.clone();
    let cancel_thread = Arc::clone(&cancel);

    let join = std::thread::Builder::new()
        .name(format!("kawkab-worker-{worker_id}"))
        .spawn(move || {
            let res =
                worker_run_blocking(worker_id, main_tx_clone, path_str, reg_tx, cancel_thread);
            if let Err(e) = res {
                tracing::error!(worker_id, error = %e, "worker run failed");
            }
        })
        .map_err(|e| e.to_string())?;

    let to_worker = match reg_rx.recv() {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("worker thread died before registration".to_string()),
    };

    registry().lock().unwrap().insert(
        worker_id,
        WorkerEntry {
            to_worker,
            cancel,
            join: Mutex::new(Some(join)),
        },
    );

    Ok(worker_id)
}

fn worker_run_blocking(
    worker_id: u64,
    main_tx: TaskSender,
    path_str: String,
    reg_tx: mpsc::Sender<Result<mpsc::Sender<Task>, String>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    let mut isolate =
        crate::isolate::Isolate::new(IsolateConfig::default()).map_err(|e| e.to_string())?;
    console::install(&mut isolate).map_err(|e| e.to_string())?;
    let ctx = isolate.ctx_ptr();

    let (task_tx, task_rx) = mpsc::channel::<Task>();
    let _ = reg_tx.send(Ok(task_tx.clone()));

    unsafe {
        install_runtime_with_embed(
            ctx,
            &path_str,
            None,
            RuntimeEmbed::Worker {
                worker_id,
                main_task_tx: main_tx,
            },
        )?;
    }

    let src =
        std::fs::read_to_string(&path_str).map_err(|e| format!("worker read {}: {e}", path_str))?;
    let transpiled = if path_str.ends_with(".ts") || path_str.ends_with(".tsx") {
        crate::transpiler::transpile_ts(&src, &path_str).unwrap_or(src)
    } else {
        src
    };
    let body = format!("var Buffer = globalThis.Buffer;\n{transpiled}");
    let wrapper =
        format!("(function(exports, require, module, __filename, __dirname) {{\n{body}\n}})");
    unsafe {
        run_cjs_wrapper(ctx, &wrapper, &path_str)?;
    }

    'worker: loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        loop {
            let res = unsafe {
                let rt_ptr = qjs::JS_GetRuntime(ctx);
                let mut ctx_out: *mut qjs::JSContext = std::ptr::null_mut();
                qjs::JS_ExecutePendingJob(rt_ptr, &mut ctx_out)
            };
            if res <= 0 {
                break;
            }
        }
        if cancel.load(Ordering::Acquire) {
            break;
        }
        loop {
            match task_rx.try_recv() {
                Ok(task) => {
                    if handle_worker_task(ctx, task)? {
                        break 'worker;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'worker,
            }
        }
        if cancel.load(Ordering::Acquire) {
            break;
        }
        match task_rx.recv() {
            Ok(task) => {
                if handle_worker_task(ctx, task)? {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    unsafe {
        crate::node::clear_module_caches(ctx);
    }
    console::flush_all();
    Ok(())
}

/// Returns `true` if the worker should stop processing tasks.
fn handle_worker_task(ctx: *mut qjs::JSContext, task: Task) -> Result<bool, String> {
    match task {
        Task::TimerCallback { timer_id } => unsafe {
            let _ = crate::node::dispatch_timer_callback(ctx, timer_id);
        },
        Task::ResolvePromise {
            promise_id,
            payload,
        } => unsafe {
            let _ = crate::node::host_resolve_promise(ctx, promise_id, payload);
        },
        Task::ResolvePromiseVoid { promise_id } => unsafe {
            let _ = crate::node::host_resolve_capability_void(ctx, promise_id);
        },
        Task::ResolvePromiseJson { promise_id, json } => unsafe {
            let _ = crate::node::host_resolve_promise_json(ctx, promise_id, &json);
        },
        Task::RejectPromise { promise_id, reason } => unsafe {
            let _ = crate::node::host_reject_promise(ctx, promise_id, &reason);
        },
        Task::WorkerPostToWorker { json } => unsafe {
            dispatch_worker_post_from_main(ctx, &json)?;
        },
        Task::WorkerThreadExit | Task::Shutdown => return Ok(true),
        _ => {}
    }
    Ok(false)
}

unsafe fn run_cjs_wrapper(
    ctx: *mut qjs::JSContext,
    wrapper: &str,
    filename: &str,
) -> Result<(), String> {
    let c_src = CString::new(wrapper.as_bytes()).map_err(|e| e.to_string())?;
    let c_filename = CString::new(filename).map_err(|e| e.to_string())?;
    let func_val = qjs_compat::eval(
        ctx,
        c_src.as_ptr(),
        c_src.as_bytes().len(),
        c_filename.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    );
    if is_exception(func_val) {
        let exc = qjs::JS_GetException(ctx);
        let msg = js_string_to_owned(ctx, exc);
        js_free_value(ctx, exc);
        js_free_value(ctx, func_val);
        return Err(msg);
    }
    let global = qjs::JS_GetGlobalObject(ctx);
    let module_obj = qjs::JS_NewObject(ctx);
    let exports_obj = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        module_obj,
        CString::new("exports").unwrap().as_ptr(),
        exports_obj,
    );
    let require_fn = qjs::JS_GetPropertyStr(ctx, global, CString::new("require").unwrap().as_ptr());
    let filename_val =
        qjs_compat::new_string_from_cstr(ctx, CString::new(filename).unwrap().as_ptr());
    let dir = Path::new(filename)
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy();
    let dirname_val =
        qjs_compat::new_string_from_cstr(ctx, CString::new(dir.as_ref()).unwrap().as_ptr());
    let exports_arg = js_dup_value(exports_obj);
    let module_arg = js_dup_value(module_obj);
    let mut args = [
        exports_arg,
        require_fn,
        module_arg,
        filename_val,
        dirname_val,
    ];
    let ret = qjs::JS_Call(ctx, func_val, global, 5, args.as_mut_ptr());
    js_free_value(ctx, func_val);
    js_free_value(ctx, exports_arg);
    js_free_value(ctx, module_arg);
    js_free_value(ctx, require_fn);
    js_free_value(ctx, filename_val);
    js_free_value(ctx, dirname_val);
    js_free_value(ctx, module_obj);
    js_free_value(ctx, global);
    if is_exception(ret) {
        let exc = qjs::JS_GetException(ctx);
        let msg = js_string_to_owned(ctx, exc);
        js_free_value(ctx, exc);
        js_free_value(ctx, ret);
        return Err(msg);
    }
    js_free_value(ctx, ret);
    Ok(())
}

unsafe fn js_value_to_json_string(
    ctx: *mut qjs::JSContext,
    v: qjs::JSValue,
) -> Result<String, String> {
    // Phase-2 foundation: carry binary payloads across workers without JSON loss.
    let mut ab_size: usize = 0;
    let ab_ptr = qjs::JS_GetArrayBuffer(ctx, &mut ab_size, v);
    if !ab_ptr.is_null() {
        let bytes = std::slice::from_raw_parts(ab_ptr as *const u8, ab_size).to_vec();
        let payload = serde_json::to_string(&bytes).map_err(|e| e.to_string())?;
        return Ok(format!("ab:{payload}"));
    }

    let mut off: usize = 0;
    let mut len: usize = 0;
    let mut el: usize = 0;
    let typed_ab = qjs::JS_GetTypedArrayBuffer(ctx, v, &mut off, &mut len, &mut el);
    if is_exception(typed_ab) {
        let exc = qjs::JS_GetException(ctx);
        js_free_value(ctx, exc);
        js_free_value(ctx, typed_ab);
    } else {
        let is_typed = qjs::JS_IsObject(typed_ab);
        if is_typed {
            let mut base_size: usize = 0;
            let base_ptr = qjs::JS_GetArrayBuffer(ctx, &mut base_size, typed_ab);
            if !base_ptr.is_null() && off.saturating_add(len) <= base_size {
                let bytes =
                    std::slice::from_raw_parts((base_ptr as *const u8).add(off), len).to_vec();
                js_free_value(ctx, typed_ab);
                let payload = serde_json::to_string(&bytes).map_err(|e| e.to_string())?;
                return Ok(format!("u8:{payload}"));
            }
        }
        js_free_value(ctx, typed_ab);
    }

    let global = qjs::JS_GetGlobalObject(ctx);
    let json_o = qjs::JS_GetPropertyStr(ctx, global, CString::new("JSON").unwrap().as_ptr());
    let stringify =
        qjs::JS_GetPropertyStr(ctx, json_o, CString::new("stringify").unwrap().as_ptr());
    js_free_value(ctx, global);
    js_free_value(ctx, json_o);
    let arg = js_dup_value(v);
    let mut args = [arg];
    let out = qjs::JS_Call(ctx, stringify, js_undefined(), 1, args.as_mut_ptr());
    js_free_value(ctx, stringify);
    js_free_value(ctx, arg);
    if is_exception(out) {
        js_free_value(ctx, out);
        return Err(
            "JSON.stringify failed (worker postMessage value must be JSON-serializable)"
                .to_string(),
        );
    }
    let s = js_string_to_owned(ctx, out);
    js_free_value(ctx, out);
    Ok(format!("json:{s}"))
}

unsafe fn payload_to_js_value(
    ctx: *mut qjs::JSContext,
    payload: &str,
) -> Result<qjs::JSValue, String> {
    if let Some(json) = payload.strip_prefix("json:") {
        let label = CString::new("kawkab:worker-msg").unwrap_or_default();
        let val = qjs::JS_ParseJSON(ctx, json.as_ptr() as *const _, json.len(), label.as_ptr());
        if is_exception(val) {
            js_free_value(ctx, val);
            return Err("invalid JSON in worker message".into());
        }
        return Ok(val);
    }
    if let Some(rest) = payload.strip_prefix("ab:") {
        let bytes: Vec<u8> = serde_json::from_str(rest).map_err(|e| e.to_string())?;
        return Ok(crate::ffi::arraybuffer_from_slice(ctx, &bytes));
    }
    if let Some(rest) = payload.strip_prefix("u8:") {
        let bytes: Vec<u8> = serde_json::from_str(rest).map_err(|e| e.to_string())?;
        let ab = crate::ffi::arraybuffer_from_slice(ctx, &bytes);
        if is_exception(ab) {
            return Err("failed to create ArrayBuffer for typed payload".into());
        }
        let global = qjs::JS_GetGlobalObject(ctx);
        let ctor =
            qjs::JS_GetPropertyStr(ctx, global, CString::new("Uint8Array").unwrap().as_ptr());
        js_free_value(ctx, global);
        if qjs::JS_IsFunction(ctx, ctor) == 0 {
            js_free_value(ctx, ctor);
            js_free_value(ctx, ab);
            return Err("Uint8Array constructor is unavailable".into());
        }
        let mut args = [ab];
        let out = qjs::JS_CallConstructor(ctx, ctor, 1, args.as_mut_ptr());
        js_free_value(ctx, ctor);
        js_free_value(ctx, ab);
        if is_exception(out) {
            js_free_value(ctx, out);
            return Err("failed to construct Uint8Array payload".into());
        }
        return Ok(out);
    }
    Err("unknown worker payload encoding".into())
}

pub fn worker_post_from_worker_to_main(worker_id: u64, json: String) {
    let main_tx = match runtime_embed() {
        Some(RuntimeEmbed::Worker { main_task_tx, .. }) => main_task_tx,
        _ => return,
    };
    main_tx.send_worker_post_to_main(worker_id, json);
}

pub fn worker_post_from_main_to_worker(worker_id: u64, json: String) -> Result<(), String> {
    let reg = registry().lock().unwrap();
    let ent = reg
        .get(&worker_id)
        .ok_or_else(|| "unknown worker id".to_string())?;
    ent.to_worker
        .send(Task::WorkerPostToWorker { json })
        .map_err(|e| e.to_string())
}

pub fn terminate_worker_thread(worker_id: u64) {
    let mut reg = registry().lock().unwrap();
    if let Some(ent) = reg.remove(&worker_id) {
        ent.cancel.store(true, Ordering::Release);
        let _ = ent.to_worker.send(Task::WorkerThreadExit);
        if let Ok(mut g) = ent.join.lock() {
            if let Some(j) = g.take() {
                let _ = j.join();
            }
        }
    }
    main_worker_ref_clear(worker_id);
}

pub unsafe fn remove_main_listener(worker_id: u64, ctx: *mut qjs::JSContext) {
    MAIN_WORKER_MSG_LISTENERS.with(|m| {
        if let Some(old) = m.borrow_mut().remove(&worker_id) {
            js_free_value(ctx, old);
        }
    });
    MAIN_WORKER_MSG_ONCE.with(|m| {
        m.borrow_mut().remove(&worker_id);
    });
    MAIN_WORKER_EXIT_LISTENERS.with(|m| {
        if let Some(old) = m.borrow_mut().remove(&worker_id) {
            js_free_value(ctx, old.cb);
        }
    });
    MAIN_WORKER_EXIT_ONCE_LISTENERS.with(|m| {
        if let Some(old) = m.borrow_mut().remove(&worker_id) {
            js_free_value(ctx, old);
        }
    });
    MAIN_WORKER_PENDING_MSGS.with(|m| {
        m.borrow_mut().remove(&worker_id);
    });
}

/// Deliver a message from worker to main: invoke the Worker's `message` listener.
pub unsafe fn dispatch_worker_post_to_main(
    ctx: *mut qjs::JSContext,
    worker_id: u64,
    json: &str,
) -> Result<(), String> {
    let has_listener = MAIN_WORKER_MSG_LISTENERS.with(|m| m.borrow().contains_key(&worker_id));
    if !has_listener {
        MAIN_WORKER_PENDING_MSGS.with(|m| {
            m.borrow_mut()
                .entry(worker_id)
                .or_default()
                .push_back(json.to_string());
        });
        return Ok(());
    }
    let val = payload_to_js_value(ctx, json)?;
    let is_once =
        MAIN_WORKER_MSG_ONCE.with(|m| m.borrow().get(&worker_id).copied().unwrap_or(false));
    let cb = if is_once {
        MAIN_WORKER_MSG_ONCE.with(|m| {
            m.borrow_mut().remove(&worker_id);
        });
        MAIN_WORKER_MSG_LISTENERS.with(|m| m.borrow_mut().remove(&worker_id))
    } else {
        MAIN_WORKER_MSG_LISTENERS.with(|m| m.borrow().get(&worker_id).map(|cbv| js_dup_value(*cbv)))
    };
    let Some(cb) = cb else {
        js_free_value(ctx, val);
        return Ok(());
    };
    let mut args = [val];
    let r = qjs::JS_Call(ctx, cb, js_undefined(), 1, args.as_mut_ptr());
    js_free_value(ctx, cb);
    js_free_value(ctx, val);
    if is_exception(r) {
        let exc = qjs::JS_GetException(ctx);
        let msg = js_string_to_owned(ctx, exc);
        js_free_value(ctx, exc);
        js_free_value(ctx, r);
        return Err(msg);
    }
    js_free_value(ctx, r);
    Ok(())
}

unsafe fn flush_main_worker_message_mailbox(ctx: *mut qjs::JSContext, worker_id: u64) {
    loop {
        let has_listener = MAIN_WORKER_MSG_LISTENERS.with(|m| m.borrow().contains_key(&worker_id));
        if !has_listener {
            break;
        }
        let next = MAIN_WORKER_PENDING_MSGS.with(|m| {
            let mut g = m.borrow_mut();
            let q = g.get_mut(&worker_id)?;
            let msg = q.pop_front();
            if q.is_empty() {
                g.remove(&worker_id);
            }
            msg
        });
        let Some(json) = next else {
            break;
        };
        if let Err(e) = dispatch_worker_post_to_main(ctx, worker_id, &json) {
            tracing::warn!(worker_id, error = %e, "worker postMessage delivery failed");
        }
    }
}

unsafe extern "C" fn worker_main_mailbox_job(
    ctx: *mut qjs::JSContext,
    _argc: std::os::raw::c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    loop {
        let next = MAIN_WORKER_MAILBOX.with(|q| q.borrow_mut().pop_front());
        let Some((wid, json)) = next else {
            break;
        };
        if let Err(e) = dispatch_worker_post_to_main(ctx, wid, &json) {
            tracing::warn!(worker_id = wid, error = %e, "worker postMessage to main failed");
        }
    }
    js_undefined()
}

/// Queue a worker → main payload and deliver it from a QuickJS job (avoids re-entrant `JS_Call`).
pub unsafe fn dispatch_worker_post_to_main_deferred(
    ctx: *mut qjs::JSContext,
    worker_id: u64,
    json: String,
) {
    MAIN_WORKER_MAILBOX.with(|q| q.borrow_mut().push_back((worker_id, json)));
    if qjs::JS_EnqueueJob(ctx, Some(worker_main_mailbox_job), 0, std::ptr::null_mut()) != 0 {
        tracing::warn!("JS_EnqueueJob failed for worker postMessage");
    }
}

/// Deliver a message from main to worker: invoke `parentPort` `message` listeners (Node order).
pub unsafe fn dispatch_worker_post_from_main(
    ctx: *mut qjs::JSContext,
    json: &str,
) -> Result<(), String> {
    if !worker_parent_has_message_listener() {
        WORKER_PARENT_MAILBOX.with(|q| q.borrow_mut().push_back(json.to_string()));
        return Ok(());
    }
    let val = payload_to_js_value(ctx, json)?;
    let snapshot: Vec<(u64, qjs::JSValue, bool)> = WORKER_PARENT_MESSAGE_LISTENERS.with(|v| {
        v.borrow()
            .iter()
            .map(|e| (e.id, js_dup_value(e.cb), e.once))
            .collect()
    });
    for (listener_id, invoke_cb, once) in snapshot {
        let arg = js_dup_value(val);
        let mut args = [arg];
        let r = qjs::JS_Call(ctx, invoke_cb, js_undefined(), 1, args.as_mut_ptr());
        let call_exc = is_exception(r);
        js_free_value(ctx, arg);
        if call_exc {
            js_free_value(ctx, invoke_cb);
            let exc = qjs::JS_GetException(ctx);
            let msg = js_string_to_owned(ctx, exc);
            js_free_value(ctx, exc);
            js_free_value(ctx, r);
            js_free_value(ctx, val);
            return Err(msg);
        }
        js_free_value(ctx, r);
        if once {
            WORKER_PARENT_MESSAGE_LISTENERS.with(|v| {
                let mut g = v.borrow_mut();
                if let Some(p) = g.iter().position(|e| e.id == listener_id) {
                    let ent = g.remove(p);
                    js_free_value(ctx, ent.cb);
                }
            });
        }
        js_free_value(ctx, invoke_cb);
    }
    js_free_value(ctx, val);
    Ok(())
}

pub unsafe fn flush_worker_parent_mailbox(ctx: *mut qjs::JSContext) {
    loop {
        if !worker_parent_has_message_listener() {
            break;
        }
        let next = WORKER_PARENT_MAILBOX.with(|q| q.borrow_mut().pop_front());
        let Some(json) = next else {
            break;
        };
        if let Err(e) = dispatch_worker_post_from_main(ctx, &json) {
            tracing::warn!(error = %e, "worker parentPort message delivery failed");
        }
    }
}

pub unsafe fn set_main_worker_message_listener(
    ctx: *mut qjs::JSContext,
    worker_id: u64,
    cb: qjs::JSValue,
    once: bool,
) {
    MAIN_WORKER_MSG_LISTENERS.with(|m| {
        let mut g = m.borrow_mut();
        if let Some(old) = g.insert(worker_id, js_dup_value(cb)) {
            js_free_value(ctx, old);
        }
    });
    MAIN_WORKER_MSG_ONCE.with(|m| {
        m.borrow_mut().insert(worker_id, once);
    });
    flush_main_worker_message_mailbox(ctx, worker_id);
}

pub unsafe fn remove_main_worker_message_listener(
    ctx: *mut qjs::JSContext,
    worker_id: u64,
    target: Option<qjs::JSValue>,
) {
    let should_remove = MAIN_WORKER_MSG_LISTENERS.with(|m| {
        let g = m.borrow();
        let Some(current) = g.get(&worker_id) else {
            return false;
        };
        match target {
            Some(t) => qjs::JS_StrictEq(ctx, *current, t) != 0,
            None => true,
        }
    });
    if !should_remove {
        return;
    }
    MAIN_WORKER_MSG_LISTENERS.with(|m| {
        if let Some(old) = m.borrow_mut().remove(&worker_id) {
            js_free_value(ctx, old);
        }
    });
    MAIN_WORKER_MSG_ONCE.with(|m| {
        m.borrow_mut().remove(&worker_id);
    });
    MAIN_WORKER_PENDING_MSGS.with(|m| {
        m.borrow_mut().remove(&worker_id);
    });
}

pub unsafe fn set_main_worker_exit_listener(
    ctx: *mut qjs::JSContext,
    worker_id: u64,
    cb: qjs::JSValue,
    once: bool,
) {
    if once {
        MAIN_WORKER_EXIT_ONCE_LISTENERS.with(|m| {
            let mut g = m.borrow_mut();
            if let Some(old) = g.insert(worker_id, js_dup_value(cb)) {
                js_free_value(ctx, old);
            }
        });
    } else {
        MAIN_WORKER_EXIT_LISTENERS.with(|m| {
            let mut g = m.borrow_mut();
            if let Some(old) = g.insert(
                worker_id,
                ExitListener {
                    cb: js_dup_value(cb),
                },
            ) {
                js_free_value(ctx, old.cb);
            }
        });
    }
}

pub unsafe fn remove_main_worker_exit_listener(
    ctx: *mut qjs::JSContext,
    worker_id: u64,
    once: bool,
    target: Option<qjs::JSValue>,
) {
    if once {
        let should_remove = MAIN_WORKER_EXIT_ONCE_LISTENERS.with(|m| {
            let g = m.borrow();
            let Some(current) = g.get(&worker_id) else {
                return false;
            };
            match target {
                Some(t) => qjs::JS_StrictEq(ctx, *current, t) != 0,
                None => true,
            }
        });
        if !should_remove {
            return;
        }
        MAIN_WORKER_EXIT_ONCE_LISTENERS.with(|m| {
            if let Some(old) = m.borrow_mut().remove(&worker_id) {
                js_free_value(ctx, old);
            }
        });
    } else {
        let should_remove = MAIN_WORKER_EXIT_LISTENERS.with(|m| {
            let g = m.borrow();
            let Some(current) = g.get(&worker_id) else {
                return false;
            };
            match target {
                Some(t) => qjs::JS_StrictEq(ctx, current.cb, t) != 0,
                None => true,
            }
        });
        if !should_remove {
            return;
        }
        MAIN_WORKER_EXIT_LISTENERS.with(|m| {
            if let Some(old) = m.borrow_mut().remove(&worker_id) {
                js_free_value(ctx, old.cb);
            }
        });
    }
}

pub fn main_worker_listener_count(worker_id: u64, event: &str) -> usize {
    match event {
        "message" => {
            MAIN_WORKER_MSG_LISTENERS.with(|m| usize::from(m.borrow().contains_key(&worker_id)))
        }
        "exit" => {
            let on_count = MAIN_WORKER_EXIT_LISTENERS
                .with(|m| usize::from(m.borrow().contains_key(&worker_id)));
            let once_count = MAIN_WORKER_EXIT_ONCE_LISTENERS
                .with(|m| usize::from(m.borrow().contains_key(&worker_id)));
            on_count + once_count
        }
        _ => 0,
    }
}

pub fn main_worker_has_message_listener(worker_id: u64) -> bool {
    MAIN_WORKER_MSG_LISTENERS.with(|m| m.borrow().contains_key(&worker_id))
}

pub fn main_worker_has_exit_listener(worker_id: u64) -> bool {
    let has_on = MAIN_WORKER_EXIT_LISTENERS.with(|m| m.borrow().contains_key(&worker_id));
    let has_once = MAIN_WORKER_EXIT_ONCE_LISTENERS.with(|m| m.borrow().contains_key(&worker_id));
    has_on || has_once
}

pub unsafe fn main_worker_get_message_listener(
    _ctx: *mut qjs::JSContext,
    worker_id: u64,
) -> Option<qjs::JSValue> {
    MAIN_WORKER_MSG_LISTENERS.with(|m| m.borrow().get(&worker_id).map(|v| js_dup_value(*v)))
}

pub unsafe fn main_worker_get_exit_listener(
    _ctx: *mut qjs::JSContext,
    worker_id: u64,
) -> Option<qjs::JSValue> {
    let on =
        MAIN_WORKER_EXIT_LISTENERS.with(|m| m.borrow().get(&worker_id).map(|v| js_dup_value(v.cb)));
    if on.is_some() {
        return on;
    }
    MAIN_WORKER_EXIT_ONCE_LISTENERS.with(|m| m.borrow().get(&worker_id).map(|v| js_dup_value(*v)))
}

pub unsafe fn dispatch_worker_exit_to_main(
    ctx: *mut qjs::JSContext,
    worker_id: u64,
    exit_code: i32,
) -> Result<(), String> {
    let cb = MAIN_WORKER_EXIT_LISTENERS.with(|m| {
        m.borrow()
            .get(&worker_id)
            .map(|listener| js_dup_value(listener.cb))
    });
    let cb_once = MAIN_WORKER_EXIT_ONCE_LISTENERS.with(|m| m.borrow_mut().remove(&worker_id));
    if cb.is_none() && cb_once.is_none() {
        return Ok(());
    }
    let code = qjs_compat::new_int(ctx, exit_code as i64);
    let mut first_err: Option<String> = None;
    if let Some(cbv) = cb {
        let arg = js_dup_value(code);
        let mut args = [arg];
        let r = qjs::JS_Call(ctx, cbv, js_undefined(), 1, args.as_mut_ptr());
        js_free_value(ctx, cbv);
        js_free_value(ctx, arg);
        if is_exception(r) && first_err.is_none() {
            let exc = qjs::JS_GetException(ctx);
            let msg = js_string_to_owned(ctx, exc);
            js_free_value(ctx, exc);
            first_err = Some(msg);
        }
        js_free_value(ctx, r);
    }
    if let Some(cbv_once) = cb_once {
        let arg = js_dup_value(code);
        let mut args = [arg];
        let r = qjs::JS_Call(ctx, cbv_once, js_undefined(), 1, args.as_mut_ptr());
        js_free_value(ctx, cbv_once);
        js_free_value(ctx, arg);
        if is_exception(r) && first_err.is_none() {
            let exc = qjs::JS_GetException(ctx);
            let msg = js_string_to_owned(ctx, exc);
            js_free_value(ctx, exc);
            first_err = Some(msg);
        }
        js_free_value(ctx, r);
    }
    js_free_value(ctx, code);
    if let Some(e) = first_err {
        return Err(e);
    }
    Ok(())
}

pub unsafe fn set_worker_parent_message_listener(
    ctx: *mut qjs::JSContext,
    cb: qjs::JSValue,
    once: bool,
    prepend: bool,
) {
    let entry = WorkerParentMessageListener {
        id: NEXT_WORKER_PARENT_LISTENER_ID.fetch_add(1, Ordering::Relaxed),
        cb: js_dup_value(cb),
        once,
    };
    WORKER_PARENT_MESSAGE_LISTENERS.with(|v| {
        let mut g = v.borrow_mut();
        if prepend {
            g.insert(0, entry);
        } else {
            g.push(entry);
        }
    });
    flush_worker_parent_mailbox(ctx);
}

pub unsafe fn remove_worker_parent_message_listener(
    ctx: *mut qjs::JSContext,
    target: Option<qjs::JSValue>,
) {
    WORKER_PARENT_MESSAGE_LISTENERS.with(|v| {
        let mut g = v.borrow_mut();
        match target {
            None => {
                for e in g.drain(..) {
                    js_free_value(ctx, e.cb);
                }
            }
            Some(t) => {
                if let Some(pos) = g.iter().position(|e| qjs::JS_StrictEq(ctx, e.cb, t) != 0) {
                    let ent = g.remove(pos);
                    js_free_value(ctx, ent.cb);
                }
            }
        }
    });
}

pub fn worker_parent_listener_count(event: &str) -> usize {
    if event != "message" {
        return 0;
    }
    WORKER_PARENT_MESSAGE_LISTENERS.with(|v| v.borrow().len())
}

pub fn worker_parent_has_message_listener() -> bool {
    WORKER_PARENT_MESSAGE_LISTENERS.with(|v| !v.borrow().is_empty())
}

pub unsafe fn js_value_to_json_for_worker(
    ctx: *mut qjs::JSContext,
    v: qjs::JSValue,
) -> Result<String, String> {
    js_value_to_json_string(ctx, v)
}

pub unsafe fn payload_to_js_value_for_worker(
    ctx: *mut qjs::JSContext,
    payload: &str,
) -> Result<qjs::JSValue, String> {
    payload_to_js_value(ctx, payload)
}

pub fn create_local_message_channel_pair() -> Result<(u64, u64), String> {
    let p1 = NEXT_PORT_ID.fetch_add(1, Ordering::Relaxed);
    let p2 = NEXT_PORT_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut peers = port_peers_registry()
            .lock()
            .map_err(|_| "worker ports lock poisoned".to_string())?;
        peers.insert(p1, p2);
        peers.insert(p2, p1);
    }
    {
        let mut q = port_queues_registry()
            .lock()
            .map_err(|_| "worker port queues lock poisoned".to_string())?;
        q.entry(p1).or_default();
        q.entry(p2).or_default();
    }
    {
        let mut refs = port_ref_registry()
            .lock()
            .map_err(|_| "worker port ref-state lock poisoned".to_string())?;
        refs.insert(p1, true);
        refs.insert(p2, true);
    }
    Ok((p1, p2))
}

pub unsafe fn set_local_port_message_listener(
    ctx: *mut qjs::JSContext,
    port_id: u64,
    cb: qjs::JSValue,
) {
    WT_PORT_LISTENERS.with(|m| {
        let mut g = m.borrow_mut();
        if let Some(old) = g.insert(port_id, js_dup_value(cb)) {
            js_free_value(ctx, old);
        }
    });
}

pub unsafe fn remove_local_port_message_listener(
    ctx: *mut qjs::JSContext,
    port_id: u64,
    target: Option<qjs::JSValue>,
) {
    let should_remove = WT_PORT_LISTENERS.with(|m| {
        let g = m.borrow();
        let Some(current) = g.get(&port_id) else {
            return false;
        };
        match target {
            Some(t) => qjs::JS_StrictEq(ctx, *current, t) != 0,
            None => true,
        }
    });
    if !should_remove {
        return;
    }
    WT_PORT_LISTENERS.with(|m| {
        if let Some(old) = m.borrow_mut().remove(&port_id) {
            js_free_value(ctx, old);
        }
    });
}

pub unsafe fn local_port_post_message(
    ctx: *mut qjs::JSContext,
    from_port_id: u64,
    value: qjs::JSValue,
) -> Result<(), String> {
    let payload = js_value_to_json_string(ctx, value)?;
    let peer_id = {
        let peers = port_peers_registry()
            .lock()
            .map_err(|_| "worker ports lock poisoned".to_string())?;
        peers
            .get(&from_port_id)
            .copied()
            .ok_or_else(|| "unknown message port".to_string())?
    };
    {
        let mut q = port_queues_registry()
            .lock()
            .map_err(|_| "worker port queues lock poisoned".to_string())?;
        q.entry(peer_id).or_default().push_back(payload);
    }
    flush_local_port_listener(ctx, peer_id)?;
    Ok(())
}

unsafe fn flush_local_port_listener(ctx: *mut qjs::JSContext, port_id: u64) -> Result<(), String> {
    let cb = WT_PORT_LISTENERS.with(|m| m.borrow().get(&port_id).map(|v| js_dup_value(*v)));
    let Some(cbv) = cb else {
        return Ok(());
    };
    loop {
        let next = {
            let mut q = port_queues_registry()
                .lock()
                .map_err(|_| "worker port queues lock poisoned".to_string())?;
            q.get_mut(&port_id).and_then(|d| d.pop_front())
        };
        let Some(payload) = next else {
            break;
        };
        let val = payload_to_js_value(ctx, &payload)?;
        let mut args = [val];
        let r = qjs::JS_Call(ctx, cbv, js_undefined(), 1, args.as_mut_ptr());
        js_free_value(ctx, val);
        if is_exception(r) {
            let exc = qjs::JS_GetException(ctx);
            let msg = js_string_to_owned(ctx, exc);
            js_free_value(ctx, exc);
            js_free_value(ctx, r);
            js_free_value(ctx, cbv);
            return Err(msg);
        }
        js_free_value(ctx, r);
    }
    js_free_value(ctx, cbv);
    Ok(())
}

pub unsafe fn local_port_receive_message(
    ctx: *mut qjs::JSContext,
    port_id: u64,
) -> Result<Option<qjs::JSValue>, String> {
    let next = {
        let mut q = port_queues_registry()
            .lock()
            .map_err(|_| "worker port queues lock poisoned".to_string())?;
        q.get_mut(&port_id).and_then(|d| d.pop_front())
    };
    let Some(payload) = next else {
        return Ok(None);
    };
    let v = payload_to_js_value(ctx, &payload)?;
    Ok(Some(v))
}

pub fn set_local_port_has_ref(port_id: u64, value: bool) -> Result<(), String> {
    let mut refs = port_ref_registry()
        .lock()
        .map_err(|_| "worker port ref-state lock poisoned".to_string())?;
    refs.insert(port_id, value);
    Ok(())
}

pub fn local_port_has_ref(port_id: u64) -> Result<bool, String> {
    let refs = port_ref_registry()
        .lock()
        .map_err(|_| "worker port ref-state lock poisoned".to_string())?;
    Ok(refs.get(&port_id).copied().unwrap_or(true))
}

pub unsafe fn set_worker_environment_data(
    ctx: *mut qjs::JSContext,
    key: qjs::JSValue,
    value: qjs::JSValue,
) -> Result<(), String> {
    let key_ser = js_value_to_json_string(ctx, key)?;
    let mut g = env_data_registry()
        .lock()
        .map_err(|_| "worker env-data lock poisoned".to_string())?;
    if qjs::JS_IsUndefined(value) {
        g.remove(&key_ser);
    } else {
        let val_ser = js_value_to_json_string(ctx, value)?;
        g.insert(key_ser, val_ser);
    }
    Ok(())
}

pub unsafe fn get_worker_environment_data(
    ctx: *mut qjs::JSContext,
    key: qjs::JSValue,
) -> Result<Option<qjs::JSValue>, String> {
    let key_ser = js_value_to_json_string(ctx, key)?;
    let val_ser = {
        let g = env_data_registry()
            .lock()
            .map_err(|_| "worker env-data lock poisoned".to_string())?;
        g.get(&key_ser).cloned()
    };
    let Some(stored) = val_ser else {
        return Ok(None);
    };
    let out = payload_to_js_value(ctx, &stored)?;
    Ok(Some(out))
}
