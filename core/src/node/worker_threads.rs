//! OS-thread `worker_threads`: one QuickJS isolate per worker, JSON `postMessage` over channels.

use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

use quickjs_sys as qjs;
use crate::console;
use crate::event_loop::{Task, TaskSender};
use crate::ffi::{js_dup_value, js_free_value, js_string_to_owned};
use crate::isolate::IsolateConfig;
use crate::node::{install_runtime_with_embed, is_exception, js_undefined};
use crate::qjs_compat;

/// How the runtime was embedded: main program vs `worker_threads` worker isolate.
#[derive(Clone)]
pub enum RuntimeEmbed {
    Main,
    Worker {
        worker_id: u64,
        main_task_tx: TaskSender,
    },
}

thread_local! {
    static RUNTIME_EMBED: std::cell::RefCell<Option<RuntimeEmbed>> = const { std::cell::RefCell::new(None) };
    /// `postMessage` listener on a worker isolate (`parentPort.on('message', fn)`).
    static WORKER_PARENT_MESSAGE_CB: std::cell::RefCell<Option<qjs::JSValue>> =
        const { std::cell::RefCell::new(None) };
}

static NEXT_WORKER_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Main-isolate thread only: `Worker#on('message')` callbacks (not `Send` — holds `JSValue`).
    static MAIN_WORKER_MSG_LISTENERS: std::cell::RefCell<HashMap<u64, qjs::JSValue>> =
        std::cell::RefCell::new(HashMap::new());
    /// Main thread: worker → main payloads drained by a QuickJS job (avoids re-entrant `JS_Call`).
    static MAIN_WORKER_MAILBOX: std::cell::RefCell<VecDeque<(u64, String)>> =
        std::cell::RefCell::new(VecDeque::new());
}

struct WorkerEntry {
    to_worker: mpsc::Sender<Task>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

static WORKER_REGISTRY: OnceLock<Mutex<HashMap<u64, WorkerEntry>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<u64, WorkerEntry>> {
    WORKER_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
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
            let res = worker_run_blocking(
                worker_id,
                main_tx_clone,
                path_str,
                reg_tx,
                cancel_thread,
            );
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
    let mut isolate = crate::isolate::Isolate::new(IsolateConfig::default()).map_err(|e| e.to_string())?;
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

    let src = std::fs::read_to_string(&path_str)
        .map_err(|e| format!("worker read {}: {e}", path_str))?;
    let transpiled = if path_str.ends_with(".ts") || path_str.ends_with(".tsx") {
        crate::transpiler::transpile_ts(&src, &path_str).unwrap_or(src)
    } else {
        src
    };
    let body = format!("var Buffer = globalThis.Buffer;\n{transpiled}");
    let wrapper = format!(
        "(function(exports, require, module, __filename, __dirname) {{\n{body}\n}})"
    );
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
        c_src.as_bytes().len().saturating_sub(1),
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
    let require_fn = qjs::JS_GetPropertyStr(
        ctx,
        global,
        CString::new("require").unwrap().as_ptr(),
    );
    let filename_val = qjs_compat::new_string_from_cstr(
        ctx,
        CString::new(filename).unwrap().as_ptr(),
    );
    let dir = Path::new(filename)
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy();
    let dirname_val =
        qjs_compat::new_string_from_cstr(ctx, CString::new(dir.as_ref()).unwrap().as_ptr());
    let exports_arg = js_dup_value(exports_obj);
    let module_arg = js_dup_value(module_obj);
    let mut args = [exports_arg, require_fn, module_arg, filename_val, dirname_val];
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

unsafe fn js_value_to_json_string(ctx: *mut qjs::JSContext, v: qjs::JSValue) -> Result<String, String> {
    let global = qjs::JS_GetGlobalObject(ctx);
    let json_o = qjs::JS_GetPropertyStr(ctx, global, CString::new("JSON").unwrap().as_ptr());
    let stringify = qjs::JS_GetPropertyStr(ctx, json_o, CString::new("stringify").unwrap().as_ptr());
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
    Ok(s)
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
}

pub unsafe fn remove_main_listener(worker_id: u64, ctx: *mut qjs::JSContext) {
    MAIN_WORKER_MSG_LISTENERS.with(|m| {
        if let Some(old) = m.borrow_mut().remove(&worker_id) {
            js_free_value(ctx, old);
        }
    });
}

/// Deliver a message from worker to main: invoke the Worker's `message` listener.
pub unsafe fn dispatch_worker_post_to_main(
    ctx: *mut qjs::JSContext,
    worker_id: u64,
    json: &str,
) -> Result<(), String> {
    let label = CString::new("kawkab:worker-msg").unwrap_or_default();
    let val = qjs::JS_ParseJSON(
        ctx,
        json.as_ptr() as *const _,
        json.len(),
        label.as_ptr(),
    );
    if is_exception(val) {
        js_free_value(ctx, val);
        return Err("invalid JSON in worker postMessage".into());
    }
    let cb = MAIN_WORKER_MSG_LISTENERS.with(|m| {
        m.borrow()
            .get(&worker_id)
            .map(|cbv| js_dup_value(*cbv))
    });
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

/// Deliver a message from main to worker: invoke `parentPort` `message` listener.
pub unsafe fn dispatch_worker_post_from_main(
    ctx: *mut qjs::JSContext,
    json: &str,
) -> Result<(), String> {
    let label = CString::new("kawkab:worker-msg").unwrap_or_default();
    let val = qjs::JS_ParseJSON(
        ctx,
        json.as_ptr() as *const _,
        json.len(),
        label.as_ptr(),
    );
    if is_exception(val) {
        js_free_value(ctx, val);
        return Err("invalid JSON in main postMessage to worker".into());
    }
    let cb = WORKER_PARENT_MESSAGE_CB.with(|c| {
        let b = c.borrow();
        b.as_ref().map(|cbv| js_dup_value(*cbv))
    });
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

pub unsafe fn set_main_worker_message_listener(
    ctx: *mut qjs::JSContext,
    worker_id: u64,
    cb: qjs::JSValue,
) {
    MAIN_WORKER_MSG_LISTENERS.with(|m| {
        let mut g = m.borrow_mut();
        if let Some(old) = g.insert(worker_id, js_dup_value(cb)) {
            js_free_value(ctx, old);
        }
    });
}

pub unsafe fn set_worker_parent_message_listener(ctx: *mut qjs::JSContext, cb: qjs::JSValue) {
    WORKER_PARENT_MESSAGE_CB.with(|slot| {
        let mut w = slot.borrow_mut();
        if let Some(old) = w.take() {
            js_free_value(ctx, old);
        }
        *w = Some(js_dup_value(cb));
    });
}

pub unsafe fn js_value_to_json_for_worker(ctx: *mut qjs::JSContext, v: qjs::JSValue) -> Result<String, String> {
    js_value_to_json_string(ctx, v)
}
