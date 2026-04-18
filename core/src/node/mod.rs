use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use quickjs_sys as qjs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener as TokioTcpListener;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use crate::error::JsError;
use crate::event_loop::TaskSender;
use crate::ffi::{js_dup_value, js_free_value, js_string_to_owned};
use crate::node::module_loader::resolve_module_path;
use crate::node::runtime_policy::RuntimePolicy;
use crate::qjs_compat;

mod buffer;
mod crypto;
mod esm_loader;
pub mod module_loader;
mod process;
mod runtime_policy;

#[cfg(test)]
mod compat_contract_tests;

pub use esm_loader::{clear_module_caches, eval_esm_entry};

struct HttpListenEntry {
    server_obj: qjs::JSValue,
    shutdown: Arc<tokio::sync::Notify>,
}

struct PendingTimer {
    callback: qjs::JSValue,
    this_val: qjs::JSValue,
    args: Vec<qjs::JSValue>,
    cancelled: Arc<AtomicBool>,
    /// `Some(ms)` for `setInterval`; `None` for `setTimeout` / `setImmediate`.
    repeat_ms: Option<u64>,
}

thread_local! {
    static REQUIRE_BASE_DIR: RefCell<String> = const { RefCell::new(String::new()) };
    static MODULE_SOURCE_CACHE: RefCell<std::collections::HashMap<String, String>> = RefCell::new(std::collections::HashMap::new());
    static TASK_SENDER_SLOT: RefCell<Option<TaskSender>> = const { RefCell::new(None) };
    static HTTP_LISTEN_REGISTRY: RefCell<HashMap<u64, HttpListenEntry>> = RefCell::new(HashMap::new());
    static TIMER_REGISTRY: RefCell<HashMap<u64, PendingTimer>> = RefCell::new(HashMap::new());
    /// Lets `clearTimeout`/`clearInterval` cancel during callback execution.
    static TIMER_CANCEL_BY_ID: RefCell<HashMap<u64, Arc<AtomicBool>>> = RefCell::new(HashMap::new());
    /// Tokio HTTP path: staged response bytes for ordered socket `write_all`.
    static HTTP_RESPONSE_WIRE_TX: RefCell<Option<mpsc::UnboundedSender<Vec<u8>>>> = RefCell::new(None);
    /// Buffered `res.write`/`res.end` bytes keyed by `__kawkabBodyAccumId`.
    static HTTP_BODY_ACCUM: RefCell<HashMap<u64, Vec<u8>>> = RefCell::new(HashMap::new());
    /// Bound UDP sockets keyed by runtime socket id.
    static DGRAM_NATIVE_SOCKET_REGISTRY: RefCell<HashMap<u64, Arc<UdpSocket>>> = RefCell::new(HashMap::new());
    /// Receiver-loop cancellation flags keyed by runtime socket id.
    static DGRAM_RECV_CANCEL_BY_ID: RefCell<HashMap<u64, Arc<AtomicBool>>> = RefCell::new(HashMap::new());
}

/// Restore [`REQUIRE_BASE_DIR`] after nested `require()` changes it.
struct RequireBaseDirGuard {
    previous: String,
}

impl Drop for RequireBaseDirGuard {
    fn drop(&mut self) {
        REQUIRE_BASE_DIR.with(|v| *v.borrow_mut() = self.previous.clone());
    }
}

/// Keep `package.json` `development` / `production` conditions aligned with `process.env.NODE_ENV`.
pub(crate) unsafe fn refresh_package_exports_node_env(ctx: *mut qjs::JSContext) {
    let global = qjs::JS_GetGlobalObject(ctx);
    let proc_val = qjs::JS_GetPropertyStr(ctx, global, CString::new("process").unwrap().as_ptr());
    js_free_value(ctx, global);
    if qjs::JS_IsUndefined(proc_val) {
        js_free_value(ctx, proc_val);
        return;
    }
    let env_val = qjs::JS_GetPropertyStr(ctx, proc_val, CString::new("env").unwrap().as_ptr());
    js_free_value(ctx, proc_val);
    let ne_val = qjs::JS_GetPropertyStr(ctx, env_val, CString::new("NODE_ENV").unwrap().as_ptr());
    js_free_value(ctx, env_val);
    let s = if qjs::JS_IsUndefined(ne_val) || qjs::JS_IsNull(ne_val) {
        js_free_value(ctx, ne_val);
        "production".to_string()
    } else {
        let out = js_string_to_owned(ctx, ne_val);
        js_free_value(ctx, ne_val);
        if out.is_empty() {
            "production".to_string()
        } else {
            out
        }
    };
    module_loader::set_package_exports_node_env_from_process(s);
}

static NEXT_HTTP_LISTEN_ID: AtomicU64 = AtomicU64::new(1);

static HTTP_BLOCKING_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

fn http_blocking_client() -> &'static reqwest::blocking::Client {
    HTTP_BLOCKING_CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest blocking client")
    })
}
static NEXT_TIMER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_HTTP_BODY_ACCUM_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_DGRAM_SOCKET_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_PROMISE_ID: AtomicU64 = AtomicU64::new(1);

/// Count of async timers that have been scheduled but not yet fired/cancelled.
/// Used by the CLI runner to decide when to exit.
pub static PENDING_ASYNC_TIMERS: AtomicU64 = AtomicU64::new(0);

/// Pending `fs.promises` (and similar) work tracked until the Promise settles.
/// The CLI runner waits on `PENDING_ASYNC_TIMERS + PENDING_HOST_ASYNC`.
pub static PENDING_HOST_ASYNC: AtomicU64 = AtomicU64::new(0);

enum HostPendingPromise {
    Capability {
        resolve: qjs::JSValue,
        reject: qjs::JSValue,
    },
    Callback {
        cb: qjs::JSValue,
    },
}

thread_local! {
    static HOST_PENDING_PROMISES: RefCell<HashMap<u64, HostPendingPromise>> =
        RefCell::new(HashMap::new());
}

static PROCESS_HRTIME_START: OnceLock<std::time::Instant> = OnceLock::new();

static PERF_NAV_START_MONO: OnceLock<std::time::Instant> = OnceLock::new();
static PERF_TIME_ORIGIN_MS: OnceLock<f64> = OnceLock::new();

#[inline]
fn performance_ensure_init() {
    PERF_NAV_START_MONO.get_or_init(std::time::Instant::now);
    PERF_TIME_ORIGIN_MS.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0)
    });
}

#[inline]
fn process_hrtime_start() -> std::time::Instant {
    *PROCESS_HRTIME_START.get_or_init(std::time::Instant::now)
}

#[inline]
fn hrtime_tuple_now() -> (u64, u32) {
    let e = process_hrtime_start().elapsed();
    (e.as_secs(), e.subsec_nanos())
}

#[inline]
fn hrtime_total_ns(sec: u64, nsec: u32) -> u128 {
    (sec as u128) * 1_000_000_000u128 + (nsec as u128)
}

unsafe fn js_hrtime_pair_array(ctx: *mut qjs::JSContext, sec: u64, nsec: u32) -> qjs::JSValue {
    let arr = qjs::JS_NewArray(ctx);
    let a = qjs::JS_NewFloat64(ctx, sec as f64);
    let b = qjs::JS_NewFloat64(ctx, nsec as f64);
    qjs::JS_SetPropertyUint32(ctx, arr, 0, a);
    qjs::JS_SetPropertyUint32(ctx, arr, 1, b);
    arr
}

#[inline]
fn deferred_host_tasks_ready() -> bool {
    TASK_SENDER_SLOT.with(|s| s.borrow().is_some()) && Handle::try_current().is_ok()
}

pub unsafe fn install_runtime(
    ctx: *mut qjs::JSContext,
    entry_filename: &str,
    task_sender: Option<TaskSender>,
) -> Result<(), String> {
    TASK_SENDER_SLOT.with(|s| *s.borrow_mut() = task_sender);
    let entry_path = PathBuf::from(entry_filename);
    let base_dir = entry_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    REQUIRE_BASE_DIR.with(|v| *v.borrow_mut() = base_dir.clone());

    let rt = qjs::JS_GetRuntime(ctx);
    esm_loader::install_module_loader(rt);

    let global = qjs::JS_GetGlobalObject(ctx);
    install_c_fn(ctx, global, "__kawkabReadFile", Some(js_read_file_sync), 1)?;
    install_c_fn(
        ctx,
        global,
        "__kawkabWriteFile",
        Some(js_write_file_sync),
        2,
    )?;
    install_c_fn(ctx, global, "__kawkabResolvePath", Some(js_resolve_path), 2)?;
    install_c_fn(ctx, global, "__kawkabCwd", Some(js_get_cwd), 0)?;
    install_c_fn(ctx, global, "__kawkabPlatform", Some(js_get_platform), 0)?;
    install_c_fn(ctx, global, "__kawkabRunCommand", Some(js_run_command), 1)?;
    install_c_fn(ctx, global, "__kawkabSleepMs", Some(js_sleep_ms), 1)?;
    install_c_fn(
        ctx,
        global,
        "__kawkabNextPromiseId",
        Some(js_next_promise_id),
        0,
    )?;
    install_c_fn(
        ctx,
        global,
        "__kawkab_register_promise_callback",
        Some(js_kawkab_register_promise_callback),
        2,
    )?;

    install_c_fn(
        ctx,
        global,
        "__kawkabCryptoRandomBytes",
        Some(js_crypto_random_bytes),
        2,
    )?;
    install_c_fn(
        ctx,
        global,
        "__kawkabCryptoRandomBytesSync",
        Some(js_crypto_random_bytes_sync),
        1,
    )?;
    install_c_fn(
        ctx,
        global,
        "__kawkabCryptoCreateHash",
        Some(js_crypto_create_hash),
        1,
    )?;
    install_c_fn(
        ctx,
        global,
        "__kawkabCryptoCreateHmac",
        Some(js_crypto_create_hmac),
        2,
    )?;
    install_c_fn(
        ctx,
        global,
        "__kawkabCryptoUpdate",
        Some(js_crypto_update),
        2,
    )?;
    install_c_fn(
        ctx,
        global,
        "__kawkabCryptoDigest",
        Some(js_crypto_digest),
        1,
    )?;
    install_c_fn(ctx, global, "setTimeout", Some(js_set_timeout), 2)?;

    install_c_fn(ctx, global, "clearTimeout", Some(js_clear_timeout), 1)?;
    install_c_fn(ctx, global, "setInterval", Some(js_set_interval), 2)?;
    install_c_fn(ctx, global, "clearInterval", Some(js_clear_timeout), 1)?;
    install_c_fn(ctx, global, "setImmediate", Some(js_set_timeout), 1)?;
    install_c_fn(ctx, global, "clearImmediate", Some(js_clear_timeout), 1)?;
    install_c_fn(ctx, global, "queueMicrotask", Some(js_queue_microtask), 1)?;

    let process = qjs::JS_NewObject(ctx);
    let argv = qjs::JS_NewArray(ctx);
    for (i, arg) in std::env::args().enumerate() {
        qjs::JS_SetPropertyUint32(
            ctx,
            argv,
            i as u32,
            qjs_compat::new_string_from_str(ctx, &arg),
        );
    }
    qjs::JS_SetPropertyStr(ctx, process, CString::new("argv").unwrap().as_ptr(), argv);
    let env_obj = qjs::JS_NewObject(ctx);
    for (k, v) in std::env::vars() {
        if let Ok(ck) = CString::new(k) {
            qjs::JS_SetPropertyStr(
                ctx,
                env_obj,
                ck.as_ptr(),
                qjs_compat::new_string_from_str(ctx, &v),
            );
        }
    }
    qjs::JS_SetPropertyStr(ctx, process, CString::new("env").unwrap().as_ptr(), env_obj);
    set_str_prop(ctx, process, "version", "v0.0.0-kawkab")?;
    let versions = qjs::JS_NewObject(ctx);
    set_str_prop(ctx, versions, "kawkab", env!("CARGO_PKG_VERSION"))?;
    set_str_prop(ctx, versions, "node", "0.0.0-kawkab-shim")?;
    qjs::JS_SetPropertyStr(
        ctx,
        process,
        CString::new("versions").unwrap().as_ptr(),
        versions,
    );
    qjs::JS_SetPropertyStr(
        ctx,
        process,
        CString::new("platform").unwrap().as_ptr(),
        js_get_platform(ctx, js_undefined(), 0, std::ptr::null_mut()),
    );
    install_obj_fn(ctx, process, "cwd", Some(js_get_cwd), 0)?;
    install_obj_fn(ctx, process, "chdir", Some(js_process_chdir), 1)?;
    install_obj_fn(ctx, process, "nextTick", Some(js_process_next_tick), 1)?;
    install_obj_fn(ctx, process, "uptime", Some(js_process_uptime), 0)?;
    let hrtime_fn = qjs::JS_NewCFunction2(
        ctx,
        Some(js_process_hrtime),
        CString::new("hrtime").unwrap().as_ptr(),
        1,
        qjs::JSCFunctionEnum_JS_CFUNC_generic,
        0,
    );
    let hrtime_bigint_fn = qjs::JS_NewCFunction2(
        ctx,
        Some(js_process_hrtime_bigint),
        CString::new("bigint").unwrap().as_ptr(),
        0,
        qjs::JSCFunctionEnum_JS_CFUNC_generic,
        0,
    );
    qjs::JS_SetPropertyStr(
        ctx,
        hrtime_fn,
        CString::new("bigint").unwrap().as_ptr(),
        hrtime_bigint_fn,
    );
    qjs::JS_SetPropertyStr(
        ctx,
        process,
        CString::new("hrtime").unwrap().as_ptr(),
        hrtime_fn,
    );
    qjs::JS_SetPropertyStr(
        ctx,
        process,
        CString::new("exitCode").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, 0),
    );
    process::install_stdio(ctx, process)?;
    qjs::JS_SetPropertyStr(
        ctx,
        global,
        CString::new("process").unwrap().as_ptr(),
        process,
    );
    refresh_package_exports_node_env(ctx);

    module_loader::install_require(ctx, global, entry_filename, &base_dir)?;

    let kawkab = qjs::JS_NewObject(ctx);
    install_obj_fn(ctx, kawkab, "fastSum", Some(js_kawkab_fast_sum), 1)?;
    install_obj_fn(ctx, kawkab, "fastSumU32", Some(js_kawkab_fast_sum_u32), 1)?;
    install_obj_fn(ctx, kawkab, "fastSumF64", Some(js_kawkab_fast_sum_f64), 1)?;
    install_obj_fn(ctx, kawkab, "fastMapU32", Some(js_kawkab_fast_map_u32), 3)?;
    install_obj_fn(
        ctx,
        kawkab,
        "fastFilterU32",
        Some(js_kawkab_fast_filter_u32),
        2,
    )?;
    install_obj_fn(ctx, kawkab, "fastMapF64", Some(js_kawkab_fast_map_f64), 3)?;
    install_obj_fn(
        ctx,
        kawkab,
        "fastFilterF64",
        Some(js_kawkab_fast_filter_f64),
        2,
    )?;

    let vec_ns = qjs::JS_NewObject(ctx);
    let vec_u32 = qjs::JS_NewObject(ctx);
    install_obj_fn(ctx, vec_u32, "sum", Some(js_kawkab_fast_sum_u32), 1)?;
    install_obj_fn(ctx, vec_u32, "map", Some(js_kawkab_fast_map_u32), 3)?;
    install_obj_fn(ctx, vec_u32, "filter", Some(js_kawkab_fast_filter_u32), 2)?;
    qjs::JS_SetPropertyStr(ctx, vec_ns, CString::new("u32").unwrap().as_ptr(), vec_u32);

    let vec_f64 = qjs::JS_NewObject(ctx);
    install_obj_fn(ctx, vec_f64, "sum", Some(js_kawkab_fast_sum_f64), 1)?;
    install_obj_fn(ctx, vec_f64, "map", Some(js_kawkab_fast_map_f64), 3)?;
    install_obj_fn(ctx, vec_f64, "filter", Some(js_kawkab_fast_filter_f64), 2)?;
    qjs::JS_SetPropertyStr(ctx, vec_ns, CString::new("f64").unwrap().as_ptr(), vec_f64);

    qjs::JS_SetPropertyStr(ctx, kawkab, CString::new("vec").unwrap().as_ptr(), vec_ns);
    qjs::JS_SetPropertyStr(
        ctx,
        global,
        CString::new("kawkab").unwrap().as_ptr(),
        kawkab,
    );

    qjs::JS_SetPropertyStr(
        ctx,
        global,
        CString::new("module").unwrap().as_ptr(),
        qjs::JS_NewObject(ctx),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        global,
        CString::new("exports").unwrap().as_ptr(),
        qjs::JS_NewObject(ctx),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        global,
        CString::new("global").unwrap().as_ptr(),
        js_dup_value(global),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        global,
        CString::new("globalThis").unwrap().as_ptr(),
        js_dup_value(global),
    );
    install_web_compat_globals(ctx);
    install_performance_global(ctx, global)?;
    prime_assertion_error_ctor(ctx, global)?;
    install_util_promisify_global(ctx, global)?;

    // Prime `querystring` before `stream`: order matters for this QuickJS embedding.
    prime_eval_shim_exports(
        ctx,
        global,
        "__kawkabPrimedQuerystring",
        "querystring",
        QUERYSTRING_SHIM_SRC,
    )?;
    prime_eval_shim_exports(
        ctx,
        global,
        "__kawkabPrimedCrypto",
        "crypto",
        CRYPTO_SHIM_SRC,
    )?;
    prime_eval_shim_exports(ctx, global, "__kawkabPrimedUrl", "url", URL_SHIM_SRC)?;
    prime_timers_promises_exports_native(ctx, global)?;
    prime_eval_shim_exports(
        ctx,
        global,
        "__kawkabPrimedStream",
        "stream",
        STREAM_SHIM_SRC,
    )?;
    prime_eval_shim_exports(
        ctx,
        global,
        "__kawkabPrimedEvents",
        "events",
        EVENTS_SHIM_SRC,
    )?;
    patch_eventemitter_prototype(ctx, global)?;

    buffer::install(ctx, global)?;

    js_free_value(ctx, global);
    Ok(())
}

unsafe fn install_util_promisify_global(
    ctx: *mut qjs::JSContext,
    global: qjs::JSValue,
) -> Result<(), String> {
    // Do not assign via `globalThis` + free the eval result (refcount / compile corruption).
    let src = CString::new("(function(){return function(fn){return function(){var a=[].slice.call(arguments);return new Promise(function(r,j){a.push(function(e,x){if(e)j(e);else r(x);});fn.apply(this,a);});}};})()").map_err(|e| e.to_string())?;
    let file = CString::new("kawkab:util-promisify-init").map_err(|e| e.to_string())?;
    let promisify_impl = qjs_compat::eval(
        ctx,
        src.as_ptr(),
        src.as_bytes().len(),
        file.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    );
    if qjs::JS_IsException(promisify_impl) {
        js_free_value(ctx, promisify_impl);
        return Err("eval failed for util.promisify bootstrap".to_string());
    }
    let key = CString::new("__kawkabUtilPromisify").map_err(|e| e.to_string())?;
    if qjs::JS_SetPropertyStr(ctx, global, key.as_ptr(), promisify_impl) == 0 {
        js_free_value(ctx, promisify_impl);
        return Err("failed to set __kawkabUtilPromisify on global".to_string());
    }
    Ok(())
}

unsafe fn install_obj_fn(
    ctx: *mut qjs::JSContext,
    obj: qjs::JSValue,
    name: &str,
    func: qjs::JSCFunction,
    length: i32,
) -> Result<(), String> {
    let c_name = CString::new(name).map_err(|e| e.to_string())?;
    let value = qjs::JS_NewCFunction2(
        ctx,
        func,
        c_name.as_ptr(),
        length,
        qjs::JSCFunctionEnum_JS_CFUNC_generic,
        0,
    );
    qjs::JS_SetPropertyStr(ctx, obj, c_name.as_ptr(), value);
    Ok(())
}

unsafe fn install_c_fn(
    ctx: *mut qjs::JSContext,
    global: qjs::JSValue,
    name: &str,
    func: qjs::JSCFunction,
    length: i32,
) -> Result<(), String> {
    let c_name = CString::new(name).map_err(|e| e.to_string())?;
    let value = qjs::JS_NewCFunction2(
        ctx,
        func,
        c_name.as_ptr(),
        length,
        qjs::JSCFunctionEnum_JS_CFUNC_generic,
        0,
    );
    qjs::JS_SetPropertyStr(ctx, global, c_name.as_ptr(), value);
    Ok(())
}

pub(crate) unsafe fn bind_require_and_entry_paths(
    ctx: *mut qjs::JSContext,
    global: qjs::JSValue,
    entry_filename: &str,
    base_dir: &str,
) -> Result<(), String> {
    REQUIRE_BASE_DIR.with(|v| *v.borrow_mut() = base_dir.to_string());
    set_str_prop(ctx, global, "__kawkabEntryFile", entry_filename)?;
    set_str_prop(ctx, global, "__filename", entry_filename)?;
    set_str_prop(ctx, global, "__dirname", base_dir)?;
    install_c_fn(ctx, global, "require", Some(js_require), 1)?;
    Ok(())
}

unsafe fn set_str_prop(
    ctx: *mut qjs::JSContext,
    obj: qjs::JSValue,
    name: &str,
    value: &str,
) -> Result<(), String> {
    let k = CString::new(name).map_err(|e| e.to_string())?;
    let v = CString::new(value).map_err(|e| e.to_string())?;
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        k.as_ptr(),
        qjs_compat::new_string_from_cstr(ctx, v.as_ptr()),
    );
    Ok(())
}

unsafe fn eval_js_module(ctx: *mut qjs::JSContext, name: &str, src: &str) -> qjs::JSValue {
    let wrapped = format!("(function(){{ {} }})()", src);
    let file = CString::new(format!("kawkab:{name}")).unwrap_or_default();
    qjs_compat::eval(
        ctx,
        wrapped.as_ptr() as *const i8,
        wrapped.len(),
        file.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    )
}

/// JS source for `assert.AssertionError` (eval once during runtime install).
const ASSERTION_ERROR_CTOR_SRC: &str = "var AssertionError=function(o){if(!(this instanceof AssertionError))return new AssertionError(o);if(typeof o===\"string\"){Error.call(this,o);}else if(o&&typeof o===\"object\"){var m=o.message;Error.call(this,m!=null?String(m):\"\");if(\"actual\" in o)this.actual=o.actual;if(\"expected\" in o)this.expected=o.expected;if(\"operator\" in o)this.operator=o.operator;if(o.generatedMessage===false)this.generatedMessage=false;else this.generatedMessage=true;}else{Error.call(this,\"AssertionError\");}this.name=\"AssertionError\";this.code=\"ERR_ASSERTION\"};AssertionError.prototype=Object.create(Error.prototype);AssertionError.prototype.constructor=AssertionError;return AssertionError;";

unsafe fn prime_assertion_error_ctor(
    ctx: *mut qjs::JSContext,
    global: qjs::JSValue,
) -> Result<(), String> {
    let key = CString::new("__kawkabPrimedAssertionError").map_err(|e| e.to_string())?;
    let existing = qjs::JS_GetPropertyStr(ctx, global, key.as_ptr());
    let already = !qjs::JS_IsUndefined(existing);
    js_free_value(ctx, existing);
    if already {
        return Ok(());
    }
    let ctor_val = eval_js_module(ctx, "assert-assertionerror", ASSERTION_ERROR_CTOR_SRC);
    if is_exception(ctor_val) {
        let exc = qjs::JS_GetException(ctx);
        let detail = crate::ffi::js_string_to_owned(ctx, exc);
        js_free_value(ctx, exc);
        js_free_value(ctx, ctor_val);
        return Err(format!(
            "failed to initialize assert.AssertionError (prim eval): {detail}"
        ));
    }
    qjs::JS_SetPropertyStr(ctx, global, key.as_ptr(), ctor_val);
    Ok(())
}

const STREAM_SHIM_SRC: &str = r#"
var Emitter = function() {
  this._events = {};
};
Emitter.prototype.on = function(name, cb) {
  if (!this._events[name]) this._events[name] = [];
  this._events[name].push(cb);
  return this;
};
Emitter.prototype.once = function(name, cb) {
  var self = this;
  var wrapped = function(arg) {
    self.removeListener(name, wrapped);
    cb(arg);
  };
  return this.on(name, wrapped);
};
Emitter.prototype.removeListener = function(name, cb) {
  var list = this._events[name];
  if (!list) return this;
  for (var i = 0; i < list.length; i++) {
    if (list[i] === cb) {
      list.splice(i, 1);
      break;
    }
  }
  if (list.length === 0) delete this._events[name];
  return this;
};
Emitter.prototype.removeAllListeners = function(name) {
  if (name === undefined) this._events = {};
  else delete this._events[name];
  return this;
};
Emitter.prototype.emit = function(name, arg) {
  var list = (this._events[name] || []).slice();
  for (var i = 0; i < list.length; i++) list[i](arg);
};

var __chunkByteLength = function(chunk, enc) {
  if (chunk == null) return 0;
  if (typeof chunk === 'string') {
    if (typeof Buffer !== 'undefined' && Buffer.byteLength)
      return Buffer.byteLength(chunk, enc || 'utf8');
    try {
      if (typeof TextEncoder !== 'undefined')
        return new TextEncoder().encode(chunk).length;
    } catch (e) {}
    return chunk.length;
  }
  if (typeof chunk === 'object' && chunk !== null && typeof chunk.length === 'number')
    return chunk.length;
  return 1;
};

var Readable = function(options) {
  Emitter.call(this);
  options = options || {};
  this.readable = true;
  this.readableEnded = false;
  this.readableFlowing = null;
  this._hwm = options.highWaterMark != null ? options.highWaterMark : 16384;
  this._buffer = [];
  this._bufferLen = 0;
  this._paused = true;
  this._readableBackpressure = false;
  this._destroyed = false;
};
Readable.prototype = Object.create(Emitter.prototype);
Readable.prototype.constructor = Readable;
Readable.prototype.isPaused = function() {
  return this._paused;
};
Readable.prototype.pause = function() {
  this._paused = true;
  this.readableFlowing = false;
  return this;
};
Readable.prototype.resume = function() {
  this._paused = false;
  this.readableFlowing = true;
  this._flushBuffer();
  return this;
};
Readable.prototype._flushBuffer = function() {
  if (this._destroyed || this._paused) return;
  while (this._buffer.length > 0 && !this._paused) {
    var item = this._buffer.shift();
    this._bufferLen -= item.len;
    this.emit('data', item.chunk);
    this._maybeEmitReadableDrain();
  }
  if (this.readableEnded && this._buffer.length === 0) {
    this.readable = false;
    this.emit('end');
  }
};
Readable.prototype._maybeEmitReadableDrain = function() {
  if (this._bufferLen <= this._hwm && this._readableBackpressure) {
    this._readableBackpressure = false;
    this.emit('drain');
  }
};
Readable.prototype.push = function(chunk, encoding) {
  if (this._destroyed) return false;
  if (chunk === null) {
    this.readableEnded = true;
    if (this._buffer.length === 0) {
      this.readable = false;
      this.emit('end');
    } else if (!this._paused) {
      this._flushBuffer();
    }
    return true;
  }
  var len = __chunkByteLength(chunk, encoding);
  this._buffer.push({ chunk: chunk, len: len });
  this._bufferLen += len;
  var over = this._bufferLen > this._hwm;
  if (over) this._readableBackpressure = true;
  if (!this._paused) this._flushBuffer();
  else if (this._buffer.length > 0) this.emit('readable');
  return !over;
};
Readable.prototype.read = function(n) {
  if (this._buffer.length === 0) return null;
  var item = this._buffer.shift();
  this._bufferLen -= item.len;
  this._maybeEmitReadableDrain();
  if (this.readableEnded && this._buffer.length === 0) {
    this.readable = false;
    this.emit('end');
  }
  return item.chunk;
};
Readable.prototype.on = function(name, fn) {
  Emitter.prototype.on.call(this, name, fn);
  if (name === 'data') {
    if (this.readableFlowing === null) this.readableFlowing = true;
    this._paused = false;
    this._flushBuffer();
  }
  return this;
};
Readable.prototype.pipe = function(dest, pipeOpts) {
  var src = this;
  pipeOpts = pipeOpts || {};
  var ended = false;
  var cleanup = function() {
    src.removeListener('data', ondata);
    src.removeListener('end', onend);
    src.removeListener('error', onerror);
    dest.removeListener('drain', ondrain);
    dest.removeListener('error', onerror);
  };
  var ondata = function(chunk) {
    var ret = dest.write(chunk);
    if (ret === false) {
      src.pause();
      dest.on('drain', ondrain);
    }
  };
  var ondrain = function() {
    dest.removeListener('drain', ondrain);
    src.resume();
  };
  var onend = function() {
    if (ended) return;
    ended = true;
    cleanup();
    dest.end();
  };
  var onerror = function(err) {
    cleanup();
    var h = pipeOpts.errorHandler;
    if (h) h(err);
  };
  src.on('data', ondata);
  src.on('end', onend);
  src.on('error', onerror);
  dest.on('error', onerror);
  src.resume();
  return dest;
};
Readable.prototype.destroy = function(err) {
  if (this._destroyed) return this;
  this._destroyed = true;
  this.readable = false;
  this._buffer = [];
  this._bufferLen = 0;
  if (err) this.emit('error', err);
  this.emit('close');
  return this;
};

var Writable = function(options) {
  Emitter.call(this);
  options = options || {};
  this.writable = true;
  this.writableEnded = false;
  this.writableFinished = false;
  this._hwm = options.highWaterMark != null ? options.highWaterMark : 16384;
  this._chunks = [];
  this._pendingLen = 0;
  this._drainScheduled = false;
  this._destroyed = false;
};
Writable.prototype = Object.create(Emitter.prototype);
Writable.prototype.constructor = Writable;
Writable.prototype.write = function(chunk, encoding, cb) {
  if (this._destroyed) return false;
  if (typeof encoding === 'function') {
    cb = encoding;
    encoding = undefined;
  }
  if (chunk === undefined || chunk === null) {
    if (cb) setTimeout(function() { cb(); }, 0);
    return true;
  }
  var len = __chunkByteLength(chunk, encoding);
  this._chunks.push(chunk);
  this._pendingLen += len;
  var ok = this._pendingLen < this._hwm;
  if (!ok) {
    var self = this;
    if (!this._drainScheduled) {
      this._drainScheduled = true;
      setTimeout(function() {
        self._drainScheduled = false;
        if (self._destroyed) return;
        while (self._chunks.length > 0 && self._pendingLen >= self._hwm) {
          var c = self._chunks.shift();
          self._pendingLen -= __chunkByteLength(c);
        }
        if (self._pendingLen < self._hwm) self.emit('drain');
      }, 0);
    }
  }
  if (cb) setTimeout(function() { cb(); }, 0);
  return ok;
};
Writable.prototype.end = function(chunk, encoding, cb) {
  if (typeof chunk === 'function') {
    cb = chunk;
    chunk = undefined;
    encoding = undefined;
  } else if (typeof encoding === 'function') {
    cb = encoding;
    encoding = undefined;
  }
  if (chunk !== undefined && chunk !== null) this.write(chunk, encoding);
  this.writable = false;
  this.writableEnded = true;
  this.writableFinished = true;
  this.emit('finish');
  if (cb) cb();
};
Writable.prototype.destroy = function(err) {
  if (this._destroyed) return this;
  this._destroyed = true;
  this.writable = false;
  this._chunks = [];
  this._pendingLen = 0;
  if (err) this.emit('error', err);
  this.emit('close');
  return this;
};

var Duplex = function(options) {
  Readable.call(this, options);
  this._w = new Writable(options);
};
Duplex.prototype = Object.create(Readable.prototype);
Duplex.prototype.constructor = Duplex;
Duplex.prototype.write = function(chunk, enc, cb) {
  return this._w.write(chunk, enc, cb);
};
Duplex.prototype.end = function(chunk, enc, cb) {
  if (typeof chunk === 'function') {
    cb = chunk;
    chunk = undefined;
    enc = undefined;
  } else if (typeof enc === 'function') {
    cb = enc;
    enc = undefined;
  }
  this._w.end(chunk, enc);
  this.push(null);
  if (cb) cb();
};

var Transform = function(options) {
  Duplex.call(this, options);
};
Transform.prototype = Object.create(Duplex.prototype);
Transform.prototype.constructor = Transform;
Transform.prototype.write = function(chunk, enc, cb) {
  var ok = this.push(chunk);
  if (cb) setTimeout(cb, 0);
  return ok;
};

var __streamExports = {};
__streamExports.Readable = Readable;
__streamExports.Writable = Writable;
__streamExports.Duplex = Duplex;
__streamExports.Transform = Transform;
return __streamExports;
"#;

const EVENTS_SHIM_SRC: &str = r##"var __defaultMaxListeners = 10;
var __isFn = function(v) { return typeof v === 'function'; };
var __isNum = function(v) { return typeof v === 'number' && v === v; };
var __findBucket = function(emitter, event) {
  var buckets = emitter._events;
  for (var i = 0; i < buckets.length; i++) {
    if (buckets[i].key === event) return buckets[i];
  }
  return null;
};
var __findOrCreateBucket = function(emitter, event) {
  var b = __findBucket(emitter, event);
  if (b) return b;
  b = { key: event, list: [] };
  emitter._events.push(b);
  return b;
};
var __deleteBucket = function(emitter, event) {
  var buckets = emitter._events;
  for (var i = 0; i < buckets.length; i++) {
    if (buckets[i].key === event) {
      buckets.splice(i, 1);
      return;
    }
  }
};
var __warnMax = function(emitter, event, count) {
  if (!emitter.__kawkabWarned) emitter.__kawkabWarned = [];
  for (var i = 0; i < emitter.__kawkabWarned.length; i++) {
    if (emitter.__kawkabWarned[i] === event) return;
  }
  emitter.__kawkabWarned.push(event);
  var msg = 'Possible EventEmitter memory leak detected. ' + String(count) + ' ' + String(event) + ' listeners added.';
  if (typeof process !== 'undefined' && process && __isFn(process.emitWarning)) process.emitWarning(msg);
  else if (typeof console !== 'undefined' && console && __isFn(console.warn)) console.warn(msg);
};
var EventEmitter = function() {
  if (!(this instanceof EventEmitter)) return new EventEmitter();
  this._events = [];
  this._maxListeners = undefined;
  this.__kawkabWarned = [];
};
EventEmitter.defaultMaxListeners = __defaultMaxListeners;
EventEmitter.prototype.setMaxListeners = function(n) {
  if (!__isNum(n) || n < 0) throw new RangeError('The value of n is out of range.');
  this._maxListeners = n;
  return this;
};
EventEmitter.prototype.getMaxListeners = function() {
  return this._maxListeners == null ? EventEmitter.defaultMaxListeners : this._maxListeners;
};
EventEmitter.prototype._add = function(event, listener, once, prepend) {
  if (!__isFn(listener)) throw new TypeError('The listener argument must be of type Function.');
  var list = __findOrCreateBucket(this, event).list;
  var entry = { fn: listener, once: !!once };
  if (prepend) list.unshift(entry); else list.push(entry);
  var max = this.getMaxListeners();
  if (max > 0 && list.length > max) __warnMax(this, event, list.length);
  return this;
};
EventEmitter.prototype.on = function(event, listener) { return this._add(event, listener, false, false); };
EventEmitter.prototype.addListener = EventEmitter.prototype.on;
EventEmitter.prototype.prependListener = function(event, listener) { return this._add(event, listener, false, true); };
EventEmitter.prototype.once = function(event, listener) { return this._add(event, listener, true, false); };
EventEmitter.prototype.prependOnceListener = function(event, listener) { return this._add(event, listener, true, true); };
EventEmitter.prototype.removeListener = function(event, listener) {
  if (!__isFn(listener)) return this;
  var bucket = __findBucket(this, event);
  var list = bucket && bucket.list;
  if (!list || !list.length) return this;
  var next = [];
  for (var i = 0; i < list.length; i++) {
    var e = list[i];
    if (e.fn !== listener && e.listener !== listener) next.push(e);
  }
  if (next.length) bucket.list = next;
  else __deleteBucket(this, event);
  return this;
};
EventEmitter.prototype.off = EventEmitter.prototype.removeListener;
EventEmitter.prototype.removeAllListeners = function(event) {
  if (arguments.length === 0) this._events = [];
  else __deleteBucket(this, event);
  return this;
};
EventEmitter.prototype.listeners = function(event) {
  var bucket = __findBucket(this, event);
  var list = bucket && bucket.list;
  if (!list) return [];
  var out = new Array(list.length);
  for (var i = 0; i < list.length; i++) {
    out[i] = list[i].fn;
  }
  return out;
};
EventEmitter.prototype.rawListeners = EventEmitter.prototype.listeners;
var eventsOnce = function() { return {}; };
var eventsOnAsyncIter = function() { return { next: function() { return { done: true }; } } ; };
return { EventEmitter: EventEmitter, defaultMaxListeners: __defaultMaxListeners, once: eventsOnce, on: eventsOnAsyncIter };"##;

const QUERYSTRING_SHIM_SRC: &str =
    "var parse=function(str){var o={},s=String(str||'');if(s.charAt(0)==='?')s=s.slice(1);if(!s)return o;var p=s.split('&');for(var i=0;i<p.length;i++){var t=p[i];if(!t)continue;var j=t.indexOf('='),k=j>=0?t.slice(0,j):t,v=j>=0?t.slice(j+1):'';k=decodeURIComponent(k.split('+').join(' '));v=decodeURIComponent(v.split('+').join(' '));if(Object.prototype.hasOwnProperty.call(o,k)){if(Array.isArray(o[k]))o[k].push(v);else o[k]=[o[k],v];}else o[k]=v;}return o;};var stringify=function(obj){if(!obj||typeof obj!=='object')return '';var ks=Object.keys(obj),r=[];for(var i=0;i<ks.length;i++){var k=ks[i],v=obj[k];if(Array.isArray(v)){for(var j=0;j<v.length;j++)r.push(encodeURIComponent(k)+'='+encodeURIComponent(String(v[j])));}else r.push(encodeURIComponent(k)+'='+encodeURIComponent(String(v)));}return r.join('&');};return{parse:parse,stringify:stringify};";

const CRYPTO_SHIM_SRC: &str = r#"
const native = {
  randomBytes: __kawkabCryptoRandomBytes,
  randomBytesSync: __kawkabCryptoRandomBytesSync,
  createHash: __kawkabCryptoCreateHash,
  createHmac: __kawkabCryptoCreateHmac,
  update: __kawkabCryptoUpdate,
  digest: __kawkabCryptoDigest
};

function Hash(alg) {
  this._id = native.createHash(alg);
}
Hash.prototype.update = function(data) {
  native.update(this._id, data);
  return this;
};
Hash.prototype.digest = function(enc) {
  const buf = native.digest(this._id);
  if (enc === 'hex') return Array.from(new Uint8Array(buf)).map(b => b.toString(16).padStart(2, '0')).join('');
  if (enc === 'base64') return btoa(String.fromCharCode(...new Uint8Array(buf)));
  return Buffer.from(buf);
};

function Hmac(alg, key) {
  this._id = native.createHmac(alg, key);
}
Hmac.prototype.update = function(data) {
  native.update(this._id, data);
  return this;
};
Hmac.prototype.digest = function(enc) {
  const buf = native.digest(this._id);
  if (enc === 'hex') return Array.from(new Uint8Array(buf)).map(b => b.toString(16).padStart(2, '0')).join('');
  if (enc === 'base64') return btoa(String.fromCharCode(...new Uint8Array(buf)));
  return Buffer.from(buf);
};

return {
  createHash: (alg) => new Hash(alg),
  createHmac: (alg, key) => new Hmac(alg, key),
  randomBytes: (size, cb) => {
    if (typeof cb === 'function') {
      const id = __kawkabNextPromiseId();
      __kawkabCryptoRandomBytes(size, id);
      __kawkab_register_promise_callback(id, (err, buf) => {
        cb(err, buf ? Buffer.from(buf) : null);
      });
      return;
    }
    return Buffer.from(__kawkabCryptoRandomBytesSync(size));
  }
};
"#;

const URL_SHIM_SRC: &str = r#"
function QS(init) {
  this._pairs = [];
  var s = String(init || '');
  if (s.charAt(0) === '?') s = s.slice(1);
  if (!s) return;
  var parts = s.split('&');
  for (var i = 0; i < parts.length; i++) {
    var p = parts[i];
    if (!p) continue;
    var j = p.indexOf('=');
    var k = j >= 0 ? p.slice(0, j) : p;
    var v = j >= 0 ? p.slice(j + 1) : '';
    this._pairs.push([decodeURIComponent(k), decodeURIComponent(v)]);
  }
}
QS.prototype.append = function(k, v) { this._pairs.push([String(k), String(v)]); };
QS.prototype.set = function(k, v) { this.delete(k); this.append(k, v); };
QS.prototype.get = function(k) {
  k = String(k);
  for (var i = 0; i < this._pairs.length; i++) if (this._pairs[i][0] === k) return this._pairs[i][1];
  return null;
};
QS.prototype.getAll = function(k) {
  k = String(k);
  var out = [];
  for (var i = 0; i < this._pairs.length; i++) if (this._pairs[i][0] === k) out.push(this._pairs[i][1]);
  return out;
};
QS.prototype.delete = function(k) {
  k = String(k);
  var out = [];
  for (var i = 0; i < this._pairs.length; i++) if (this._pairs[i][0] !== k) out.push(this._pairs[i]);
  this._pairs = out;
};
QS.prototype.toString = function() {
  var out = [];
  for (var i = 0; i < this._pairs.length; i++) out.push(encodeURIComponent(this._pairs[i][0]) + '=' + encodeURIComponent(this._pairs[i][1]));
  return out.join('&');
};
function U(input, base) {
  var raw = String(input || '');
  var abs = raw.indexOf('://') >= 0 ? raw : (String(base || 'http://localhost').replace(/\/$/, '') + '/' + raw.replace(/^\//, ''));
  var qIdx = abs.indexOf('?');
  var hIdx = abs.indexOf('#');
  var endPath = qIdx >= 0 ? qIdx : (hIdx >= 0 ? hIdx : abs.length);
  var protoIdx = abs.indexOf('://');
  this.protocol = protoIdx >= 0 ? abs.slice(0, protoIdx + 1) : 'http:';
  var hostStart = protoIdx >= 0 ? protoIdx + 3 : 0;
  var slashIdx = abs.indexOf('/', hostStart);
  if (slashIdx < 0) slashIdx = endPath;
  this.host = abs.slice(hostStart, slashIdx);
  this.pathname = abs.slice(slashIdx, endPath) || '/';
  this.search = qIdx >= 0 ? abs.slice(qIdx, hIdx >= 0 ? hIdx : abs.length) : '';
  this.hash = hIdx >= 0 ? abs.slice(hIdx) : '';
  this.searchParams = new QS(this.search);
}
Object.defineProperty(U.prototype, 'href', {
  get: function() {
    var s = this.searchParams && this.searchParams.toString ? this.searchParams.toString() : '';
    var q = s ? ('?' + s) : '';
    return this.protocol + '//' + this.host + this.pathname + q + this.hash;
  }
});
U.prototype.toString = function() { return this.href; };
return { URL: U, URLSearchParams: QS };
"#;

const READLINE_SHIM_SRC: &str = "var createInterface=function(){return{question:function(q,cb){if(typeof cb==\"function\")setTimeout(function(){cb(\"\");},0);},on:function(){},close:function(){},pause:function(){},resume:function(){}};};return{createInterface:createInterface};";

const DNS_SHIM_SRC: &str =
    "var __dnsS=['127.0.0.1'];function __h(v){return String(v==null?'':v);}function __a(h,f){var s=__h(h);if(f===6)return(!s||s==='localhost')?'::1':s;return(!s||s==='localhost')?'127.0.0.1':s;}function __cb(cb,fn){if(typeof cb!=='function')throw new TypeError('callback must be a function');try{fn();}catch(e){cb(e);}}function __rt(h,t){var n=__h(h)||'localhost';var r=String(t||'A').toUpperCase();if(r==='A')return[__a(n,4)];if(r==='AAAA')return[__a(n,6)];if(r==='ANY')return[{type:'A',address:__a(n,4),ttl:60}];if(r==='CAA')return[{critical:0,issue:'letsencrypt.org'}];if(r==='CNAME')return[n==='localhost'?'localhost':'alias.'+n];if(r==='MX')return[{exchange:'mail.'+n,priority:10}];if(r==='NAPTR')return[{flags:'s',service:'SIP+D2U',regexp:'',replacement:'.',order:1,preference:1}];if(r==='NS')return['ns1.'+n,'ns2.'+n];if(r==='PTR')return[n];if(r==='SOA')return[{nsname:'ns1.'+n,hostmaster:'hostmaster.'+n,serial:1,refresh:3600,retry:600,expire:86400,minttl:60}];if(r==='SRV')return[{name:n,port:0,priority:10,weight:0}];if(r==='TXT')return[['v=spf1 -all']];var e=new Error('query type is not supported');e.code='ENODATA';throw e;}function lookup(h,o,c){var cb=c;var opts={};if(typeof o==='function')cb=o;else if(typeof o==='number')opts.family=o;else if(o&&typeof o==='object')opts=o;return __cb(cb,function(){var f=(opts.family===4||opts.family===6)?opts.family:(__h(h).indexOf(':')>=0?6:4);var first={address:__a(h,f),family:f};var both=[first];if(opts.family!==4&&opts.family!==6)both=[{address:__a(h,4),family:4},{address:__a(h,6),family:6}];if(opts.all)cb(null,both);else cb(null,first.address,first.family);});}function resolve(h,t,c){var rr=t;var cb=c;if(typeof t==='function'){cb=t;rr='A';}return __cb(cb,function(){cb(null,__rt(h,rr));});}function resolve4(h,cb){return resolve(h,'A',cb);}function resolve6(h,cb){return resolve(h,'AAAA',cb);}function resolveAny(h,cb){return resolve(h,'ANY',cb);}function resolveCaa(h,cb){return resolve(h,'CAA',cb);}function resolveCname(h,cb){return resolve(h,'CNAME',cb);}function resolveMx(h,cb){return resolve(h,'MX',cb);}function resolveNaptr(h,cb){return resolve(h,'NAPTR',cb);}function resolveNs(h,cb){return resolve(h,'NS',cb);}function resolvePtr(h,cb){return resolve(h,'PTR',cb);}function resolveSoa(h,cb){return resolve(h,'SOA',cb);}function resolveSrv(h,cb){return resolve(h,'SRV',cb);}function resolveTxt(h,cb){return resolve(h,'TXT',cb);}function reverse(ip,cb){return __cb(cb,function(){cb(null,[__h(ip)||'localhost']);});}function lookupService(a,p,cb){return __cb(cb,function(){cb(null,__h(a)||'localhost',String(p||0));});}function getServers(){return __dnsS.slice();}function setServers(s){if(!s||typeof s.length!=='number')throw new TypeError('servers must be an array');var o=[];for(var i=0;i<s.length;i++)o.push(String(s[i]));__dnsS=o;}function Resolver(){}Resolver.prototype.getServers=getServers;Resolver.prototype.setServers=setServers;Resolver.prototype.cancel=function(){};Resolver.prototype.lookup=lookup;Resolver.prototype.resolve=resolve;Resolver.prototype.resolve4=resolve4;Resolver.prototype.resolve6=resolve6;Resolver.prototype.resolveAny=resolveAny;Resolver.prototype.resolveCaa=resolveCaa;Resolver.prototype.resolveCname=resolveCname;Resolver.prototype.resolveMx=resolveMx;Resolver.prototype.resolveNaptr=resolveNaptr;Resolver.prototype.resolveNs=resolveNs;Resolver.prototype.resolvePtr=resolvePtr;Resolver.prototype.resolveSoa=resolveSoa;Resolver.prototype.resolveSrv=resolveSrv;Resolver.prototype.resolveTxt=resolveTxt;Resolver.prototype.reverse=reverse;return {ADDRCONFIG:32,V4MAPPED:8,ALL:16,ORDER_VERBATIM:0,ORDER_IPV4FIRST:1,ORDER_IPV6FIRST:2,lookup:lookup,resolve:resolve,resolve4:resolve4,resolve6:resolve6,resolveAny:resolveAny,resolveCaa:resolveCaa,resolveCname:resolveCname,resolveMx:resolveMx,resolveNaptr:resolveNaptr,resolveNs:resolveNs,resolvePtr:resolvePtr,resolveSoa:resolveSoa,resolveSrv:resolveSrv,resolveTxt:resolveTxt,reverse:reverse,lookupService:lookupService,getServers:getServers,setServers:setServers,Resolver:Resolver};";

const DNS_PROMISES_SHIM_SRC: &str =
    "var d=require('dns');function p(n,m){return function(){var a=[];for(var i=0;i<arguments.length;i++)a.push(arguments[i]);return new Promise(function(res,rej){a.push(function(err,x,y){if(err){rej(err);return;}if(m){res(m(x,y));return;}if(arguments.length<=2){res(x);return;}res([x,y]);});d[n].apply(d,a);});};}var out={};out.lookup=function(h,o){return new Promise(function(res,rej){d.lookup(h,o,function(err,address,family){if(err){rej(err);return;}if(o&&o.all){res(address);return;}res({address:address,family:family});});});};out.resolve=p('resolve');out.resolve4=p('resolve4');out.resolve6=p('resolve6');out.resolveAny=p('resolveAny');out.resolveCaa=p('resolveCaa');out.resolveCname=p('resolveCname');out.resolveMx=p('resolveMx');out.resolveNaptr=p('resolveNaptr');out.resolveNs=p('resolveNs');out.resolvePtr=p('resolvePtr');out.resolveSoa=p('resolveSoa');out.resolveSrv=p('resolveSrv');out.resolveTxt=p('resolveTxt');out.reverse=p('reverse');out.lookupService=p('lookupService',function(h,s){return {hostname:h,service:s};});out.getServers=function(){return d.getServers();};out.setServers=function(s){d.setServers(s);};function R(){}R.prototype.getServers=out.getServers;R.prototype.setServers=out.setServers;R.prototype.cancel=function(){};R.prototype.lookup=out.lookup;R.prototype.resolve=out.resolve;R.prototype.resolve4=out.resolve4;R.prototype.resolve6=out.resolve6;R.prototype.resolveAny=out.resolveAny;R.prototype.resolveCaa=out.resolveCaa;R.prototype.resolveCname=out.resolveCname;R.prototype.resolveMx=out.resolveMx;R.prototype.resolveNaptr=out.resolveNaptr;R.prototype.resolveNs=out.resolveNs;R.prototype.resolvePtr=out.resolvePtr;R.prototype.resolveSoa=out.resolveSoa;R.prototype.resolveSrv=out.resolveSrv;R.prototype.resolveTxt=out.resolveTxt;R.prototype.reverse=out.reverse;out.Resolver=R;return out;";

unsafe fn prime_eval_shim_exports(
    ctx: *mut qjs::JSContext,
    global: qjs::JSValue,
    storage_key: &str,
    eval_name: &str,
    src: &str,
) -> Result<(), String> {
    let key = CString::new(storage_key).map_err(|e| e.to_string())?;
    let existing = qjs::JS_GetPropertyStr(ctx, global, key.as_ptr());
    let already = !qjs::JS_IsUndefined(existing);
    js_free_value(ctx, existing);
    if already {
        return Ok(());
    }
    let exports_val = eval_js_module(ctx, eval_name, src);
    if is_exception(exports_val) {
        let exc = qjs::JS_GetException(ctx);
        let detail = crate::ffi::js_string_to_owned(ctx, exc);
        js_free_value(ctx, exc);
        js_free_value(ctx, exports_val);
        return Err(format!(
            "failed to initialize built-in eval module '{eval_name}' (prim eval): {detail}"
        ));
    }
    qjs::JS_SetPropertyStr(ctx, global, key.as_ptr(), exports_val);
    Ok(())
}

unsafe fn prime_timers_promises_exports_native(
    ctx: *mut qjs::JSContext,
    global: qjs::JSValue,
) -> Result<(), String> {
    let key = CString::new("__kawkabPrimedTimersPromises").map_err(|e| e.to_string())?;
    let existing = qjs::JS_GetPropertyStr(ctx, global, key.as_ptr());
    let already = !qjs::JS_IsUndefined(existing);
    js_free_value(ctx, existing);
    if already {
        return Ok(());
    }
    let obj = qjs::JS_NewObject(ctx);
    install_obj_fn(
        ctx,
        obj,
        "setTimeout",
        Some(js_timers_promises_set_timeout),
        2,
    )?;
    install_obj_fn(
        ctx,
        obj,
        "setImmediate",
        Some(js_timers_promises_set_immediate),
        1,
    )?;
    qjs::JS_SetPropertyStr(ctx, global, key.as_ptr(), obj);
    Ok(())
}

unsafe fn require_primmed_or_eval(
    ctx: *mut qjs::JSContext,
    storage_key: &str,
    eval_name: &str,
    src: &str,
) -> qjs::JSValue {
    let k = CString::new(storage_key).unwrap();
    let global = qjs::JS_GetGlobalObject(ctx);
    let exports_val = qjs::JS_GetPropertyStr(ctx, global, k.as_ptr());
    js_free_value(ctx, global);
    if qjs::JS_IsUndefined(exports_val) {
        js_free_value(ctx, exports_val);
        return eval_js_module(ctx, eval_name, src);
    }
    let out = js_dup_value(exports_val);
    js_free_value(ctx, exports_val);
    out
}

fn base64_encode_bytes(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut r = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i];
        let b1 = data.get(i + 1).copied();
        let b2 = data.get(i + 2).copied();
        r.push(T[(b0 >> 2) as usize] as char);
        r.push(
            T[(match b1 {
                Some(b1) => ((b0 & 3) << 4) | (b1 >> 4),
                None => (b0 & 3) << 4,
            }) as usize] as char,
        );
        match (b1, b2) {
            (Some(b1), Some(b2)) => {
                r.push(T[(((b1 & 15) << 2) | (b2 >> 6)) as usize] as char);
                r.push(T[(b2 & 63) as usize] as char);
            }
            (Some(b1), None) => {
                r.push(T[((b1 & 15) << 2) as usize] as char);
                r.push('=');
            }
            (None, _) => {
                r.push('=');
                r.push('=');
            }
        }
        i += match (b1, b2) {
            (_, Some(_)) => 3,
            (Some(_), None) => 2,
            (None, None) => 1,
        };
    }
    r
}

fn base64_decode_char(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        b'=' => Some(0),
        _ => None,
    }
}

fn base64_decode_data(s: &str) -> Result<Vec<u8>, ()> {
    let mut bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    while !bytes.is_empty() && bytes.len() % 4 != 0 {
        bytes.push(b'=');
    }
    if bytes.len() % 4 != 0 {
        return Err(());
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        let a = base64_decode_char(bytes[i]).ok_or(())?;
        let b = base64_decode_char(bytes[i + 1]).ok_or(())?;
        let c = base64_decode_char(bytes[i + 2]).ok_or(())?;
        let d = base64_decode_char(bytes[i + 3]).ok_or(())?;
        let tri = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | (d as u32);
        out.push((tri >> 16) as u8);
        if bytes[i + 2] != b'=' {
            out.push((tri >> 8) as u8);
        }
        if bytes[i + 3] != b'=' {
            out.push(tri as u8);
        }
        i += 4;
    }
    Ok(out)
}

unsafe extern "C" fn js_global_btoa(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("btoa requires a string").unwrap().as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let s = js_string_to_owned(ctx, args[0]);
    let raw: Vec<u8> = s.chars().map(|c| (c as u32).min(255) as u8).collect();
    let enc = base64_encode_bytes(&raw);
    qjs_compat::new_string_from_str(ctx, &enc)
}

unsafe extern "C" fn js_global_atob(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("atob requires a string").unwrap().as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let s = js_string_to_owned(ctx, args[0]);
    let dec = match base64_decode_data(&s) {
        Ok(b) => b,
        Err(()) => {
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new("atob failed: invalid character")
                    .unwrap()
                    .as_ptr(),
            );
        }
    };
    let out: String = dec.into_iter().map(|b| b as char).collect();
    qjs_compat::new_string_from_str(ctx, &out)
}

unsafe extern "C" fn js_global_structured_clone_json(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("structuredClone requires a value")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let global = qjs::JS_GetGlobalObject(ctx);
    let json_o = qjs::JS_GetPropertyStr(ctx, global, CString::new("JSON").unwrap().as_ptr());
    js_free_value(ctx, global);
    if !qjs::JS_IsObject(json_o) {
        js_free_value(ctx, json_o);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("JSON is not available").unwrap().as_ptr(),
        );
    }
    let stringify =
        qjs::JS_GetPropertyStr(ctx, json_o, CString::new("stringify").unwrap().as_ptr());
    let parse = qjs::JS_GetPropertyStr(ctx, json_o, CString::new("parse").unwrap().as_ptr());
    js_free_value(ctx, json_o);
    if qjs::JS_IsFunction(ctx, stringify) == 0 || qjs::JS_IsFunction(ctx, parse) == 0 {
        js_free_value(ctx, stringify);
        js_free_value(ctx, parse);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("JSON.stringify/parse missing")
                .unwrap()
                .as_ptr(),
        );
    }
    let mut argv_s = [js_dup_value(args[0])];
    let mid = qjs::JS_Call(ctx, stringify, js_undefined(), 1, argv_s.as_mut_ptr());
    js_free_value(ctx, stringify);
    js_free_value(ctx, argv_s[0]);
    if is_exception(mid) {
        js_free_value(ctx, parse);
        return mid;
    }
    let mut argv_p = [mid];
    let out = qjs::JS_Call(ctx, parse, js_undefined(), 1, argv_p.as_mut_ptr());
    js_free_value(ctx, parse);
    js_free_value(ctx, argv_p[0]);
    out
}

unsafe fn install_web_compat_globals(ctx: *mut qjs::JSContext) {
    let global = qjs::JS_GetGlobalObject(ctx);
    let _ = install_c_fn(ctx, global, "btoa", Some(js_global_btoa), 1);
    let _ = install_c_fn(ctx, global, "atob", Some(js_global_atob), 1);
    let _ = install_c_fn(
        ctx,
        global,
        "structuredClone",
        Some(js_global_structured_clone_json),
        1,
    );
    let loose = CString::new("globalThis.__kawkabLooseEq=function(a,b){return a==b;};").unwrap();
    let file = CString::new("kawkab:loose-eq").unwrap();
    let _ = qjs_compat::eval(
        ctx,
        loose.as_ptr(),
        loose.as_bytes().len(),
        file.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    );
    js_free_value(ctx, global);
}

unsafe fn install_performance_global(
    ctx: *mut qjs::JSContext,
    global: qjs::JSValue,
) -> Result<(), String> {
    performance_ensure_init();
    let perf = qjs::JS_NewObject(ctx);
    let origin = *PERF_TIME_ORIGIN_MS
        .get()
        .expect("performance time origin initialized");
    qjs::JS_SetPropertyStr(
        ctx,
        perf,
        CString::new("timeOrigin").unwrap().as_ptr(),
        qjs::JS_NewFloat64(ctx, origin),
    );
    install_obj_fn(ctx, perf, "now", Some(js_performance_now), 0)?;
    qjs::JS_SetPropertyStr(
        ctx,
        global,
        CString::new("performance").unwrap().as_ptr(),
        perf,
    );
    Ok(())
}

unsafe extern "C" fn js_performance_now(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    performance_ensure_init();
    let ms = PERF_NAV_START_MONO
        .get()
        .expect("performance nav start")
        .elapsed()
        .as_secs_f64()
        * 1000.0;
    qjs::JS_NewFloat64(ctx, ms)
}

unsafe extern "C" fn js_read_file_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("readFileSync(path) requires path")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    match std::fs::read_to_string(&path) {
        Ok(data) => match CString::new(data) {
            Ok(c) => qjs_compat::new_string_from_cstr(ctx, c.as_ptr()),
            Err(_) => qjs::JS_ThrowTypeError(
                ctx,
                CString::new("file contains interior NUL byte")
                    .unwrap()
                    .as_ptr(),
            ),
        },
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("readFileSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_write_file_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("writeFileSync(path, data) requires arguments")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    let data = js_string_to_owned(ctx, args[1]);
    match std::fs::write(&path, data) {
        Ok(_) => js_undefined(),
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("writeFileSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_fs_copy_file_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("copyFileSync(src, dest) requires two paths")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let src = js_string_to_owned(ctx, args[0]);
    let dest = js_string_to_owned(ctx, args[1]);
    match std::fs::copy(&src, &dest) {
        Ok(n) => qjs::JS_NewFloat64(ctx, n as f64),
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("copyFileSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_fs_rm_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("rmSync(path) requires path").unwrap().as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    let mut recursive = false;
    let mut force = false;
    if argc >= 2 && qjs::JS_IsObject(args[1]) {
        let rec_k = CString::new("recursive").unwrap();
        let rec_v = qjs::JS_GetPropertyStr(ctx, args[1], rec_k.as_ptr());
        if qjs::JS_IsBool(rec_v) {
            recursive = qjs::JS_ToBool(ctx, rec_v) != 0;
        }
        js_free_value(ctx, rec_v);
        let force_k = CString::new("force").unwrap();
        let force_v = qjs::JS_GetPropertyStr(ctx, args[1], force_k.as_ptr());
        if qjs::JS_IsBool(force_v) {
            force = qjs::JS_ToBool(ctx, force_v) != 0;
        }
        js_free_value(ctx, force_v);
    }
    let p = Path::new(&path);
    let r = if p.is_dir() {
        if recursive {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_dir(&path)
        }
    } else {
        std::fs::remove_file(&path)
    };
    match r {
        Ok(()) => js_undefined(),
        Err(e) if force && e.kind() == ErrorKind::NotFound => js_undefined(),
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("rmSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_noop(
    _ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    js_undefined()
}

unsafe extern "C" fn js_return_empty_object(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    qjs::JS_NewObject(ctx)
}

unsafe fn dgram_set_socket_error(ctx: *mut qjs::JSContext, sock: qjs::JSValue, message: &str) {
    qjs::JS_SetPropertyStr(
        ctx,
        sock,
        CString::new("__kawkabDgramLastError").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, message),
    );
}

unsafe fn dgram_clear_error(ctx: *mut qjs::JSContext, sock: qjs::JSValue) {
    qjs::JS_SetPropertyStr(
        ctx,
        sock,
        CString::new("__kawkabDgramLastError").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, ""),
    );
}

unsafe fn dgram_make_error(ctx: *mut qjs::JSContext, code: &str, message: &str) -> qjs::JSValue {
    let err = qjs::JS_NewError(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        err,
        CString::new("name").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, "Error"),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        err,
        CString::new("message").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, message),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        err,
        CString::new("code").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, code),
    );
    err
}

unsafe fn dgram_push_listener(
    ctx: *mut qjs::JSContext,
    sock: qjs::JSValue,
    event: &str,
    cb: qjs::JSValue,
    once: bool,
) -> Result<(), qjs::JSValue> {
    let k_list = CString::new("__kawkabListeners").unwrap();
    let listeners = qjs::JS_GetPropertyStr(ctx, sock, k_list.as_ptr());
    if qjs::JS_IsUndefined(listeners) {
        js_free_value(ctx, listeners);
        return Err(qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dgram socket: internal state missing")
                .unwrap()
                .as_ptr(),
        ));
    }
    let key = match CString::new(event) {
        Ok(s) => s,
        Err(_) => {
            js_free_value(ctx, listeners);
            return Err(qjs::JS_ThrowTypeError(
                ctx,
                CString::new("dgram: invalid event name").unwrap().as_ptr(),
            ));
        }
    };
    let arr = qjs::JS_GetPropertyStr(ctx, listeners, key.as_ptr());
    let arr = if qjs::JS_IsUndefined(arr) {
        js_free_value(ctx, arr);
        let na = qjs::JS_NewArray(ctx);
        qjs::JS_SetPropertyStr(ctx, listeners, key.as_ptr(), js_dup_value(na));
        na
    } else {
        arr
    };
    let entry = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        entry,
        CString::new("cb").unwrap().as_ptr(),
        js_dup_value(cb),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        entry,
        CString::new("orig").unwrap().as_ptr(),
        js_dup_value(cb),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        entry,
        CString::new("once").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, once),
    );
    let len_key = CString::new("length").unwrap();
    let len_v = qjs::JS_GetPropertyStr(ctx, arr, len_key.as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_v);
    js_free_value(ctx, len_v);
    qjs::JS_SetPropertyUint32(ctx, arr, len as u32, entry);
    js_free_value(ctx, arr);
    js_free_value(ctx, listeners);
    Ok(())
}

unsafe fn dgram_emit(
    ctx: *mut qjs::JSContext,
    sock: qjs::JSValue,
    event: &str,
    arg0: qjs::JSValue,
    has_arg0: bool,
    arg1: qjs::JSValue,
    has_arg1: bool,
) {
    let k_list = CString::new("__kawkabListeners").unwrap();
    let listeners = qjs::JS_GetPropertyStr(ctx, sock, k_list.as_ptr());
    if qjs::JS_IsUndefined(listeners) {
        js_free_value(ctx, listeners);
        return;
    }
    let key = match CString::new(event) {
        Ok(s) => s,
        Err(_) => {
            js_free_value(ctx, listeners);
            return;
        }
    };
    let arr = qjs::JS_GetPropertyStr(ctx, listeners, key.as_ptr());
    if qjs::JS_IsUndefined(arr) {
        js_free_value(ctx, arr);
        return;
    }
    let len_key = CString::new("length").unwrap();
    let len_v = qjs::JS_GetPropertyStr(ctx, arr, len_key.as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_v);
    js_free_value(ctx, len_v);
    let mut i: i32 = 0;
    let next_arr = qjs::JS_NewArray(ctx);
    let mut ni: u32 = 0;
    while i < len {
        let entry = qjs::JS_GetPropertyUint32(ctx, arr, i as u32);
        let mut keep = true;
        let cb = if qjs::JS_IsFunction(ctx, entry) != 0 {
            js_dup_value(entry)
        } else {
            let maybe_cb = qjs::JS_GetPropertyStr(ctx, entry, CString::new("cb").unwrap().as_ptr());
            let once_v = qjs::JS_GetPropertyStr(ctx, entry, CString::new("once").unwrap().as_ptr());
            let once_i = qjs::JS_ToBool(ctx, once_v);
            js_free_value(ctx, once_v);
            if once_i != 0 {
                keep = false;
            }
            maybe_cb
        };
        if qjs::JS_IsFunction(ctx, cb) != 0 {
            let ret = if has_arg0 && has_arg1 {
                let mut argv = [arg0, arg1];
                qjs::JS_Call(ctx, cb, sock, 2, argv.as_mut_ptr())
            } else if has_arg0 {
                let mut argv = [arg0];
                qjs::JS_Call(ctx, cb, sock, 1, argv.as_mut_ptr())
            } else {
                qjs::JS_Call(ctx, cb, sock, 0, std::ptr::null_mut())
            };
            if is_exception(ret) {
                js_free_value(ctx, ret);
            } else {
                js_free_value(ctx, ret);
            }
        }
        if keep {
            qjs::JS_SetPropertyUint32(ctx, next_arr, ni, js_dup_value(entry));
            ni += 1;
        }
        js_free_value(ctx, cb);
        js_free_value(ctx, entry);
        i += 1;
    }
    qjs::JS_SetPropertyStr(ctx, listeners, key.as_ptr(), next_arr);
    js_free_value(ctx, arr);
    js_free_value(ctx, listeners);
}

unsafe extern "C" fn js_dgram_sock_on(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dgram socket.on(name, listener) requires arguments")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let name = js_string_to_owned(ctx, args[0]);
    if qjs::JS_IsFunction(ctx, args[1]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dgram socket.on: listener must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    if let Err(e) = dgram_push_listener(ctx, this, &name, args[1], false) {
        return e;
    }
    js_dup_value(this)
}

unsafe extern "C" fn js_dgram_sock_once(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dgram socket.once(name, listener) requires arguments")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let name = js_string_to_owned(ctx, args[0]);
    if qjs::JS_IsFunction(ctx, args[1]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dgram socket.once: listener must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    if let Err(e) = dgram_push_listener(ctx, this, &name, args[1], true) {
        return e;
    }
    js_dup_value(this)
}

unsafe extern "C" fn js_dgram_sock_remove_listener(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return js_dup_value(this);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let name = js_string_to_owned(ctx, args[0]);
    let target = args[1];
    let k_list = CString::new("__kawkabListeners").unwrap();
    let listeners = qjs::JS_GetPropertyStr(ctx, this, k_list.as_ptr());
    if qjs::JS_IsUndefined(listeners) {
        js_free_value(ctx, listeners);
        return js_dup_value(this);
    }
    let key = match CString::new(name) {
        Ok(s) => s,
        Err(_) => {
            js_free_value(ctx, listeners);
            return js_dup_value(this);
        }
    };
    let arr = qjs::JS_GetPropertyStr(ctx, listeners, key.as_ptr());
    if qjs::JS_IsUndefined(arr) {
        js_free_value(ctx, arr);
        js_free_value(ctx, listeners);
        return js_dup_value(this);
    }
    let len_key = CString::new("length").unwrap();
    let len_v = qjs::JS_GetPropertyStr(ctx, arr, len_key.as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_v);
    js_free_value(ctx, len_v);
    let new_arr = qjs::JS_NewArray(ctx);
    let mut ni: u32 = 0;
    let mut i: i32 = 0;
    while i < len {
        let entry = qjs::JS_GetPropertyUint32(ctx, arr, i as u32);
        let mut keep = true;
        if qjs::JS_IsFunction(ctx, entry) != 0 {
            if qjs::JS_StrictEq(ctx, entry, target) != 0 {
                keep = false;
            }
        } else {
            let cb = qjs::JS_GetPropertyStr(ctx, entry, CString::new("cb").unwrap().as_ptr());
            let orig = qjs::JS_GetPropertyStr(ctx, entry, CString::new("orig").unwrap().as_ptr());
            if qjs::JS_StrictEq(ctx, cb, target) != 0 || qjs::JS_StrictEq(ctx, orig, target) != 0 {
                keep = false;
            }
            js_free_value(ctx, cb);
            js_free_value(ctx, orig);
        }
        if keep {
            qjs::JS_SetPropertyUint32(ctx, new_arr, ni, js_dup_value(entry));
            ni += 1;
        }
        js_free_value(ctx, entry);
        i += 1;
    }
    qjs::JS_SetPropertyStr(ctx, listeners, key.as_ptr(), new_arr);
    js_free_value(ctx, arr);
    js_free_value(ctx, listeners);
    js_dup_value(this)
}

unsafe extern "C" fn js_dgram_sock_remove_all_listeners(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let k_list = CString::new("__kawkabListeners").unwrap();
    let listeners = qjs::JS_GetPropertyStr(ctx, this, k_list.as_ptr());
    if qjs::JS_IsUndefined(listeners) {
        js_free_value(ctx, listeners);
        return js_dup_value(this);
    }
    if argc >= 1 {
        let args = std::slice::from_raw_parts(argv, argc as usize);
        let name = js_string_to_owned(ctx, args[0]);
        let key = match CString::new(name) {
            Ok(s) => s,
            Err(_) => {
                js_free_value(ctx, listeners);
                return js_dup_value(this);
            }
        };
        let empty = qjs::JS_NewArray(ctx);
        qjs::JS_SetPropertyStr(ctx, listeners, key.as_ptr(), empty);
    } else {
        let fresh = qjs::JS_NewObject(ctx);
        qjs::JS_SetPropertyStr(ctx, this, k_list.as_ptr(), fresh);
        js_free_value(ctx, listeners);
        return js_dup_value(this);
    }
    js_free_value(ctx, listeners);
    js_dup_value(this)
}

unsafe extern "C" fn js_dgram_sock_bind(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = if argc > 0 {
        std::slice::from_raw_parts(argv, argc as usize)
    } else {
        &[]
    };
    let mut port: i64 = 0;
    let mut host: Option<String> = None;
    let mut cb: Option<qjs::JSValue> = None;
    for a in args.iter().copied() {
        if qjs::JS_IsFunction(ctx, a) != 0 {
            cb = Some(a);
            continue;
        }
        if qjs::JS_IsString(a) {
            host = Some(js_string_to_owned(ctx, a));
            continue;
        }
        let mut n: i64 = 0;
        if qjs::JS_ToInt64(ctx, &mut n, a) == 0 {
            port = n;
        }
    }
    let socket_id_v = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabDgramSocketId").unwrap().as_ptr(),
    );
    let mut socket_id_i: i64 = 0;
    let _ = qjs::JS_ToInt64(ctx, &mut socket_id_i, socket_id_v);
    js_free_value(ctx, socket_id_v);
    let socket_id = if socket_id_i > 0 {
        socket_id_i as u64
    } else {
        0
    };

    let kind_v =
        qjs::JS_GetPropertyStr(ctx, this, CString::new("__kawkabUdpKind").unwrap().as_ptr());
    let kind = js_string_to_owned(ctx, kind_v);
    js_free_value(ctx, kind_v);
    let default_host = if kind == "udp6" { "::" } else { "0.0.0.0" };
    let bind_host = host.unwrap_or_else(|| default_host.to_string());
    let bind_addr = if bind_host.contains(':') {
        format!("[{}]:{}", bind_host, port.max(0))
    } else {
        format!("{}:{}", bind_host, port.max(0))
    };

    let udp = match UdpSocket::bind(&bind_addr) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("dgram.bind failed: {e}");
            dgram_set_socket_error(ctx, this, &msg);
            let err = dgram_make_error(ctx, "EADDRNOTAVAIL", &msg);
            dgram_emit(
                ctx,
                this,
                "error",
                js_dup_value(err),
                true,
                js_undefined(),
                false,
            );
            js_free_value(ctx, err);
            return js_dup_value(this);
        }
    };
    let _ = udp.set_read_timeout(Some(Duration::from_millis(200)));
    let local_addr = match udp.local_addr() {
        Ok(a) => a,
        Err(e) => {
            let msg = format!("dgram.bind local_addr failed: {e}");
            dgram_set_socket_error(ctx, this, &msg);
            let err = dgram_make_error(ctx, "EADDRNOTAVAIL", &msg);
            dgram_emit(
                ctx,
                this,
                "error",
                js_dup_value(err),
                true,
                js_undefined(),
                false,
            );
            js_free_value(ctx, err);
            return js_dup_value(this);
        }
    };
    dgram_clear_error(ctx, this);
    let udp = Arc::new(udp);
    DGRAM_NATIVE_SOCKET_REGISTRY.with(|reg| {
        reg.borrow_mut().insert(socket_id, udp.clone());
    });
    let global = qjs::JS_GetGlobalObject(ctx);
    let key_store = CString::new("__kawkabDgramSocketStore").unwrap();
    let mut store = qjs::JS_GetPropertyStr(ctx, global, key_store.as_ptr());
    if qjs::JS_IsUndefined(store) {
        js_free_value(ctx, store);
        store = qjs::JS_NewObject(ctx);
        qjs::JS_SetPropertyStr(ctx, global, key_store.as_ptr(), js_dup_value(store));
    }
    if let Ok(id_key) = CString::new(socket_id.to_string()) {
        qjs::JS_SetPropertyStr(ctx, store, id_key.as_ptr(), js_dup_value(this));
    }
    js_free_value(ctx, store);
    js_free_value(ctx, global);
    let cancel = Arc::new(AtomicBool::new(false));
    DGRAM_RECV_CANCEL_BY_ID.with(|m| {
        if let Some(old) = m.borrow_mut().insert(socket_id, cancel.clone()) {
            old.store(true, Ordering::Release);
        }
    });
    let sender = TASK_SENDER_SLOT.with(|s| s.borrow().clone());
    if let Some(sender) = sender {
        if let Ok(rx_sock) = udp.try_clone() {
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    if cancel.load(Ordering::Acquire) {
                        break;
                    }
                    match rx_sock.recv_from(&mut buf) {
                        Ok((n, src)) => {
                            sender.send_udp_message(
                                socket_id,
                                Arc::<[u8]>::from(buf[..n].to_vec()),
                                src.ip().to_string(),
                                src.port(),
                            );
                        }
                        Err(e)
                            if e.kind() == ErrorKind::WouldBlock
                                || e.kind() == ErrorKind::TimedOut => {}
                        Err(_) => break,
                    }
                }
            });
        }
    }
    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabBoundPort").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, local_addr.port() as i64),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabBoundAddress").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &local_addr.ip().to_string()),
    );
    if let Some(c) = cb {
        let ret = qjs::JS_Call(ctx, c, this, 0, std::ptr::null_mut());
        if is_exception(ret) {
            return ret;
        }
        js_free_value(ctx, ret);
    }
    dgram_emit(
        ctx,
        this,
        "listening",
        js_undefined(),
        false,
        js_undefined(),
        false,
    );
    js_dup_value(this)
}

unsafe extern "C" fn js_dgram_sock_send(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = if argc > 0 {
        std::slice::from_raw_parts(argv, argc as usize)
    } else {
        &[]
    };
    let cb = if !args.is_empty() {
        let last = args[args.len() - 1];
        if qjs::JS_IsFunction(ctx, last) != 0 {
            Some(last)
        } else {
            None
        }
    } else {
        None
    };
    if args.is_empty() {
        return js_dup_value(this);
    }
    let mut payload = buffer::buffer_bytes_from_value(ctx, args[0]);
    let mut offset: usize = 0;
    let mut length: usize = payload.len();
    let mut port: i64 = 0;
    let mut host: Option<String> = None;

    let is_num = |v: qjs::JSValue| -> bool { qjs::JS_IsNumber(v) };
    if args.len() >= 4 && is_num(args[1]) && is_num(args[2]) && is_num(args[3]) {
        let mut n: i64 = 0;
        let _ = qjs::JS_ToInt64(ctx, &mut n, args[1]);
        offset = n.max(0) as usize;
        n = 0;
        let _ = qjs::JS_ToInt64(ctx, &mut n, args[2]);
        length = n.max(0) as usize;
        n = 0;
        let _ = qjs::JS_ToInt64(ctx, &mut n, args[3]);
        port = n;
        if args.len() >= 5 && qjs::JS_IsString(args[4]) {
            host = Some(js_string_to_owned(ctx, args[4]));
        }
    } else if args.len() >= 3 && is_num(args[1]) {
        let mut n: i64 = 0;
        let _ = qjs::JS_ToInt64(ctx, &mut n, args[1]);
        port = n;
        if qjs::JS_IsString(args[2]) {
            host = Some(js_string_to_owned(ctx, args[2]));
        }
    }
    if offset > payload.len() {
        offset = payload.len();
    }
    let end = offset.saturating_add(length).min(payload.len());
    payload = payload[offset..end].to_vec();

    let kind_v =
        qjs::JS_GetPropertyStr(ctx, this, CString::new("__kawkabUdpKind").unwrap().as_ptr());
    let kind = js_string_to_owned(ctx, kind_v);
    js_free_value(ctx, kind_v);
    let target_host = host.unwrap_or_else(|| {
        if kind == "udp6" {
            "::1".to_string()
        } else {
            "127.0.0.1".to_string()
        }
    });
    let target = if target_host.contains(':') {
        format!("[{}]:{}", target_host, port.max(0))
    } else {
        format!("{}:{}", target_host, port.max(0))
    };

    let socket_id_v = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabDgramSocketId").unwrap().as_ptr(),
    );
    let mut socket_id_i: i64 = 0;
    let _ = qjs::JS_ToInt64(ctx, &mut socket_id_i, socket_id_v);
    js_free_value(ctx, socket_id_v);
    let socket_id = if socket_id_i > 0 {
        socket_id_i as u64
    } else {
        0
    };

    let send_res: Result<usize, std::io::Error> = if let Some(sock) =
        DGRAM_NATIVE_SOCKET_REGISTRY.with(|reg| reg.borrow().get(&socket_id).cloned())
    {
        sock.send_to(&payload, &target)
    } else {
        let bind_addr = if kind == "udp6" {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        };
        match UdpSocket::bind(bind_addr) {
            Ok(s) => s.send_to(&payload, &target),
            Err(e) => Err(e),
        }
    };
    match send_res {
        Ok(sent) => {
            dgram_clear_error(ctx, this);
            let bound_port_v = qjs::JS_GetPropertyStr(
                ctx,
                this,
                CString::new("__kawkabBoundPort").unwrap().as_ptr(),
            );
            let mut bound_port_i: i64 = 0;
            let _ = qjs::JS_ToInt64(ctx, &mut bound_port_i, bound_port_v);
            js_free_value(ctx, bound_port_v);
            let target_is_local =
                target_host == "127.0.0.1" || target_host == "::1" || target_host == "localhost";
            if bound_port_i > 0 && port.max(0) == bound_port_i && target_is_local {
                let payload_val = if payload.is_empty() {
                    qjs::JS_NewArrayBufferCopy(ctx, std::ptr::null(), 0)
                } else {
                    qjs::JS_NewArrayBufferCopy(ctx, payload.as_ptr(), payload.len())
                };
                let global = qjs::JS_GetGlobalObject(ctx);
                let buffer_ctor =
                    qjs::JS_GetPropertyStr(ctx, global, CString::new("Buffer").unwrap().as_ptr());
                let mut msg = payload_val;
                if qjs::JS_IsObject(buffer_ctor) {
                    let from_fn = qjs::JS_GetPropertyStr(
                        ctx,
                        buffer_ctor,
                        CString::new("from").unwrap().as_ptr(),
                    );
                    if qjs::JS_IsFunction(ctx, from_fn) != 0 {
                        let mut argv = [js_dup_value(payload_val)];
                        let out = qjs::JS_Call(ctx, from_fn, buffer_ctor, 1, argv.as_mut_ptr());
                        js_free_value(ctx, argv[0]);
                        if !is_exception(out) {
                            js_free_value(ctx, msg);
                            msg = out;
                        } else {
                            js_free_value(ctx, out);
                        }
                    }
                    js_free_value(ctx, from_fn);
                }
                js_free_value(ctx, buffer_ctor);
                js_free_value(ctx, global);

                let rinfo = qjs::JS_NewObject(ctx);
                qjs::JS_SetPropertyStr(
                    ctx,
                    rinfo,
                    CString::new("address").unwrap().as_ptr(),
                    qjs_compat::new_string_from_str(ctx, &target_host),
                );
                qjs::JS_SetPropertyStr(
                    ctx,
                    rinfo,
                    CString::new("family").unwrap().as_ptr(),
                    qjs_compat::new_string_from_str(
                        ctx,
                        if target_host.contains(':') {
                            "IPv6"
                        } else {
                            "IPv4"
                        },
                    ),
                );
                qjs::JS_SetPropertyStr(
                    ctx,
                    rinfo,
                    CString::new("port").unwrap().as_ptr(),
                    qjs_compat::new_int(ctx, bound_port_i),
                );
                qjs::JS_SetPropertyStr(
                    ctx,
                    rinfo,
                    CString::new("size").unwrap().as_ptr(),
                    qjs_compat::new_int(ctx, payload.len() as i64),
                );
                dgram_emit(ctx, this, "message", msg, true, rinfo, true);
                js_free_value(ctx, msg);
                js_free_value(ctx, rinfo);
            }
            if let Some(c) = cb {
                let mut argv = [js_undefined(), qjs_compat::new_int(ctx, sent as i64)];
                let ret = qjs::JS_Call(ctx, c, this, 2, argv.as_mut_ptr());
                js_free_value(ctx, argv[0]);
                js_free_value(ctx, argv[1]);
                if is_exception(ret) {
                    return ret;
                }
                js_free_value(ctx, ret);
            }
        }
        Err(e) => {
            let msg = format!("dgram.send failed: {e}");
            dgram_set_socket_error(ctx, this, &msg);
            let err = dgram_make_error(ctx, "EIO", &msg);
            if let Some(c) = cb {
                let mut argv = [js_dup_value(err)];
                let ret = qjs::JS_Call(ctx, c, this, 1, argv.as_mut_ptr());
                js_free_value(ctx, argv[0]);
                if is_exception(ret) {
                    js_free_value(ctx, err);
                    return ret;
                }
                js_free_value(ctx, ret);
            }
            dgram_emit(ctx, this, "error", err, true, js_undefined(), false);
            js_free_value(ctx, err);
        }
    }
    js_dup_value(this)
}

unsafe extern "C" fn js_dgram_sock_close(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = if argc > 0 {
        std::slice::from_raw_parts(argv, argc as usize)
    } else {
        &[]
    };
    let cb = if !args.is_empty() {
        let last = args[args.len() - 1];
        if qjs::JS_IsFunction(ctx, last) != 0 {
            Some(last)
        } else {
            None
        }
    } else {
        None
    };
    let socket_id_v = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabDgramSocketId").unwrap().as_ptr(),
    );
    let mut socket_id_i: i64 = 0;
    let _ = qjs::JS_ToInt64(ctx, &mut socket_id_i, socket_id_v);
    js_free_value(ctx, socket_id_v);
    if socket_id_i > 0 {
        let socket_id = socket_id_i as u64;
        DGRAM_NATIVE_SOCKET_REGISTRY.with(|reg| {
            reg.borrow_mut().remove(&socket_id);
        });
        let global = qjs::JS_GetGlobalObject(ctx);
        let store = qjs::JS_GetPropertyStr(
            ctx,
            global,
            CString::new("__kawkabDgramSocketStore").unwrap().as_ptr(),
        );
        if !qjs::JS_IsUndefined(store) {
            if let Ok(id_key) = CString::new(socket_id.to_string()) {
                qjs::JS_SetPropertyStr(ctx, store, id_key.as_ptr(), js_undefined());
            }
        }
        js_free_value(ctx, store);
        js_free_value(ctx, global);
        DGRAM_RECV_CANCEL_BY_ID.with(|m| {
            if let Some(cancel) = m.borrow_mut().remove(&socket_id) {
                cancel.store(true, Ordering::Release);
            }
        });
    }
    if let Some(c) = cb {
        let ret = qjs::JS_Call(ctx, c, this, 0, std::ptr::null_mut());
        if is_exception(ret) {
            return ret;
        }
        js_free_value(ctx, ret);
    }
    dgram_emit(
        ctx,
        this,
        "close",
        js_undefined(),
        false,
        js_undefined(),
        false,
    );
    js_dup_value(this)
}

unsafe extern "C" fn js_dgram_sock_address(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let k_kind = CString::new("__kawkabUdpKind").unwrap();
    let k_port = CString::new("__kawkabBoundPort").unwrap();
    let kind = qjs::JS_GetPropertyStr(ctx, this, k_kind.as_ptr());
    let kind_s = js_string_to_owned(ctx, kind);
    js_free_value(ctx, kind);
    let is_udp6 = kind_s == "udp6";
    let port_v = qjs::JS_GetPropertyStr(ctx, this, k_port.as_ptr());
    let mut port: i64 = 0;
    let _ = qjs::JS_ToInt64(ctx, &mut port, port_v);
    js_free_value(ctx, port_v);
    let bound_addr_v = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabBoundAddress").unwrap().as_ptr(),
    );
    let bound_addr = if qjs::JS_IsUndefined(bound_addr_v) {
        if is_udp6 { "::" } else { "0.0.0.0" }.to_string()
    } else {
        let s = js_string_to_owned(ctx, bound_addr_v);
        if s.is_empty() {
            if is_udp6 { "::" } else { "0.0.0.0" }.to_string()
        } else {
            s
        }
    };
    js_free_value(ctx, bound_addr_v);
    let out = qjs::JS_NewObject(ctx);
    let addr_s = bound_addr;
    let fam_s = if is_udp6 { "IPv6" } else { "IPv4" };
    qjs::JS_SetPropertyStr(
        ctx,
        out,
        CString::new("address").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &addr_s),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        out,
        CString::new("family").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, fam_s),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        out,
        CString::new("port").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, port),
    );
    out
}

unsafe fn dgram_install_socket_methods(ctx: *mut qjs::JSContext, sock: qjs::JSValue) {
    let _ = install_obj_fn(ctx, sock, "on", Some(js_dgram_sock_on), 2);
    let _ = install_obj_fn(ctx, sock, "addListener", Some(js_dgram_sock_on), 2);
    let _ = install_obj_fn(ctx, sock, "once", Some(js_dgram_sock_once), 2);
    let _ = install_obj_fn(
        ctx,
        sock,
        "removeListener",
        Some(js_dgram_sock_remove_listener),
        2,
    );
    let _ = install_obj_fn(
        ctx,
        sock,
        "removeAllListeners",
        Some(js_dgram_sock_remove_all_listeners),
        1,
    );
    let _ = install_obj_fn(ctx, sock, "bind", Some(js_dgram_sock_bind), 3);
    let _ = install_obj_fn(ctx, sock, "send", Some(js_dgram_sock_send), 6);
    let _ = install_obj_fn(ctx, sock, "close", Some(js_dgram_sock_close), 1);
    let _ = install_obj_fn(ctx, sock, "address", Some(js_dgram_sock_address), 0);
    let _ = install_obj_fn(ctx, sock, "setBroadcast", Some(js_noop), 1);
    let _ = install_obj_fn(ctx, sock, "setTTL", Some(js_noop), 1);
    let _ = install_obj_fn(ctx, sock, "setMulticastTTL", Some(js_noop), 1);
    let _ = install_obj_fn(ctx, sock, "addMembership", Some(js_noop), 2);
    let _ = install_obj_fn(ctx, sock, "dropMembership", Some(js_noop), 2);
    let _ = install_obj_fn(ctx, sock, "ref", Some(js_noop), 0);
    let _ = install_obj_fn(ctx, sock, "unref", Some(js_noop), 0);
}

unsafe extern "C" fn js_dgram_create_socket(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = if argc > 0 {
        std::slice::from_raw_parts(argv, argc as usize)
    } else {
        &[]
    };
    let kind = if !args.is_empty() {
        js_string_to_owned(ctx, args[0])
    } else {
        "udp4".to_string()
    };
    let sock = qjs::JS_NewObject(ctx);
    let k_kind = CString::new("__kawkabUdpKind").unwrap();
    let k_port = CString::new("__kawkabBoundPort").unwrap();
    let k_list = CString::new("__kawkabListeners").unwrap();
    let k_id = CString::new("__kawkabDgramSocketId").unwrap();
    let k_addr = CString::new("__kawkabBoundAddress").unwrap();
    let k_err = CString::new("__kawkabDgramLastError").unwrap();
    let id = NEXT_DGRAM_SOCKET_ID.fetch_add(1, Ordering::Relaxed);
    qjs::JS_SetPropertyStr(
        ctx,
        sock,
        k_kind.as_ptr(),
        qjs_compat::new_string_from_str(ctx, &kind),
    );
    qjs::JS_SetPropertyStr(ctx, sock, k_port.as_ptr(), qjs_compat::new_int(ctx, 0));
    qjs::JS_SetPropertyStr(
        ctx,
        sock,
        k_id.as_ptr(),
        qjs_compat::new_int(ctx, id as i64),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        sock,
        k_addr.as_ptr(),
        qjs_compat::new_string_from_str(ctx, if kind == "udp6" { "::" } else { "0.0.0.0" }),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        sock,
        k_err.as_ptr(),
        qjs_compat::new_string_from_str(ctx, ""),
    );
    qjs::JS_SetPropertyStr(ctx, sock, k_list.as_ptr(), qjs::JS_NewObject(ctx));
    dgram_install_socket_methods(ctx, sock);
    if args.len() > 1 && qjs::JS_IsFunction(ctx, args[1]) != 0 {
        if let Err(e) = dgram_push_listener(ctx, sock, "message", args[1], false) {
            js_free_value(ctx, sock);
            return e;
        }
    }
    sock
}

unsafe fn diag_ensure_channel(
    ctx: *mut qjs::JSContext,
    name: &str,
) -> Result<qjs::JSValue, qjs::JSValue> {
    let global = qjs::JS_GetGlobalObject(ctx);
    let k_store = CString::new("__kawkabDiagChannels").unwrap();
    let mut store = qjs::JS_GetPropertyStr(ctx, global, k_store.as_ptr());
    if qjs::JS_IsUndefined(store) {
        js_free_value(ctx, store);
        store = qjs::JS_NewObject(ctx);
        qjs::JS_SetPropertyStr(ctx, global, k_store.as_ptr(), js_dup_value(store));
    }
    js_free_value(ctx, global);

    let cname = match CString::new(name) {
        Ok(s) => s,
        Err(_) => {
            js_free_value(ctx, store);
            return Err(qjs::JS_ThrowTypeError(
                ctx,
                CString::new("diagnostics_channel: invalid channel name")
                    .unwrap()
                    .as_ptr(),
            ));
        }
    };
    let existing = qjs::JS_GetPropertyStr(ctx, store, cname.as_ptr());
    if !qjs::JS_IsUndefined(existing) {
        js_free_value(ctx, store);
        return Ok(existing);
    }
    js_free_value(ctx, existing);

    let ch = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        ch,
        CString::new("name").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, name),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        ch,
        CString::new("__kawkabSubs").unwrap().as_ptr(),
        qjs::JS_NewArray(ctx),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        ch,
        CString::new("__kawkabBoundStores").unwrap().as_ptr(),
        qjs::JS_NewArray(ctx),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        ch,
        CString::new("hasSubscribers").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, false),
    );
    let _ = install_obj_fn(ctx, ch, "publish", Some(js_diag_channel_publish), 1);
    let _ = install_obj_fn(ctx, ch, "subscribe", Some(js_diag_channel_subscribe), 1);
    let _ = install_obj_fn(ctx, ch, "unsubscribe", Some(js_diag_channel_unsubscribe), 1);
    let _ = install_obj_fn(ctx, ch, "bindStore", Some(js_diag_channel_bind_store), 2);
    let _ = install_obj_fn(
        ctx,
        ch,
        "unbindStore",
        Some(js_diag_channel_unbind_store),
        1,
    );
    let _ = install_obj_fn(ctx, ch, "runStores", Some(js_diag_channel_run_stores), 3);

    qjs::JS_SetPropertyStr(ctx, store, cname.as_ptr(), js_dup_value(ch));
    js_free_value(ctx, store);
    Ok(ch)
}

unsafe fn diag_update_has_subscribers(ctx: *mut qjs::JSContext, ch: qjs::JSValue) {
    let subs = qjs::JS_GetPropertyStr(ctx, ch, CString::new("__kawkabSubs").unwrap().as_ptr());
    if qjs::JS_IsUndefined(subs) {
        js_free_value(ctx, subs);
        return;
    }
    let len_v = qjs::JS_GetPropertyStr(ctx, subs, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_v);
    js_free_value(ctx, len_v);
    js_free_value(ctx, subs);
    qjs::JS_SetPropertyStr(
        ctx,
        ch,
        CString::new("hasSubscribers").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, len > 0),
    );
}

unsafe extern "C" fn js_diag_channel_subscribe(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("diagnostics_channel subscribe(callback) requires callback")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    if qjs::JS_IsFunction(ctx, args[0]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("diagnostics_channel subscribe callback must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    let subs = qjs::JS_GetPropertyStr(ctx, this, CString::new("__kawkabSubs").unwrap().as_ptr());
    if qjs::JS_IsUndefined(subs) {
        js_free_value(ctx, subs);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("diagnostics_channel: internal subscribers array missing")
                .unwrap()
                .as_ptr(),
        );
    }
    let len_v = qjs::JS_GetPropertyStr(ctx, subs, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_v);
    js_free_value(ctx, len_v);
    qjs::JS_SetPropertyUint32(ctx, subs, len as u32, js_dup_value(args[0]));
    js_free_value(ctx, subs);
    diag_update_has_subscribers(ctx, this);
    js_undefined()
}

unsafe extern "C" fn js_diag_channel_unsubscribe(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewBool(ctx, false);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let target = args[0];
    let subs = qjs::JS_GetPropertyStr(ctx, this, CString::new("__kawkabSubs").unwrap().as_ptr());
    if qjs::JS_IsUndefined(subs) {
        js_free_value(ctx, subs);
        return qjs::JS_NewBool(ctx, false);
    }
    let len_v = qjs::JS_GetPropertyStr(ctx, subs, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_v);
    js_free_value(ctx, len_v);
    let next = qjs::JS_NewArray(ctx);
    let mut ni: u32 = 0;
    let mut removed = false;
    let mut i: i32 = 0;
    while i < len {
        let cb = qjs::JS_GetPropertyUint32(ctx, subs, i as u32);
        if qjs::JS_StrictEq(ctx, cb, target) == 0 {
            qjs::JS_SetPropertyUint32(ctx, next, ni, js_dup_value(cb));
            ni += 1;
        } else {
            removed = true;
        }
        js_free_value(ctx, cb);
        i += 1;
    }
    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabSubs").unwrap().as_ptr(),
        next,
    );
    js_free_value(ctx, subs);
    diag_update_has_subscribers(ctx, this);
    qjs::JS_NewBool(ctx, removed)
}

unsafe extern "C" fn js_diag_channel_bind_store(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new(
                "diagnostics_channel channel.bindStore(store[, transform]) requires store",
            )
            .unwrap()
            .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let store = args[0];
    let transform = if argc > 1 && qjs::JS_IsFunction(ctx, args[1]) != 0 {
        Some(args[1])
    } else {
        None
    };
    let k_bounds = CString::new("__kawkabBoundStores").unwrap();
    let mut bounds = qjs::JS_GetPropertyStr(ctx, this, k_bounds.as_ptr());
    if qjs::JS_IsUndefined(bounds) {
        js_free_value(ctx, bounds);
        bounds = qjs::JS_NewArray(ctx);
    }
    let len_v = qjs::JS_GetPropertyStr(ctx, bounds, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_v);
    js_free_value(ctx, len_v);
    let mut i: i32 = 0;
    while i < len {
        let entry = qjs::JS_GetPropertyUint32(ctx, bounds, i as u32);
        let cur_store = qjs::JS_GetPropertyStr(ctx, entry, CString::new("store").unwrap().as_ptr());
        if qjs::JS_StrictEq(ctx, cur_store, store) != 0 {
            qjs::JS_SetPropertyStr(
                ctx,
                entry,
                CString::new("transform").unwrap().as_ptr(),
                match transform {
                    Some(v) => js_dup_value(v),
                    None => js_undefined(),
                },
            );
            js_free_value(ctx, cur_store);
            js_free_value(ctx, entry);
            qjs::JS_SetPropertyStr(ctx, this, k_bounds.as_ptr(), bounds);
            return js_undefined();
        }
        js_free_value(ctx, cur_store);
        js_free_value(ctx, entry);
        i += 1;
    }
    let entry = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        entry,
        CString::new("store").unwrap().as_ptr(),
        js_dup_value(store),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        entry,
        CString::new("transform").unwrap().as_ptr(),
        match transform {
            Some(v) => js_dup_value(v),
            None => js_undefined(),
        },
    );
    qjs::JS_SetPropertyUint32(ctx, bounds, len as u32, entry);
    qjs::JS_SetPropertyStr(ctx, this, k_bounds.as_ptr(), bounds);
    js_undefined()
}

unsafe extern "C" fn js_diag_channel_unbind_store(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewBool(ctx, false);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let target = args[0];
    let stores = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabBoundStores").unwrap().as_ptr(),
    );
    if qjs::JS_IsUndefined(stores) {
        js_free_value(ctx, stores);
        return qjs::JS_NewBool(ctx, false);
    }
    let len_v = qjs::JS_GetPropertyStr(ctx, stores, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_v);
    js_free_value(ctx, len_v);
    let next = qjs::JS_NewArray(ctx);
    let mut ni: u32 = 0;
    let mut removed = false;
    let mut i: i32 = 0;
    while i < len {
        let entry = qjs::JS_GetPropertyUint32(ctx, stores, i as u32);
        let store = qjs::JS_GetPropertyStr(ctx, entry, CString::new("store").unwrap().as_ptr());
        if qjs::JS_StrictEq(ctx, store, target) == 0 {
            qjs::JS_SetPropertyUint32(ctx, next, ni, js_dup_value(entry));
            ni += 1;
        } else {
            removed = true;
        }
        js_free_value(ctx, store);
        js_free_value(ctx, entry);
        i += 1;
    }
    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabBoundStores").unwrap().as_ptr(),
        next,
    );
    js_free_value(ctx, stores);
    qjs::JS_NewBool(ctx, removed)
}

unsafe extern "C" fn js_diag_channel_run_stores(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new(
                "diagnostics_channel.runStores(data, callback[, thisArg]) requires arguments",
            )
            .unwrap()
            .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let data = args[0];
    let cb = args[1];
    if qjs::JS_IsFunction(ctx, cb) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("diagnostics_channel.runStores callback must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    let this_arg = if argc >= 3 { args[2] } else { js_undefined() };
    let stores = qjs::JS_GetPropertyStr(
        ctx,
        _this,
        CString::new("__kawkabBoundStores").unwrap().as_ptr(),
    );
    if qjs::JS_IsUndefined(stores) {
        js_free_value(ctx, stores);
        let mut call_args = [js_dup_value(data)];
        let ret = qjs::JS_Call(ctx, cb, this_arg, 1, call_args.as_mut_ptr());
        js_free_value(ctx, call_args[0]);
        return ret;
    }
    let first = qjs::JS_GetPropertyUint32(ctx, stores, 0);
    js_free_value(ctx, stores);
    if qjs::JS_IsUndefined(first) {
        js_free_value(ctx, first);
        let mut call_args = [js_dup_value(data)];
        let ret = qjs::JS_Call(ctx, cb, this_arg, 1, call_args.as_mut_ptr());
        js_free_value(ctx, call_args[0]);
        return ret;
    }
    let store = qjs::JS_GetPropertyStr(ctx, first, CString::new("store").unwrap().as_ptr());
    let transform = qjs::JS_GetPropertyStr(ctx, first, CString::new("transform").unwrap().as_ptr());
    js_free_value(ctx, first);
    let transformed = if qjs::JS_IsFunction(ctx, transform) != 0 {
        let mut targs = [js_dup_value(data)];
        let tv = qjs::JS_Call(ctx, transform, js_undefined(), 1, targs.as_mut_ptr());
        js_free_value(ctx, targs[0]);
        tv
    } else {
        js_dup_value(data)
    };
    js_free_value(ctx, transform);
    let run_fn = qjs::JS_GetPropertyStr(ctx, store, CString::new("run").unwrap().as_ptr());
    if qjs::JS_IsFunction(ctx, run_fn) == 0 {
        js_free_value(ctx, run_fn);
        js_free_value(ctx, store);
        let mut call_args = [transformed];
        let ret = qjs::JS_Call(ctx, cb, this_arg, 1, call_args.as_mut_ptr());
        js_free_value(ctx, call_args[0]);
        return ret;
    }
    let mut run_args = [transformed, js_dup_value(cb)];
    let ret = qjs::JS_Call(ctx, run_fn, store, 2, run_args.as_mut_ptr());
    js_free_value(ctx, run_args[0]);
    js_free_value(ctx, run_args[1]);
    js_free_value(ctx, run_fn);
    js_free_value(ctx, store);
    ret
}

unsafe extern "C" fn js_diag_channel_publish(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let payload = if argc > 0 {
        let args = std::slice::from_raw_parts(argv, argc as usize);
        args[0]
    } else {
        js_undefined()
    };
    let name_v = qjs::JS_GetPropertyStr(ctx, this, CString::new("name").unwrap().as_ptr());
    let subs = qjs::JS_GetPropertyStr(ctx, this, CString::new("__kawkabSubs").unwrap().as_ptr());
    if qjs::JS_IsUndefined(subs) {
        js_free_value(ctx, subs);
        js_free_value(ctx, name_v);
        return js_undefined();
    }
    let len_v = qjs::JS_GetPropertyStr(ctx, subs, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_v);
    js_free_value(ctx, len_v);
    let mut i: i32 = 0;
    while i < len {
        let cb = qjs::JS_GetPropertyUint32(ctx, subs, i as u32);
        if qjs::JS_IsFunction(ctx, cb) != 0 {
            let arg0 = js_dup_value(payload);
            let arg1 = js_dup_value(name_v);
            let mut call_args = [arg0, arg1];
            let ret = qjs::JS_Call(ctx, cb, js_undefined(), 2, call_args.as_mut_ptr());
            js_free_value(ctx, call_args[0]);
            js_free_value(ctx, call_args[1]);
            if is_exception(ret) {
                return ret;
            }
            js_free_value(ctx, ret);
        }
        js_free_value(ctx, cb);
        i += 1;
    }
    js_free_value(ctx, subs);
    js_free_value(ctx, name_v);
    js_undefined()
}

unsafe extern "C" fn js_diag_channel(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("diagnostics_channel.channel(name) requires name")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let name = js_string_to_owned(ctx, args[0]);
    match diag_ensure_channel(ctx, &name) {
        Ok(v) => v,
        Err(e) => e,
    }
}

unsafe extern "C" fn js_diag_has_subscribers(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewBool(ctx, false);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let name = js_string_to_owned(ctx, args[0]);
    let ch = match diag_ensure_channel(ctx, &name) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let hs = qjs::JS_GetPropertyStr(ctx, ch, CString::new("hasSubscribers").unwrap().as_ptr());
    let out = qjs::JS_ToBool(ctx, hs);
    js_free_value(ctx, hs);
    js_free_value(ctx, ch);
    qjs::JS_NewBool(ctx, out != 0)
}

unsafe extern "C" fn js_diag_subscribe(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("diagnostics_channel.subscribe(name, callback) requires arguments")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    if qjs::JS_IsFunction(ctx, args[1]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("diagnostics_channel.subscribe callback must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    let name = js_string_to_owned(ctx, args[0]);
    let ch = match diag_ensure_channel(ctx, &name) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut cargs = [js_dup_value(args[1])];
    let ret = js_diag_channel_subscribe(ctx, ch, 1, cargs.as_mut_ptr());
    js_free_value(ctx, cargs[0]);
    js_free_value(ctx, ch);
    ret
}

unsafe extern "C" fn js_diag_unsubscribe(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_NewBool(ctx, false);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let name = js_string_to_owned(ctx, args[0]);
    let ch = match diag_ensure_channel(ctx, &name) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut cargs = [js_dup_value(args[1])];
    let removed = js_diag_channel_unsubscribe(ctx, ch, 1, cargs.as_mut_ptr());
    js_free_value(ctx, cargs[0]);
    js_free_value(ctx, ch);
    removed
}

unsafe fn diag_tracing_update_has_subscribers(ctx: *mut qjs::JSContext, tracing: qjs::JSValue) {
    let start = qjs::JS_GetPropertyStr(ctx, tracing, CString::new("start").unwrap().as_ptr());
    let end = qjs::JS_GetPropertyStr(ctx, tracing, CString::new("end").unwrap().as_ptr());
    let async_start =
        qjs::JS_GetPropertyStr(ctx, tracing, CString::new("asyncStart").unwrap().as_ptr());
    let async_end =
        qjs::JS_GetPropertyStr(ctx, tracing, CString::new("asyncEnd").unwrap().as_ptr());
    let error = qjs::JS_GetPropertyStr(ctx, tracing, CString::new("error").unwrap().as_ptr());
    let mut has = false;
    for ch in [start, end, async_start, async_end, error] {
        if !qjs::JS_IsUndefined(ch) {
            let hs =
                qjs::JS_GetPropertyStr(ctx, ch, CString::new("hasSubscribers").unwrap().as_ptr());
            has = has || qjs::JS_ToBool(ctx, hs) != 0;
            js_free_value(ctx, hs);
        }
        js_free_value(ctx, ch);
    }
    qjs::JS_SetPropertyStr(
        ctx,
        tracing,
        CString::new("hasSubscribers").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, has),
    );
}

unsafe extern "C" fn js_diag_tracing_subscribe(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return js_undefined();
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let subs = args[0];
    for (event, key) in [
        ("start", "start"),
        ("end", "end"),
        ("asyncStart", "asyncStart"),
        ("asyncEnd", "asyncEnd"),
        ("error", "error"),
    ] {
        let fn_v = qjs::JS_GetPropertyStr(ctx, subs, CString::new(key).unwrap().as_ptr());
        if qjs::JS_IsFunction(ctx, fn_v) != 0 {
            let ch = qjs::JS_GetPropertyStr(ctx, this, CString::new(event).unwrap().as_ptr());
            let mut cargs = [js_dup_value(fn_v)];
            let ret = js_diag_channel_subscribe(ctx, ch, 1, cargs.as_mut_ptr());
            js_free_value(ctx, cargs[0]);
            if !qjs::JS_IsUndefined(ret) {
                js_free_value(ctx, ret);
            }
            js_free_value(ctx, ch);
        }
        js_free_value(ctx, fn_v);
    }
    diag_tracing_update_has_subscribers(ctx, this);
    js_undefined()
}

unsafe extern "C" fn js_diag_tracing_unsubscribe(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewBool(ctx, false);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let subs = args[0];
    let mut ok = true;
    for (event, key) in [
        ("start", "start"),
        ("end", "end"),
        ("asyncStart", "asyncStart"),
        ("asyncEnd", "asyncEnd"),
        ("error", "error"),
    ] {
        let fn_v = qjs::JS_GetPropertyStr(ctx, subs, CString::new(key).unwrap().as_ptr());
        if qjs::JS_IsFunction(ctx, fn_v) != 0 {
            let ch = qjs::JS_GetPropertyStr(ctx, this, CString::new(event).unwrap().as_ptr());
            let mut cargs = [js_dup_value(fn_v)];
            let removed = js_diag_channel_unsubscribe(ctx, ch, 1, cargs.as_mut_ptr());
            js_free_value(ctx, cargs[0]);
            ok = ok && qjs::JS_ToBool(ctx, removed) != 0;
            js_free_value(ctx, removed);
            js_free_value(ctx, ch);
        }
        js_free_value(ctx, fn_v);
    }
    diag_tracing_update_has_subscribers(ctx, this);
    qjs::JS_NewBool(ctx, ok)
}

unsafe extern "C" fn js_diag_tracing_trace_sync(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("tracingChannel.traceSync requires fn")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    if qjs::JS_IsFunction(ctx, args[0]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("tracingChannel.traceSync fn must be function")
                .unwrap()
                .as_ptr(),
        );
    }
    let context = if argc > 1 {
        args[1]
    } else {
        qjs::JS_NewObject(ctx)
    };
    let this_arg = if argc > 2 { args[2] } else { js_undefined() };
    let rest_len = if argc > 3 { (argc - 3) as usize } else { 0 };
    let mut call_args = Vec::with_capacity(rest_len);
    for i in 0..rest_len {
        call_args.push(js_dup_value(args[i + 3]));
    }

    let start = qjs::JS_GetPropertyStr(ctx, this, CString::new("start").unwrap().as_ptr());
    let end = qjs::JS_GetPropertyStr(ctx, this, CString::new("end").unwrap().as_ptr());
    let err = qjs::JS_GetPropertyStr(ctx, this, CString::new("error").unwrap().as_ptr());
    let hs_v = qjs::JS_GetPropertyStr(ctx, this, CString::new("hasSubscribers").unwrap().as_ptr());
    let has_subs = qjs::JS_ToBool(ctx, hs_v) != 0;
    js_free_value(ctx, hs_v);
    if has_subs {
        let mut pargs = [js_dup_value(context)];
        let sret = js_diag_channel_publish(ctx, start, 1, pargs.as_mut_ptr());
        js_free_value(ctx, pargs[0]);
        if !qjs::JS_IsUndefined(sret) {
            js_free_value(ctx, sret);
        }
    }
    let ret = qjs::JS_Call(
        ctx,
        args[0],
        this_arg,
        call_args.len() as i32,
        call_args.as_mut_ptr(),
    );
    for v in call_args {
        js_free_value(ctx, v);
    }
    if is_exception(ret) {
        let mut eargs = [ret];
        let _ = js_diag_channel_publish(ctx, err, 1, eargs.as_mut_ptr());
        js_free_value(ctx, start);
        js_free_value(ctx, end);
        js_free_value(ctx, err);
        return ret;
    }
    let mut eargs = [js_dup_value(context)];
    let eret = js_diag_channel_publish(ctx, end, 1, eargs.as_mut_ptr());
    js_free_value(ctx, eargs[0]);
    if !qjs::JS_IsUndefined(eret) {
        js_free_value(ctx, eret);
    }
    js_free_value(ctx, start);
    js_free_value(ctx, end);
    js_free_value(ctx, err);
    ret
}

unsafe extern "C" fn js_diag_tracing_channel(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("diagnostics_channel.tracingChannel(nameOrChannels) requires input")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let tracing = qjs::JS_NewObject(ctx);
    if qjs::JS_IsString(args[0]) {
        let base = js_string_to_owned(ctx, args[0]);
        let start =
            diag_ensure_channel(ctx, &format!("tracing:{}:start", base)).unwrap_or_else(|e| e);
        let end = diag_ensure_channel(ctx, &format!("tracing:{}:end", base)).unwrap_or_else(|e| e);
        let async_start =
            diag_ensure_channel(ctx, &format!("tracing:{}:asyncStart", base)).unwrap_or_else(|e| e);
        let async_end =
            diag_ensure_channel(ctx, &format!("tracing:{}:asyncEnd", base)).unwrap_or_else(|e| e);
        let error =
            diag_ensure_channel(ctx, &format!("tracing:{}:error", base)).unwrap_or_else(|e| e);
        qjs::JS_SetPropertyStr(ctx, tracing, CString::new("start").unwrap().as_ptr(), start);
        qjs::JS_SetPropertyStr(ctx, tracing, CString::new("end").unwrap().as_ptr(), end);
        qjs::JS_SetPropertyStr(
            ctx,
            tracing,
            CString::new("asyncStart").unwrap().as_ptr(),
            async_start,
        );
        qjs::JS_SetPropertyStr(
            ctx,
            tracing,
            CString::new("asyncEnd").unwrap().as_ptr(),
            async_end,
        );
        qjs::JS_SetPropertyStr(ctx, tracing, CString::new("error").unwrap().as_ptr(), error);
    } else {
        for key in ["start", "end", "asyncStart", "asyncEnd", "error"] {
            qjs::JS_SetPropertyStr(
                ctx,
                tracing,
                CString::new(key).unwrap().as_ptr(),
                qjs::JS_GetPropertyStr(ctx, args[0], CString::new(key).unwrap().as_ptr()),
            );
        }
    }
    let _ = install_obj_fn(
        ctx,
        tracing,
        "subscribe",
        Some(js_diag_tracing_subscribe),
        1,
    );
    let _ = install_obj_fn(
        ctx,
        tracing,
        "unsubscribe",
        Some(js_diag_tracing_unsubscribe),
        1,
    );
    let _ = install_obj_fn(
        ctx,
        tracing,
        "traceSync",
        Some(js_diag_tracing_trace_sync),
        4,
    );
    qjs::JS_SetPropertyStr(
        ctx,
        tracing,
        CString::new("tracePromise").unwrap().as_ptr(),
        qjs::JS_GetPropertyStr(ctx, tracing, CString::new("traceSync").unwrap().as_ptr()),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        tracing,
        CString::new("traceCallback").unwrap().as_ptr(),
        qjs::JS_GetPropertyStr(ctx, tracing, CString::new("traceSync").unwrap().as_ptr()),
    );
    diag_tracing_update_has_subscribers(ctx, tracing);
    tracing
}

unsafe extern "C" fn js_diag_bounded_channel(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let tr = js_diag_tracing_channel(ctx, _this, argc, argv);
    if is_exception(tr) {
        return tr;
    }
    qjs::JS_DeleteProperty(
        ctx,
        tr,
        qjs::JS_NewAtom(ctx, CString::new("asyncStart").unwrap().as_ptr()),
        0,
    );
    qjs::JS_DeleteProperty(
        ctx,
        tr,
        qjs::JS_NewAtom(ctx, CString::new("asyncEnd").unwrap().as_ptr()),
        0,
    );
    qjs::JS_DeleteProperty(
        ctx,
        tr,
        qjs::JS_NewAtom(ctx, CString::new("error").unwrap().as_ptr()),
        0,
    );
    tr
}

fn dns_family_and_addr(host: &str, force_family: Option<i32>) -> (String, i32) {
    let family = force_family.unwrap_or_else(|| if host.contains(':') { 6 } else { 4 });
    if family == 6 {
        if host.is_empty() || host == "localhost" {
            ("::1".to_string(), 6)
        } else {
            (host.to_string(), 6)
        }
    } else if host.is_empty() || host == "localhost" {
        ("127.0.0.1".to_string(), 4)
    } else {
        (host.to_string(), 4)
    }
}

unsafe extern "C" fn js_dns_lookup(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dns.lookup(hostname, callback) requires arguments")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let host = js_string_to_owned(ctx, args[0]);
    let mut cb_idx = 1usize;
    let mut forced_family: Option<i32> = None;
    if argc >= 3 && qjs::JS_IsFunction(ctx, args[2]) != 0 {
        cb_idx = 2;
        let family_v =
            qjs::JS_GetPropertyStr(ctx, args[1], CString::new("family").unwrap().as_ptr());
        if !qjs::JS_IsUndefined(family_v) {
            let mut fam: i32 = 0;
            let _ = qjs::JS_ToInt32(ctx, &mut fam, family_v);
            if fam == 4 || fam == 6 {
                forced_family = Some(fam);
            }
        }
        js_free_value(ctx, family_v);
    }
    if qjs::JS_IsFunction(ctx, args[cb_idx]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dns.lookup callback must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    let (addr, family) = dns_family_and_addr(&host, forced_family);
    let cb = args[cb_idx];
    let err = js_null();
    let a = qjs_compat::new_string_from_str(ctx, &addr);
    let fam = qjs_compat::new_int(ctx, family as i64);
    let mut call_args = [err, a, fam];
    let ret = qjs::JS_Call(ctx, cb, js_undefined(), 3, call_args.as_mut_ptr());
    js_free_value(ctx, call_args[1]);
    js_free_value(ctx, call_args[2]);
    if is_exception(ret) {
        return ret;
    }
    js_free_value(ctx, ret);
    js_undefined()
}

unsafe extern "C" fn js_dns_resolve4(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dns.resolve4(hostname, callback) requires arguments")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    if qjs::JS_IsFunction(ctx, args[1]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dns.resolve4 callback must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    let host = js_string_to_owned(ctx, args[0]);
    let (addr, _) = dns_family_and_addr(&host, Some(4));
    let out = qjs::JS_NewArray(ctx);
    qjs::JS_SetPropertyUint32(ctx, out, 0, qjs_compat::new_string_from_str(ctx, &addr));
    let err = js_null();
    let mut call_args = [err, out];
    let ret = qjs::JS_Call(ctx, args[1], js_undefined(), 2, call_args.as_mut_ptr());
    if is_exception(ret) {
        return ret;
    }
    js_free_value(ctx, ret);
    js_undefined()
}

unsafe extern "C" fn js_dns_resolve6(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dns.resolve6(hostname, callback) requires arguments")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    if qjs::JS_IsFunction(ctx, args[1]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("dns.resolve6 callback must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    let host = js_string_to_owned(ctx, args[0]);
    let (addr, _) = dns_family_and_addr(&host, Some(6));
    let out = qjs::JS_NewArray(ctx);
    qjs::JS_SetPropertyUint32(ctx, out, 0, qjs_compat::new_string_from_str(ctx, &addr));
    let err = js_null();
    let mut call_args = [err, out];
    let ret = qjs::JS_Call(ctx, args[1], js_undefined(), 2, call_args.as_mut_ptr());
    if is_exception(ret) {
        return ret;
    }
    js_free_value(ctx, ret);
    js_undefined()
}

unsafe extern "C" fn js_vm_run_in_this_context(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return js_undefined();
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let code = js_string_to_owned(ctx, args[0]);
    let file = CString::new("kawkab:vm").unwrap_or_default();
    qjs_compat::eval(
        ctx,
        code.as_ptr() as *const i8,
        code.len(),
        file.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    )
}

/// `vm.runInNewContext`: sandbox string keys become params; compile `return (<code>);`.
unsafe fn vm_run_in_new_context_with_sandbox(
    ctx: *mut qjs::JSContext,
    code: &str,
    sandbox: qjs::JSValue,
) -> qjs::JSValue {
    let mut ptab: *mut qjs::JSPropertyEnum = std::ptr::null_mut();
    let mut plen: u32 = 0;
    if qjs::JS_GetOwnPropertyNames(
        ctx,
        &mut ptab,
        &mut plen,
        sandbox,
        qjs::JS_GPN_STRING_MASK as i32,
    ) < 0
    {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("runInNewContext: failed to enumerate context")
                .unwrap_or_default()
                .as_ptr(),
        );
    }

    let mut keys: Vec<String> = Vec::new();
    let mut vals: Vec<qjs::JSValue> = Vec::new();

    for i in 0..plen {
        let atom = (*ptab.add(i as usize)).atom;
        let mut alen: usize = 0;
        let name_ptr = qjs::JS_AtomToCStringLen(ctx, &mut alen, atom);
        let key = if !name_ptr.is_null() {
            std::ffi::CStr::from_ptr(name_ptr)
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        if !name_ptr.is_null() {
            qjs::JS_FreeCString(ctx, name_ptr);
        }
        if key.is_empty() {
            continue;
        }
        let val = qjs::JS_GetPropertyInternal(ctx, sandbox, atom, sandbox, 0);
        vals.push(js_dup_value(val));
        js_free_value(ctx, val);
        keys.push(key);
    }
    qjs::JS_FreePropertyEnum(ctx, ptab, plen);

    let body = format!("return ({});", code);
    let body_c = match CString::new(body) {
        Ok(s) => s,
        Err(_) => {
            for v in vals {
                js_free_value(ctx, v);
            }
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new("runInNewContext: code contains NUL byte")
                    .unwrap_or_default()
                    .as_ptr(),
            );
        }
    };

    let global = qjs::JS_GetGlobalObject(ctx);
    let fn_ctor = qjs::JS_GetPropertyStr(ctx, global, CString::new("Function").unwrap().as_ptr());
    js_free_value(ctx, global);

    let mut ctor_argv: Vec<qjs::JSValue> = Vec::with_capacity(keys.len() + 1);
    for k in &keys {
        let ck = match CString::new(k.as_str()) {
            Ok(s) => s,
            Err(_) => {
                for v in ctor_argv {
                    js_free_value(ctx, v);
                }
                for v in vals {
                    js_free_value(ctx, v);
                }
                js_free_value(ctx, fn_ctor);
                return qjs::JS_ThrowTypeError(
                    ctx,
                    CString::new("runInNewContext: invalid context key")
                        .unwrap_or_default()
                        .as_ptr(),
                );
            }
        };
        ctor_argv.push(qjs::JS_NewStringLen(ctx, ck.as_ptr(), ck.as_bytes().len()));
    }
    ctor_argv.push(qjs::JS_NewStringLen(
        ctx,
        body_c.as_ptr(),
        body_c.as_bytes().len(),
    ));

    let fn_obj = qjs::JS_CallConstructor(
        ctx,
        fn_ctor,
        ctor_argv.len() as c_int,
        ctor_argv.as_mut_ptr(),
    );

    for v in ctor_argv {
        js_free_value(ctx, v);
    }
    js_free_value(ctx, fn_ctor);

    if is_exception(fn_obj) {
        for v in vals {
            js_free_value(ctx, v);
        }
        return fn_obj;
    }

    let out = if vals.is_empty() {
        qjs::JS_Call(ctx, fn_obj, js_undefined(), 0, std::ptr::null_mut())
    } else {
        qjs::JS_Call(
            ctx,
            fn_obj,
            js_undefined(),
            vals.len() as c_int,
            vals.as_mut_ptr(),
        )
    };
    js_free_value(ctx, fn_obj);
    for v in vals {
        js_free_value(ctx, v);
    }
    out
}

unsafe extern "C" fn js_vm_run_in_new_context(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return js_undefined();
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let code = js_string_to_owned(ctx, args[0]);
    let file = CString::new("kawkab:vm_new_context").unwrap_or_default();
    if argc >= 2 && qjs::JS_IsObject(args[1]) {
        return vm_run_in_new_context_with_sandbox(ctx, &code, args[1]);
    }
    qjs_compat::eval(
        ctx,
        code.as_ptr() as *const i8,
        code.len(),
        file.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    )
}

unsafe extern "C" fn js_string_decoder_write(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let next = if argc < 1 {
        String::new()
    } else {
        let args = std::slice::from_raw_parts(argv, argc as usize);
        js_string_to_owned(ctx, args[0])
    };
    let carry_js =
        qjs::JS_GetPropertyStr(ctx, this, CString::new("__kawkabCarry").unwrap().as_ptr());
    let carry = js_string_to_owned(ctx, carry_js);
    js_free_value(ctx, carry_js);

    let mut bytes: Vec<u8> = carry.chars().map(|c| (c as u32 & 0xff) as u8).collect();
    bytes.extend(next.chars().map(|c| (c as u32 & 0xff) as u8));

    let (decoded, remain) = match std::str::from_utf8(&bytes) {
        Ok(s) => (s.to_string(), Vec::new()),
        Err(e) => {
            let up = e.valid_up_to();
            if e.error_len().is_none() {
                let ok = String::from_utf8_lossy(&bytes[..up]).to_string();
                let rem = bytes[up..].to_vec();
                (ok, rem)
            } else {
                (String::from_utf8_lossy(&bytes).to_string(), Vec::new())
            }
        }
    };

    let remain_str: String = remain.iter().map(|b| *b as char).collect();
    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabCarry").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &remain_str),
    );

    qjs_compat::new_string_from_str(ctx, &decoded)
}

unsafe extern "C" fn js_string_decoder_ctor(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let obj = qjs::JS_NewObject(ctx);
    let _ = install_obj_fn(ctx, obj, "write", Some(js_string_decoder_write), 1);
    let _ = install_obj_fn(ctx, obj, "end", Some(js_string_decoder_write), 1);
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("__kawkabCarry").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, ""),
    );
    obj
}

fn to_hex_bytes(input: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(input.len() * 2);
    for b in input {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

unsafe extern "C" fn js_worker_ctor(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let obj = qjs::JS_NewObject(ctx);
    let _ = install_obj_fn(ctx, obj, "on", Some(js_worker_on), 2);
    let _ = install_obj_fn(ctx, obj, "postMessage", Some(js_worker_post_message), 1);
    let _ = install_obj_fn(ctx, obj, "terminate", Some(js_worker_terminate), 0);
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("threadId").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, 0),
    );
    obj
}

unsafe extern "C" fn js_worker_on(
    _ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    this
}

unsafe extern "C" fn js_worker_post_message(
    _ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    js_undefined()
}

unsafe extern "C" fn js_worker_terminate(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    qjs_compat::new_int(ctx, 0)
}

unsafe extern "C" fn js_node_test_run(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return js_undefined();
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let cb = args[1];
    if qjs::JS_IsFunction(ctx, cb) == 0 {
        return js_undefined();
    }
    let ret = qjs::JS_Call(ctx, cb, js_undefined(), 0, std::ptr::null_mut());
    if is_exception(ret) {
        return ret;
    }
    js_free_value(ctx, ret);
    js_undefined()
}

unsafe extern "C" fn js_resolve_path(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("resolvePath(baseDir, request) requires arguments")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let base = js_string_to_owned(ctx, args[0]);
    let request = js_string_to_owned(ctx, args[1]);
    let mut resolved = if request.starts_with('/') {
        PathBuf::from(&request)
    } else {
        Path::new(&base).join(&request)
    };
    if resolved.extension().is_none() {
        resolved = resolved.with_extension("js");
    }
    let normalized = resolved.canonicalize().unwrap_or(resolved);
    match CString::new(normalized.to_string_lossy().to_string()) {
        Ok(c) => qjs_compat::new_string_from_cstr(ctx, c.as_ptr()),
        Err(_) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new("resolved path contains interior NUL byte")
                .unwrap()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_get_cwd(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    match std::env::current_dir() {
        Ok(p) => match CString::new(p.to_string_lossy().to_string()) {
            Ok(c) => qjs_compat::new_string_from_cstr(ctx, c.as_ptr()),
            Err(_) => qjs::JS_ThrowTypeError(
                ctx,
                CString::new("cwd contains interior NUL byte")
                    .unwrap()
                    .as_ptr(),
            ),
        },
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("cwd failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_get_platform(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let platform = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "unknown"
    };
    qjs_compat::new_string_from_cstr(ctx, CString::new(platform).unwrap().as_ptr())
}

unsafe extern "C" fn js_run_command(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let policy = RuntimePolicy::from_env();
    if !policy.allow_child_process {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new(
                "child_process is disabled by policy (set KAWKAB_ALLOW_CHILD_PROCESS=1 to enable)",
            )
            .unwrap()
            .as_ptr(),
        );
    }
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("runCommand(command) requires command")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let cmd = js_string_to_owned(ctx, args[0]);
    let output = Command::new("sh").arg("-lc").arg(cmd).output();
    match output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            match CString::new(text) {
                Ok(c) => qjs_compat::new_string_from_cstr(ctx, c.as_ptr()),
                Err(_) => qjs::JS_ThrowTypeError(
                    ctx,
                    CString::new("command output contains interior NUL byte")
                        .unwrap()
                        .as_ptr(),
                ),
            }
        }
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("command failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_sleep_ms(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("sleepMs(ms) requires milliseconds")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let ms = js_string_to_owned(ctx, args[0]).parse::<u64>().unwrap_or(0);
    let d = Duration::from_millis(ms.min(86_400_000));
    if let Ok(h) = Handle::try_current() {
        let _ = h.block_on(tokio::time::sleep(d));
    } else {
        std::thread::sleep(d);
    }
    js_undefined()
}

unsafe extern "C" fn js_process_uptime(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    qjs::JS_NewFloat64(ctx, process_hrtime_start().elapsed().as_secs_f64())
}

unsafe extern "C" fn js_process_hrtime_bigint(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let ns = process_hrtime_start().elapsed().as_nanos();
    let v = (ns.min(i64::MAX as u128)) as i64;
    qjs::JS_NewBigInt64(ctx, v)
}

unsafe extern "C" fn js_process_hrtime(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let (cur_s, cur_ns) = hrtime_tuple_now();
    if argc < 1 {
        return js_hrtime_pair_array(ctx, cur_s, cur_ns);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let prev = args[0];
    if !qjs::JS_IsObject(prev) {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("process.hrtime() optional argument must be [seconds, nanoseconds]")
                .unwrap()
                .as_ptr(),
        );
    }
    let k0 = CString::new("0").unwrap();
    let k1 = CString::new("1").unwrap();
    let v0 = qjs::JS_GetPropertyStr(ctx, prev, k0.as_ptr());
    let v1 = qjs::JS_GetPropertyStr(ctx, prev, k1.as_ptr());
    let mut ps = 0.0f64;
    let mut pns = 0.0f64;
    let _ = qjs::JS_ToFloat64(ctx, &mut ps as *mut f64, v0);
    let _ = qjs::JS_ToFloat64(ctx, &mut pns as *mut f64, v1);
    js_free_value(ctx, v0);
    js_free_value(ctx, v1);
    let ps_u = ps.max(0.0) as u64;
    let pns_u = (pns.max(0.0) as u64).min(999_999_999) as u32;
    let cur_total = hrtime_total_ns(cur_s, cur_ns);
    let prev_total = hrtime_total_ns(ps_u, pns_u);
    let diff = cur_total.saturating_sub(prev_total);
    let ds = (diff / 1_000_000_000) as u64;
    let dns = (diff % 1_000_000_000) as u32;
    js_hrtime_pair_array(ctx, ds, dns)
}

unsafe fn assert_type_error(ctx: *mut qjs::JSContext, msg: &str) -> qjs::JSValue {
    qjs::JS_ThrowTypeError(ctx, CString::new(msg).unwrap_or_default().as_ptr())
}

unsafe fn assert_optional_message(
    ctx: *mut qjs::JSContext,
    argc: c_int,
    argv: *mut qjs::JSValue,
    msg_idx: usize,
    default: &str,
) -> String {
    if argc as usize > msg_idx {
        js_string_to_owned(ctx, *argv.add(msg_idx))
    } else {
        default.to_string()
    }
}

unsafe fn throw_assertion_error(
    ctx: *mut qjs::JSContext,
    message: &str,
    actual: Option<qjs::JSValue>,
    expected: Option<qjs::JSValue>,
    operator: Option<&str>,
) -> qjs::JSValue {
    let err = qjs::JS_NewError(ctx);
    let k_msg = CString::new("message").unwrap();
    let msg_s = qjs_compat::new_string_from_str(ctx, message);
    qjs::JS_SetPropertyStr(ctx, err, k_msg.as_ptr(), msg_s);
    let k_name = CString::new("name").unwrap();
    let n_s = qjs_compat::new_string_from_str(ctx, "AssertionError");
    qjs::JS_SetPropertyStr(ctx, err, k_name.as_ptr(), n_s);
    let k_code = CString::new("code").unwrap();
    let c_s = qjs_compat::new_string_from_str(ctx, "ERR_ASSERTION");
    qjs::JS_SetPropertyStr(ctx, err, k_code.as_ptr(), c_s);
    if let Some(a) = actual {
        let k = CString::new("actual").unwrap();
        qjs::JS_SetPropertyStr(ctx, err, k.as_ptr(), a);
    }
    if let Some(e) = expected {
        let k = CString::new("expected").unwrap();
        qjs::JS_SetPropertyStr(ctx, err, k.as_ptr(), e);
    }
    if let Some(op) = operator {
        let k = CString::new("operator").unwrap();
        let v = qjs_compat::new_string_from_str(ctx, op);
        qjs::JS_SetPropertyStr(ctx, err, k.as_ptr(), v);
    }
    let k_gen = CString::new("generatedMessage").unwrap();
    qjs::JS_SetPropertyStr(ctx, err, k_gen.as_ptr(), qjs::JS_NewBool(ctx, true));
    qjs::JS_Throw(ctx, err)
}

/// Uses `globalThis.__kawkabLooseEq` for real JS `==`.
unsafe fn assert_loose_equal_bool(
    ctx: *mut qjs::JSContext,
    a: qjs::JSValue,
    b: qjs::JSValue,
) -> Result<bool, qjs::JSValue> {
    if qjs::JS_StrictEq(ctx, a, b) != 0 {
        return Ok(true);
    }
    let a_n = a.tag == qjs::JS_TAG_NULL as i64 || a.tag == qjs::JS_TAG_UNDEFINED as i64;
    let b_n = b.tag == qjs::JS_TAG_NULL as i64 || b.tag == qjs::JS_TAG_UNDEFINED as i64;
    if a_n && b_n {
        return Ok(true);
    }
    let global = qjs::JS_GetGlobalObject(ctx);
    let helper_k = CString::new("__kawkabLooseEq").unwrap();
    let helper = qjs::JS_GetPropertyStr(ctx, global, helper_k.as_ptr());
    js_free_value(ctx, global);
    if qjs::JS_IsFunction(ctx, helper) == 0 {
        js_free_value(ctx, helper);
        return assert_loose_equal_string_fallback(ctx, a, b);
    }
    let mut argv = [js_dup_value(a), js_dup_value(b)];
    let r = qjs::JS_Call(ctx, helper, js_undefined(), 2, argv.as_mut_ptr());
    js_free_value(ctx, helper);
    js_free_value(ctx, argv[0]);
    js_free_value(ctx, argv[1]);
    if is_exception(r) {
        return Err(r);
    }
    let ok = qjs::JS_ToBool(ctx, r) != 0;
    js_free_value(ctx, r);
    Ok(ok)
}

unsafe fn assert_loose_equal_string_fallback(
    ctx: *mut qjs::JSContext,
    a: qjs::JSValue,
    b: qjs::JSValue,
) -> Result<bool, qjs::JSValue> {
    let t0 = qjs::JS_ToString(ctx, a);
    if is_exception(t0) {
        return Err(t0);
    }
    let t1 = qjs::JS_ToString(ctx, b);
    if is_exception(t1) {
        js_free_value(ctx, t0);
        return Err(t1);
    }
    let out = qjs::JS_StrictEq(ctx, t0, t1) != 0;
    js_free_value(ctx, t0);
    js_free_value(ctx, t1);
    Ok(out)
}

#[inline]
unsafe fn js_object_ptr_key(v: qjs::JSValue) -> Option<usize> {
    if v.tag != qjs::JS_TAG_OBJECT as i64 {
        return None;
    }
    Some(v.u.ptr as usize)
}

unsafe fn is_date_object(ctx: *mut qjs::JSContext, v: qjs::JSValue) -> bool {
    if v.tag != qjs::JS_TAG_OBJECT as i64 {
        return false;
    }
    let global = qjs::JS_GetGlobalObject(ctx);
    let date_k = CString::new("Date").unwrap();
    let dt = qjs::JS_GetPropertyStr(ctx, global, date_k.as_ptr());
    js_free_value(ctx, global);
    if qjs::JS_IsConstructor(ctx, dt) == 0 {
        js_free_value(ctx, dt);
        return false;
    }
    let ok = qjs::JS_IsInstanceOf(ctx, v, dt) != 0;
    js_free_value(ctx, dt);
    ok
}

unsafe fn is_regexp_object(ctx: *mut qjs::JSContext, v: qjs::JSValue) -> bool {
    if v.tag != qjs::JS_TAG_OBJECT as i64 {
        return false;
    }
    let global = qjs::JS_GetGlobalObject(ctx);
    let re_k = CString::new("RegExp").unwrap();
    let re_ctor = qjs::JS_GetPropertyStr(ctx, global, re_k.as_ptr());
    js_free_value(ctx, global);
    if qjs::JS_IsConstructor(ctx, re_ctor) == 0 {
        js_free_value(ctx, re_ctor);
        return false;
    }
    let ok = qjs::JS_IsInstanceOf(ctx, v, re_ctor) != 0;
    js_free_value(ctx, re_ctor);
    ok
}

unsafe fn date_get_time(ctx: *mut qjs::JSContext, d: qjs::JSValue) -> Result<f64, qjs::JSValue> {
    let get_time = CString::new("getTime").unwrap();
    let g = qjs::JS_GetPropertyStr(ctx, d, get_time.as_ptr());
    if qjs::JS_IsFunction(ctx, g) == 0 {
        js_free_value(ctx, g);
        return Ok(f64::NAN);
    }
    let r = qjs::JS_Call(ctx, g, d, 0, ptr::null_mut());
    js_free_value(ctx, g);
    if is_exception(r) {
        return Err(r);
    }
    let mut t = 0.0f64;
    qjs::JS_ToFloat64(ctx, &mut t, r);
    js_free_value(ctx, r);
    Ok(t)
}

unsafe fn regexp_source_flags_equal(
    ctx: *mut qjs::JSContext,
    a: qjs::JSValue,
    b: qjs::JSValue,
) -> Result<bool, qjs::JSValue> {
    let k_src = CString::new("source").unwrap();
    let k_fl = CString::new("flags").unwrap();
    let sa = qjs::JS_GetPropertyStr(ctx, a, k_src.as_ptr());
    let sb = qjs::JS_GetPropertyStr(ctx, b, k_src.as_ptr());
    if is_exception(sa) {
        js_free_value(ctx, sb);
        return Err(sa);
    }
    if is_exception(sb) {
        js_free_value(ctx, sa);
        return Err(sb);
    }
    if qjs::JS_StrictEq(ctx, sa, sb) == 0 {
        js_free_value(ctx, sa);
        js_free_value(ctx, sb);
        return Ok(false);
    }
    js_free_value(ctx, sa);
    js_free_value(ctx, sb);
    let fa = qjs::JS_GetPropertyStr(ctx, a, k_fl.as_ptr());
    let fb = qjs::JS_GetPropertyStr(ctx, b, k_fl.as_ptr());
    if is_exception(fa) {
        js_free_value(ctx, fb);
        return Err(fa);
    }
    if is_exception(fb) {
        js_free_value(ctx, fa);
        return Err(fb);
    }
    let ok = qjs::JS_StrictEq(ctx, fa, fb) != 0;
    js_free_value(ctx, fa);
    js_free_value(ctx, fb);
    Ok(ok)
}

unsafe fn js_array_length_u32(
    ctx: *mut qjs::JSContext,
    arr: qjs::JSValue,
) -> Result<u32, qjs::JSValue> {
    let lk = CString::new("length").unwrap();
    let lv = qjs::JS_GetPropertyStr(ctx, arr, lk.as_ptr());
    if is_exception(lv) {
        return Err(lv);
    }
    let mut n = 0.0f64;
    qjs::JS_ToFloat64(ctx, &mut n, lv);
    js_free_value(ctx, lv);
    Ok(if n <= 0.0 { 0 } else { n as u32 })
}

unsafe fn assert_own_enum_string_keys(
    ctx: *mut qjs::JSContext,
    obj: qjs::JSValue,
) -> Result<Vec<String>, qjs::JSValue> {
    let mut ptab: *mut qjs::JSPropertyEnum = std::ptr::null_mut();
    let mut plen: u32 = 0;
    let flags = (qjs::JS_GPN_STRING_MASK | qjs::JS_GPN_ENUM_ONLY) as i32;
    if qjs::JS_GetOwnPropertyNames(ctx, &mut ptab, &mut plen, obj, flags) < 0 {
        return Err(qjs::JS_ThrowTypeError(
            ctx,
            CString::new("assert: failed to enumerate object keys")
                .unwrap_or_default()
                .as_ptr(),
        ));
    }
    let mut keys: Vec<String> = Vec::new();
    for i in 0..plen {
        let atom = (*ptab.add(i as usize)).atom;
        let mut alen: usize = 0;
        let name_ptr = qjs::JS_AtomToCStringLen(ctx, &mut alen, atom);
        let key = if !name_ptr.is_null() {
            std::ffi::CStr::from_ptr(name_ptr)
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        if !name_ptr.is_null() {
            qjs::JS_FreeCString(ctx, name_ptr);
        }
        keys.push(key);
    }
    qjs::JS_FreePropertyEnum(ctx, ptab, plen);
    keys.sort();
    Ok(keys)
}

unsafe fn assert_deep_equal_inner(
    ctx: *mut qjs::JSContext,
    a: qjs::JSValue,
    b: qjs::JSValue,
    strict: bool,
    stack: &mut Vec<(usize, usize)>,
) -> Result<bool, qjs::JSValue> {
    if qjs::JS_SameValue(ctx, a, b) != 0 {
        return Ok(true);
    }
    if qjs::JS_IsFunction(ctx, a) != 0 && qjs::JS_IsFunction(ctx, b) != 0 {
        return Ok(qjs::JS_StrictEq(ctx, a, b) != 0);
    }
    let obj_a = a.tag == qjs::JS_TAG_OBJECT as i64;
    let obj_b = b.tag == qjs::JS_TAG_OBJECT as i64;
    if !obj_a || !obj_b {
        if strict {
            return Ok(qjs::JS_SameValue(ctx, a, b) != 0);
        }
        return assert_loose_equal_bool(ctx, a, b);
    }
    if qjs::JS_IsArray(ctx, a) != 0 && qjs::JS_IsArray(ctx, b) != 0 {
        let pair = match (js_object_ptr_key(a), js_object_ptr_key(b)) {
            (Some(pa), Some(pb)) => Some((pa, pb)),
            _ => None,
        };
        let mut pushed = false;
        if let Some(p) = pair {
            if stack.contains(&p) {
                return Ok(true);
            }
            stack.push(p);
            pushed = true;
        }
        let res: Result<bool, qjs::JSValue> = (|| {
            let la = js_array_length_u32(ctx, a)?;
            let lb = js_array_length_u32(ctx, b)?;
            if la != lb {
                return Ok(false);
            }
            for i in 0..la {
                let av = qjs::JS_GetPropertyUint32(ctx, a, i);
                if is_exception(av) {
                    return Err(av);
                }
                let bv = qjs::JS_GetPropertyUint32(ctx, b, i);
                if is_exception(bv) {
                    js_free_value(ctx, av);
                    return Err(bv);
                }
                if !assert_deep_equal_inner(ctx, av, bv, strict, stack)? {
                    js_free_value(ctx, av);
                    js_free_value(ctx, bv);
                    return Ok(false);
                }
                js_free_value(ctx, av);
                js_free_value(ctx, bv);
            }
            Ok(true)
        })();
        if pushed {
            stack.pop();
        }
        return res;
    }
    if is_date_object(ctx, a) && is_date_object(ctx, b) {
        let ta = date_get_time(ctx, a)?;
        let tb = date_get_time(ctx, b)?;
        if ta.is_nan() && tb.is_nan() {
            return Ok(true);
        }
        return Ok(ta == tb);
    }
    if is_regexp_object(ctx, a) && is_regexp_object(ctx, b) {
        return regexp_source_flags_equal(ctx, a, b);
    }
    if qjs::JS_IsArray(ctx, a) != 0 || qjs::JS_IsArray(ctx, b) != 0 {
        return Ok(false);
    }
    let pair = match (js_object_ptr_key(a), js_object_ptr_key(b)) {
        (Some(pa), Some(pb)) => Some((pa, pb)),
        _ => None,
    };
    let mut pushed = false;
    if let Some(p) = pair {
        if stack.contains(&p) {
            return Ok(true);
        }
        stack.push(p);
        pushed = true;
    }
    let res: Result<bool, qjs::JSValue> = (|| {
        let keys_a = assert_own_enum_string_keys(ctx, a)?;
        let keys_b = assert_own_enum_string_keys(ctx, b)?;
        if keys_a.len() != keys_b.len() {
            return Ok(false);
        }
        for i in 0..keys_a.len() {
            if keys_a[i] != keys_b[i] {
                return Ok(false);
            }
            let ka = match CString::new(keys_a[i].as_str()) {
                Ok(s) => s,
                Err(_) => return Ok(false),
            };
            let va = qjs::JS_GetPropertyStr(ctx, a, ka.as_ptr());
            let vb = qjs::JS_GetPropertyStr(ctx, b, ka.as_ptr());
            if is_exception(va) {
                js_free_value(ctx, vb);
                return Err(va);
            }
            if is_exception(vb) {
                js_free_value(ctx, va);
                return Err(vb);
            }
            if !assert_deep_equal_inner(ctx, va, vb, strict, stack)? {
                js_free_value(ctx, va);
                js_free_value(ctx, vb);
                return Ok(false);
            }
            js_free_value(ctx, va);
            js_free_value(ctx, vb);
        }
        Ok(true)
    })();
    if pushed {
        stack.pop();
    }
    res
}

unsafe fn assert_throws_second_is_validator(ctx: *mut qjs::JSContext, v: qjs::JSValue) -> bool {
    if qjs::JS_IsFunction(ctx, v) != 0 {
        return true;
    }
    if is_regexp_object(ctx, v) {
        return true;
    }
    qjs::JS_IsConstructor(ctx, v) != 0
}

unsafe fn assert_throws_validate(
    ctx: *mut qjs::JSContext,
    exc: qjs::JSValue,
    expected: qjs::JSValue,
) -> Result<bool, qjs::JSValue> {
    if qjs::JS_IsFunction(ctx, expected) != 0 {
        let mut argv = [js_dup_value(exc)];
        let r = qjs::JS_Call(ctx, expected, js_undefined(), 1, argv.as_mut_ptr());
        js_free_value(ctx, argv[0]);
        if is_exception(r) {
            return Err(r);
        }
        let ok = qjs::JS_ToBool(ctx, r) != 0;
        js_free_value(ctx, r);
        return Ok(ok);
    }
    if is_regexp_object(ctx, expected) {
        let s = qjs::JS_ToString(ctx, exc);
        if is_exception(s) {
            return Err(s);
        }
        let test_k = CString::new("test").unwrap();
        let test_fn = qjs::JS_GetPropertyStr(ctx, expected, test_k.as_ptr());
        if qjs::JS_IsFunction(ctx, test_fn) == 0 {
            js_free_value(ctx, test_fn);
            js_free_value(ctx, s);
            return Ok(false);
        }
        let mut ta = [s];
        let hit = qjs::JS_Call(ctx, test_fn, expected, 1, ta.as_mut_ptr());
        js_free_value(ctx, test_fn);
        if is_exception(hit) {
            return Err(hit);
        }
        let ok = qjs::JS_ToBool(ctx, hit) != 0;
        js_free_value(ctx, hit);
        return Ok(ok);
    }
    if qjs::JS_IsConstructor(ctx, expected) != 0 {
        return Ok(qjs::JS_IsInstanceOf(ctx, exc, expected) != 0);
    }
    Ok(true)
}

unsafe extern "C" fn js_assert_call(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return assert_type_error(ctx, "assert() requires a value");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    if qjs::JS_ToBool(ctx, args[0]) == 0 {
        let msg = assert_optional_message(ctx, argc, argv, 1, "Assertion failed");
        return throw_assertion_error(ctx, &msg, None, None, None);
    }
    js_undefined()
}

unsafe extern "C" fn js_assert_ok(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    js_assert_call(ctx, this, argc, argv)
}

unsafe extern "C" fn js_assert_strict_equal(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return assert_type_error(
            ctx,
            "strictEqual(actual, expected[, message]) requires two values",
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    if qjs::JS_StrictEq(ctx, args[0], args[1]) == 0 {
        let msg = assert_optional_message(ctx, argc, argv, 2, "strictEqual mismatch");
        return throw_assertion_error(
            ctx,
            &msg,
            Some(js_dup_value(args[0])),
            Some(js_dup_value(args[1])),
            Some("==="),
        );
    }
    js_undefined()
}

unsafe extern "C" fn js_assert_equal(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return assert_type_error(
            ctx,
            "equal(actual, expected[, message]) requires two values",
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    match assert_loose_equal_bool(ctx, args[0], args[1]) {
        Ok(true) => js_undefined(),
        Ok(false) => {
            let msg = assert_optional_message(ctx, argc, argv, 2, "equal mismatch");
            throw_assertion_error(
                ctx,
                &msg,
                Some(js_dup_value(args[0])),
                Some(js_dup_value(args[1])),
                Some("=="),
            )
        }
        Err(e) => e,
    }
}

unsafe extern "C" fn js_assert_not_strict_equal(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return assert_type_error(
            ctx,
            "notStrictEqual(actual, expected[, message]) requires two values",
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    if qjs::JS_StrictEq(ctx, args[0], args[1]) != 0 {
        let msg = assert_optional_message(ctx, argc, argv, 2, "notStrictEqual mismatch");
        return throw_assertion_error(
            ctx,
            &msg,
            Some(js_dup_value(args[0])),
            Some(js_dup_value(args[1])),
            Some("!=="),
        );
    }
    js_undefined()
}

unsafe extern "C" fn js_assert_not_equal(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return assert_type_error(
            ctx,
            "notEqual(actual, expected[, message]) requires two values",
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    match assert_loose_equal_bool(ctx, args[0], args[1]) {
        Ok(true) => {
            let msg = assert_optional_message(ctx, argc, argv, 2, "notEqual mismatch");
            throw_assertion_error(
                ctx,
                &msg,
                Some(js_dup_value(args[0])),
                Some(js_dup_value(args[1])),
                Some("!="),
            )
        }
        Ok(false) => js_undefined(),
        Err(e) => e,
    }
}

unsafe extern "C" fn js_assert_deep_equal(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return assert_type_error(
            ctx,
            "deepEqual(actual, expected[, message]) requires two values",
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let mut stack: Vec<(usize, usize)> = Vec::new();
    match assert_deep_equal_inner(ctx, args[0], args[1], false, &mut stack) {
        Ok(true) => js_undefined(),
        Ok(false) => {
            let msg = assert_optional_message(ctx, argc, argv, 2, "deepEqual mismatch");
            throw_assertion_error(
                ctx,
                &msg,
                Some(js_dup_value(args[0])),
                Some(js_dup_value(args[1])),
                Some("deepEqual"),
            )
        }
        Err(e) => e,
    }
}

unsafe extern "C" fn js_assert_deep_strict_equal(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return assert_type_error(
            ctx,
            "deepStrictEqual(actual, expected[, message]) requires two values",
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let mut stack: Vec<(usize, usize)> = Vec::new();
    match assert_deep_equal_inner(ctx, args[0], args[1], true, &mut stack) {
        Ok(true) => js_undefined(),
        Ok(false) => {
            let msg = assert_optional_message(ctx, argc, argv, 2, "deepStrictEqual mismatch");
            throw_assertion_error(
                ctx,
                &msg,
                Some(js_dup_value(args[0])),
                Some(js_dup_value(args[1])),
                Some("deepStrictEqual"),
            )
        }
        Err(e) => e,
    }
}

unsafe extern "C" fn js_assert_throws(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return assert_type_error(ctx, "throws(block[, error][, message]) requires a function");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let block = args[0];
    if qjs::JS_IsFunction(ctx, block) == 0 {
        return assert_type_error(ctx, "throws() first argument must be a function");
    }
    let msg_idx = if argc >= 2 && assert_throws_second_is_validator(ctx, args[1]) {
        2usize
    } else {
        1usize
    };
    let r = qjs::JS_Call(ctx, block, js_undefined(), 0, ptr::null_mut());
    if is_exception(r) {
        let exc = qjs::JS_GetException(ctx);
        js_free_value(ctx, r);
        if argc >= 2 && assert_throws_second_is_validator(ctx, args[1]) {
            match assert_throws_validate(ctx, exc, args[1]) {
                Ok(true) => {
                    js_free_value(ctx, exc);
                    js_undefined()
                }
                Ok(false) => {
                    let msg = assert_optional_message(
                        ctx,
                        argc,
                        argv,
                        msg_idx,
                        "Threw non-matching exception",
                    );
                    js_free_value(ctx, exc);
                    throw_assertion_error(ctx, &msg, None, None, None)
                }
                Err(e) => {
                    js_free_value(ctx, exc);
                    e
                }
            }
        } else {
            js_free_value(ctx, exc);
            js_undefined()
        }
    } else {
        js_free_value(ctx, r);
        let msg = assert_optional_message(ctx, argc, argv, msg_idx, "Missing expected exception");
        throw_assertion_error(ctx, &msg, None, None, None)
    }
}

unsafe extern "C" fn js_assert_does_not_throw(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return assert_type_error(ctx, "doesNotThrow(block[, message]) requires a function");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let block = args[0];
    if qjs::JS_IsFunction(ctx, block) == 0 {
        return assert_type_error(ctx, "doesNotThrow() first argument must be a function");
    }
    let r = qjs::JS_Call(ctx, block, js_undefined(), 0, std::ptr::null_mut());
    if is_exception(r) {
        r
    } else {
        js_free_value(ctx, r);
        js_undefined()
    }
}

unsafe extern "C" fn js_assert_fail(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let msg = if argc >= 1 {
        let args = std::slice::from_raw_parts(argv, argc as usize);
        js_string_to_owned(ctx, args[0])
    } else {
        "Failed".to_string()
    };
    throw_assertion_error(ctx, &msg, None, None, None)
}

unsafe extern "C" fn js_assert_match(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return assert_type_error(ctx, "match(string, regexp[, message]) requires arguments");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let s = qjs::JS_ToString(ctx, args[0]);
    if is_exception(s) {
        return s;
    }
    let re = args[1];
    if !qjs::JS_IsObject(re) {
        js_free_value(ctx, s);
        return assert_type_error(ctx, "match requires a RegExp");
    }
    let test_k = CString::new("test").unwrap();
    let test_fn = qjs::JS_GetPropertyStr(ctx, re, test_k.as_ptr());
    if qjs::JS_IsFunction(ctx, test_fn) == 0 {
        js_free_value(ctx, test_fn);
        js_free_value(ctx, s);
        return assert_type_error(ctx, "match requires RegExp with test");
    }
    let mut argv_call = [s];
    let hit = qjs::JS_Call(ctx, test_fn, re, 1, argv_call.as_mut_ptr());
    js_free_value(ctx, test_fn);
    if is_exception(hit) {
        js_free_value(ctx, argv_call[0]);
        return hit;
    }
    let ok = qjs::JS_ToBool(ctx, hit) != 0;
    js_free_value(ctx, hit);
    js_free_value(ctx, argv_call[0]);
    if ok {
        js_undefined()
    } else {
        let msg = assert_optional_message(ctx, argc, argv, 2, "match failed");
        throw_assertion_error(
            ctx,
            &msg,
            Some(js_dup_value(args[0])),
            Some(js_dup_value(args[1])),
            Some("match"),
        )
    }
}

unsafe extern "C" fn js_assert_rejects(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return assert_type_error(ctx, "rejects requires a function or Promise");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let global = qjs::JS_GetGlobalObject(ctx);
    let k = CString::new("__kawkabAssertRejectsArg0").unwrap();
    qjs::JS_SetPropertyStr(ctx, global, k.as_ptr(), js_dup_value(args[0]));
    js_free_value(ctx, global);
    let src = r#"
var arg0 = globalThis.__kawkabAssertRejectsArg0;
delete globalThis.__kawkabAssertRejectsArg0;
var p = (typeof arg0 === 'function') ? Promise.resolve(arg0()) : Promise.resolve(arg0);
return p.then(
  function () {
    var e = new Error('Missing expected rejection');
    e.code = 'ERR_ASSERTION';
    e.name = 'AssertionError';
    throw e;
  },
  function () { }
);
"#;
    eval_js_module(ctx, "assert-rejects", src)
}

unsafe fn install_builtin_assert(ctx: *mut qjs::JSContext) -> qjs::JSValue {
    let main = qjs::JS_NewCFunction2(
        ctx,
        Some(js_assert_call),
        CString::new("assert").unwrap().as_ptr(),
        2,
        qjs::JSCFunctionEnum_JS_CFUNC_generic,
        0,
    );
    let _ = install_obj_fn(ctx, main, "ok", Some(js_assert_ok), 2);
    let _ = install_obj_fn(ctx, main, "equal", Some(js_assert_equal), 3);
    let _ = install_obj_fn(ctx, main, "strictEqual", Some(js_assert_strict_equal), 3);
    let _ = install_obj_fn(ctx, main, "notEqual", Some(js_assert_not_equal), 3);
    let _ = install_obj_fn(
        ctx,
        main,
        "notStrictEqual",
        Some(js_assert_not_strict_equal),
        3,
    );
    let _ = install_obj_fn(ctx, main, "deepEqual", Some(js_assert_deep_equal), 3);
    let _ = install_obj_fn(
        ctx,
        main,
        "deepStrictEqual",
        Some(js_assert_deep_strict_equal),
        3,
    );
    let _ = install_obj_fn(ctx, main, "throws", Some(js_assert_throws), 3);
    let _ = install_obj_fn(ctx, main, "doesNotThrow", Some(js_assert_does_not_throw), 2);
    let _ = install_obj_fn(ctx, main, "fail", Some(js_assert_fail), 1);
    let _ = install_obj_fn(ctx, main, "match", Some(js_assert_match), 3);
    let _ = install_obj_fn(ctx, main, "rejects", Some(js_assert_rejects), 3);
    let k_prim = CString::new("__kawkabPrimedAssertionError").unwrap();
    let global = qjs::JS_GetGlobalObject(ctx);
    let ctor_val = qjs::JS_GetPropertyStr(ctx, global, k_prim.as_ptr());
    js_free_value(ctx, global);
    let ctor_val = if qjs::JS_IsUndefined(ctor_val) {
        js_free_value(ctx, ctor_val);
        eval_js_module(ctx, "assert-assertionerror", ASSERTION_ERROR_CTOR_SRC)
    } else {
        ctor_val
    };
    if is_exception(ctor_val) {
        js_free_value(ctx, main);
        return ctor_val;
    }
    let k_ae = CString::new("AssertionError").unwrap();
    qjs::JS_SetPropertyStr(ctx, main, k_ae.as_ptr(), js_dup_value(ctor_val));
    js_free_value(ctx, ctor_val);
    main
}

unsafe fn punycode_baseline_input(
    ctx: *mut qjs::JSContext,
    argc: c_int,
    argv: *mut qjs::JSValue,
    api: &str,
    forbid_xn: bool,
) -> Result<String, qjs::JSValue> {
    if argc < 1 {
        return Err(qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("punycode.{api} requires a string"))
                .unwrap_or_default()
                .as_ptr(),
        ));
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let s = js_string_to_owned(ctx, args[0]);
    if !s.is_ascii() {
        return Err(qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!(
                "punycode.{api}: non-ASCII not supported in kawkab baseline"
            ))
            .unwrap_or_default()
            .as_ptr(),
        ));
    }
    if forbid_xn && s.contains("xn--") {
        return Err(qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!(
                "punycode.{api}: ACE (xn--) handling not supported in kawkab baseline"
            ))
            .unwrap_or_default()
            .as_ptr(),
        ));
    }
    Ok(s)
}

unsafe extern "C" fn js_punycode_decode(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    match punycode_baseline_input(ctx, argc, argv, "decode", true) {
        Ok(s) => qjs_compat::new_string_from_str(ctx, &s),
        Err(e) => e,
    }
}

unsafe extern "C" fn js_punycode_encode(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    match punycode_baseline_input(ctx, argc, argv, "encode", false) {
        Ok(s) => qjs_compat::new_string_from_str(ctx, &s),
        Err(e) => e,
    }
}

unsafe extern "C" fn js_punycode_to_ascii(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("punycode.toASCII requires a string")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let s = js_string_to_owned(ctx, args[0]);
    match idna::domain_to_ascii(&s) {
        Ok(out) => qjs_compat::new_string_from_str(ctx, &out),
        Err(e) => crate::ffi::throw_type_error(ctx, &format!("punycode.toASCII: {e}")),
    }
}

unsafe extern "C" fn js_punycode_to_unicode(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("punycode.toUnicode requires a string")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let s = js_string_to_owned(ctx, args[0]);
    let (out, res) = idna::domain_to_unicode(&s);
    if let Err(e) = res {
        return crate::ffi::throw_type_error(ctx, &format!("punycode.toUnicode: {e:?}"));
    }
    qjs_compat::new_string_from_str(ctx, &out)
}

unsafe extern "C" fn js_process_chdir(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    qjs::JS_ThrowTypeError(
        ctx,
        CString::new("process.chdir is not supported yet")
            .unwrap()
            .as_ptr(),
    )
}

/// QuickJS job callback: `argv[0]` is callee, rest forwarded to `JS_Call`.
unsafe extern "C" fn js_enqueue_js_call_job(
    ctx: *mut qjs::JSContext,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return js_undefined();
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let cb = args[0];
    let rest = argc - 1;
    if rest <= 0 {
        qjs::JS_Call(ctx, cb, js_undefined(), 0, ptr::null_mut())
    } else {
        qjs::JS_Call(
            ctx,
            cb,
            js_undefined(),
            rest,
            args.as_ptr().add(1) as *mut qjs::JSValue,
        )
    }
}

unsafe extern "C" fn js_process_next_tick(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("process.nextTick(callback, ...args) requires callback")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let cb = args[0];
    if qjs::JS_IsFunction(ctx, cb) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("process.nextTick callback must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    if qjs::JS_EnqueueJob(ctx, Some(js_enqueue_js_call_job), argc, argv) != 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("process.nextTick: failed to enqueue job")
                .unwrap()
                .as_ptr(),
        );
    }
    js_undefined()
}

/// `timers/promises`: deferred timer path like `setTimeout`, but resolves a Promise.
unsafe fn timers_promises_schedule_resolve(
    ctx: *mut qjs::JSContext,
    ms: u64,
    value: qjs::JSValue,
) -> qjs::JSValue {
    let mut cap = [js_undefined(), js_undefined()];
    let promise = qjs::JS_NewPromiseCapability(ctx, cap.as_mut_ptr());
    if is_exception(promise) {
        js_free_value(ctx, value);
        return promise;
    }
    let resolve = js_dup_value(cap[0]);
    js_free_value(ctx, cap[0]);
    js_free_value(ctx, cap[1]);

    let ms = ms.min(86_400_000);

    if deferred_host_tasks_ready() {
        let sender = TASK_SENDER_SLOT.with(|s| s.borrow().clone()).unwrap();
        let rt = Handle::try_current().unwrap();
        let id = NEXT_TIMER_ID.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        TIMER_CANCEL_BY_ID.with(|m| {
            m.borrow_mut().insert(id, cancelled.clone());
        });
        let pending = PendingTimer {
            callback: resolve,
            this_val: js_undefined(),
            args: vec![value],
            cancelled: cancelled.clone(),
            repeat_ms: None,
        };
        TIMER_REGISTRY.with(|r| r.borrow_mut().insert(id, pending));
        PENDING_ASYNC_TIMERS.fetch_add(1, Ordering::Relaxed);

        rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            if cancelled.load(Ordering::Acquire) {
                PENDING_ASYNC_TIMERS.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            sender.send_timer_callback(id);
        });
    } else {
        std::thread::sleep(Duration::from_millis(ms));
        let mut args = vec![value];
        let ret = qjs::JS_Call(ctx, resolve, js_undefined(), 1, args.as_mut_ptr());
        js_free_value(ctx, resolve);
        let v = args.pop().unwrap();
        js_free_value(ctx, v);
        if is_exception(ret) {
            js_free_value(ctx, promise);
            return ret;
        }
        js_free_value(ctx, ret);
    }

    promise
}

unsafe extern "C" fn js_timers_promises_set_timeout(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("timers/promises.setTimeout(delay[, value]) requires delay")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let mut delay_i: i64 = 0;
    if qjs::JS_ToInt64(ctx, &mut delay_i, args[0]) != 0 {
        return qjs::JS_GetException(ctx);
    }
    let delay_ms = delay_i.max(0).min(86_400_000_i64) as u64;
    let value = if argc > 1 {
        js_dup_value(args[1])
    } else {
        js_undefined()
    };
    timers_promises_schedule_resolve(ctx, delay_ms, value)
}

unsafe extern "C" fn js_timers_promises_set_immediate(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let value = if argc >= 1 {
        let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
        js_dup_value(args[0])
    } else {
        js_undefined()
    };
    timers_promises_schedule_resolve(ctx, 0, value)
}

unsafe extern "C" fn js_set_timeout(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    schedule_timer_inner(ctx, this, argc, argv, false)
}

unsafe extern "C" fn js_set_interval(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    schedule_timer_inner(ctx, this, argc, argv, true)
}

unsafe fn schedule_timer_inner(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
    repeating: bool,
) -> qjs::JSValue {
    let req_cb = if repeating {
        "setInterval(callback, ms) requires callback"
    } else {
        "setTimeout(callback, ms) requires callback"
    };
    if argc < 1 {
        return qjs::JS_ThrowTypeError(ctx, CString::new(req_cb).unwrap().as_ptr());
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let cb = args[0];
    if qjs::JS_IsFunction(ctx, cb) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("timer callback must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    let ms = if argc > 1 {
        js_string_to_owned(ctx, args[1]).parse::<u64>().unwrap_or(0)
    } else {
        0
    };
    let ms = ms.min(86_400_000);
    let repeat_ms = if repeating { Some(ms) } else { None };

    if deferred_host_tasks_ready() {
        let sender = TASK_SENDER_SLOT.with(|s| s.borrow().clone()).unwrap();
        let rt = Handle::try_current().unwrap();
        let id = NEXT_TIMER_ID.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        TIMER_CANCEL_BY_ID.with(|m| {
            m.borrow_mut().insert(id, cancelled.clone());
        });
        let callback = js_dup_value(cb);
        let this_val = js_undefined();
        let call_args: Vec<qjs::JSValue> = if argc > 2 {
            args[2..].iter().copied().map(|v| js_dup_value(v)).collect()
        } else {
            Vec::new()
        };
        let pending = PendingTimer {
            callback,
            this_val,
            args: call_args,
            cancelled: cancelled.clone(),
            repeat_ms,
        };
        TIMER_REGISTRY.with(|r| r.borrow_mut().insert(id, pending));
        PENDING_ASYNC_TIMERS.fetch_add(1, Ordering::Relaxed);

        rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            if cancelled.load(Ordering::Acquire) {
                PENDING_ASYNC_TIMERS.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            sender.send_timer_callback(id);
        });
        return qjs_compat::new_int(ctx, id as i64);
    }

    std::thread::sleep(Duration::from_millis(ms));
    let mut call_args: Vec<qjs::JSValue> = if argc > 2 {
        args[2..].to_vec()
    } else {
        Vec::new()
    };
    let ret = qjs::JS_Call(
        ctx,
        cb,
        js_undefined(),
        call_args.len() as c_int,
        if call_args.is_empty() {
            std::ptr::null_mut()
        } else {
            call_args.as_mut_ptr()
        },
    );
    if is_exception(ret) {
        return ret;
    }
    js_free_value(ctx, ret);
    qjs_compat::new_int(ctx, 1)
}

unsafe extern "C" fn js_clear_timeout(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return js_undefined();
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let mut id: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut id as *mut i32, args[0]);
    if id > 0 {
        cancel_deferred_timer(ctx, id as u64);
    }
    js_undefined()
}

unsafe fn new_js_error(ctx: *mut qjs::JSContext, msg: &str) -> qjs::JSValue {
    let err = qjs::JS_NewError(ctx);
    let mkey = CString::new("message").unwrap();
    let s = qjs_compat::new_string_from_str(ctx, msg);
    qjs::JS_SetPropertyStr(ctx, err, mkey.as_ptr(), s);
    err
}

pub(crate) unsafe fn host_register_promise_callback(id: u64, cb: qjs::JSValue) {
    let cb = js_dup_value(cb);
    HOST_PENDING_PROMISES.with(|m| {
        m.borrow_mut()
            .insert(id, HostPendingPromise::Callback { cb });
    });
}

unsafe fn host_register_capability(id: u64, resolve: qjs::JSValue, reject: qjs::JSValue) {
    let resolve = js_dup_value(resolve);
    let reject = js_dup_value(reject);
    HOST_PENDING_PROMISES.with(|m| {
        m.borrow_mut()
            .insert(id, HostPendingPromise::Capability { resolve, reject });
    });
}

/// Deliver bytes to pending promise/callback (`fs.promises.readFile` or `(err, buf)`).
pub unsafe fn host_resolve_promise(
    ctx: *mut qjs::JSContext,
    promise_id: u64,
    payload: Arc<[u8]>,
) -> Result<(), String> {
    let entry = HOST_PENDING_PROMISES.with(|m| m.borrow_mut().remove(&promise_id));
    let Some(entry) = entry else {
        return Ok(());
    };
    match entry {
        HostPendingPromise::Callback { cb } => {
            let null_v = js_null();
            let buf = crate::ffi::arraybuffer_from_arc(ctx, payload);
            let mut args = [null_v, buf];
            let r = qjs::JS_Call(ctx, cb, js_undefined(), 2, args.as_mut_ptr());
            let failed = is_exception(r);
            js_free_value(ctx, r);
            js_free_value(ctx, cb);
            js_free_value(ctx, buf);
            if failed {
                Err("promise callback failed".to_string())
            } else {
                Ok(())
            }
        }
        HostPendingPromise::Capability { resolve, reject } => {
            let buf = buffer::buffer_uint8_from_arc(ctx, payload);
            if is_exception(buf) {
                js_free_value(ctx, resolve);
                js_free_value(ctx, reject);
                PENDING_HOST_ASYNC.fetch_sub(1, Ordering::Relaxed);
                return Err("failed to allocate Buffer for fs.promises".to_string());
            }
            let mut args = [buf];
            let r = qjs::JS_Call(ctx, resolve, js_undefined(), 1, args.as_mut_ptr());
            let failed = is_exception(r);
            js_free_value(ctx, r);
            js_free_value(ctx, resolve);
            js_free_value(ctx, reject);
            js_free_value(ctx, buf);
            PENDING_HOST_ASYNC.fetch_sub(1, Ordering::Relaxed);
            if failed {
                Err("Promise resolve threw".to_string())
            } else {
                Ok(())
            }
        }
    }
}

/// Fulfill capability promise with JSON-parsed value (e.g. `fs.promises.stat`).
pub unsafe fn host_resolve_promise_json(
    ctx: *mut qjs::JSContext,
    promise_id: u64,
    json: &str,
) -> Result<(), String> {
    let entry = HOST_PENDING_PROMISES.with(|m| m.borrow_mut().remove(&promise_id));
    let Some(entry) = entry else {
        return Ok(());
    };
    let label = CString::new("kawkab:fs-promises-json").unwrap_or_default();
    let val = qjs::JS_ParseJSON(
        ctx,
        json.as_ptr() as *const std::os::raw::c_char,
        json.len(),
        label.as_ptr(),
    );
    if is_exception(val) {
        js_free_value(ctx, val);
        match entry {
            HostPendingPromise::Callback { cb } => {
                js_free_value(ctx, cb);
            }
            HostPendingPromise::Capability { resolve, reject } => {
                js_free_value(ctx, resolve);
                js_free_value(ctx, reject);
                PENDING_HOST_ASYNC.fetch_sub(1, Ordering::Relaxed);
            }
        }
        return Err("JS_ParseJSON failed for fs.promises payload".to_string());
    }
    match entry {
        HostPendingPromise::Callback { cb } => {
            js_free_value(ctx, val);
            js_free_value(ctx, cb);
            Ok(())
        }
        HostPendingPromise::Capability { resolve, reject } => {
            let mut args = [val];
            let r = qjs::JS_Call(ctx, resolve, js_undefined(), 1, args.as_mut_ptr());
            let failed = is_exception(r);
            js_free_value(ctx, r);
            js_free_value(ctx, resolve);
            js_free_value(ctx, reject);
            js_free_value(ctx, val);
            PENDING_HOST_ASYNC.fetch_sub(1, Ordering::Relaxed);
            if failed {
                Err("Promise resolve threw".to_string())
            } else {
                Ok(())
            }
        }
    }
}

/// Fulfill a Capability promise with `undefined` (e.g. `fs.promises.writeFile`).
pub unsafe fn host_resolve_capability_void(
    ctx: *mut qjs::JSContext,
    promise_id: u64,
) -> Result<(), String> {
    let entry = HOST_PENDING_PROMISES.with(|m| m.borrow_mut().remove(&promise_id));
    match entry {
        None => Ok(()),
        Some(HostPendingPromise::Callback { cb }) => {
            js_free_value(ctx, cb);
            Ok(())
        }
        Some(HostPendingPromise::Capability { resolve, reject }) => {
            let r = qjs::JS_Call(ctx, resolve, js_undefined(), 0, ptr::null_mut());
            let failed = is_exception(r);
            js_free_value(ctx, r);
            js_free_value(ctx, resolve);
            js_free_value(ctx, reject);
            PENDING_HOST_ASYNC.fetch_sub(1, Ordering::Relaxed);
            if failed {
                Err("Promise resolve threw".to_string())
            } else {
                Ok(())
            }
        }
    }
}

pub unsafe fn host_reject_promise(
    ctx: *mut qjs::JSContext,
    promise_id: u64,
    reason: &str,
) -> Result<(), String> {
    let entry = HOST_PENDING_PROMISES.with(|m| m.borrow_mut().remove(&promise_id));
    let Some(entry) = entry else {
        return Ok(());
    };
    let err = new_js_error(ctx, reason);
    if is_exception(err) {
        return Err("failed to construct Error object".to_string());
    }
    match entry {
        HostPendingPromise::Callback { cb } => {
            let null_data = js_null();
            let mut args = [err, null_data];
            let r = qjs::JS_Call(ctx, cb, js_undefined(), 2, args.as_mut_ptr());
            let failed = is_exception(r);
            js_free_value(ctx, r);
            js_free_value(ctx, cb);
            js_free_value(ctx, err);
            if failed {
                Err("promise callback failed".to_string())
            } else {
                Ok(())
            }
        }
        HostPendingPromise::Capability { resolve, reject } => {
            let mut args = [err];
            let r = qjs::JS_Call(ctx, reject, js_undefined(), 1, args.as_mut_ptr());
            let failed = is_exception(r);
            js_free_value(ctx, r);
            js_free_value(ctx, resolve);
            js_free_value(ctx, reject);
            js_free_value(ctx, err);
            PENDING_HOST_ASYNC.fetch_sub(1, Ordering::Relaxed);
            if failed {
                Err("Promise reject threw".to_string())
            } else {
                Ok(())
            }
        }
    }
}

unsafe extern "C" fn js_kawkab_register_promise_callback(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("__kawkab_register_promise_callback(id, fn) requires arguments")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let mut id_f: f64 = 0.0;
    qjs::JS_ToFloat64(ctx, &mut id_f as *mut f64, args[0]);
    let id = id_f as u64;
    if qjs::JS_IsFunction(ctx, args[1]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("second argument must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    host_register_promise_callback(id, args[1]);
    js_undefined()
}

fn json_quote_for_fs(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if c.is_control() => {
                use std::fmt::Write;
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn fs_metadata_to_json(meta: &std::fs::Metadata) -> String {
    let ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    format!(
        r#"{{"size":{},"mtimeMs":{},"isFile":{},"isDirectory":{}}}"#,
        meta.len(),
        ms,
        meta.is_file(),
        meta.is_dir()
    )
}

fn fs_readdir_to_json(path: &str) -> Result<String, std::io::Error> {
    let rd = std::fs::read_dir(path)?;
    let mut names = Vec::new();
    for e in rd {
        names.push(e?.file_name().to_string_lossy().into_owned());
    }
    let mut out = String::from("[");
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_quote_for_fs(n));
    }
    out.push(']');
    Ok(out)
}

unsafe fn fs_promises_begin_capability(
    ctx: *mut qjs::JSContext,
) -> Result<(u64, qjs::JSValue), qjs::JSValue> {
    let mut cap = [js_undefined(), js_undefined()];
    let promise = qjs::JS_NewPromiseCapability(ctx, cap.as_mut_ptr());
    if is_exception(promise) {
        return Err(promise);
    }
    let id = NEXT_PROMISE_ID.fetch_add(1, Ordering::SeqCst);
    host_register_capability(id, cap[0], cap[1]);
    js_free_value(ctx, cap[0]);
    js_free_value(ctx, cap[1]);
    PENDING_HOST_ASYNC.fetch_add(1, Ordering::Relaxed);
    Ok((id, promise))
}

unsafe extern "C" fn js_fs_promises_read_file(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "fs.promises.readFile(path) requires path");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    let (id, promise) = match fs_promises_begin_capability(ctx) {
        Ok(x) => x,
        Err(p) => return p,
    };

    let sender_opt = TASK_SENDER_SLOT.with(|s| s.borrow().clone());
    if let Some(sender) = sender_opt {
        let path2 = path.clone();
        tokio::spawn(async move {
            match tokio::fs::read(&path2).await {
                Ok(bytes) => sender.resolve_promise(id, Arc::from(bytes.into_boxed_slice())),
                Err(e) => sender.reject_promise(id, e.to_string()),
            }
        });
        return promise;
    }

    match std::fs::read(&path) {
        Ok(bytes) => {
            let _ = host_resolve_promise(ctx, id, Arc::from(bytes.into_boxed_slice()));
        }
        Err(e) => {
            let _ = host_reject_promise(ctx, id, &e.to_string());
        }
    }
    promise
}

unsafe extern "C" fn js_fs_promises_write_file(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return crate::ffi::throw_type_error(
            ctx,
            "fs.promises.writeFile(path, data) requires path and data",
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    let data = buffer::buffer_bytes_from_value(ctx, args[1]);
    let (id, promise) = match fs_promises_begin_capability(ctx) {
        Ok(x) => x,
        Err(p) => return p,
    };

    let sender_opt = TASK_SENDER_SLOT.with(|s| s.borrow().clone());
    if let Some(sender) = sender_opt {
        let path2 = path.clone();
        tokio::spawn(async move {
            match tokio::fs::write(&path2, &data).await {
                Ok(()) => sender.resolve_promise_void(id),
                Err(e) => sender.reject_promise(id, e.to_string()),
            }
        });
        return promise;
    }

    match std::fs::write(&path, &data) {
        Ok(()) => {
            let _ = host_resolve_capability_void(ctx, id);
        }
        Err(e) => {
            let _ = host_reject_promise(ctx, id, &e.to_string());
        }
    }
    promise
}

unsafe extern "C" fn js_fs_promises_stat(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "fs.promises.stat(path) requires path");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    let (id, promise) = match fs_promises_begin_capability(ctx) {
        Ok(x) => x,
        Err(p) => return p,
    };
    let sender_opt = TASK_SENDER_SLOT.with(|s| s.borrow().clone());
    if let Some(sender) = sender_opt {
        let path2 = path.clone();
        tokio::spawn(async move {
            match tokio::fs::metadata(&path2).await {
                Ok(m) => {
                    let j = fs_metadata_to_json(&m);
                    sender.resolve_promise_json(id, j);
                }
                Err(e) => sender.reject_promise(id, e.to_string()),
            }
        });
        return promise;
    }
    match std::fs::metadata(&path) {
        Ok(m) => {
            let j = fs_metadata_to_json(&m);
            let _ = host_resolve_promise_json(ctx, id, &j);
        }
        Err(e) => {
            let _ = host_reject_promise(ctx, id, &e.to_string());
        }
    }
    promise
}

unsafe extern "C" fn js_fs_promises_readdir(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "fs.promises.readdir(path) requires path");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    let (id, promise) = match fs_promises_begin_capability(ctx) {
        Ok(x) => x,
        Err(p) => return p,
    };
    let sender_opt = TASK_SENDER_SLOT.with(|s| s.borrow().clone());
    if let Some(sender) = sender_opt {
        let path2 = path.clone();
        tokio::spawn(async move {
            let mut names = Vec::new();
            match tokio::fs::read_dir(&path2).await {
                Ok(mut rd) => loop {
                    match rd.next_entry().await {
                        Ok(Some(e)) => {
                            names.push(e.file_name().to_string_lossy().into_owned());
                        }
                        Ok(None) => {
                            let mut json = String::from("[");
                            for (i, n) in names.iter().enumerate() {
                                if i > 0 {
                                    json.push(',');
                                }
                                json.push_str(&json_quote_for_fs(n));
                            }
                            json.push(']');
                            sender.resolve_promise_json(id, json);
                            break;
                        }
                        Err(e) => {
                            sender.reject_promise(id, e.to_string());
                            break;
                        }
                    }
                },
                Err(e) => sender.reject_promise(id, e.to_string()),
            }
        });
        return promise;
    }
    match fs_readdir_to_json(&path) {
        Ok(j) => {
            let _ = host_resolve_promise_json(ctx, id, &j);
        }
        Err(e) => {
            let _ = host_reject_promise(ctx, id, &e.to_string());
        }
    }
    promise
}

unsafe extern "C" fn js_fs_promises_mkdir(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "fs.promises.mkdir(path) requires path");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    let mut recursive = false;
    if argc >= 2 {
        let opts = args[1];
        if qjs::JS_IsObject(opts) {
            let r = qjs::JS_GetPropertyStr(ctx, opts, CString::new("recursive").unwrap().as_ptr());
            if qjs::JS_ToBool(ctx, r) != 0 {
                recursive = true;
            }
            js_free_value(ctx, r);
        }
    }
    let (id, promise) = match fs_promises_begin_capability(ctx) {
        Ok(x) => x,
        Err(p) => return p,
    };
    let sender_opt = TASK_SENDER_SLOT.with(|s| s.borrow().clone());
    if let Some(sender) = sender_opt {
        let path2 = path.clone();
        tokio::spawn(async move {
            let r = if recursive {
                tokio::fs::create_dir_all(&path2).await
            } else {
                tokio::fs::create_dir(&path2).await
            };
            match r {
                Ok(()) => sender.resolve_promise_void(id),
                Err(e) => sender.reject_promise(id, e.to_string()),
            }
        });
        return promise;
    }
    let r = if recursive {
        std::fs::create_dir_all(&path)
    } else {
        std::fs::create_dir(&path)
    };
    match r {
        Ok(()) => {
            let _ = host_resolve_capability_void(ctx, id);
        }
        Err(e) => {
            let _ = host_reject_promise(ctx, id, &e.to_string());
        }
    }
    promise
}

unsafe extern "C" fn js_fs_promises_unlink(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "fs.promises.unlink(path) requires path");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    let (id, promise) = match fs_promises_begin_capability(ctx) {
        Ok(x) => x,
        Err(p) => return p,
    };
    let sender_opt = TASK_SENDER_SLOT.with(|s| s.borrow().clone());
    if let Some(sender) = sender_opt {
        let path2 = path.clone();
        tokio::spawn(async move {
            match tokio::fs::remove_file(&path2).await {
                Ok(()) => sender.resolve_promise_void(id),
                Err(e) => sender.reject_promise(id, e.to_string()),
            }
        });
        return promise;
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {
            let _ = host_resolve_capability_void(ctx, id);
        }
        Err(e) => {
            let _ = host_reject_promise(ctx, id, &e.to_string());
        }
    }
    promise
}

unsafe extern "C" fn js_zlib_gzip_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "gzipSync(data) requires data");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = buffer::buffer_bytes_from_value(ctx, args[0]);
    let mut enc =
        flate2::read::GzEncoder::new(std::io::Cursor::new(input), flate2::Compression::default());
    let mut out = Vec::new();
    if let Err(e) = enc.read_to_end(&mut out) {
        return crate::ffi::throw_type_error(ctx, &format!("gzipSync: {e}"));
    }
    unsafe { crate::ffi::arraybuffer_from_arc(ctx, Arc::from(out.into_boxed_slice())) }
}

unsafe extern "C" fn js_zlib_gunzip_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "gunzipSync(data) requires data");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = buffer::buffer_bytes_from_value(ctx, args[0]);
    let mut dec = flate2::read::GzDecoder::new(std::io::Cursor::new(input));
    let mut out = Vec::new();
    if let Err(e) = dec.read_to_end(&mut out) {
        return crate::ffi::throw_type_error(ctx, &format!("gunzipSync: {e}"));
    }
    unsafe { crate::ffi::arraybuffer_from_arc(ctx, Arc::from(out.into_boxed_slice())) }
}

unsafe extern "C" fn js_zlib_deflate_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "deflateSync(data) requires data");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = buffer::buffer_bytes_from_value(ctx, args[0]);
    let mut enc = flate2::read::DeflateEncoder::new(
        std::io::Cursor::new(input),
        flate2::Compression::default(),
    );
    let mut out = Vec::new();
    if let Err(e) = enc.read_to_end(&mut out) {
        return crate::ffi::throw_type_error(ctx, &format!("deflateSync: {e}"));
    }
    unsafe { crate::ffi::arraybuffer_from_arc(ctx, Arc::from(out.into_boxed_slice())) }
}

unsafe extern "C" fn js_zlib_inflate_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "inflateSync(data) requires data");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = buffer::buffer_bytes_from_value(ctx, args[0]);
    let mut dec = flate2::read::DeflateDecoder::new(std::io::Cursor::new(input));
    let mut out = Vec::new();
    if let Err(e) = dec.read_to_end(&mut out) {
        return crate::ffi::throw_type_error(ctx, &format!("inflateSync: {e}"));
    }
    unsafe { crate::ffi::arraybuffer_from_arc(ctx, Arc::from(out.into_boxed_slice())) }
}

unsafe fn build_http_client_response_object(
    ctx: *mut qjs::JSContext,
    status: u16,
    status_text: &str,
    headers: &std::collections::HashMap<String, String>,
    body: Vec<u8>,
) -> qjs::JSValue {
    let res = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        res,
        CString::new("statusCode").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, status as i64),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        res,
        CString::new("statusMessage").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, status_text),
    );
    let hdr = qjs::JS_NewObject(ctx);
    for (k, v) in headers {
        let ck = CString::new(k.as_str()).unwrap_or_default();
        qjs::JS_SetPropertyStr(
            ctx,
            hdr,
            ck.as_ptr(),
            qjs_compat::new_string_from_str(ctx, v),
        );
    }
    qjs::JS_SetPropertyStr(ctx, res, CString::new("headers").unwrap().as_ptr(), hdr);
    let body_val =
        unsafe { crate::ffi::arraybuffer_from_arc(ctx, Arc::from(body.into_boxed_slice())) };
    qjs::JS_SetPropertyStr(ctx, res, CString::new("body").unwrap().as_ptr(), body_val);
    res
}

unsafe extern "C" fn js_http_client_get(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("http.get(url, callback) requires url and callback")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let url = js_string_to_owned(ctx, args[0]);
    if qjs::JS_IsFunction(ctx, args[1]) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("http.get: callback must be a function")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let cb = args[1];
    let client = http_blocking_client();
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => {
            return crate::ffi::throw_type_error(ctx, &format!("http.get: request failed: {e}"));
        }
    };
    let status = resp.status().as_u16();
    let status_text = resp.status().canonical_reason().unwrap_or("").to_string();
    let mut hdr_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (name, value) in resp.headers().iter() {
        let k = name.as_str().to_ascii_lowercase();
        let v = value.to_str().unwrap_or("");
        hdr_map
            .entry(k)
            .and_modify(|e| {
                e.push_str(", ");
                e.push_str(v);
            })
            .or_insert_with(|| v.to_string());
    }
    let body = match resp.bytes() {
        Ok(b) => b.to_vec(),
        Err(e) => {
            return crate::ffi::throw_type_error(ctx, &format!("http.get: read body: {e}"));
        }
    };
    let res_obj = build_http_client_response_object(ctx, status, &status_text, &hdr_map, body);
    let mut arg = [res_obj];
    let out = qjs::JS_Call(ctx, cb, js_undefined(), 1, arg.as_mut_ptr());
    js_free_value(ctx, out);
    js_undefined()
}

unsafe extern "C" fn js_http_client_request(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    js_http_client_get(ctx, this, argc, argv)
}

unsafe extern "C" fn js_kawkab_fast_sum(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("kawkab.fastSum(input) requires input")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = args[0];

    if input.tag == qjs::JS_TAG_OBJECT as i64 {
        if let Some(bytes) = crate::ffi::arraybuffer_bytes(ctx, input) {
            let sum: u64 = bytes.iter().map(|b| *b as u64).sum();
            return qjs::JS_NewFloat64(ctx, sum as f64);
        }

        let mut off: usize = 0;
        let mut len: usize = 0;
        let mut el: usize = 0;
        let ab = qjs::JS_GetTypedArrayBuffer(ctx, input, &mut off, &mut len, &mut el);
        if qjs::JS_IsObject(ab) {
            let mut ab_size: usize = 0;
            let ptr = qjs::JS_GetArrayBuffer(ctx, &mut ab_size, ab);
            let sum = if !ptr.is_null() && off + len <= ab_size {
                let s = std::slice::from_raw_parts(ptr.add(off), len);
                s.iter().map(|b| *b as u64).sum::<u64>()
            } else {
                0
            };
            js_free_value(ctx, ab);
            return qjs::JS_NewFloat64(ctx, sum as f64);
        }
        js_free_value(ctx, ab);
    }

    if qjs::JS_IsArray(ctx, input) != 0 {
        let len_v = qjs::JS_GetPropertyStr(ctx, input, CString::new("length").unwrap().as_ptr());
        let mut n: i32 = 0;
        let _ = qjs::JS_ToInt32(ctx, &mut n as *mut i32, len_v);
        js_free_value(ctx, len_v);
        let mut sum = 0.0f64;
        for i in 0..(n.max(0) as u32) {
            let v = qjs::JS_GetPropertyUint32(ctx, input, i);
            let mut x = 0.0f64;
            let _ = qjs::JS_ToFloat64(ctx, &mut x as *mut f64, v);
            js_free_value(ctx, v);
            sum += x;
        }
        return qjs::JS_NewFloat64(ctx, sum);
    }

    let mut x = 0.0f64;
    let _ = qjs::JS_ToFloat64(ctx, &mut x as *mut f64, input);
    qjs::JS_NewFloat64(ctx, x)
}

unsafe extern "C" fn js_kawkab_fast_sum_u32(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("kawkab.fastSumU32(input) requires Uint32Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = args[0];

    let mut off: usize = 0;
    let mut len: usize = 0;
    let mut el: usize = 0;
    let ab = qjs::JS_GetTypedArrayBuffer(ctx, input, &mut off, &mut len, &mut el);
    if !qjs::JS_IsObject(ab) {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastSumU32 expects Uint32Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    if el != 4 || len % 4 != 0 || off % 4 != 0 {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastSumU32 expects Uint32Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let mut ab_size: usize = 0;
    let ptr = qjs::JS_GetArrayBuffer(ctx, &mut ab_size, ab);
    if ptr.is_null() || off + len > ab_size {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastSumU32 invalid TypedArray backing buffer")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let p = ptr.add(off);
    let sum_f64;
    if (p as usize) % 4 == 0 {
        let n = len / 4;
        let s = std::slice::from_raw_parts(p as *const u32, n);
        let mut a0: u64 = 0;
        let mut a1: u64 = 0;
        let mut a2: u64 = 0;
        let mut a3: u64 = 0;
        let mut i = 0usize;
        while i + 4 <= n {
            a0 = a0.wrapping_add(s[i] as u64);
            a1 = a1.wrapping_add(s[i + 1] as u64);
            a2 = a2.wrapping_add(s[i + 2] as u64);
            a3 = a3.wrapping_add(s[i + 3] as u64);
            i += 4;
        }
        while i < n {
            a0 = a0.wrapping_add(s[i] as u64);
            i += 1;
        }
        sum_f64 = (a0.wrapping_add(a1).wrapping_add(a2).wrapping_add(a3)) as f64;
    } else {
        let bytes = std::slice::from_raw_parts(p, len);
        let mut acc: u64 = 0;
        for c in bytes.chunks_exact(4) {
            let v = u32::from_ne_bytes([c[0], c[1], c[2], c[3]]);
            acc = acc.wrapping_add(v as u64);
        }
        sum_f64 = acc as f64;
    }
    js_free_value(ctx, ab);
    qjs::JS_NewFloat64(ctx, sum_f64)
}

unsafe extern "C" fn js_kawkab_fast_sum_f64(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("kawkab.fastSumF64(input) requires Float64Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = args[0];

    let mut off: usize = 0;
    let mut len: usize = 0;
    let mut el: usize = 0;
    let ab = qjs::JS_GetTypedArrayBuffer(ctx, input, &mut off, &mut len, &mut el);
    if !qjs::JS_IsObject(ab) {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastSumF64 expects Float64Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    if el != 8 || len % 8 != 0 || off % 8 != 0 {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastSumF64 expects Float64Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let mut ab_size: usize = 0;
    let ptr = qjs::JS_GetArrayBuffer(ctx, &mut ab_size, ab);
    if ptr.is_null() || off + len > ab_size {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastSumF64 invalid TypedArray backing buffer")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let p = ptr.add(off);
    let mut sum = 0.0f64;
    if (p as usize) % 8 == 0 {
        let n = len / 8;
        let s = std::slice::from_raw_parts(p as *const f64, n);
        let mut a0 = 0.0f64;
        let mut a1 = 0.0f64;
        let mut a2 = 0.0f64;
        let mut a3 = 0.0f64;
        let mut i = 0usize;
        while i + 4 <= n {
            a0 += s[i];
            a1 += s[i + 1];
            a2 += s[i + 2];
            a3 += s[i + 3];
            i += 4;
        }
        while i < n {
            a0 += s[i];
            i += 1;
        }
        sum = a0 + a1 + a2 + a3;
    } else {
        let bytes = std::slice::from_raw_parts(p, len);
        for c in bytes.chunks_exact(8) {
            let v = f64::from_ne_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
            sum += v;
        }
    }
    js_free_value(ctx, ab);
    qjs::JS_NewFloat64(ctx, sum)
}

unsafe fn new_uint32_typed_array(ctx: *mut qjs::JSContext, data: &[u32]) -> qjs::JSValue {
    let byte_len = data.len() * 4;
    let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, byte_len);
    let ab = qjs::JS_NewArrayBufferCopy(ctx, bytes.as_ptr(), bytes.len());
    let global = qjs::JS_GetGlobalObject(ctx);
    let ctor = qjs::JS_GetPropertyStr(ctx, global, CString::new("Uint32Array").unwrap().as_ptr());
    let mut argv = [ab];
    let out = qjs::JS_CallConstructor(ctx, ctor, 1, argv.as_mut_ptr());
    js_free_value(ctx, ctor);
    js_free_value(ctx, global);
    js_free_value(ctx, ab);
    out
}

unsafe fn new_float64_typed_array(ctx: *mut qjs::JSContext, data: &[f64]) -> qjs::JSValue {
    let byte_len = data.len() * 8;
    let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, byte_len);
    let ab = qjs::JS_NewArrayBufferCopy(ctx, bytes.as_ptr(), bytes.len());
    let global = qjs::JS_GetGlobalObject(ctx);
    let ctor = qjs::JS_GetPropertyStr(ctx, global, CString::new("Float64Array").unwrap().as_ptr());
    let mut argv = [ab];
    let out = qjs::JS_CallConstructor(ctx, ctor, 1, argv.as_mut_ptr());
    js_free_value(ctx, ctor);
    js_free_value(ctx, global);
    js_free_value(ctx, ab);
    out
}

unsafe extern "C" fn js_kawkab_fast_map_u32(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("kawkab.fastMapU32(input, mul=1, add=0) requires Uint32Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = args[0];
    let mut mul: u32 = 1;
    let mut add: u32 = 0;
    if let Some(v) = args.get(1) {
        let mut x: i32 = 1;
        let _ = qjs::JS_ToInt32(ctx, &mut x as *mut i32, *v);
        mul = x.max(0) as u32;
    }
    if let Some(v) = args.get(2) {
        let mut x: i32 = 0;
        let _ = qjs::JS_ToInt32(ctx, &mut x as *mut i32, *v);
        add = x.max(0) as u32;
    }

    let mut off: usize = 0;
    let mut len: usize = 0;
    let mut el: usize = 0;
    let ab = qjs::JS_GetTypedArrayBuffer(ctx, input, &mut off, &mut len, &mut el);
    if !qjs::JS_IsObject(ab) || el != 4 || len % 4 != 0 || off % 4 != 0 {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastMapU32 expects Uint32Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let mut ab_size: usize = 0;
    let ptr = qjs::JS_GetArrayBuffer(ctx, &mut ab_size, ab);
    if ptr.is_null() || off + len > ab_size {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastMapU32 invalid TypedArray backing buffer")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let n = len / 4;
    let src = std::slice::from_raw_parts(ptr.add(off) as *const u32, n);
    let mut out = Vec::with_capacity(n);
    out.extend(src.iter().map(|v| v.wrapping_mul(mul).wrapping_add(add)));
    js_free_value(ctx, ab);
    new_uint32_typed_array(ctx, &out)
}

unsafe extern "C" fn js_kawkab_fast_filter_u32(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("kawkab.fastFilterU32(input, min) requires Uint32Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = args[0];
    let mut min_v: u32 = 0;
    if let Some(v) = args.get(1) {
        let mut x: i32 = 0;
        let _ = qjs::JS_ToInt32(ctx, &mut x as *mut i32, *v);
        min_v = x.max(0) as u32;
    }

    let mut off: usize = 0;
    let mut len: usize = 0;
    let mut el: usize = 0;
    let ab = qjs::JS_GetTypedArrayBuffer(ctx, input, &mut off, &mut len, &mut el);
    if !qjs::JS_IsObject(ab) || el != 4 || len % 4 != 0 || off % 4 != 0 {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastFilterU32 expects Uint32Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let mut ab_size: usize = 0;
    let ptr = qjs::JS_GetArrayBuffer(ctx, &mut ab_size, ab);
    if ptr.is_null() || off + len > ab_size {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastFilterU32 invalid TypedArray backing buffer")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let n = len / 4;
    let src = std::slice::from_raw_parts(ptr.add(off) as *const u32, n);
    let mut out = Vec::with_capacity(n);
    out.extend(src.iter().copied().filter(|v| *v >= min_v));
    js_free_value(ctx, ab);
    new_uint32_typed_array(ctx, &out)
}

unsafe extern "C" fn js_kawkab_fast_map_f64(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("kawkab.fastMapF64(input, mul=1, add=0) requires Float64Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = args[0];
    let mut mul = 1.0f64;
    let mut add = 0.0f64;
    if let Some(v) = args.get(1) {
        let _ = qjs::JS_ToFloat64(ctx, &mut mul as *mut f64, *v);
    }
    if let Some(v) = args.get(2) {
        let _ = qjs::JS_ToFloat64(ctx, &mut add as *mut f64, *v);
    }

    let mut off: usize = 0;
    let mut len: usize = 0;
    let mut el: usize = 0;
    let ab = qjs::JS_GetTypedArrayBuffer(ctx, input, &mut off, &mut len, &mut el);
    if !qjs::JS_IsObject(ab) || el != 8 || len % 8 != 0 || off % 8 != 0 {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastMapF64 expects Float64Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let mut ab_size: usize = 0;
    let ptr = qjs::JS_GetArrayBuffer(ctx, &mut ab_size, ab);
    if ptr.is_null() || off + len > ab_size {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastMapF64 invalid TypedArray backing buffer")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let n = len / 8;
    let src = std::slice::from_raw_parts(ptr.add(off) as *const f64, n);
    let mut out = Vec::with_capacity(n);
    out.extend(src.iter().map(|v| *v * mul + add));
    js_free_value(ctx, ab);
    new_float64_typed_array(ctx, &out)
}

unsafe extern "C" fn js_kawkab_fast_filter_f64(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("kawkab.fastFilterF64(input, min) requires Float64Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let input = args[0];
    let mut min_v = 0.0f64;
    if let Some(v) = args.get(1) {
        let _ = qjs::JS_ToFloat64(ctx, &mut min_v as *mut f64, *v);
    }

    let mut off: usize = 0;
    let mut len: usize = 0;
    let mut el: usize = 0;
    let ab = qjs::JS_GetTypedArrayBuffer(ctx, input, &mut off, &mut len, &mut el);
    if !qjs::JS_IsObject(ab) || el != 8 || len % 8 != 0 || off % 8 != 0 {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastFilterF64 expects Float64Array")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let mut ab_size: usize = 0;
    let ptr = qjs::JS_GetArrayBuffer(ctx, &mut ab_size, ab);
    if ptr.is_null() || off + len > ab_size {
        js_free_value(ctx, ab);
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("fastFilterF64 invalid TypedArray backing buffer")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let n = len / 8;
    let src = std::slice::from_raw_parts(ptr.add(off) as *const f64, n);
    let mut out = Vec::with_capacity(n);
    out.extend(src.iter().copied().filter(|v| *v >= min_v));
    js_free_value(ctx, ab);
    new_float64_typed_array(ctx, &out)
}

unsafe extern "C" fn js_queue_microtask(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("queueMicrotask(callback) requires callback")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let cb = args[0];
    if qjs::JS_IsFunction(ctx, cb) == 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("queueMicrotask callback must be a function")
                .unwrap()
                .as_ptr(),
        );
    }
    if qjs::JS_EnqueueJob(ctx, Some(js_enqueue_js_call_job), 1, argv) != 0 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("queueMicrotask: failed to enqueue job")
                .unwrap()
                .as_ptr(),
        );
    }
    js_undefined()
}

unsafe extern "C" fn js_next_promise_id(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let id = NEXT_PROMISE_ID.fetch_add(1, Ordering::SeqCst);
    qjs::JS_NewFloat64(ctx, id as f64)
}

unsafe extern "C" fn js_crypto_random_bytes_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "randomBytesSync(size) requires size");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let mut size: i32 = 0;
    if qjs::JS_ToInt32(ctx, &mut size, args[0]) < 0 {
        return crate::ffi::throw_type_error(ctx, "size must be a number");
    }
    match crypto::random_bytes(size as usize) {
        Ok(bytes) => crate::ffi::arraybuffer_from_slice(ctx, &bytes),
        Err(e) => crate::ffi::throw_type_error(ctx, &e),
    }
}

unsafe extern "C" fn js_crypto_random_bytes(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return js_undefined();
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let mut size: i32 = 0;
    qjs::JS_ToInt32(ctx, &mut size, args[0]);
    let mut promise_id_f: f64 = 0.0;
    qjs::JS_ToFloat64(ctx, &mut promise_id_f as *mut f64, args[1]);
    let _promise_id = promise_id_f as u64;

    /*
    PENDING_ASYNC_TIMERS.fetch_add(1, Ordering::SeqCst);
    tokio::task::spawn_blocking(move || {
        match crypto::random_bytes(size as usize) {
            Ok(bytes) => sender.resolve_promise(promise_id, Arc::from(bytes.as_slice())),
            Err(e) => sender.reject_promise(promise_id, e),
        }
        PENDING_ASYNC_TIMERS.fetch_sub(1, Ordering::SeqCst);
    });
    */
    js_undefined()
}

unsafe extern "C" fn js_crypto_create_hash(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "createHash(algorithm) requires algorithm");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let alg = js_string_to_owned(ctx, args[0]);
    match crypto::create_hash(&alg) {
        Ok(id) => qjs::JS_NewFloat64(ctx, id as f64),
        Err(e) => crate::ffi::throw_type_error(ctx, &e),
    }
}

unsafe extern "C" fn js_crypto_create_hmac(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return crate::ffi::throw_type_error(ctx, "createHmac(algorithm, key) requires arguments");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let alg = js_string_to_owned(ctx, args[0]);
    let key = buffer::buffer_bytes_from_value(ctx, args[1]);
    match crypto::create_hmac(&alg, &key) {
        Ok(id) => qjs::JS_NewFloat64(ctx, id as f64),
        Err(e) => crate::ffi::throw_type_error(ctx, &e),
    }
}

unsafe extern "C" fn js_crypto_update(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return js_undefined();
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let mut id_f: f64 = 0.0;
    qjs::JS_ToFloat64(ctx, &mut id_f as *mut f64, args[0]);
    let id = id_f as u64;

    if let Some(bytes) = crate::ffi::arraybuffer_bytes(ctx, args[1]) {
        if let Err(e) = crypto::update(id, bytes) {
            return crate::ffi::throw_type_error(ctx, &e);
        }
    } else {
        let bytes = buffer::buffer_bytes_from_value(ctx, args[1]);
        if let Err(e) = crypto::update(id, &bytes) {
            return crate::ffi::throw_type_error(ctx, &e);
        }
    }
    js_undefined()
}

unsafe extern "C" fn js_crypto_digest(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "digest(id) requires id");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let mut id_f: f64 = 0.0;
    qjs::JS_ToFloat64(ctx, &mut id_f as *mut f64, args[0]);
    let id = id_f as u64;

    match crypto::digest(id) {
        Ok(bytes) => crate::ffi::arraybuffer_from_slice(ctx, &bytes),
        Err(e) => crate::ffi::throw_type_error(ctx, &e),
    }
}

unsafe extern "C" fn js_require(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("require(name) requires module name")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let mut request = js_string_to_owned(ctx, args[0]);
    if let Some(stripped) = request.strip_prefix("node:") {
        request = stripped.to_string();
    }
    if request == "assert" {
        return install_builtin_assert(ctx);
    }
    if request == "console" {
        let global = qjs::JS_GetGlobalObject(ctx);
        let console_val =
            qjs::JS_GetPropertyStr(ctx, global, CString::new("console").unwrap().as_ptr());
        js_free_value(ctx, global);
        if qjs::JS_IsUndefined(console_val) {
            js_free_value(ctx, console_val);
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new("global console is not available")
                    .unwrap_or_default()
                    .as_ptr(),
            );
        }
        let out = js_dup_value(console_val);
        js_free_value(ctx, console_val);
        return out;
    }
    if request == "process" {
        let global = qjs::JS_GetGlobalObject(ctx);
        let proc = qjs::JS_GetPropertyStr(ctx, global, CString::new("process").unwrap().as_ptr());
        js_free_value(ctx, global);
        if qjs::JS_IsUndefined(proc) {
            js_free_value(ctx, proc);
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new("global process is not available")
                    .unwrap_or_default()
                    .as_ptr(),
            );
        }
        let out = js_dup_value(proc);
        js_free_value(ctx, proc);
        return out;
    }
    if request == "buffer" {
        let global = qjs::JS_GetGlobalObject(ctx);
        let buf_ctor =
            qjs::JS_GetPropertyStr(ctx, global, CString::new("Buffer").unwrap().as_ptr());
        if !qjs::JS_IsObject(buf_ctor) {
            js_free_value(ctx, buf_ctor);
            js_free_value(ctx, global);
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new("global Buffer is not available")
                    .unwrap_or_default()
                    .as_ptr(),
            );
        }
        let exports = qjs::JS_NewObject(ctx);
        qjs::JS_SetPropertyStr(
            ctx,
            exports,
            CString::new("Buffer").unwrap().as_ptr(),
            js_dup_value(buf_ctor),
        );
        qjs::JS_SetPropertyStr(
            ctx,
            exports,
            CString::new("default").unwrap().as_ptr(),
            js_dup_value(buf_ctor),
        );
        qjs::JS_SetPropertyStr(
            ctx,
            exports,
            CString::new("SlowBuffer").unwrap().as_ptr(),
            js_dup_value(buf_ctor),
        );
        let kmax = qjs::JS_NewFloat64(ctx, 0x7fff_ffff as f64);
        qjs::JS_SetPropertyStr(
            ctx,
            exports,
            CString::new("kMaxLength").unwrap().as_ptr(),
            kmax,
        );
        let constants = qjs::JS_NewObject(ctx);
        qjs::JS_SetPropertyStr(
            ctx,
            constants,
            CString::new("MAX_LENGTH").unwrap().as_ptr(),
            qjs::JS_NewFloat64(ctx, 0x7fff_ffff as f64),
        );
        qjs::JS_SetPropertyStr(
            ctx,
            constants,
            CString::new("MAX_STRING_LENGTH").unwrap().as_ptr(),
            qjs::JS_NewFloat64(ctx, (1usize << 28) as f64 - 16.0),
        );
        qjs::JS_SetPropertyStr(
            ctx,
            exports,
            CString::new("constants").unwrap().as_ptr(),
            constants,
        );
        js_free_value(ctx, buf_ctor);
        js_free_value(ctx, global);
        return exports;
    }
    if request == "fs" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "readFileSync", Some(js_read_file_sync), 1);
        let _ = install_obj_fn(ctx, obj, "writeFileSync", Some(js_write_file_sync), 2);
        let _ = install_obj_fn(ctx, obj, "copyFileSync", Some(js_fs_copy_file_sync), 2);
        let _ = install_obj_fn(ctx, obj, "rmSync", Some(js_fs_rm_sync), 2);
        let _ = install_obj_fn(ctx, obj, "existsSync", Some(js_fs_exists_sync), 1);
        let _ = install_obj_fn(ctx, obj, "mkdirSync", Some(js_fs_mkdir_sync), 2);
        let _ = install_obj_fn(ctx, obj, "readdirSync", Some(js_fs_readdir_sync), 1);
        let _ = install_obj_fn(ctx, obj, "unlinkSync", Some(js_fs_unlink_sync), 1);
        let _ = install_obj_fn(ctx, obj, "rmdirSync", Some(js_fs_rmdir_sync), 1);
        let _ = install_obj_fn(ctx, obj, "statSync", Some(js_fs_stat_sync), 1);
        let promises = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, promises, "readFile", Some(js_fs_promises_read_file), 1);
        let _ = install_obj_fn(
            ctx,
            promises,
            "writeFile",
            Some(js_fs_promises_write_file),
            2,
        );
        let _ = install_obj_fn(ctx, promises, "stat", Some(js_fs_promises_stat), 1);
        let _ = install_obj_fn(ctx, promises, "readdir", Some(js_fs_promises_readdir), 1);
        let _ = install_obj_fn(ctx, promises, "mkdir", Some(js_fs_promises_mkdir), 2);
        let _ = install_obj_fn(ctx, promises, "unlink", Some(js_fs_promises_unlink), 1);
        qjs::JS_SetPropertyStr(
            ctx,
            obj,
            CString::new("promises").unwrap().as_ptr(),
            promises,
        );
        return obj;
    }
    if request == "path" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "join", Some(js_path_join), 2);
        let _ = install_obj_fn(ctx, obj, "dirname", Some(js_path_dirname), 1);
        let _ = install_obj_fn(ctx, obj, "basename", Some(js_path_basename), 1);
        let _ = install_obj_fn(ctx, obj, "extname", Some(js_path_extname), 1);
        let _ = install_obj_fn(ctx, obj, "resolve", Some(js_path_resolve), 4);
        let _ = install_obj_fn(ctx, obj, "normalize", Some(js_path_normalize), 1);
        let _ = install_obj_fn(ctx, obj, "relative", Some(js_path_relative), 2);
        let _ = install_obj_fn(ctx, obj, "parse", Some(js_path_parse), 1);
        #[cfg(windows)]
        {
            let _ = set_str_prop(ctx, obj, "sep", "\\");
            let _ = set_str_prop(ctx, obj, "delimiter", ";");
        }
        #[cfg(not(windows))]
        {
            let _ = set_str_prop(ctx, obj, "sep", "/");
            let _ = set_str_prop(ctx, obj, "delimiter", ":");
        }
        return obj;
    }
    if request == "os" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "platform", Some(js_get_platform), 0);
        let _ = install_obj_fn(ctx, obj, "tmpdir", Some(js_os_tmpdir), 0);
        let _ = install_obj_fn(ctx, obj, "homedir", Some(js_os_homedir), 0);
        return obj;
    }
    if request == "punycode" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "decode", Some(js_punycode_decode), 1);
        let _ = install_obj_fn(ctx, obj, "encode", Some(js_punycode_encode), 1);
        let _ = install_obj_fn(ctx, obj, "toASCII", Some(js_punycode_to_ascii), 1);
        let _ = install_obj_fn(ctx, obj, "toUnicode", Some(js_punycode_to_unicode), 1);
        return obj;
    }
    if request == "events" {
        return require_primmed_or_eval(ctx, "__kawkabPrimedEvents", "events", EVENTS_SHIM_SRC);
    }
    if request == "util" || request == "sys" {
        let obj = qjs::JS_NewObject(ctx);
        let inspect = qjs::JS_NewCFunction2(
            ctx,
            Some(js_util_inspect),
            CString::new("inspect").unwrap().as_ptr(),
            1,
            qjs::JSCFunctionEnum_JS_CFUNC_generic,
            0,
        );
        qjs::JS_SetPropertyStr(ctx, obj, CString::new("inspect").unwrap().as_ptr(), inspect);
        let types = qjs::JS_NewObject(ctx);
        let is_date = qjs::JS_NewCFunction2(
            ctx,
            Some(js_util_is_date),
            CString::new("isDate").unwrap().as_ptr(),
            1,
            qjs::JSCFunctionEnum_JS_CFUNC_generic,
            0,
        );
        qjs::JS_SetPropertyStr(
            ctx,
            types,
            CString::new("isDate").unwrap().as_ptr(),
            is_date,
        );
        qjs::JS_SetPropertyStr(ctx, obj, CString::new("types").unwrap().as_ptr(), types);
        let g2 = qjs::JS_GetGlobalObject(ctx);
        let pf = qjs::JS_GetPropertyStr(
            ctx,
            g2,
            CString::new("__kawkabUtilPromisify").unwrap().as_ptr(),
        );
        js_free_value(ctx, g2);
        if !qjs::JS_IsUndefined(pf) && qjs::JS_IsFunction(ctx, pf) != 0 {
            qjs::JS_SetPropertyStr(ctx, obj, CString::new("promisify").unwrap().as_ptr(), pf);
        } else {
            js_free_value(ctx, pf);
        }
        return obj;
    }
    if request == "net" {
        let obj = qjs::JS_NewObject(ctx);
        let create = qjs::JS_NewCFunction2(
            ctx,
            Some(js_net_http_create_server),
            CString::new("createServer").unwrap().as_ptr(),
            1,
            qjs::JSCFunctionEnum_JS_CFUNC_generic,
            0,
        );
        qjs::JS_SetPropertyStr(
            ctx,
            obj,
            CString::new("createServer").unwrap().as_ptr(),
            create,
        );
        return obj;
    }
    if request == "http" || request == "https" {
        let obj = qjs::JS_NewObject(ctx);
        let create = qjs::JS_NewCFunction2(
            ctx,
            Some(js_net_http_create_server),
            CString::new("createServer").unwrap().as_ptr(),
            1,
            qjs::JSCFunctionEnum_JS_CFUNC_generic,
            0,
        );
        qjs::JS_SetPropertyStr(
            ctx,
            obj,
            CString::new("createServer").unwrap().as_ptr(),
            create,
        );
        let _ = install_obj_fn(ctx, obj, "get", Some(js_http_client_get), 2);
        let _ = install_obj_fn(ctx, obj, "request", Some(js_http_client_request), 2);
        return obj;
    }
    if request == "child_process" {
        let policy = RuntimePolicy::from_env();
        if !policy.allow_child_process {
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new(
                    "child_process is disabled by policy (set KAWKAB_ALLOW_CHILD_PROCESS=1 to enable)",
                )
                .unwrap()
                .as_ptr(),
            );
        }
        let obj = qjs::JS_NewObject(ctx);
        let exec_fn = qjs::JS_NewCFunction2(
            ctx,
            Some(js_run_command),
            CString::new("execSync").unwrap().as_ptr(),
            1,
            qjs::JSCFunctionEnum_JS_CFUNC_generic,
            0,
        );
        qjs::JS_SetPropertyStr(
            ctx,
            obj,
            CString::new("execSync").unwrap().as_ptr(),
            exec_fn,
        );
        let _ = install_obj_fn(ctx, obj, "spawnSync", Some(js_spawn_sync), 2);
        return obj;
    }
    if request == "stream" {
        return require_primmed_or_eval(ctx, "__kawkabPrimedStream", "stream", STREAM_SHIM_SRC);
    }
    if request == "crypto" {
        return require_primmed_or_eval(ctx, "__kawkabPrimedCrypto", "crypto", CRYPTO_SHIM_SRC);
    }

    if request == "url" {
        return require_primmed_or_eval(ctx, "__kawkabPrimedUrl", "url", URL_SHIM_SRC);
    }
    if request == "querystring" {
        return require_primmed_or_eval(
            ctx,
            "__kawkabPrimedQuerystring",
            "querystring",
            QUERYSTRING_SHIM_SRC,
        );
    }
    if request == "string_decoder" {
        let obj = qjs::JS_NewObject(ctx);
        let ctor = qjs::JS_NewCFunction2(
            ctx,
            Some(js_string_decoder_ctor),
            CString::new("StringDecoder").unwrap().as_ptr(),
            1,
            qjs::JSCFunctionEnum_JS_CFUNC_constructor,
            0,
        );
        qjs::JS_SetPropertyStr(
            ctx,
            obj,
            CString::new("StringDecoder").unwrap().as_ptr(),
            ctor,
        );
        return obj;
    }
    if request == "dgram" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "createSocket", Some(js_dgram_create_socket), 2);
        return obj;
    }
    if request == "diagnostics_channel" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "channel", Some(js_diag_channel), 1);
        let _ = install_obj_fn(ctx, obj, "hasSubscribers", Some(js_diag_has_subscribers), 1);
        let _ = install_obj_fn(ctx, obj, "subscribe", Some(js_diag_subscribe), 2);
        let _ = install_obj_fn(ctx, obj, "unsubscribe", Some(js_diag_unsubscribe), 2);
        let _ = install_obj_fn(ctx, obj, "tracingChannel", Some(js_diag_tracing_channel), 1);
        let _ = install_obj_fn(ctx, obj, "boundedChannel", Some(js_diag_bounded_channel), 1);
        return obj;
    }
    if request == "dns" {
        return require_primmed_or_eval(ctx, "__kawkabPrimedDns", "dns", DNS_SHIM_SRC);
    }
    if request == "dns/promises" {
        return require_primmed_or_eval(
            ctx,
            "__kawkabPrimedDnsPromises",
            "dns-promises",
            DNS_PROMISES_SHIM_SRC,
        );
    }
    if request == "readline" {
        return require_primmed_or_eval(
            ctx,
            "__kawkabPrimedReadline",
            "readline",
            READLINE_SHIM_SRC,
        );
    }
    if request == "zlib" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "gzipSync", Some(js_zlib_gzip_sync), 1);
        let _ = install_obj_fn(ctx, obj, "gunzipSync", Some(js_zlib_gunzip_sync), 1);
        let _ = install_obj_fn(ctx, obj, "deflateSync", Some(js_zlib_deflate_sync), 1);
        let _ = install_obj_fn(ctx, obj, "inflateSync", Some(js_zlib_inflate_sync), 1);
        return obj;
    }
    if request == "tls" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "connect", Some(js_noop), 0);
        let _ = install_obj_fn(ctx, obj, "createServer", Some(js_noop), 0);
        return obj;
    }
    if request == "vm" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(
            ctx,
            obj,
            "runInThisContext",
            Some(js_vm_run_in_this_context),
            1,
        );
        let _ = install_obj_fn(
            ctx,
            obj,
            "runInNewContext",
            Some(js_vm_run_in_new_context),
            2,
        );
        let _ = install_obj_fn(ctx, obj, "Script", Some(js_return_empty_object), 1);
        let _ = install_obj_fn(ctx, obj, "createContext", Some(js_return_empty_object), 1);
        let _ = install_obj_fn(ctx, obj, "isContext", Some(js_noop), 1);
        return obj;
    }
    if request == "worker_threads" {
        let obj = qjs::JS_NewObject(ctx);
        let ctor = qjs::JS_NewCFunction2(
            ctx,
            Some(js_worker_ctor),
            CString::new("Worker").unwrap().as_ptr(),
            2,
            qjs::JSCFunctionEnum_JS_CFUNC_constructor,
            0,
        );
        qjs::JS_SetPropertyStr(ctx, obj, CString::new("Worker").unwrap().as_ptr(), ctor);
        let _ = install_obj_fn(ctx, obj, "MessageChannel", Some(js_return_empty_object), 0);
        qjs::JS_SetPropertyStr(
            ctx,
            obj,
            CString::new("isMainThread").unwrap().as_ptr(),
            qjs::JS_NewBool(ctx, true),
        );
        qjs::JS_SetPropertyStr(
            ctx,
            obj,
            CString::new("parentPort").unwrap().as_ptr(),
            js_undefined(),
        );
        return obj;
    }
    if request == "timers" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_c_fn(ctx, obj, "setTimeout", Some(js_set_timeout), 2);
        let _ = install_c_fn(ctx, obj, "clearTimeout", Some(js_clear_timeout), 1);
        let _ = install_c_fn(ctx, obj, "setInterval", Some(js_set_interval), 2);
        let _ = install_c_fn(ctx, obj, "clearInterval", Some(js_clear_timeout), 1);
        let _ = install_c_fn(ctx, obj, "setImmediate", Some(js_set_timeout), 1);
        let _ = install_c_fn(ctx, obj, "clearImmediate", Some(js_clear_timeout), 1);
        return obj;
    }
    if request == "timers/promises" {
        let global = qjs::JS_GetGlobalObject(ctx);
        let k = CString::new("__kawkabPrimedTimersPromises").unwrap();
        let exports_val = qjs::JS_GetPropertyStr(ctx, global, k.as_ptr());
        js_free_value(ctx, global);
        if qjs::JS_IsUndefined(exports_val) {
            js_free_value(ctx, exports_val);
            return qjs::JS_ThrowInternalError(
                ctx,
                CString::new("timers/promises builtin not initialized")
                    .unwrap()
                    .as_ptr(),
            );
        }
        let out = js_dup_value(exports_val);
        js_free_value(ctx, exports_val);
        return out;
    }
    if request == "perf_hooks" {
        let global = qjs::JS_GetGlobalObject(ctx);
        let perf =
            qjs::JS_GetPropertyStr(ctx, global, CString::new("performance").unwrap().as_ptr());
        let obj = qjs::JS_NewObject(ctx);
        qjs::JS_SetPropertyStr(
            ctx,
            obj,
            CString::new("performance").unwrap().as_ptr(),
            js_dup_value(perf),
        );
        js_free_value(ctx, perf);
        js_free_value(ctx, global);
        return obj;
    }
    if request == "node:test" || request == "test" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "test", Some(js_node_test_run), 2);
        let _ = install_obj_fn(ctx, obj, "it", Some(js_node_test_run), 2);
        let _ = install_obj_fn(ctx, obj, "describe", Some(js_node_test_run), 2);
        return obj;
    }
    if request == "module" {
        let obj = qjs::JS_NewObject(ctx);
        let _ = install_obj_fn(ctx, obj, "createRequire", Some(js_module_create_require), 1);
        return obj;
    }
    refresh_package_exports_node_env(ctx);
    let base = REQUIRE_BASE_DIR.with(|v| v.borrow().clone());
    let resolved = resolve_module_path(&base, &request);

    if resolved.ends_with(".json") {
        let data = match std::fs::read_to_string(&resolved) {
            Ok(s) => s,
            Err(e) => {
                return qjs::JS_ThrowTypeError(
                    ctx,
                    CString::new(format!("require: cannot read JSON '{}': {}", resolved, e))
                        .unwrap_or_default()
                        .as_ptr(),
                )
            }
        };
        let c_path = CString::new(resolved.clone()).unwrap_or_default();
        return qjs::JS_ParseJSON(
            ctx,
            data.as_ptr() as *const std::os::raw::c_char,
            data.len(),
            c_path.as_ptr(),
        );
    }

    let source_for_detect = std::fs::read_to_string(&resolved).unwrap_or_default();
    let src_type = module_loader::detect_source_type(&resolved, &source_for_detect);

    if src_type == module_loader::SourceType::Esm {
        let dir = Path::new(&resolved)
            .parent()
            .unwrap_or(Path::new("."))
            .to_string_lossy()
            .to_string();
        return esm_loader::require_esm_as_cjs(ctx, &resolved, &dir);
    }

    let source = MODULE_SOURCE_CACHE.with(|cache| cache.borrow().get(&resolved).cloned());
    let source = match source {
        Some(s) => s,
        None => match std::fs::read_to_string(&resolved) {
            Ok(s) => {
                let loaded = match crate::transpiler::transpile_ts(&s, &resolved) {
                    Ok(js) => js,
                    Err(e) => {
                        return qjs::JS_ThrowTypeError(
                            ctx,
                            CString::new(format!("require transpile failed: {e}"))
                                .unwrap_or_default()
                                .as_ptr(),
                        )
                    }
                };
                MODULE_SOURCE_CACHE.with(|cache| {
                    cache.borrow_mut().insert(resolved.clone(), loaded.clone());
                });
                loaded
            }
            Err(e) => {
                return qjs::JS_ThrowTypeError(
                    ctx,
                    CString::new(format!("require failed: {e}"))
                        .unwrap_or_default()
                        .as_ptr(),
                )
            }
        },
    };
    let wrapper_start = "(function(exports, require, module, __filename, __dirname) {\n";
    let wrapper_end = "\n});";
    let wrapped_source = format!("{}{}{}", wrapper_start, source, wrapper_end);

    let func_val = qjs_compat::eval(
        ctx,
        wrapped_source.as_ptr() as *const i8,
        wrapped_source.len(),
        CString::new(resolved.clone()).unwrap().as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    );

    if is_exception(func_val) {
        return func_val;
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
        qjs_compat::new_string_from_cstr(ctx, CString::new(resolved.clone()).unwrap().as_ptr());
    let dir = Path::new(&resolved)
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy()
        .to_string();
    let dirname_val = qjs_compat::new_string_from_cstr(
        ctx,
        CString::new(dir.clone()).unwrap_or_default().as_ptr(),
    );
    let previous = REQUIRE_BASE_DIR.with(|v| {
        let p = v.borrow().clone();
        *v.borrow_mut() = dir;
        p
    });
    let _require_dir_guard = RequireBaseDirGuard { previous };

    let exports_arg = crate::ffi::js_dup_value(exports_obj);
    let module_arg = crate::ffi::js_dup_value(module_obj);

    let mut args = [
        exports_arg,
        require_fn,
        module_arg,
        filename_val,
        dirname_val,
    ];
    let ret = qjs::JS_Call(ctx, func_val, global, 5, args.as_mut_ptr());

    crate::ffi::js_free_value(ctx, func_val);
    crate::ffi::js_free_value(ctx, exports_arg);
    crate::ffi::js_free_value(ctx, module_arg);
    crate::ffi::js_free_value(ctx, require_fn);
    crate::ffi::js_free_value(ctx, filename_val);
    crate::ffi::js_free_value(ctx, dirname_val);
    crate::ffi::js_free_value(ctx, global);

    if is_exception(ret) {
        crate::ffi::js_free_value(ctx, module_obj);
        return ret;
    }
    crate::ffi::js_free_value(ctx, ret);

    let out = qjs::JS_GetPropertyStr(ctx, module_obj, CString::new("exports").unwrap().as_ptr());
    crate::ffi::js_free_value(ctx, module_obj);
    out
}

fn strip_file_url(s: &str) -> String {
    let s = s.trim();
    if let Some(mut rest) = s.strip_prefix("file://") {
        if let Some(r) = rest.strip_prefix("localhost") {
            rest = r;
        }
        if rest.starts_with('/') && rest.as_bytes().get(3) == Some(&b':') {
            return rest[1..].to_string();
        }
        rest.to_string()
    } else {
        s.to_string()
    }
}

unsafe extern "C" fn js_module_create_require(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("createRequire(filename) requires filename")
                .unwrap_or_default()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let url_or_path = js_string_to_owned(ctx, args[0]);
    let filename = strip_file_url(&url_or_path);
    let parent = Path::new(&filename)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    let parent_val = qjs_compat::new_string_from_str(ctx, &parent);
    qjs::JS_NewCFunctionData(
        ctx,
        Some(js_require_bound_to_dir),
        1,
        0,
        1,
        &mut [parent_val] as *mut qjs::JSValue,
    )
}

unsafe extern "C" fn js_require_bound_to_dir(
    ctx: *mut qjs::JSContext,
    this_val: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
    _magic: c_int,
    data: *mut qjs::JSValue,
) -> qjs::JSValue {
    let base_dir = js_string_to_owned(ctx, *data);
    let previous = REQUIRE_BASE_DIR.with(|v| {
        let p = v.borrow().clone();
        *v.borrow_mut() = base_dir;
        p
    });
    let _guard = RequireBaseDirGuard { previous };
    js_require(ctx, this_val, argc, argv)
}

unsafe extern "C" fn js_fs_exists_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewBool(ctx, false);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    qjs::JS_NewBool(ctx, Path::new(&path).exists())
}

unsafe fn build_stat_object(ctx: *mut qjs::JSContext, meta: &std::fs::Metadata) -> qjs::JSValue {
    let obj = qjs::JS_NewObject(ctx);
    let kind: i32 = if meta.is_dir() { 2 } else { 1 };
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("__kawkabStatKind").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, kind as i64),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("size").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, meta.len() as i64),
    );
    let ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("mtimeMs").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, ms),
    );
    let _ = install_obj_fn(ctx, obj, "isFile", Some(js_stat_is_file), 0);
    let _ = install_obj_fn(ctx, obj, "isDirectory", Some(js_stat_is_directory), 0);
    obj
}

unsafe extern "C" fn js_stat_is_file(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let v = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabStatKind").unwrap().as_ptr(),
    );
    let mut k: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut k as *mut i32, v);
    js_free_value(ctx, v);
    qjs::JS_NewBool(ctx, k == 1)
}

unsafe extern "C" fn js_stat_is_directory(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let v = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabStatKind").unwrap().as_ptr(),
    );
    let mut k: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut k as *mut i32, v);
    js_free_value(ctx, v);
    qjs::JS_NewBool(ctx, k == 2)
}

unsafe extern "C" fn js_fs_stat_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("statSync(path) requires path")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    match std::fs::metadata(&path) {
        Ok(m) => build_stat_object(ctx, &m),
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("statSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_fs_mkdir_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("mkdirSync(path) requires path")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    let mut recursive = false;
    if argc >= 2 {
        let opts = args[1];
        if qjs::JS_IsObject(opts) {
            let r = qjs::JS_GetPropertyStr(ctx, opts, CString::new("recursive").unwrap().as_ptr());
            if qjs::JS_ToBool(ctx, r) != 0 {
                recursive = true;
            }
            js_free_value(ctx, r);
        }
    }
    let res = if recursive {
        std::fs::create_dir_all(&path)
    } else {
        std::fs::create_dir(&path)
    };
    match res {
        Ok(()) => js_undefined(),
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("mkdirSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_fs_readdir_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("readdirSync(path) requires path")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    match std::fs::read_dir(&path) {
        Ok(rd) => {
            let arr = qjs::JS_NewArray(ctx);
            let mut i: u32 = 0;
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().to_string();
                qjs::JS_SetPropertyUint32(ctx, arr, i, qjs_compat::new_string_from_str(ctx, &name));
                i += 1;
            }
            arr
        }
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("readdirSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_fs_unlink_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("unlinkSync(path) requires path")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    match std::fs::remove_file(&path) {
        Ok(()) => js_undefined(),
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("unlinkSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_fs_rmdir_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("rmdirSync(path) requires path")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let path = js_string_to_owned(ctx, args[0]);
    match std::fs::remove_dir(&path) {
        Ok(()) => js_undefined(),
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("rmdirSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_path_relative(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("path.relative(from, to) requires two paths")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let from = js_string_to_owned(ctx, args[0]);
    let to = js_string_to_owned(ctx, args[1]);
    let rel = pathdiff::diff_paths(Path::new(&to), Path::new(&from))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| to.clone());
    qjs_compat::new_string_from_str(ctx, &rel)
}

unsafe extern "C" fn js_path_parse(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("path.parse(path) requires path")
                .unwrap()
                .as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let pstr = js_string_to_owned(ctx, args[0]);
    let path = Path::new(&pstr);
    let dir = path
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root = if pstr.starts_with('/') {
        "/".to_string()
    } else {
        String::new()
    };
    let base = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let obj = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("root").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &root),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("dir").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &dir),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("base").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &base),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("ext").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &ext),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("name").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &stem),
    );
    obj
}

unsafe extern "C" fn js_path_join(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let mut out = PathBuf::new();
    for arg in args {
        out.push(js_string_to_owned(ctx, *arg));
    }
    qjs_compat::new_string_from_cstr(
        ctx,
        CString::new(out.to_string_lossy().to_string())
            .unwrap_or_default()
            .as_ptr(),
    )
}
unsafe extern "C" fn js_path_dirname(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let p = args
        .get(0)
        .map(|v| js_string_to_owned(ctx, *v))
        .unwrap_or_else(|| ".".to_string());
    let dir = Path::new(&p)
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy()
        .to_string();
    qjs_compat::new_string_from_cstr(ctx, CString::new(dir).unwrap_or_default().as_ptr())
}
unsafe extern "C" fn js_path_basename(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let p = args
        .get(0)
        .map(|v| js_string_to_owned(ctx, *v))
        .unwrap_or_default();
    let base = Path::new(&p)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    qjs_compat::new_string_from_cstr(ctx, CString::new(base).unwrap_or_default().as_ptr())
}
unsafe extern "C" fn js_path_extname(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let p = args
        .get(0)
        .map(|v| js_string_to_owned(ctx, *v))
        .unwrap_or_default();
    let ext = Path::new(&p)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();
    qjs_compat::new_string_from_cstr(ctx, CString::new(ext).unwrap_or_default().as_ptr())
}
unsafe extern "C" fn js_path_resolve(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let mut out = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for arg in args {
        let p = js_string_to_owned(ctx, *arg);
        if p.is_empty() {
            continue;
        }
        let segment = PathBuf::from(&p);
        if segment.is_absolute() {
            out = segment;
        } else {
            out.push(segment);
        }
    }
    let normalized = normalize_path_like_node(&out);
    qjs_compat::new_string_from_str(ctx, &normalized)
}
unsafe extern "C" fn js_path_normalize(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let p = args
        .get(0)
        .map(|v| js_string_to_owned(ctx, *v))
        .unwrap_or_else(|| ".".to_string());
    let normalized = normalize_path_like_node(Path::new(&p));
    qjs_compat::new_string_from_str(ctx, &normalized)
}
unsafe extern "C" fn js_os_tmpdir(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    qjs_compat::new_string_from_cstr(ctx, CString::new("/tmp").unwrap().as_ptr())
}
unsafe extern "C" fn js_os_homedir(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let home = std::env::var("HOME").unwrap_or_default();
    qjs_compat::new_string_from_cstr(ctx, CString::new(home).unwrap_or_default().as_ptr())
}
unsafe extern "C" fn js_spawn_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let mut line = args
        .get(0)
        .map(|v| js_string_to_owned(ctx, *v))
        .unwrap_or_default();
    if let Some(v) = args.get(1) {
        let rest = js_string_to_owned(ctx, *v);
        if !rest.is_empty() {
            line.push(' ');
            line.push_str(&rest);
        }
    }
    let out = Command::new("sh").arg("-lc").arg(line).output();
    let obj = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("status").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, 0),
    );
    let stdout = out
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("stdout").unwrap().as_ptr(),
        qjs_compat::new_string_from_cstr(ctx, CString::new(stdout).unwrap_or_default().as_ptr()),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("stderr").unwrap().as_ptr(),
        qjs_compat::new_string_from_cstr(ctx, CString::new("").unwrap().as_ptr()),
    );
    obj
}

unsafe extern "C" fn js_util_inspect(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs_compat::new_string_from_cstr(ctx, CString::new("undefined").unwrap().as_ptr());
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let s = js_string_to_owned(ctx, args[0]);
    qjs_compat::new_string_from_cstr(ctx, CString::new(s).unwrap_or_default().as_ptr())
}

unsafe extern "C" fn js_util_is_date(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewBool(ctx, false);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let v = args[0];
    if !qjs::JS_IsObject(v) {
        return qjs::JS_NewBool(ctx, false);
    }
    let probe = qjs::JS_NewDate(ctx, 0.0);
    let cid_date = qjs::JS_GetClassID(probe);
    js_free_value(ctx, probe);
    qjs::JS_NewBool(ctx, qjs::JS_GetClassID(v) == cid_date)
}

fn http_status_text(code: i32) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => {
            if code >= 500 {
                "Internal Server Error"
            } else if code >= 400 {
                "Bad Request"
            } else {
                "OK"
            }
        }
    }
}

fn read_http_request_bytes(
    stream: &mut std::net::TcpStream,
) -> std::io::Result<(
    String,
    String,
    HashMap<String, String>,
    Arc<[u8]>,
    usize,
    usize,
)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(head) = io::http::parse_request_head(&buf)? {
            let content_len = io::http::content_length(&head.headers);
            let header_len = head.header_len;
            let total_needed = header_len + content_len;
            while buf.len() < total_needed {
                let n = stream.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body_len = content_len.min(buf.len().saturating_sub(header_len));
            let method = head.method;
            let path = head.path;
            let headers = head.headers;
            let storage = io::http::arc_buffer(buf);
            return Ok((method, path, headers, storage, header_len, body_len));
        }
        if buf.len() > 256 * 1024 * 1024 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "headers too large",
            ));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            if buf.is_empty() {
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "empty request",
                ));
            }
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "incomplete headers",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

unsafe fn js_get_server_closed(ctx: *mut qjs::JSContext, server: qjs::JSValue) -> bool {
    let v = qjs::JS_GetPropertyStr(
        ctx,
        server,
        CString::new("__kawkabClosed").unwrap().as_ptr(),
    );
    let b = qjs::JS_ToBool(ctx, v) != 0;
    js_free_value(ctx, v);
    b
}

unsafe fn merge_js_object_into(ctx: *mut qjs::JSContext, from: qjs::JSValue, to: qjs::JSValue) {
    let mut ptab: *mut qjs::JSPropertyEnum = std::ptr::null_mut();
    let mut plen: u32 = 0;
    if qjs::JS_GetOwnPropertyNames(
        ctx,
        &mut ptab,
        &mut plen,
        from,
        qjs::JS_GPN_STRING_MASK as i32,
    ) < 0
    {
        return;
    }
    for i in 0..plen {
        let atom = (*ptab.add(i as usize)).atom;
        let mut alen: usize = 0;
        let name_ptr = qjs::JS_AtomToCStringLen(ctx, &mut alen, atom);
        let key = if !name_ptr.is_null() {
            std::ffi::CStr::from_ptr(name_ptr)
                .to_string_lossy()
                .to_lowercase()
        } else {
            String::new()
        };
        if !name_ptr.is_null() {
            qjs::JS_FreeCString(ctx, name_ptr);
        }
        let val = qjs::JS_GetPropertyInternal(ctx, from, atom, from, 0);
        let ck = CString::new(key).unwrap_or_default();
        qjs::JS_SetPropertyStr(ctx, to, ck.as_ptr(), js_dup_value(val));
        js_free_value(ctx, val);
    }
    qjs::JS_FreePropertyEnum(ctx, ptab, plen);
}

unsafe fn js_resp_headers_to_lines(
    ctx: *mut qjs::JSContext,
    store: qjs::JSValue,
    out: &mut Vec<String>,
) {
    let mut ptab: *mut qjs::JSPropertyEnum = std::ptr::null_mut();
    let mut plen: u32 = 0;
    if qjs::JS_GetOwnPropertyNames(
        ctx,
        &mut ptab,
        &mut plen,
        store,
        qjs::JS_GPN_STRING_MASK as i32,
    ) < 0
    {
        return;
    }
    for i in 0..plen {
        let atom = (*ptab.add(i as usize)).atom;
        let mut alen: usize = 0;
        let name_ptr = qjs::JS_AtomToCStringLen(ctx, &mut alen, atom);
        let key = if !name_ptr.is_null() {
            std::ffi::CStr::from_ptr(name_ptr)
                .to_string_lossy()
                .to_string()
        } else {
            String::new()
        };
        if !name_ptr.is_null() {
            qjs::JS_FreeCString(ctx, name_ptr);
        }
        let val = qjs::JS_GetPropertyInternal(ctx, store, atom, store, 0);
        let val_str = js_string_to_owned(ctx, val);
        js_free_value(ctx, val);
        out.push(format!("{}: {}", key, val_str));
    }
    qjs::JS_FreePropertyEnum(ctx, ptab, plen);
}

/// Build full HTTP/1.1 response with binary body from `HTTP_BODY_ACCUM`.
unsafe fn build_http_response_bytes(
    ctx: *mut qjs::JSContext,
    res: qjs::JSValue,
) -> Result<Vec<u8>, ()> {
    let status_val = qjs::JS_GetPropertyStr(ctx, res, CString::new("statusCode").unwrap().as_ptr());
    let mut status: i32 = 200;
    let _ = qjs::JS_ToInt32(ctx, &mut status as *mut i32, status_val);
    js_free_value(ctx, status_val);

    let body = if let Some(id) = http_res_body_accum_id(ctx, res) {
        HTTP_BODY_ACCUM
            .with(|m| m.borrow_mut().remove(&id))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut header_lines = Vec::new();
    let store = qjs::JS_GetPropertyStr(
        ctx,
        res,
        CString::new("__kawkabRespHeaders").unwrap().as_ptr(),
    );
    if qjs::JS_IsObject(store) {
        js_resp_headers_to_lines(ctx, store, &mut header_lines);
    }
    js_free_value(ctx, store);

    let has_ct = header_lines
        .iter()
        .any(|l| l.to_lowercase().starts_with("content-type:"));
    if !has_ct {
        header_lines.push("Content-Type: text/plain; charset=utf-8".to_string());
    }
    let has_cl = header_lines
        .iter()
        .any(|l| l.to_lowercase().starts_with("content-length:"));
    if !has_cl {
        header_lines.push(format!("Content-Length: {}", body.len()));
    }
    header_lines.push("Connection: close".to_string());

    let status_text = http_status_text(status);
    let mut out = format!("HTTP/1.1 {} {}\r\n", status, status_text).into_bytes();
    for h in header_lines {
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&body);
    Ok(out)
}

unsafe fn http_res_body_accum_id(ctx: *mut qjs::JSContext, res: qjs::JSValue) -> Option<u64> {
    let id_prop = qjs::JS_GetPropertyStr(
        ctx,
        res,
        CString::new("__kawkabBodyAccumId").unwrap().as_ptr(),
    );
    let mut id: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut id as *mut i32, id_prop);
    js_free_value(ctx, id_prop);
    if id > 0 {
        Some(id as u64)
    } else {
        None
    }
}

unsafe fn http_res_body_accum_drop(ctx: *mut qjs::JSContext, res: qjs::JSValue) {
    if let Some(id) = http_res_body_accum_id(ctx, res) {
        HTTP_BODY_ACCUM.with(|m| {
            m.borrow_mut().remove(&id);
        });
    }
}

unsafe fn http_res_append_body_ab(ctx: *mut qjs::JSContext, res: qjs::JSValue, chunk: &[u8]) {
    if chunk.is_empty() {
        return;
    }
    let Some(id) = http_res_body_accum_id(ctx, res) else {
        return;
    };
    HTTP_BODY_ACCUM.with(|m| {
        if let Some(acc) = m.borrow_mut().get_mut(&id) {
            acc.extend_from_slice(chunk);
        }
    });
}

/// Build chunked response status/headers (no body), dropping old length/transfer headers.
unsafe fn build_http_chunked_response_headers(
    ctx: *mut qjs::JSContext,
    res: qjs::JSValue,
) -> Result<String, ()> {
    let status_val = qjs::JS_GetPropertyStr(ctx, res, CString::new("statusCode").unwrap().as_ptr());
    let mut status: i32 = 200;
    let _ = qjs::JS_ToInt32(ctx, &mut status as *mut i32, status_val);
    js_free_value(ctx, status_val);

    let mut header_lines = Vec::new();
    let store = qjs::JS_GetPropertyStr(
        ctx,
        res,
        CString::new("__kawkabRespHeaders").unwrap().as_ptr(),
    );
    if qjs::JS_IsObject(store) {
        js_resp_headers_to_lines(ctx, store, &mut header_lines);
    }
    js_free_value(ctx, store);

    header_lines.retain(|l| {
        let low = l.to_lowercase();
        !low.starts_with("content-length:") && !low.starts_with("transfer-encoding:")
    });

    let has_ct = header_lines
        .iter()
        .any(|l| l.to_lowercase().starts_with("content-type:"));
    if !has_ct {
        header_lines.push("Content-Type: text/plain; charset=utf-8".to_string());
    }
    header_lines.push("Transfer-Encoding: chunked".to_string());
    header_lines.push("Connection: close".to_string());

    let status_text = http_status_text(status);
    let mut out = format!("HTTP/1.1 {} {}\r\n", status, status_text);
    for h in header_lines {
        out.push_str(&h);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    Ok(out)
}

fn http_chunked_frame(data: &[u8]) -> Vec<u8> {
    let mut v = format!("{:x}\r\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\r\n");
    v
}

fn http_wire_try_send(data: Vec<u8>) -> bool {
    HTTP_RESPONSE_WIRE_TX.with(|t| {
        if let Some(tx) = t.borrow().as_ref() {
            tx.send(data).is_ok()
        } else {
            false
        }
    })
}

unsafe fn js_res_bool_prop(ctx: *mut qjs::JSContext, obj: qjs::JSValue, name: &str) -> bool {
    let k = CString::new(name).unwrap();
    let v = qjs::JS_GetPropertyStr(ctx, obj, k.as_ptr());
    let b = qjs::JS_ToBool(ctx, v) != 0;
    js_free_value(ctx, v);
    b
}

unsafe fn js_res_set_bool(ctx: *mut qjs::JSContext, obj: qjs::JSValue, name: &str, val: bool) {
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new(name).unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, val),
    );
}

unsafe fn http_ensure_chunked_headers_sent(ctx: *mut qjs::JSContext, this: qjs::JSValue) -> bool {
    if js_res_bool_prop(ctx, this, "__kawkabHdrSent") {
        return true;
    }
    let Ok(h) = build_http_chunked_response_headers(ctx, this) else {
        return false;
    };
    if !http_wire_try_send(h.into_bytes()) {
        return false;
    }
    js_res_set_bool(ctx, this, "__kawkabHdrSent", true);
    js_res_set_bool(ctx, this, "__kawkabWireChunked", true);
    true
}

unsafe fn new_http_response_object(ctx: *mut qjs::JSContext) -> qjs::JSValue {
    let res = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        res,
        CString::new("statusCode").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, 200),
    );
    let acc_id = NEXT_HTTP_BODY_ACCUM_ID.fetch_add(1, Ordering::Relaxed);
    HTTP_BODY_ACCUM.with(|m| {
        m.borrow_mut().insert(acc_id, Vec::new());
    });
    qjs::JS_SetPropertyStr(
        ctx,
        res,
        CString::new("__kawkabBodyAccumId").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, acc_id as i64),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        res,
        CString::new("__kawkabEnded").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, false),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        res,
        CString::new("__kawkabHdrSent").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, false),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        res,
        CString::new("__kawkabWireChunked").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, false),
    );
    let hdr = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        res,
        CString::new("__kawkabRespHeaders").unwrap().as_ptr(),
        hdr,
    );
    let _ = install_obj_fn(ctx, res, "setHeader", Some(js_http_res_set_header), 2);
    let _ = install_obj_fn(ctx, res, "writeHead", Some(js_http_res_write_head), 3);
    let _ = install_obj_fn(ctx, res, "write", Some(js_http_res_write), 3);
    let _ = install_obj_fn(ctx, res, "end", Some(js_http_res_end), 3);
    res
}

async fn read_http_request_bytes_async(
    stream: &mut tokio::net::TcpStream,
) -> std::io::Result<(
    String,
    String,
    HashMap<String, String>,
    Arc<[u8]>,
    usize,
    usize,
)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(head) = io::http::parse_request_head(&buf)? {
            let content_len = io::http::content_length(&head.headers);
            let header_len = head.header_len;
            let total_needed = header_len + content_len;
            while buf.len() < total_needed {
                let n = stream.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body_len = content_len.min(buf.len().saturating_sub(header_len));
            let method = head.method;
            let path = head.path;
            let headers = head.headers;
            let storage = io::http::arc_buffer(buf);
            return Ok((method, path, headers, storage, header_len, body_len));
        }
        if buf.len() > 256 * 1024 * 1024 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "headers too large",
            ));
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            if buf.is_empty() {
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "empty request",
                ));
            }
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "incomplete headers",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Invoke Node-style `(req, res)` handler (HTTP stack + isolate dispatch).
pub(crate) unsafe fn http_invoke_handler(
    ctx: *mut qjs::JSContext,
    handler: qjs::JSValue,
    this_val: qjs::JSValue,
    req: qjs::JSValue,
    res: qjs::JSValue,
) -> Result<(), JsError> {
    if qjs::JS_IsFunction(ctx, handler) == 0 {
        return Ok(());
    }
    let res_arg = js_dup_value(res);
    let mut args = [req, res_arg];
    let ret = qjs::JS_Call(ctx, handler, this_val, 2, args.as_mut_ptr());
    js_free_value(ctx, res_arg);
    if is_exception(ret) {
        let exc = qjs::JS_GetException(ctx);
        let msg = js_string_to_owned(ctx, exc);
        js_free_value(ctx, exc);
        js_free_value(ctx, ret);
        return Err(JsError::Js(format!("HTTP handler: {msg}")));
    }
    js_free_value(ctx, ret);
    Ok(())
}

unsafe fn install_event_emitter_on_http_request(ctx: *mut qjs::JSContext, req: qjs::JSValue) {
    qjs::JS_SetPropertyStr(
        ctx,
        req,
        CString::new("__kawkabListeners").unwrap().as_ptr(),
        qjs::JS_NewObject(ctx),
    );
    let _ = install_obj_fn(ctx, req, "on", Some(js_events_on), 2);
    let _ = install_obj_fn(ctx, req, "off", Some(js_events_off), 2);
    let _ = install_obj_fn(ctx, req, "emit", Some(js_events_emit), 2);
}

/// Run `createServer` callback and return raw HTTP response bytes.
unsafe fn serve_http_from_parsed(
    ctx: *mut qjs::JSContext,
    server: qjs::JSValue,
    method: String,
    url: String,
    headers: HashMap<String, String>,
    storage: Arc<[u8]>,
    body_off: usize,
    body_len: usize,
) -> Result<Option<Vec<u8>>, String> {
    if body_off.saturating_add(body_len) > storage.len() {
        return Err("http body slice out of bounds".to_string());
    }
    let req = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        req,
        CString::new("method").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &method),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        req,
        CString::new("url").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, &url),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        req,
        CString::new("httpVersion").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, "1.1"),
    );
    let hdr_obj = qjs::JS_NewObject(ctx);
    for (k, v) in &headers {
        let ck = CString::new(k.as_str()).unwrap_or_default();
        qjs::JS_SetPropertyStr(
            ctx,
            hdr_obj,
            ck.as_ptr(),
            qjs_compat::new_string_from_str(ctx, v),
        );
    }
    qjs::JS_SetPropertyStr(ctx, req, CString::new("headers").unwrap().as_ptr(), hdr_obj);
    let body_ab = crate::ffi::arraybuffer_from_arc_slice(ctx, storage, body_off, body_len);
    qjs::JS_SetPropertyStr(ctx, req, CString::new("body").unwrap().as_ptr(), body_ab);

    install_event_emitter_on_http_request(ctx, req);

    let res = new_http_response_object(ctx);

    let handler = qjs::JS_GetPropertyStr(
        ctx,
        server,
        CString::new("__kawkabHandler").unwrap().as_ptr(),
    );
    if let Err(e) = http_invoke_handler(ctx, handler, server, req, res) {
        js_free_value(ctx, handler);
        http_res_body_accum_drop(ctx, res);
        js_free_value(ctx, req);
        js_free_value(ctx, res);
        return Err(e.to_string());
    }
    js_free_value(ctx, handler);

    let wire_active = HTTP_RESPONSE_WIRE_TX.with(|t| t.borrow().is_some());
    if wire_active {
        let chunked = js_res_bool_prop(ctx, res, "__kawkabWireChunked");
        if chunked {
            if !js_http_res_is_ended(ctx, res) {
                let _ = http_wire_try_send(b"0\r\n\r\n".to_vec());
            }
        } else {
            let response = match build_http_response_bytes(ctx, res) {
                Ok(b) => b,
                Err(()) => {
                    http_res_body_accum_drop(ctx, res);
                    js_free_value(ctx, req);
                    js_free_value(ctx, res);
                    return Err("response build failed".to_string());
                }
            };
            let _ = http_wire_try_send(response);
        }
        http_res_body_accum_drop(ctx, res);
        js_free_value(ctx, req);
        js_free_value(ctx, res);
        Ok(None)
    } else {
        let response = match build_http_response_bytes(ctx, res) {
            Ok(b) => b,
            Err(()) => {
                http_res_body_accum_drop(ctx, res);
                js_free_value(ctx, req);
                js_free_value(ctx, res);
                return Err("response build failed".to_string());
            }
        };
        http_res_body_accum_drop(ctx, res);
        js_free_value(ctx, req);
        js_free_value(ctx, res);
        Ok(Some(response))
    }
}

unsafe fn serve_http_request(
    ctx: *mut qjs::JSContext,
    server: qjs::JSValue,
    stream: &mut std::net::TcpStream,
) -> Result<(), String> {
    let (method, url, headers, storage, body_off, body_len) =
        read_http_request_bytes(stream).map_err(|e| e.to_string())?;
    let response = serve_http_from_parsed(
        ctx, server, method, url, headers, storage, body_off, body_len,
    )?;
    let Some(response) = response else {
        return Err("internal: async wire path used without Tokio channel".to_string());
    };
    stream.write_all(&response).map_err(|e| e.to_string())?;
    stream.flush().ok();
    Ok(())
}

pub(crate) async unsafe fn dispatch_http_connection(
    ctx: *mut qjs::JSContext,
    server_id: u64,
    mut stream: tokio::net::TcpStream,
) -> Result<(), JsError> {
    let server = HTTP_LISTEN_REGISTRY.with(|reg| {
        reg.borrow()
            .get(&server_id)
            .map(|e| js_dup_value(e.server_obj))
    });
    let Some(server) = server else {
        return Ok(());
    };
    let (method, url, headers, storage, body_off, body_len) =
        read_http_request_bytes_async(&mut stream)
            .await
            .map_err(JsError::Io)?;

    let stream = Arc::new(tokio::sync::Mutex::new(stream));
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let stream_wr = stream.clone();
    let writer = tokio::spawn(async move {
        while let Some(chunk) = rx.recv().await {
            let mut g = stream_wr.lock().await;
            if g.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let mut g = stream_wr.lock().await;
        let _ = g.flush().await;
        let _ = g.shutdown().await;
    });

    HTTP_RESPONSE_WIRE_TX.with(|c| *c.borrow_mut() = Some(tx.clone()));
    let serve_result = serve_http_from_parsed(
        ctx, server, method, url, headers, storage, body_off, body_len,
    )
    .map_err(JsError::Js);
    HTTP_RESPONSE_WIRE_TX.with(|c| *c.borrow_mut() = None);
    drop(tx);
    writer.await.ok();
    serve_result?;
    js_free_value(ctx, server);
    Ok(())
}

unsafe fn http_stop_listen(ctx: *mut qjs::JSContext, server: qjs::JSValue) {
    let id_prop = qjs::JS_GetPropertyStr(
        ctx,
        server,
        CString::new("__kawkabListenId").unwrap().as_ptr(),
    );
    let mut id: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut id as *mut i32, id_prop);
    js_free_value(ctx, id_prop);
    if id <= 0 {
        return;
    }
    let id = id as u64;
    if let Some(entry) = HTTP_LISTEN_REGISTRY.with(|reg| reg.borrow_mut().remove(&id)) {
        entry.shutdown.notify_waiters();
        js_free_value(ctx, entry.server_obj);
    }
    qjs::JS_SetPropertyStr(
        ctx,
        server,
        CString::new("__kawkabListenId").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, 0),
    );
}

unsafe fn cancel_deferred_timer(ctx: *mut qjs::JSContext, id: u64) {
    if let Some(c) = TIMER_CANCEL_BY_ID.with(|m| m.borrow_mut().remove(&id)) {
        c.store(true, Ordering::Release);
    }
    if let Some(t) = TIMER_REGISTRY.with(|r| r.borrow_mut().remove(&id)) {
        js_free_value(ctx, t.callback);
        js_free_value(ctx, t.this_val);
        for a in t.args {
            js_free_value(ctx, a);
        }
    }
}

pub unsafe fn dispatch_timer_callback(
    ctx: *mut qjs::JSContext,
    timer_id: u64,
) -> Result<(), JsError> {
    let Some(mut t) = TIMER_REGISTRY.with(|r| r.borrow_mut().remove(&timer_id)) else {
        return Ok(());
    };
    if t.cancelled.load(Ordering::Acquire) {
        js_free_value(ctx, t.callback);
        js_free_value(ctx, t.this_val);
        for a in t.args {
            js_free_value(ctx, a);
        }
        TIMER_CANCEL_BY_ID.with(|m| {
            m.borrow_mut().remove(&timer_id);
        });
        return Ok(());
    }

    let repeat_ms = t.repeat_ms;
    let cancel = t.cancelled.clone();

    let next_gen = if repeat_ms.is_some() {
        Some((
            js_dup_value(t.callback),
            js_dup_value(t.this_val),
            t.args.iter().map(|&v| js_dup_value(v)).collect::<Vec<_>>(),
        ))
    } else {
        None
    };

    let ret = qjs::JS_Call(
        ctx,
        t.callback,
        t.this_val,
        t.args.len() as c_int,
        if t.args.is_empty() {
            std::ptr::null_mut()
        } else {
            t.args.as_mut_ptr()
        },
    );

    js_free_value(ctx, t.callback);
    js_free_value(ctx, t.this_val);
    for a in t.args.drain(..) {
        js_free_value(ctx, a);
    }

    if is_exception(ret) {
        js_free_value(ctx, ret);
        if let Some((cb, th, ag)) = next_gen {
            js_free_value(ctx, cb);
            js_free_value(ctx, th);
            for a in ag {
                js_free_value(ctx, a);
            }
        }
        TIMER_CANCEL_BY_ID.with(|m| {
            m.borrow_mut().remove(&timer_id);
        });
        return Err(JsError::Js("timer callback threw".to_string()));
    }
    js_free_value(ctx, ret);

    if let Some(ms) = repeat_ms {
        if cancel.load(Ordering::Acquire) {
            TIMER_CANCEL_BY_ID.with(|m| {
                m.borrow_mut().remove(&timer_id);
            });
            return Ok(());
        }
        if let Some((cb, th, ag)) = next_gen {
            let pending = PendingTimer {
                callback: cb,
                this_val: th,
                args: ag,
                cancelled: cancel.clone(),
                repeat_ms: Some(ms),
            };
            TIMER_REGISTRY.with(|r| r.borrow_mut().insert(timer_id, pending));
            let sender = TASK_SENDER_SLOT.with(|s| s.borrow().clone()).unwrap();
            let rt = Handle::try_current().unwrap();
            rt.spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                if cancel.load(Ordering::Acquire) {
                    return;
                }
                sender.send_timer_callback(timer_id);
            });
        }
    } else {
        PENDING_ASYNC_TIMERS.fetch_sub(1, Ordering::Relaxed);
        TIMER_CANCEL_BY_ID.with(|m| {
            m.borrow_mut().remove(&timer_id);
        });
    }

    Ok(())
}

pub unsafe fn dispatch_dgram_message(
    ctx: *mut qjs::JSContext,
    socket_id: u64,
    payload: Arc<[u8]>,
    host: &str,
    port: u16,
) -> Result<(), JsError> {
    let global = qjs::JS_GetGlobalObject(ctx);
    let store = qjs::JS_GetPropertyStr(
        ctx,
        global,
        CString::new("__kawkabDgramSocketStore").unwrap().as_ptr(),
    );
    js_free_value(ctx, global);
    if qjs::JS_IsUndefined(store) {
        js_free_value(ctx, store);
        return Ok(());
    }
    let sock = if let Ok(id_key) = CString::new(socket_id.to_string()) {
        qjs::JS_GetPropertyStr(ctx, store, id_key.as_ptr())
    } else {
        js_free_value(ctx, store);
        return Ok(());
    };
    js_free_value(ctx, store);
    if qjs::JS_IsUndefined(sock) {
        js_free_value(ctx, sock);
        return Ok(());
    }

    let payload_val = if payload.is_empty() {
        qjs::JS_NewArrayBufferCopy(ctx, std::ptr::null(), 0)
    } else {
        qjs::JS_NewArrayBufferCopy(ctx, payload.as_ptr(), payload.len())
    };
    let global = qjs::JS_GetGlobalObject(ctx);
    let buffer_ctor = qjs::JS_GetPropertyStr(ctx, global, CString::new("Buffer").unwrap().as_ptr());
    let mut msg = payload_val;
    if qjs::JS_IsObject(buffer_ctor) {
        let from_fn =
            qjs::JS_GetPropertyStr(ctx, buffer_ctor, CString::new("from").unwrap().as_ptr());
        if qjs::JS_IsFunction(ctx, from_fn) != 0 {
            let mut argv = [js_dup_value(payload_val)];
            let out = qjs::JS_Call(ctx, from_fn, buffer_ctor, 1, argv.as_mut_ptr());
            js_free_value(ctx, argv[0]);
            if !is_exception(out) {
                js_free_value(ctx, msg);
                msg = out;
            } else {
                js_free_value(ctx, out);
            }
        }
        js_free_value(ctx, from_fn);
    }
    js_free_value(ctx, buffer_ctor);
    js_free_value(ctx, global);

    let rinfo = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        rinfo,
        CString::new("address").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, host),
    );
    let fam = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(_)) => "IPv6",
        _ => "IPv4",
    };
    qjs::JS_SetPropertyStr(
        ctx,
        rinfo,
        CString::new("family").unwrap().as_ptr(),
        qjs_compat::new_string_from_str(ctx, fam),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        rinfo,
        CString::new("port").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, port as i64),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        rinfo,
        CString::new("size").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, payload.len() as i64),
    );
    dgram_emit(ctx, sock, "message", msg, true, rinfo, true);
    js_free_value(ctx, sock);
    js_free_value(ctx, msg);
    js_free_value(ctx, rinfo);
    Ok(())
}

unsafe extern "C" fn js_http_res_set_header(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return js_dup_value(this);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let name = js_string_to_owned(ctx, args[0]).to_lowercase();
    let value = js_string_to_owned(ctx, args[1]);
    let store = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabRespHeaders").unwrap().as_ptr(),
    );
    let ck = CString::new(name).unwrap_or_default();
    qjs::JS_SetPropertyStr(
        ctx,
        store,
        ck.as_ptr(),
        qjs_compat::new_string_from_str(ctx, &value),
    );
    js_free_value(ctx, store);
    js_dup_value(this)
}

unsafe extern "C" fn js_http_res_write_head(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    if args.is_empty() {
        return js_dup_value(this);
    }
    let mut code: i32 = 200;
    let _ = qjs::JS_ToInt32(ctx, &mut code as *mut i32, args[0]);
    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("statusCode").unwrap().as_ptr(),
        qjs_compat::new_int(ctx, code as i64),
    );
    if let Some(v) = args.get(1) {
        if qjs::JS_IsObject(*v) {
            let store = qjs::JS_GetPropertyStr(
                ctx,
                this,
                CString::new("__kawkabRespHeaders").unwrap().as_ptr(),
            );
            merge_js_object_into(ctx, *v, store);
            js_free_value(ctx, store);
        }
    }
    if let Some(v) = args.get(2) {
        if qjs::JS_IsObject(*v) {
            let store = qjs::JS_GetPropertyStr(
                ctx,
                this,
                CString::new("__kawkabRespHeaders").unwrap().as_ptr(),
            );
            merge_js_object_into(ctx, *v, store);
            js_free_value(ctx, store);
        }
    }
    js_dup_value(this)
}

unsafe fn js_http_res_is_ended(ctx: *mut qjs::JSContext, this: qjs::JSValue) -> bool {
    let v = qjs::JS_GetPropertyStr(ctx, this, CString::new("__kawkabEnded").unwrap().as_ptr());
    let b = qjs::JS_ToBool(ctx, v) != 0;
    js_free_value(ctx, v);
    b
}

unsafe fn js_worker_id(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    qjs_compat::new_int(ctx, 0)
}

unsafe extern "C" fn js_http_res_write(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return js_dup_value(this);
    }
    if js_http_res_is_ended(ctx, this) {
        return qjs::JS_ThrowTypeError(
            ctx,
            CString::new("write after end").unwrap_or_default().as_ptr(),
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let wire = HTTP_RESPONSE_WIRE_TX.with(|t| t.borrow().is_some());
    if wire {
        let chunk = buffer::buffer_bytes_from_value(ctx, args[0]);
        if !http_ensure_chunked_headers_sent(ctx, this) {
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new("response write failed")
                    .unwrap_or_default()
                    .as_ptr(),
            );
        }
        if !chunk.is_empty() {
            let frame = http_chunked_frame(&chunk);
            if !http_wire_try_send(frame) {
                return qjs::JS_ThrowTypeError(
                    ctx,
                    CString::new("response write failed")
                        .unwrap_or_default()
                        .as_ptr(),
                );
            }
        }
        return js_dup_value(this);
    }
    let chunk = buffer::buffer_bytes_from_value(ctx, args[0]);
    http_res_append_body_ab(ctx, this, &chunk);
    js_dup_value(this)
}

unsafe extern "C" fn js_net_http_create_server(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let server = qjs::JS_NewObject(ctx);
    if let Some(handler) = args.first().copied() {
        qjs::JS_SetPropertyStr(
            ctx,
            server,
            CString::new("__kawkabHandler").unwrap().as_ptr(),
            js_dup_value(handler),
        );
    }
    qjs::JS_SetPropertyStr(
        ctx,
        server,
        CString::new("__kawkabClosed").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, false),
    );
    let listen = qjs::JS_NewCFunction2(
        ctx,
        Some(js_server_listen),
        CString::new("listen").unwrap().as_ptr(),
        3,
        qjs::JSCFunctionEnum_JS_CFUNC_generic,
        0,
    );
    let close = qjs::JS_NewCFunction2(
        ctx,
        Some(js_server_close),
        CString::new("close").unwrap().as_ptr(),
        1,
        qjs::JSCFunctionEnum_JS_CFUNC_generic,
        0,
    );
    qjs::JS_SetPropertyStr(
        ctx,
        server,
        CString::new("listen").unwrap().as_ptr(),
        listen,
    );
    qjs::JS_SetPropertyStr(ctx, server, CString::new("close").unwrap().as_ptr(), close);
    server
}

unsafe extern "C" fn js_server_listen(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let mut port: i32 = 3000;
    if let Some(v) = args.first() {
        let _ = qjs::JS_ToInt32(ctx, &mut port as *mut i32, *v);
    }
    let mut on_listen: Option<qjs::JSValue> = None;
    if let Some(v) = args.get(1) {
        if qjs::JS_IsFunction(ctx, *v) != 0 {
            on_listen = Some(*v);
        }
    }
    if let Some(v) = args.get(2) {
        if qjs::JS_IsFunction(ctx, *v) != 0 {
            on_listen = Some(*v);
        }
    }

    let addr = format!("127.0.0.1:{port}");

    let use_async = TASK_SENDER_SLOT.with(|s| s.borrow().is_some())
        && tokio::runtime::Handle::try_current().is_ok();

    if use_async {
        let std_listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                return qjs::JS_ThrowTypeError(
                    ctx,
                    CString::new(format!("listen failed: {e}"))
                        .unwrap_or_default()
                        .as_ptr(),
                )
            }
        };
        if let Err(e) = std_listener.set_nonblocking(true) {
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new(format!("listen set_nonblocking failed: {e}"))
                    .unwrap_or_default()
                    .as_ptr(),
            );
        }
        let tokio_listener = match TokioTcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                return qjs::JS_ThrowTypeError(
                    ctx,
                    CString::new(format!("listen tokio handoff failed: {e}"))
                        .unwrap_or_default()
                        .as_ptr(),
                )
            }
        };

        let sender = TASK_SENDER_SLOT.with(|s| s.borrow().clone()).unwrap();
        let rt = tokio::runtime::Handle::try_current().unwrap();

        let listen_id = NEXT_HTTP_LISTEN_ID.fetch_add(1, Ordering::Relaxed);
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let server_dup = js_dup_value(this);
        HTTP_LISTEN_REGISTRY.with(|reg| {
            reg.borrow_mut().insert(
                listen_id,
                HttpListenEntry {
                    server_obj: server_dup,
                    shutdown: shutdown.clone(),
                },
            );
        });
        qjs::JS_SetPropertyStr(
            ctx,
            this,
            CString::new("__kawkabListenId").unwrap().as_ptr(),
            qjs_compat::new_int(ctx, listen_id as i64),
        );

        qjs::JS_SetPropertyStr(
            ctx,
            this,
            CString::new("__kawkabListening").unwrap().as_ptr(),
            qjs::JS_NewBool(ctx, true),
        );
        if let Some(cb) = on_listen {
            let ret = qjs::JS_Call(ctx, cb, this, 0, std::ptr::null_mut());
            js_free_value(ctx, ret);
        }

        let sid = listen_id;
        let shutdown2 = shutdown.clone();
        rt.spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown2.notified() => {
                        break;
                    }
                    accept_res = tokio_listener.accept() => {
                        match accept_res {
                            Ok((stream, _)) => sender.send_http_connection(sid, stream),
                            Err(e) => {
                                let _ = writeln!(std::io::stderr(), "kawkab http: accept error: {e}");
                            }
                        }
                    }
                }
            }
        });

        return js_undefined();
    }

    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new(format!("listen failed: {e}"))
                    .unwrap_or_default()
                    .as_ptr(),
            )
        }
    };
    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabListening").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, true),
    );
    if let Some(cb) = on_listen {
        let ret = qjs::JS_Call(ctx, cb, this, 0, std::ptr::null_mut());
        js_free_value(ctx, ret);
    }

    loop {
        if js_get_server_closed(ctx, this) {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Err(e) = serve_http_request(ctx, this, &mut stream) {
                    let _ = writeln!(std::io::stderr(), "kawkab http: request error: {e}");
                }
            }
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "kawkab http: accept error: {e}");
                break;
            }
        }
    }

    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabListening").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, false),
    );
    js_undefined()
}

unsafe fn patch_eventemitter_prototype(
    ctx: *mut qjs::JSContext,
    global: qjs::JSValue,
) -> Result<(), String> {
    let primed = qjs::JS_GetPropertyStr(
        ctx,
        global,
        CString::new("__kawkabPrimedEvents")
            .map_err(|e| e.to_string())?
            .as_ptr(),
    );
    if !qjs::JS_IsObject(primed) {
        js_free_value(ctx, primed);
        return Err("primed events export missing".into());
    }
    let ctor = qjs::JS_GetPropertyStr(
        ctx,
        primed,
        CString::new("EventEmitter")
            .map_err(|e| e.to_string())?
            .as_ptr(),
    );
    js_free_value(ctx, primed);
    if qjs::JS_IsFunction(ctx, ctor) == 0 {
        js_free_value(ctx, ctor);
        return Err("EventEmitter constructor missing".into());
    }
    let proto = qjs::JS_GetPropertyStr(
        ctx,
        ctor,
        CString::new("prototype")
            .map_err(|e| e.to_string())?
            .as_ptr(),
    );
    install_obj_fn(ctx, proto, "emit", Some(js_eventemitter_emit_shim), 1)?;
    install_obj_fn(
        ctx,
        proto,
        "listenerCount",
        Some(js_eventemitter_listener_count_shim),
        2,
    )?;
    install_obj_fn(
        ctx,
        proto,
        "eventNames",
        Some(js_eventemitter_event_names_shim),
        0,
    )?;
    install_obj_fn(
        ctx,
        ctor,
        "listenerCount",
        Some(js_eventemitter_listener_count_static),
        2,
    )?;
    js_free_value(ctx, proto);
    js_free_value(ctx, ctor);
    Ok(())
}

unsafe fn ee_get_listener_list(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    event_name: &str,
) -> qjs::JSValue {
    let events_key = CString::new("_events").unwrap();
    let events = qjs::JS_GetPropertyStr(ctx, this, events_key.as_ptr());
    if !qjs::JS_IsObject(events) {
        js_free_value(ctx, events);
        return js_null();
    }
    let len_key = CString::new("length").unwrap();
    let len_prop = qjs::JS_GetPropertyStr(ctx, events, len_key.as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len as *mut i32, len_prop);
    js_free_value(ctx, len_prop);
    let key_key = CString::new("key").unwrap();
    let list_key = CString::new("list").unwrap();
    for i in 0..len {
        let bucket = qjs::JS_GetPropertyUint32(ctx, events, i as u32);
        if !qjs::JS_IsObject(bucket) {
            js_free_value(ctx, bucket);
            continue;
        }
        let k = qjs::JS_GetPropertyStr(ctx, bucket, key_key.as_ptr());
        let k_owned = js_string_to_owned(ctx, k);
        js_free_value(ctx, k);
        if k_owned == event_name {
            let list = qjs::JS_GetPropertyStr(ctx, bucket, list_key.as_ptr());
            js_free_value(ctx, bucket);
            js_free_value(ctx, events);
            return list;
        }
        js_free_value(ctx, bucket);
    }
    js_free_value(ctx, events);
    js_null()
}

unsafe extern "C" fn js_eventemitter_emit_shim(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewBool(ctx, false);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let event_name = js_string_to_owned(ctx, args[0]);
    let list = ee_get_listener_list(ctx, this, &event_name);
    if !qjs::JS_IsObject(list) {
        js_free_value(ctx, list);
        return qjs::JS_NewBool(ctx, false);
    }
    let len_prop = qjs::JS_GetPropertyStr(ctx, list, CString::new("length").unwrap().as_ptr());
    let mut list_len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut list_len as *mut i32, len_prop);
    js_free_value(ctx, len_prop);
    if list_len <= 0 {
        js_free_value(ctx, list);
        return qjs::JS_NewBool(ctx, false);
    }
    for ej in 0..list_len {
        let entry = qjs::JS_GetPropertyUint32(ctx, list, ej as u32);
        if !qjs::JS_IsObject(entry) {
            js_free_value(ctx, entry);
            continue;
        }
        let fnv = qjs::JS_GetPropertyStr(ctx, entry, CString::new("fn").unwrap().as_ptr());
        let oncev = qjs::JS_GetPropertyStr(ctx, entry, CString::new("once").unwrap().as_ptr());
        let once_b = qjs::JS_ToBool(ctx, oncev) != 0;
        js_free_value(ctx, oncev);

        if once_b && qjs::JS_IsFunction(ctx, fnv) != 0 {
            let rm =
                qjs::JS_GetPropertyStr(ctx, this, CString::new("removeListener").unwrap().as_ptr());
            if qjs::JS_IsFunction(ctx, rm) != 0 {
                let mut call_argv = [js_dup_value(args[0]), js_dup_value(fnv)];
                let cr = qjs::JS_Call(ctx, rm, this, 2, call_argv.as_mut_ptr());
                for v in call_argv {
                    js_free_value(ctx, v);
                }
                if is_exception(cr) {
                    js_free_value(ctx, fnv);
                    js_free_value(ctx, entry);
                    js_free_value(ctx, list);
                    return cr;
                }
                js_free_value(ctx, cr);
            }
            js_free_value(ctx, rm);
        }

        if qjs::JS_IsFunction(ctx, fnv) != 0 {
            let tail = if argc > 1 {
                let mut v: Vec<qjs::JSValue> = args[1..].iter().map(|x| js_dup_value(*x)).collect();
                let r = qjs::JS_Call(ctx, fnv, this, v.len() as c_int, v.as_mut_ptr());
                for x in v {
                    js_free_value(ctx, x);
                }
                r
            } else {
                qjs::JS_Call(ctx, fnv, this, 0, std::ptr::null_mut())
            };
            if is_exception(tail) {
                js_free_value(ctx, fnv);
                js_free_value(ctx, entry);
                js_free_value(ctx, list);
                return tail;
            }
            js_free_value(ctx, tail);
        }
        js_free_value(ctx, fnv);
        js_free_value(ctx, entry);
    }
    js_free_value(ctx, list);
    qjs::JS_NewBool(ctx, true)
}

unsafe extern "C" fn js_eventemitter_listener_count_shim(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewInt32(ctx, 0);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let event_name = js_string_to_owned(ctx, args[0]);
    let list = ee_get_listener_list(ctx, this, &event_name);
    if !qjs::JS_IsObject(list) {
        js_free_value(ctx, list);
        return qjs::JS_NewInt32(ctx, 0);
    }
    let len_prop = qjs::JS_GetPropertyStr(ctx, list, CString::new("length").unwrap().as_ptr());
    let mut list_len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut list_len as *mut i32, len_prop);
    js_free_value(ctx, len_prop);
    if argc < 2 || qjs::JS_IsFunction(ctx, args[1]) == 0 {
        js_free_value(ctx, list);
        return qjs::JS_NewInt32(ctx, list_len);
    }
    let needle = args[1];
    let mut c: i32 = 0;
    for i in 0..list_len {
        let entry = qjs::JS_GetPropertyUint32(ctx, list, i as u32);
        if !qjs::JS_IsObject(entry) {
            js_free_value(ctx, entry);
            continue;
        }
        let fnv = qjs::JS_GetPropertyStr(ctx, entry, CString::new("fn").unwrap().as_ptr());
        let listener_alt =
            qjs::JS_GetPropertyStr(ctx, entry, CString::new("listener").unwrap().as_ptr());
        if qjs::JS_StrictEq(ctx, fnv, needle) != 0
            || qjs::JS_StrictEq(ctx, listener_alt, needle) != 0
        {
            c += 1;
        }
        js_free_value(ctx, fnv);
        js_free_value(ctx, listener_alt);
        js_free_value(ctx, entry);
    }
    js_free_value(ctx, list);
    qjs::JS_NewInt32(ctx, c)
}

unsafe extern "C" fn js_eventemitter_listener_count_static(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return qjs::JS_NewInt32(ctx, 0);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let inst_lc = qjs::JS_GetPropertyStr(
        ctx,
        args[0],
        CString::new("listenerCount").unwrap().as_ptr(),
    );
    if qjs::JS_IsFunction(ctx, inst_lc) == 0 {
        js_free_value(ctx, inst_lc);
        return qjs::JS_NewInt32(ctx, 0);
    }
    let mut call_argv = [js_dup_value(args[1])];
    let r = qjs::JS_Call(ctx, inst_lc, args[0], 1, call_argv.as_mut_ptr());
    js_free_value(ctx, call_argv[0]);
    js_free_value(ctx, inst_lc);
    r
}

unsafe extern "C" fn js_eventemitter_event_names_shim(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let events = qjs::JS_GetPropertyStr(ctx, this, CString::new("_events").unwrap().as_ptr());
    if !qjs::JS_IsObject(events) {
        js_free_value(ctx, events);
        return qjs::JS_NewArray(ctx);
    }
    let len_prop = qjs::JS_GetPropertyStr(ctx, events, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len as *mut i32, len_prop);
    js_free_value(ctx, len_prop);
    let out = qjs::JS_NewArray(ctx);
    let mut oi: u32 = 0;
    for i in 0..len {
        let bucket = qjs::JS_GetPropertyUint32(ctx, events, i as u32);
        if !qjs::JS_IsObject(bucket) {
            js_free_value(ctx, bucket);
            continue;
        }
        let key = qjs::JS_GetPropertyStr(ctx, bucket, CString::new("key").unwrap().as_ptr());
        qjs::JS_SetPropertyUint32(ctx, out, oi, js_dup_value(key));
        js_free_value(ctx, key);
        oi += 1;
        js_free_value(ctx, bucket);
    }
    js_free_value(ctx, events);
    out
}

unsafe extern "C" fn js_events_emitter_ctor(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let obj = qjs::JS_NewObject(ctx);
    qjs::JS_SetPropertyStr(
        ctx,
        obj,
        CString::new("__kawkabListeners").unwrap().as_ptr(),
        qjs::JS_NewObject(ctx),
    );
    let _ = install_obj_fn(ctx, obj, "on", Some(js_events_on), 2);
    let _ = install_obj_fn(ctx, obj, "addListener", Some(js_events_on), 2);
    let _ = install_obj_fn(ctx, obj, "once", Some(js_events_on), 2);
    let _ = install_obj_fn(ctx, obj, "prependListener", Some(js_events_on), 2);
    let _ = install_obj_fn(ctx, obj, "prependOnceListener", Some(js_events_on), 2);
    let _ = install_obj_fn(ctx, obj, "off", Some(js_events_off), 2);
    let _ = install_obj_fn(ctx, obj, "removeListener", Some(js_events_off), 2);
    let _ = install_obj_fn(
        ctx,
        obj,
        "removeAllListeners",
        Some(js_events_remove_all_listeners),
        1,
    );
    let _ = install_obj_fn(ctx, obj, "listenerCount", Some(js_events_listener_count), 1);
    let _ = install_obj_fn(ctx, obj, "listeners", Some(js_events_listeners), 1);
    let _ = install_obj_fn(ctx, obj, "eventNames", Some(js_events_event_names), 0);
    let _ = install_obj_fn(ctx, obj, "emit", Some(js_events_emit), 2);
    obj
}

unsafe extern "C" fn js_events_on(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return js_dup_value(this);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    if qjs::JS_IsFunction(ctx, args[1]) == 0 {
        return js_dup_value(this);
    }
    let event_name = js_string_to_owned(ctx, args[0]);
    let event_key = CString::new(event_name).unwrap_or_default();
    let listeners_key = CString::new("__kawkabListeners").unwrap();
    let listeners = qjs::JS_GetPropertyStr(ctx, this, listeners_key.as_ptr());
    if !qjs::JS_IsObject(listeners) {
        js_free_value(ctx, listeners);
        return js_dup_value(this);
    }
    let arr = qjs::JS_GetPropertyStr(ctx, listeners, event_key.as_ptr());
    let arr = if qjs::JS_IsObject(arr) {
        arr
    } else {
        js_free_value(ctx, arr);
        let created = qjs::JS_NewArray(ctx);
        qjs::JS_SetPropertyStr(ctx, listeners, event_key.as_ptr(), js_dup_value(created));
        created
    };
    let len_prop = qjs::JS_GetPropertyStr(ctx, arr, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len as *mut i32, len_prop);
    js_free_value(ctx, len_prop);
    qjs::JS_SetPropertyUint32(ctx, arr, len as u32, js_dup_value(args[1]));
    js_free_value(ctx, arr);
    js_free_value(ctx, listeners);
    js_dup_value(this)
}

unsafe extern "C" fn js_events_off(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return js_dup_value(this);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let event_name = js_string_to_owned(ctx, args[0]);
    let event_key = CString::new(event_name).unwrap_or_default();
    let listeners = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabListeners").unwrap().as_ptr(),
    );
    if !qjs::JS_IsObject(listeners) {
        js_free_value(ctx, listeners);
        return js_dup_value(this);
    }
    let arr = qjs::JS_GetPropertyStr(ctx, listeners, event_key.as_ptr());
    if !qjs::JS_IsObject(arr) {
        js_free_value(ctx, arr);
        js_free_value(ctx, listeners);
        return js_dup_value(this);
    }
    let filtered = qjs::JS_NewArray(ctx);
    let len_prop = qjs::JS_GetPropertyStr(ctx, arr, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len as *mut i32, len_prop);
    js_free_value(ctx, len_prop);
    let mut out_idx: u32 = 0;
    for i in 0..len {
        let cb = qjs::JS_GetPropertyUint32(ctx, arr, i as u32);
        let same = qjs::JS_StrictEq(ctx, cb, args[1]) != 0;
        if !same {
            qjs::JS_SetPropertyUint32(ctx, filtered, out_idx, cb);
            out_idx += 1;
        } else {
            js_free_value(ctx, cb);
        }
    }
    qjs::JS_SetPropertyStr(ctx, listeners, event_key.as_ptr(), filtered);
    js_free_value(ctx, arr);
    js_free_value(ctx, listeners);
    js_dup_value(this)
}

unsafe extern "C" fn js_events_emit(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewBool(ctx, false);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let event_name = js_string_to_owned(ctx, args[0]);
    let event_key = CString::new(event_name).unwrap_or_default();
    let listeners_key = CString::new("__kawkabListeners").unwrap();
    let listeners = qjs::JS_GetPropertyStr(ctx, this, listeners_key.as_ptr());
    if !qjs::JS_IsObject(listeners) {
        js_free_value(ctx, listeners);
        return qjs::JS_NewBool(ctx, false);
    }
    let arr = qjs::JS_GetPropertyStr(ctx, listeners, event_key.as_ptr());
    if !qjs::JS_IsObject(arr) {
        js_free_value(ctx, arr);
        js_free_value(ctx, listeners);
        return qjs::JS_NewBool(ctx, false);
    }
    let len_prop = qjs::JS_GetPropertyStr(ctx, arr, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len as *mut i32, len_prop);
    js_free_value(ctx, len_prop);
    for i in 0..len {
        let cb = qjs::JS_GetPropertyUint32(ctx, arr, i as u32);
        if qjs::JS_IsFunction(ctx, cb) != 0 {
            let mut owned_args: Vec<qjs::JSValue> = Vec::new();
            if argc > 1 {
                for j in 1..(argc as usize) {
                    owned_args.push(js_dup_value(args[j]));
                }
            }
            let ret = qjs::JS_Call(
                ctx,
                cb,
                this,
                owned_args.len() as c_int,
                if owned_args.is_empty() {
                    std::ptr::null_mut()
                } else {
                    owned_args.as_mut_ptr()
                },
            );
            for v in owned_args {
                js_free_value(ctx, v);
            }
            if !is_exception(ret) {
                js_free_value(ctx, ret);
            }
        }
        js_free_value(ctx, cb);
    }
    js_free_value(ctx, arr);
    js_free_value(ctx, listeners);
    qjs::JS_NewBool(ctx, len > 0)
}

unsafe extern "C" fn js_events_remove_all_listeners(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        qjs::JS_SetPropertyStr(
            ctx,
            this,
            CString::new("__kawkabListeners").unwrap().as_ptr(),
            qjs::JS_NewObject(ctx),
        );
        return js_dup_value(this);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let event_name = js_string_to_owned(ctx, args[0]);
    let listeners = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabListeners").unwrap().as_ptr(),
    );
    if qjs::JS_IsObject(listeners) {
        qjs::JS_SetPropertyStr(
            ctx,
            listeners,
            CString::new(event_name).unwrap_or_default().as_ptr(),
            qjs::JS_NewArray(ctx),
        );
    }
    js_free_value(ctx, listeners);
    js_dup_value(this)
}

unsafe extern "C" fn js_events_listener_count(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs_compat::new_int(ctx, 0);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let event_name = js_string_to_owned(ctx, args[0]);
    let listeners = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabListeners").unwrap().as_ptr(),
    );
    if !qjs::JS_IsObject(listeners) {
        js_free_value(ctx, listeners);
        return qjs_compat::new_int(ctx, 0);
    }
    let arr = qjs::JS_GetPropertyStr(
        ctx,
        listeners,
        CString::new(event_name).unwrap_or_default().as_ptr(),
    );
    js_free_value(ctx, listeners);
    if !qjs::JS_IsObject(arr) {
        js_free_value(ctx, arr);
        return qjs_compat::new_int(ctx, 0);
    }
    let len_prop = qjs::JS_GetPropertyStr(ctx, arr, CString::new("length").unwrap().as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len as *mut i32, len_prop);
    js_free_value(ctx, len_prop);
    js_free_value(ctx, arr);
    qjs_compat::new_int(ctx, len as i64)
}

unsafe extern "C" fn js_events_listeners(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_NewArray(ctx);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let event_name = js_string_to_owned(ctx, args[0]);
    let listeners = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabListeners").unwrap().as_ptr(),
    );
    if !qjs::JS_IsObject(listeners) {
        js_free_value(ctx, listeners);
        return qjs::JS_NewArray(ctx);
    }
    let arr = qjs::JS_GetPropertyStr(
        ctx,
        listeners,
        CString::new(event_name).unwrap_or_default().as_ptr(),
    );
    js_free_value(ctx, listeners);
    if qjs::JS_IsObject(arr) {
        return arr;
    }
    js_free_value(ctx, arr);
    qjs::JS_NewArray(ctx)
}

unsafe extern "C" fn js_events_event_names(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    _argc: c_int,
    _argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let listeners = qjs::JS_GetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabListeners").unwrap().as_ptr(),
    );
    if !qjs::JS_IsObject(listeners) {
        js_free_value(ctx, listeners);
        return qjs::JS_NewArray(ctx);
    }
    let script = "(function(o){ return Object.keys(o || {}); })";
    let file = CString::new("kawkab:events-keys").unwrap();
    let fn_val = qjs_compat::eval(
        ctx,
        script.as_ptr() as *const i8,
        script.len(),
        file.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    );
    if is_exception(fn_val) {
        js_free_value(ctx, listeners);
        return qjs::JS_NewArray(ctx);
    }
    let mut args = [listeners];
    let out = qjs::JS_Call(ctx, fn_val, js_undefined(), 1, args.as_mut_ptr());
    js_free_value(ctx, fn_val);
    if is_exception(out) {
        js_free_value(ctx, out);
        return qjs::JS_NewArray(ctx);
    }
    out
}

unsafe extern "C" fn js_server_close(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    http_stop_listen(ctx, this);
    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabListening").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, false),
    );
    qjs::JS_SetPropertyStr(
        ctx,
        this,
        CString::new("__kawkabClosed").unwrap().as_ptr(),
        qjs::JS_NewBool(ctx, true),
    );
    if argc > 0 {
        let args = std::slice::from_raw_parts(argv, argc as usize);
        if qjs::JS_IsFunction(ctx, args[0]) != 0 {
            let ret = qjs::JS_Call(ctx, args[0], this, 0, std::ptr::null_mut());
            js_free_value(ctx, ret);
        }
    }
    js_undefined()
}

unsafe extern "C" fn js_http_res_end(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let ended_key = CString::new("__kawkabEnded").unwrap();

    if js_http_res_is_ended(ctx, this) {
        return js_undefined();
    }

    let wire = HTTP_RESPONSE_WIRE_TX.with(|t| t.borrow().is_some());
    if wire && js_res_bool_prop(ctx, this, "__kawkabWireChunked") {
        let extra = args
            .first()
            .copied()
            .map(|v| buffer::buffer_bytes_from_value(ctx, v))
            .unwrap_or_default();
        if !http_ensure_chunked_headers_sent(ctx, this) {
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new("response end failed")
                    .unwrap_or_default()
                    .as_ptr(),
            );
        }
        if !extra.is_empty() {
            let frame = http_chunked_frame(&extra);
            if !http_wire_try_send(frame) {
                return qjs::JS_ThrowTypeError(
                    ctx,
                    CString::new("response end failed")
                        .unwrap_or_default()
                        .as_ptr(),
                );
            }
        }
        if !http_wire_try_send(b"0\r\n\r\n".to_vec()) {
            return qjs::JS_ThrowTypeError(
                ctx,
                CString::new("response end failed")
                    .unwrap_or_default()
                    .as_ptr(),
            );
        }
        qjs::JS_SetPropertyStr(ctx, this, ended_key.as_ptr(), qjs::JS_NewBool(ctx, true));
        return js_undefined();
    }

    let extra = args
        .first()
        .copied()
        .map(|v| buffer::buffer_bytes_from_value(ctx, v))
        .unwrap_or_default();
    http_res_append_body_ab(ctx, this, &extra);

    qjs::JS_SetPropertyStr(ctx, this, ended_key.as_ptr(), qjs::JS_NewBool(ctx, true));
    js_undefined()
}

fn is_exception(value: qjs::JSValue) -> bool {
    value.tag == qjs::JS_TAG_EXCEPTION as i64
}

fn js_undefined() -> qjs::JSValue {
    qjs::JSValue {
        u: qjs::JSValueUnion { int32: 0 },
        tag: qjs::JS_TAG_UNDEFINED as i64,
    }
}

fn js_null() -> qjs::JSValue {
    qjs::JSValue {
        u: qjs::JSValueUnion { int32: 0 },
        tag: qjs::JS_TAG_NULL as i64,
    }
}

fn normalize_path_like_node(input: &Path) -> String {
    let is_abs = input.is_absolute();
    let mut parts: Vec<String> = Vec::new();
    for comp in input.components() {
        match comp {
            std::path::Component::RootDir => {}
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !parts.is_empty() && parts.last().is_some_and(|v| v != "..") {
                    parts.pop();
                } else if !is_abs {
                    parts.push("..".to_string());
                }
            }
            std::path::Component::Normal(v) => parts.push(v.to_string_lossy().to_string()),
            std::path::Component::Prefix(v) => {
                parts.push(v.as_os_str().to_string_lossy().to_string())
            }
        }
    }
    let mut out = String::new();
    if is_abs {
        out.push('/');
    }
    out.push_str(&parts.join("/"));
    if out.is_empty() {
        ".".to_string()
    } else {
        out
    }
}
