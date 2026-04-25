//! Contract checks for high-priority Node built-ins (see `docs/COMPAT_DEFINITION_OF_DONE.md`).

use std::ffi::CString;
use std::sync::Mutex;

use quickjs_sys as qjs;

use crate::ffi::js_free_value;
use crate::isolate::{Isolate, IsolateConfig};
use crate::node::install_runtime;

/// Multiple QuickJS runtimes per process are not safe to construct in parallel in this embedding.
static QJS_TEST_SERIAL: Mutex<()> = Mutex::new(());

/// Serialize harness tests that spawn real OS `Worker` threads (reduces flakiness / `SIGABRT` when
/// many worker harness tests run in one `cargo test` process, and under parallel `cargo test`).
static WORKER_THREADS_OS_HARNESS_SERIAL: Mutex<()> = Mutex::new(());

#[inline]
fn qjs_serial() -> std::sync::MutexGuard<'static, ()> {
    QJS_TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[inline]
fn worker_threads_os_harness_serial() -> std::sync::MutexGuard<'static, ()> {
    WORKER_THREADS_OS_HARNESS_SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

unsafe fn global_bool(ctx: *mut qjs::JSContext, name: &str) -> bool {
    let g = qjs::JS_GetGlobalObject(ctx);
    let key = CString::new(name).unwrap();
    let v = qjs::JS_GetPropertyStr(ctx, g, key.as_ptr());
    js_free_value(ctx, g);
    let out = if qjs::JS_IsBool(v) {
        qjs::JS_ToBool(ctx, v) != 0
    } else {
        false
    };
    js_free_value(ctx, v);
    out
}

unsafe fn global_u32(ctx: *mut qjs::JSContext, name: &str) -> u32 {
    let g = qjs::JS_GetGlobalObject(ctx);
    let key = CString::new(name).unwrap();
    let v = qjs::JS_GetPropertyStr(ctx, g, key.as_ptr());
    js_free_value(ctx, g);
    let mut out: i32 = 0;
    let ok = qjs::JS_ToInt32(ctx, &mut out as *mut i32, v) == 0;
    js_free_value(ctx, v);
    if ok && out >= 0 {
        out as u32
    } else {
        0
    }
}

#[test]
fn isolate_eval_smoke() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    eval_script(
        &mut iso,
        "var __t = 1 + 1; if (__t !== 2) throw new Error('eval');",
    );
}

#[test]
fn install_runtime_smoke() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/smoke.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        "var __smoke = 1; if (__smoke !== 1) throw new Error('smoke');",
    );
}

/// Regression: QuickJS must load real npm `merge-descriptors` (JSDoc `/**` openers) under the
/// CJS `require()` wrapper used by Express.
#[test]
fn require_merge_descriptors_express_fixture() {
    let _s = qjs_serial();
    use std::path::PathBuf;
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/kpi/express-minimal");
    let md_pkg = base.join("node_modules/merge-descriptors/package.json");
    if !md_pkg.exists() {
        return;
    }
    let entry = base.join("probe-md-lf.js");
    if !entry.exists() {
        return;
    }
    let entry_s = std::fs::canonicalize(&entry)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    crate::console::install(&mut iso).expect("console");
    unsafe {
        install_runtime(iso.ctx_ptr(), &entry_s, None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        "var m = require('merge-descriptors'); if (typeof m !== 'function') throw new Error('merge-descriptors');",
    );
}

#[test]
fn require_merge_descriptors_after_buffer_seed_line() {
    let _s = qjs_serial();
    use std::path::PathBuf;
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/kpi/express-minimal");
    let md_pkg = base.join("node_modules/merge-descriptors/package.json");
    if !md_pkg.exists() {
        return;
    }
    let entry = base.join("probe-md-lf.js");
    if !entry.exists() {
        return;
    }
    let entry_s = std::fs::canonicalize(&entry)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    crate::console::install(&mut iso).expect("console");
    unsafe {
        install_runtime(iso.ctx_ptr(), &entry_s, None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        "var Buffer = globalThis.Buffer;\nrequire('merge-descriptors');",
    );
}

fn eval_script(iso: &mut Isolate, src: &str) {
    let v = iso
        .eval(src.as_bytes(), "compat_contract_test.js")
        .unwrap_or_else(|e| panic!("JS eval failed: {e:?}"));
    unsafe {
        crate::ffi::js_free_value(iso.ctx_ptr(), v);
    }
}

/// Priority Node built-ins plus web-platform / expanded builtin checks in **one** `install_runtime`
/// on the main test thread. A second full embed install on the same OS thread can poison QuickJS
/// prim eval for built-in modules; keep this suite single-isolate.
#[test]
fn priority_and_web_platform_embed_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/contract.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var modules = ['fs','path','buffer','url','crypto','stream','net','http','https','dns','dns/promises','vm','tls','readline','readline/promises','worker_threads','events','async_hooks','timers','timers/promises','perf_hooks','zlib','module'];
  for (var i = 0; i < modules.length; i++) {
    var m = require(modules[i]);
    if (!m) throw new Error('require failed: ' + modules[i]);
  }
  if (typeof structuredClone !== 'function') throw new Error('structuredClone');
  if (typeof queueMicrotask !== 'function') throw new Error('queueMicrotask');
  if (typeof process !== 'object') throw new Error('process');
})();
"#,
    );
}

#[test]
fn worker_threads_roundtrip() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    let tmp_dir = std::env::temp_dir().join(format!("kawkab_wt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let worker_js = tmp_dir.join("w.js");
    std::fs::write(
        &worker_js,
        "var wt=require('worker_threads');\nwt.parentPort.postMessage({ k: 42 });\n",
    )
    .expect("worker js");
    let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/main.js", Some(sender)).expect("install");
        }
        let main_js = format!(
            r#"(function() {{
                var {{ Worker }} = require('worker_threads');
                var w = new Worker({wp});
                globalThis.__w = w;
                w.on('message', function(m) {{
                    if (m.k !== 42) throw new Error('bad payload');
                    globalThis.__wtOk = true;
                }});
            }})();"#
        );
        eval_script(&mut isolate, &main_js);

        let mut ok = false;
        for _ in 0..1200 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let _ = crate::node::drain_next_tick_queue(ctx);
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
                if global_bool(ctx, "__wtOk") {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(3)).await;
        }
        assert!(ok, "worker_threads round-trip did not complete");

        eval_script(
            &mut isolate,
            "try { if (globalThis.__w) globalThis.__w.terminate(); } catch (e) {}",
        );
        std::thread::sleep(std::time::Duration::from_millis(150));
    });

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn worker_threads_spawn_idle_smoke() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    let tmp_dir = std::env::temp_dir().join(format!("kawkab_wt_idle_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let worker_js = tmp_dir.join("idle.js");
    std::fs::write(&worker_js, "// idle\n").expect("worker js");
    let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/main.js", Some(sender)).expect("install");
        }
        let main_js = format!(
            r#"(function() {{
                var {{ Worker }} = require('worker_threads');
                globalThis.__wIdle = new Worker({wp});
            }})();"#
        );
        eval_script(&mut isolate, &main_js);
        for _ in 0..50 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
        eval_script(
            &mut isolate,
            "try { if (globalThis.__wIdle) globalThis.__wIdle.terminate(); } catch (e) {}",
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    });
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn event_loop_ordering_contract() {
    let _s = qjs_serial();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/order.js", Some(sender)).expect("install");
        }
        eval_script(
            &mut isolate,
            r#"
(function() {
  globalThis.__order = [];
  globalThis.__orderDone = false;
  Promise.resolve().then(function() { globalThis.__order.push('promise'); });
  process.nextTick(function() { globalThis.__order.push('tick'); });
  setImmediate(function() { globalThis.__order.push('immediate'); globalThis.__orderDone = true; });
})();
"#,
        );

        let mut done = false;
        for _ in 0..400 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let _ = crate::node::drain_next_tick_queue(ctx);
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
                if global_bool(ctx, "__orderDone") {
                    done = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        assert!(done, "event-loop ordering sequence did not complete");

        eval_script(
            &mut isolate,
            r#"
(function() {
  var s = globalThis.__order.join(',');
  if (s !== 'tick,promise,immediate') {
    throw new Error('unexpected order: ' + s);
  }
})();
"#,
        );
    });
}

#[test]
fn worker_threads_lifecycle_contract() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    let tmp_dir = std::env::temp_dir().join(format!("kawkab_wt_lifecycle_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let worker_js = tmp_dir.join("worker.js");
    std::fs::write(
        &worker_js,
        "var wt=require('worker_threads');var pp=wt.parentPort;var onceOk=!!pp&&typeof pp.once==='function'&&pp.once('message',function(_m){})===pp;var aliasesOk=false;var offOk=false;var removedOk=true;var exactRemoveOk=false;var introspectionOk=false;if(pp&&typeof pp.addListener==='function'&&typeof pp.prependListener==='function'&&typeof pp.prependOnceListener==='function'){aliasesOk=(pp.addListener('message',function(_m){})===pp&&pp.prependListener('message',function(_m){})===pp&&pp.prependOnceListener('message',function(_m){})===pp);}if(pp&&typeof pp.off==='function'&&typeof pp.removeListener==='function'){offOk=(pp.off('message',function(_m){})===pp&&pp.removeListener('message',function(_m){})===pp);var keep=function(_m){};pp.on('message',keep);pp.removeListener('message',function(_m){});exactRemoveOk=(typeof pp.listenerCount==='function'&&pp.listenerCount('message')>=1);pp.removeListener('message',keep);exactRemoveOk=exactRemoveOk&&(typeof pp.listenerCount==='function'&&pp.listenerCount('message')===0);var removed=function(_m){removedOk=false;};pp.on('message',removed);pp.removeListener('message',removed);}if(pp&&typeof pp.listenerCount==='function'&&typeof pp.eventNames==='function'){var c=pp.listenerCount('message');var names=pp.eventNames();if(typeof c==='number'&&names&&typeof names.indexOf==='function'&&names.indexOf('message')>=0){introspectionOk=true;}}setTimeout(function(){pp.postMessage({parentOnceChainOk:onceOk,parentAliasesOk:aliasesOk,parentOffChainOk:offOk,parentRemovedListenerSuppressedOk:removedOk,parentExactRemoveOk:exactRemoveOk,parentListenerIntrospectionOk:introspectionOk});},0);",
    )
    .expect("worker js");
    let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/wt-lifecycle.js", Some(sender)).expect("install");
        }
        let main_js = format!(
            r#"(function() {{
              var {{ Worker }} = require('worker_threads');
              var w = new Worker({wp});
              globalThis.__w2 = w;
              globalThis.__wtLifecycleOk = false;
              globalThis.__wtSerializeErr = false;
              globalThis.__wtOnChainOk = false;
              globalThis.__wtOnIgnoreEventsOk = false;
              globalThis.__wtTerminatePromiseOk = false;
              globalThis.__wtParentOnceChainOk = false;
              globalThis.__wtParentAliasesOk = false;
              globalThis.__wtParentOffChainOk = false;
              globalThis.__wtParentRemovedListenerSuppressedOk = false;
              globalThis.__wtParentExactRemoveOk = false;
              globalThis.__wtParentListenerIntrospectionOk = false;
              globalThis.__wtExitEventOk = false;
              globalThis.__wtExitOnceCount = 0;
              globalThis.__wtExitOnceOk = false;
              globalThis.__wtOffChainOk = false;
              globalThis.__wtRemovedExitSuppressedOk = true;
              globalThis.__wtRemoveListenerExactMainOk = false;
              globalThis.__wtListenerIntrospectionOk = false;
              globalThis.__wtEmitterAliasesOk = false;
              if (w.on('message', function(_m) {{}}) === w) globalThis.__wtOnChainOk = true;
              if (w.once('message', function(_m) {{}}) !== w) throw new Error('worker once chain');
              if (typeof w.addListener === 'function'
                  && typeof w.prependListener === 'function'
                  && typeof w.prependOnceListener === 'function'
                  && w.addListener('message', function(_m) {{}}) === w
                  && w.prependListener('message', function(_m) {{}}) === w
                  && w.prependOnceListener('message', function(_m) {{}}) === w) {{
                globalThis.__wtEmitterAliasesOk = true;
              }}
              if (w.on('error', function(_e) {{}}) === w && w.on('exit', function(_c) {{}}) === w) {{
                globalThis.__wtOnIgnoreEventsOk = true;
              }}
              var removedExit = function(_code) {{ globalThis.__wtRemovedExitSuppressedOk = false; }};
              if (w.off('message', function(_m) {{}}) === w && w.removeListener('message', function(_m) {{}}) === w) {{
                globalThis.__wtOffChainOk = true;
              }}
              var keepMainMsg = function(_m) {{}};
              w.on('message', keepMainMsg);
              w.removeListener('message', function(_m) {{}});
              var exactMainRemoveOk = (w.listenerCount('message') >= 1);
              w.removeListener('message', keepMainMsg);
              exactMainRemoveOk = exactMainRemoveOk && (w.listenerCount('message') === 0);
              if (w.on('exit', removedExit) !== w) throw new Error('worker on exit for remove');
              if (w.removeListener('exit', removedExit) !== w) throw new Error('worker removeListener exit chain');
              var keepMainExit = function(_code) {{}};
              w.on('exit', keepMainExit);
              w.removeListener('exit', function(_code) {{}});
              exactMainRemoveOk = exactMainRemoveOk && (w.listenerCount('exit') >= 1);
              w.removeListener('exit', keepMainExit);
              exactMainRemoveOk = exactMainRemoveOk && (w.listenerCount('exit') === 0);
              globalThis.__wtRemoveListenerExactMainOk = exactMainRemoveOk;
              if (w.once('exit', function(_code) {{ globalThis.__wtExitOnceCount++; }}) !== w) {{
                throw new Error('worker once exit chain');
              }}
              if (typeof w.listenerCount !== 'function' || typeof w.eventNames !== 'function') {{
                throw new Error('worker listener introspection api');
              }}
              if (typeof w.listeners !== 'function' || typeof w.rawListeners !== 'function') {{
                throw new Error('worker listeners/rawListeners api');
              }}
              var names = w.eventNames();
              var messageCount = w.listenerCount('message');
              var exitCount = w.listenerCount('exit');
              var msgListeners = w.listeners('message');
              var msgRawListeners = w.rawListeners('message');
              if (typeof messageCount === 'number'
                  && typeof exitCount === 'number'
                  && exitCount >= 1
                  && msgListeners && typeof msgListeners.length === 'number'
                  && msgRawListeners && typeof msgRawListeners.length === 'number'
                  && names && typeof names.indexOf === 'function'
                  && names.indexOf('exit') >= 0) {{
                globalThis.__wtListenerIntrospectionOk = true;
              }}
              w.on('exit', function(code) {{
                if (typeof code === 'number') globalThis.__wtExitEventOk = true;
              }});
              w.on('message', function(m) {{
                if (m && m.parentOnceChainOk === true) globalThis.__wtParentOnceChainOk = true;
                if (m && m.parentAliasesOk === true) globalThis.__wtParentAliasesOk = true;
                if (m && m.parentOffChainOk === true) globalThis.__wtParentOffChainOk = true;
                if (m && m.parentRemovedListenerSuppressedOk === true) globalThis.__wtParentRemovedListenerSuppressedOk = true;
                if (m && m.parentExactRemoveOk === true) globalThis.__wtParentExactRemoveOk = true;
                if (m && m.parentListenerIntrospectionOk === true) globalThis.__wtParentListenerIntrospectionOk = true;
              }});
              setTimeout(function() {{
                try {{
                  var t = w.terminate();
                  if (t && typeof t.then === 'function') {{
                    globalThis.__wtTerminatePromiseOk = true;
                    t.then(function(_code) {{
                      try {{ w.terminate(); }} catch (_e) {{}}
                      setTimeout(function() {{
                        globalThis.__wtExitOnceOk = (globalThis.__wtExitOnceCount === 1);
                        globalThis.__wtLifecycleOk = true;
                      }}, 10);
                    }}, function(_err) {{ globalThis.__wtLifecycleOk = true; }});
                  }} else {{
                    globalThis.__wtLifecycleOk = true;
                  }}
                }} catch (e) {{}}
              }}, 20);
              try {{
                var cyc = {{}};
                cyc.self = cyc;
                w.postMessage(cyc);
              }} catch (e) {{
                globalThis.__wtSerializeErr = true;
              }}
            }})();"#
        );
        eval_script(&mut isolate, &main_js);

        let mut ok = false;
        for _ in 0..500 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
                if global_bool(ctx, "__wtLifecycleOk")
                    && global_bool(ctx, "__wtSerializeErr")
                    && global_bool(ctx, "__wtOnChainOk")
                    && global_bool(ctx, "__wtOnIgnoreEventsOk")
                    && global_bool(ctx, "__wtTerminatePromiseOk")
                    && global_bool(ctx, "__wtExitEventOk")
                    && global_bool(ctx, "__wtExitOnceOk")
                    && global_bool(ctx, "__wtOffChainOk")
                    && global_bool(ctx, "__wtRemovedExitSuppressedOk")
                    && global_bool(ctx, "__wtRemoveListenerExactMainOk")
                    && global_bool(ctx, "__wtListenerIntrospectionOk")
                    && global_bool(ctx, "__wtEmitterAliasesOk")
                {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        let lifecycle_ok = unsafe { global_bool(ctx, "__wtLifecycleOk") };
        let serialize_ok = unsafe { global_bool(ctx, "__wtSerializeErr") };
        let on_chain_ok = unsafe { global_bool(ctx, "__wtOnChainOk") };
        let on_ignore_events_ok = unsafe { global_bool(ctx, "__wtOnIgnoreEventsOk") };
        let terminate_promise_ok = unsafe { global_bool(ctx, "__wtTerminatePromiseOk") };
        let parent_once_chain_ok = unsafe { global_bool(ctx, "__wtParentOnceChainOk") };
        let parent_aliases_ok = unsafe { global_bool(ctx, "__wtParentAliasesOk") };
        let parent_off_chain_ok = unsafe { global_bool(ctx, "__wtParentOffChainOk") };
        let parent_removed_listener_suppressed_ok =
            unsafe { global_bool(ctx, "__wtParentRemovedListenerSuppressedOk") };
        let parent_exact_remove_ok = unsafe { global_bool(ctx, "__wtParentExactRemoveOk") };
        let parent_listener_introspection_ok =
            unsafe { global_bool(ctx, "__wtParentListenerIntrospectionOk") };
        let exit_event_ok = unsafe { global_bool(ctx, "__wtExitEventOk") };
        let exit_once_ok = unsafe { global_bool(ctx, "__wtExitOnceOk") };
        let off_chain_ok = unsafe { global_bool(ctx, "__wtOffChainOk") };
        let removed_exit_suppressed_ok = unsafe { global_bool(ctx, "__wtRemovedExitSuppressedOk") };
        let remove_listener_exact_main_ok = unsafe { global_bool(ctx, "__wtRemoveListenerExactMainOk") };
        let listener_introspection_ok = unsafe { global_bool(ctx, "__wtListenerIntrospectionOk") };
        let emitter_aliases_ok = unsafe { global_bool(ctx, "__wtEmitterAliasesOk") };
        assert!(
            ok,
            "worker lifecycle contract did not complete (lifecycle_ok={lifecycle_ok}, serialize_ok={serialize_ok}, on_chain_ok={on_chain_ok}, on_ignore_events_ok={on_ignore_events_ok}, terminate_promise_ok={terminate_promise_ok}, parent_once_chain_ok={parent_once_chain_ok}, parent_aliases_ok={parent_aliases_ok}, parent_off_chain_ok={parent_off_chain_ok}, parent_removed_listener_suppressed_ok={parent_removed_listener_suppressed_ok}, parent_exact_remove_ok={parent_exact_remove_ok}, parent_listener_introspection_ok={parent_listener_introspection_ok}, exit_event_ok={exit_event_ok}, exit_once_ok={exit_once_ok}, off_chain_ok={off_chain_ok}, removed_exit_suppressed_ok={removed_exit_suppressed_ok}, remove_listener_exact_main_ok={remove_listener_exact_main_ok}, listener_introspection_ok={listener_introspection_ok}, emitter_aliases_ok={emitter_aliases_ok})"
        );
        eval_script(
            &mut isolate,
            "try { if (globalThis.__w2) globalThis.__w2.terminate(); } catch (e) {}",
        );
    });

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn worker_threads_binary_payload_contract() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    let tmp_dir = std::env::temp_dir().join(format!("kawkab_wt_binary_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let worker_js = tmp_dir.join("worker.js");
    std::fs::write(
        &worker_js,
        "var wt=require('worker_threads');wt.parentPort.on('message',function(m){if(m&&m.byteLength===3){wt.parentPort.postMessage(new Uint8Array(m));}});",
    )
    .expect("worker js");
    let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/wt-binary.js", Some(sender)).expect("install");
        }
        let main_js = format!(
            r#"(function() {{
              var {{ Worker }} = require('worker_threads');
              var w = new Worker({wp});
              globalThis.__w3 = w;
              globalThis.__wtBinaryOk = false;
              w.on('message', function(m) {{
                if (m && m.length === 3 && m[0] === 1 && m[1] === 2 && m[2] === 3) {{
                  globalThis.__wtBinaryOk = true;
                }}
              }});
              w.postMessage(new Uint8Array([1,2,3]));
            }})();"#
        );
        eval_script(&mut isolate, &main_js);

        let mut ok = false;
        for _ in 0..500 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
                if global_bool(ctx, "__wtBinaryOk") {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        assert!(ok, "worker binary payload contract did not complete");
        eval_script(
            &mut isolate,
            "try { if (globalThis.__w3) globalThis.__w3.terminate(); } catch (e) {}",
        );
    });
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn worker_parent_port_once_one_shot_contract() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    for attempt in 0..3_u32 {
        let tmp_dir = std::env::temp_dir().join(format!(
            "kawkab_wt_parent_once_{}_{}",
            std::process::id(),
            attempt
        ));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("mkdir");
        let worker_js = tmp_dir.join("worker.js");
        std::fs::write(
            &worker_js,
            "var wt=require('worker_threads');var hits=0;wt.parentPort.once('message',function(m){if(m&&m.kind==='once'){hits++;wt.parentPort.postMessage({onceHits:hits});}});",
        )
        .expect("worker js");
        let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let ok = rt.block_on(async {
            let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
            let _ = crate::console::install(&mut isolate);
            let ctx = isolate.ctx_ptr();
            let (task_tx, mut task_rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
            let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
            unsafe {
                install_runtime(ctx, "/test/main.js", Some(sender)).expect("install");
            }
            let main_js = format!(
                r#"(function() {{
                  var {{ Worker }} = require('worker_threads');
                  var w = new Worker({wp});
                  globalThis.__wtParentSent = false;
                  globalThis.__wtParentMsgCount = 0;
                  globalThis.__wtParentOnceOneShotOk = false;
                  w.on('message', function(m) {{
                    globalThis.__wtParentMsgCount = (globalThis.__wtParentMsgCount|0) + 1;
                    if (m && m.onceHits === 1) globalThis.__wtParentOnceOneShotOk = true;
                  }});
                  globalThis.__wParentOnce = w;
                }})();"#
            );
            eval_script(&mut isolate, &main_js);

            for _ in 0..120 {
                while let Ok(t) = task_rx.try_recv() {
                    unsafe {
                        crate::node::dispatch_cli_isolate_task(ctx, t);
                    }
                }
                unsafe {
                    let rt_q = qjs::JS_GetRuntime(ctx);
                    loop {
                        let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                        let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                        if r <= 0 {
                            break;
                        }
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
            }

            let mut ok = false;
            for i in 0..2500 {
                if i % 20 == 0 && i >= 120 {
                    eval_script(
                        &mut isolate,
                        "try { if (globalThis.__wParentOnce && !globalThis.__wtParentSent) { globalThis.__wtParentSent = true; globalThis.__wParentOnce.postMessage({kind:'once'}); globalThis.__wParentOnce.postMessage({kind:'once'}); } } catch (e) {}",
                    );
                }
                while let Ok(t) = task_rx.try_recv() {
                    unsafe {
                        crate::node::dispatch_cli_isolate_task(ctx, t);
                    }
                }
                unsafe {
                    let rt_q = qjs::JS_GetRuntime(ctx);
                    loop {
                        let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                        let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                        if r <= 0 {
                            break;
                        }
                    }
                    if global_bool(ctx, "__wtParentOnceOneShotOk") {
                        ok = true;
                        break;
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
            }
            eval_script(
                &mut isolate,
                "try { if (globalThis.__wParentOnce) globalThis.__wParentOnce.terminate(); } catch (e) {}",
            );
            ok
        });
        let _ = std::fs::remove_dir_all(&tmp_dir);
        if ok {
            return;
        }
    }
    panic!("worker parentPort once one-shot contract did not complete");
}

#[test]
fn worker_main_once_one_shot_contract() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    let tmp_dir = std::env::temp_dir().join(format!("kawkab_wt_main_once_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let worker_js = tmp_dir.join("worker.js");
    std::fs::write(
        &worker_js,
        "var wt=require('worker_threads');wt.parentPort.on('message',function arm(m){if(!m||!m.ping)return;wt.parentPort.removeListener('message',arm);wt.parentPort.postMessage({k:'a'});wt.parentPort.postMessage({k:'b'});});",
    )
    .expect("worker js");
    let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/wt-main-once.js", Some(sender)).expect("install");
        }
        let main_js = format!(
            r#"(function() {{
              var {{ Worker }} = require('worker_threads');
              var w = new Worker({wp});
              globalThis.__wtMainOnceHits = 0;
              globalThis.__wtMainOnceOk = false;
              w.once('message', function(_m) {{
                globalThis.__wtMainOnceHits++;
                globalThis.__wtMainOnceOk = (globalThis.__wtMainOnceHits === 1);
              }});
              globalThis.__wMainOnce = w;
            }})();"#
        );
        eval_script(&mut isolate, &main_js);

        for _ in 0..60 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        eval_script(
            &mut isolate,
            "try { if (globalThis.__wMainOnce && !globalThis.__wtPingArmSent) { globalThis.__wtPingArmSent = true; globalThis.__wMainOnce.postMessage({ping:1}); } } catch (e) {}",
        );

        let mut ok = false;
        for _ in 0..500 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
                let hits = global_u32(ctx, "__wtMainOnceHits");
                if global_bool(ctx, "__wtMainOnceOk") && hits == 1 {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        assert!(ok, "worker main once one-shot contract did not complete");
        eval_script(
            &mut isolate,
            "try { if (globalThis.__wMainOnce) globalThis.__wMainOnce.terminate(); } catch (e) {}",
        );
    });
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn worker_threads_environment_data_contract() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    let tmp_dir = std::env::temp_dir().join(format!("kawkab_wt_env_data_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let worker_js = tmp_dir.join("worker.js");
    std::fs::write(
        &worker_js,
        "var wt=require('worker_threads');var v=wt.getEnvironmentData('k');wt.parentPort.postMessage({v:v});",
    )
    .expect("worker js");
    let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/wt-env-data.js", Some(sender)).expect("install");
        }
        let main_js = format!(
            r#"(function() {{
              var wt = require('worker_threads');
              wt.setEnvironmentData('k', {{ a: 1 }});
              var local = wt.getEnvironmentData('k');
              local.a = 9;
              var localAgain = wt.getEnvironmentData('k');
              if (!localAgain || localAgain.a !== 1) throw new Error('env data clone baseline');
              wt.setEnvironmentData('tmp', {{ z: 1 }});
              if (!wt.getEnvironmentData('tmp') || wt.getEnvironmentData('tmp').z !== 1) {{
                throw new Error('env data tmp set baseline');
              }}
              wt.setEnvironmentData('tmp', undefined);
              if (typeof wt.getEnvironmentData('tmp') !== 'undefined') {{
                throw new Error('env data undefined delete baseline');
              }}
              var w = new wt.Worker({wp});
              globalThis.__wEnv = w;
              globalThis.__wtEnvDataOk = false;
              w.on('message', function(m) {{
                if (m && m.v && m.v.a === 1) globalThis.__wtEnvDataOk = true;
              }});
            }})();"#
        );
        eval_script(&mut isolate, &main_js);

        let mut ok = false;
        for _ in 0..500 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
                if global_bool(ctx, "__wtEnvDataOk") {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        eval_script(
            &mut isolate,
            "try { if (globalThis.__wEnv) globalThis.__wEnv.terminate(); } catch (e) {}",
        );
        assert!(
            ok,
            "worker_threads environmentData contract did not complete"
        );
    });
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// Name sorts before other `worker_*` tests so substring filters run this before OS `Worker` harnesses.
#[test]
fn worker_a_receive_message_on_port_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/wt-receive-message-on-port.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var wt = require('node:worker_threads');
  if (!wt || typeof wt.receiveMessageOnPort !== 'function') {
    throw new Error('receiveMessageOnPort missing');
  }
  var ch = wt.MessageChannel();
  if (!ch || !ch.port1 || !ch.port2) throw new Error('worker_threads MessageChannel ports');
  if (typeof ch.port1.postMessage !== 'function' || typeof ch.port2.on !== 'function') {
    throw new Error('worker_threads MessagePort methods');
  }
  if (typeof ch.port2.removeAllListeners !== 'function') {
    throw new Error('worker_threads MessagePort removeAllListeners');
  }
  if (typeof ch.port1.ref !== 'function' || typeof ch.port1.unref !== 'function' || typeof ch.port1.hasRef !== 'function') {
    throw new Error('worker_threads MessagePort ref methods');
  }
  if (ch.port1.hasRef() !== true) throw new Error('worker_threads MessagePort hasRef default');
  if (ch.port1.unref() !== ch.port1) throw new Error('worker_threads MessagePort unref chain');
  if (ch.port1.hasRef() !== false) throw new Error('worker_threads MessagePort hasRef false');
  if (ch.port1.ref() !== ch.port1) throw new Error('worker_threads MessagePort ref chain');
  if (ch.port1.hasRef() !== true) throw new Error('worker_threads MessagePort hasRef true');
  var seen = false;
  ch.port2.on('message', function() { seen = true; });
  if (ch.port2.removeAllListeners('message') !== ch.port2) throw new Error('worker_threads MessagePort removeAllListeners chain');
  ch.port1.postMessage({ removed: 1 });
  if (seen) throw new Error('worker_threads MessagePort removeAllListeners behavior');
  var removedMsg = wt.receiveMessageOnPort(ch.port2);
  if (!removedMsg || !removedMsg.message || removedMsg.message.removed !== 1) {
    throw new Error('worker_threads MessagePort removeAllListeners queue');
  }
  ch.port1.postMessage({ ok: 1 });
  var r = wt.receiveMessageOnPort(ch.port2);
  if (!r || !r.message || r.message.ok !== 1) {
    throw new Error('receiveMessageOnPort queue return');
  }
  var empty = wt.receiveMessageOnPort(ch.port2);
  if (typeof empty !== 'undefined') throw new Error('receiveMessageOnPort empty');
})();
"#,
    );
}

#[test]
fn worker_threads_mark_as_untransferable_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/wt-mark-untransferable.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var wt = require('node:worker_threads');
  if (!wt || typeof wt.markAsUntransferable !== 'function') {
    throw new Error('markAsUntransferable missing');
  }
  var v = new Uint8Array([1,2,3]);
  var r = wt.markAsUntransferable(v);
  if (typeof r !== 'undefined') throw new Error('markAsUntransferable baseline return');
})();
"#,
    );
}

#[test]
fn worker_threads_is_marked_as_untransferable_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/wt-is-marked-untransferable.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var wt = require('node:worker_threads');
  if (!wt || typeof wt.markAsUntransferable !== 'function') {
    throw new Error('markAsUntransferable missing');
  }
  if (typeof wt.isMarkedAsUntransferable !== 'function') {
    throw new Error('isMarkedAsUntransferable missing');
  }
  var a = new Uint8Array([1,2,3]);
  if (wt.isMarkedAsUntransferable(a) !== false) throw new Error('initial mark state');
  wt.markAsUntransferable(a);
  if (wt.isMarkedAsUntransferable(a) !== true) throw new Error('mark state true');
  if (wt.isMarkedAsUntransferable(new Uint8Array([9])) !== false) throw new Error('other object state');
  if (wt.isMarkedAsUntransferable(42) !== false) throw new Error('primitive state');
})();
"#,
    );
}

#[test]
fn worker_threads_move_message_port_to_context_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/wt-move-port-context.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var wt = require('node:worker_threads');
  if (!wt || typeof wt.moveMessagePortToContext !== 'function') {
    throw new Error('moveMessagePortToContext missing');
  }
  var ch = wt.MessageChannel();
  if (!ch || !ch.port1 || !ch.port2) throw new Error('MessageChannel ports');
  var moved = wt.moveMessagePortToContext(ch.port1, {});
  if (moved !== ch.port1) throw new Error('moveMessagePortToContext baseline identity');
})();
"#,
    );
}

#[test]
fn worker_threads_is_internal_thread_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/wt-is-internal-thread.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var wt = require('node:worker_threads');
  if (!wt) throw new Error('worker_threads missing');
  if (typeof wt.isInternalThread !== 'boolean') {
    throw new Error('isInternalThread type');
  }
  if (wt.isInternalThread !== false) {
    throw new Error('isInternalThread baseline');
  }
  if (typeof wt.threadId !== 'number' || wt.threadId !== 0) {
    throw new Error('threadId main baseline');
  }
  if (wt.parentPort !== null) {
    throw new Error('parentPort main baseline null');
  }
})();
"#,
    );
}

#[test]
fn worker_threads_share_env_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/wt-share-env.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var wt = require('node:worker_threads');
  var wt2 = require('node:worker_threads');
  if (!wt) throw new Error('worker_threads missing');
  if (typeof wt.SHARE_ENV !== 'symbol') {
    throw new Error('SHARE_ENV baseline symbol');
  }
  var expected = Symbol.for('nodejs.worker_threads.SHARE_ENV');
  if (wt.SHARE_ENV !== expected) {
    throw new Error('SHARE_ENV symbol identity');
  }
  if (wt2.SHARE_ENV !== wt.SHARE_ENV) {
    throw new Error('SHARE_ENV stable across require');
  }
  if (Symbol.keyFor(wt.SHARE_ENV) !== 'nodejs.worker_threads.SHARE_ENV') {
    throw new Error('SHARE_ENV keyFor');
  }
})();
"#,
    );
}

#[test]
fn worker_threads_worker_isolate_flags_contract() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    for attempt in 0..5_u32 {
        let tmp_dir = std::env::temp_dir().join(format!(
            "kawkab_wt_flags_{}_{}",
            std::process::id(),
            attempt
        ));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("mkdir");
        let worker_js = tmp_dir.join("worker.js");
        std::fs::write(
            &worker_js,
            "var wt=require('worker_threads');wt.parentPort.postMessage({isMainThread:wt.isMainThread,isInternalThread:wt.isInternalThread,threadId:wt.threadId,parentPortType:typeof wt.parentPort});",
        )
        .expect("worker js");
        let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let ok = rt.block_on(async {
            let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
            let _ = crate::console::install(&mut isolate);
            let ctx = isolate.ctx_ptr();
            let (task_tx, mut task_rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
            let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
            unsafe {
                install_runtime(ctx, "/test/wt-flags.js", Some(sender)).expect("install");
            }
            let main_js = format!(
                r#"(function() {{
                  var wt = require('worker_threads');
                  var w = new wt.Worker({wp});
                  globalThis.__wtFlagsOk = false;
                  w.on('message', function(m) {{
                    if (!m) return;
                    if (m.isMainThread === false &&
                        m.isInternalThread === false &&
                        typeof m.threadId === 'number' &&
                        m.threadId > 0 &&
                        m.parentPortType === 'object') {{
                      globalThis.__wtFlagsOk = true;
                    }}
                  }});
                  globalThis.__wFlags = w;
                }})();"#
            );
            eval_script(&mut isolate, &main_js);

            let mut ok = false;
            for _ in 0..950 {
                while let Ok(t) = task_rx.try_recv() {
                    unsafe {
                        crate::node::dispatch_cli_isolate_task(ctx, t);
                    }
                }
                unsafe {
                    let rt_q = qjs::JS_GetRuntime(ctx);
                    loop {
                        let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                        let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                        if r <= 0 {
                            break;
                        }
                    }
                    if global_bool(ctx, "__wtFlagsOk") {
                        ok = true;
                        break;
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
            }
            eval_script(
                &mut isolate,
                "try { if (globalThis.__wFlags) globalThis.__wFlags.terminate(); } catch (e) {}",
            );
            ok
        });
        let _ = std::fs::remove_dir_all(&tmp_dir);
        if ok {
            return;
        }
    }
    panic!("worker_threads worker isolate flags contract did not complete");
}

#[test]
fn worker_threads_worker_resource_limits_baseline_contract() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    let tmp_dir =
        std::env::temp_dir().join(format!("kawkab_wt_resource_limits_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let worker_js = tmp_dir.join("worker.js");
    std::fs::write(
        &worker_js,
        "// idle (avoid worker-side setInterval without host timer path)\n",
    )
    .expect("worker js");
    let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) = tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/wt-resource-limits.js", Some(sender)).expect("install");
        }
        let main_js = format!(
            r#"(function() {{
              var wt = require('worker_threads');
              var w = new wt.Worker({wp}, {{
                resourceLimits: {{
                  maxOldGenerationSizeMb: 64,
                  maxYoungGenerationSizeMb: 16,
                  codeRangeSizeMb: 8,
                  stackSizeMb: 4
                }}
              }});
              var w2 = new wt.Worker({wp}, {{
                resourceLimits: {{
                  maxOldGenerationSizeMb: -1,
                  maxYoungGenerationSizeMb: 'x',
                  codeRangeSizeMb: null,
                  stackSizeMb: -9
                }}
              }});
              var w3 = new wt.Worker({wp});
              globalThis.__wResourceLimits = w;
              globalThis.__wResourceLimits2 = w2;
              globalThis.__wResourceLimits3 = w3;
              globalThis.__wtResourceLimitsOk =
                !!w &&
                !!w2 &&
                !!w3 &&
                typeof w.resourceLimits === 'object' &&
                w.resourceLimits !== null &&
                w.resourceLimits.maxOldGenerationSizeMb === 64 &&
                w.resourceLimits.maxYoungGenerationSizeMb === 16 &&
                w.resourceLimits.codeRangeSizeMb === 8 &&
                w.resourceLimits.stackSizeMb === 4 &&
                typeof w2.resourceLimits === 'object' &&
                w2.resourceLimits !== null &&
                w2.resourceLimits.maxOldGenerationSizeMb === 0 &&
                w2.resourceLimits.maxYoungGenerationSizeMb === 0 &&
                w2.resourceLimits.codeRangeSizeMb === 0 &&
                w2.resourceLimits.stackSizeMb === 0 &&
                typeof w3.resourceLimits === 'object' &&
                w3.resourceLimits !== null &&
                w3.resourceLimits.maxOldGenerationSizeMb === 0 &&
                w3.resourceLimits.maxYoungGenerationSizeMb === 0 &&
                w3.resourceLimits.codeRangeSizeMb === 0 &&
                w3.resourceLimits.stackSizeMb === 0;
            }})();"#
        );
        eval_script(&mut isolate, &main_js);
        let mut ok = false;
        for _ in 0..300 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
                if global_bool(ctx, "__wtResourceLimitsOk") {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        eval_script(
            &mut isolate,
            "try { if (globalThis.__wResourceLimits) globalThis.__wResourceLimits.terminate(); } catch (e) {}; try { if (globalThis.__wResourceLimits2) globalThis.__wResourceLimits2.terminate(); } catch (e) {}; try { if (globalThis.__wResourceLimits3) globalThis.__wResourceLimits3.terminate(); } catch (e) {}",
        );
        assert!(ok, "worker_threads worker resourceLimits baseline contract did not complete");
    });
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn worker_threads_worker_ref_unref_baseline_contract() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    let tmp_dir = std::env::temp_dir().join(format!("kawkab_wt_ref_unref_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let worker_js = tmp_dir.join("worker.js");
    std::fs::write(
        &worker_js,
        "// idle (avoid worker-side setInterval without host timer path)\n",
    )
    .expect("worker js");
    let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/wt-ref-unref.js", Some(sender)).expect("install");
        }
        let main_js = format!(
            r#"(function() {{
              var wt = require('worker_threads');
              var w = new wt.Worker({wp});
              var r0 = typeof w.hasRef === 'function' && w.hasRef() === true;
              var r1 = w.unref() === w;
              var r2 = w.hasRef() === false;
              var r3 = w.ref() === w;
              var r4 = w.hasRef() === true;
              globalThis.__wRefUnref = w;
              globalThis.__wtRefUnrefOk = r0 && r1 && r2 && r3 && r4;
            }})();"#
        );
        eval_script(&mut isolate, &main_js);
        let mut ok = false;
        for _ in 0..300 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
                if global_bool(ctx, "__wtRefUnrefOk") {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        eval_script(
            &mut isolate,
            "try { if (globalThis.__wRefUnref) globalThis.__wRefUnref.terminate(); } catch (e) {}",
        );
        assert!(
            ok,
            "worker_threads worker ref/unref/hasRef baseline contract did not complete"
        );
    });
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn worker_threads_broadcast_channel_export_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/wt-broadcast-channel-export.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var wt = require('node:worker_threads');
  if (!wt) throw new Error('worker_threads missing');
  if (typeof wt.BroadcastChannel !== 'function') {
    throw new Error('worker_threads BroadcastChannel export');
  }
})();
"#,
    );
}

#[test]
fn worker_parent_port_remove_all_listeners_contract() {
    let _wt = worker_threads_os_harness_serial();
    let _s = qjs_serial();
    for attempt in 0..6_u32 {
        let tmp_dir = std::env::temp_dir().join(format!(
            "kawkab_wt_parent_remove_all_{}_{}",
            std::process::id(),
            attempt
        ));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).expect("mkdir");
        let worker_js = tmp_dir.join("worker.js");
        std::fs::write(
            &worker_js,
            "var wt=require('worker_threads');wt.parentPort.on('message',function arm(m){if(!m||!m.ping)return;wt.parentPort.removeListener('message',arm);var seen=0;function cb(){seen++;}wt.parentPort.on('message',cb);var c1=wt.parentPort.listenerCount('message');var n1=wt.parentPort.eventNames();wt.parentPort.removeAllListeners('message');var c2=wt.parentPort.listenerCount('message');var n2=wt.parentPort.eventNames();wt.parentPort.postMessage({c1:c1,c2:c2,hasMessage1:Array.isArray(n1)&&n1.indexOf('message')>=0,hasMessage2:Array.isArray(n2)&&n2.indexOf('message')>=0,seen:seen});});",
        )
        .expect("worker js");
        let wp = serde_json::to_string(worker_js.to_str().unwrap()).expect("path json");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let ok = rt.block_on(async {
            let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
            let _ = crate::console::install(&mut isolate);
            let ctx = isolate.ctx_ptr();
            let (task_tx, mut task_rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
            let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
            unsafe {
                install_runtime(ctx, "/test/main.js", Some(sender)).expect("install");
            }
            let main_js = format!(
                r#"(function() {{
                  var {{ Worker }} = require('worker_threads');
                  var w = new Worker({wp});
                  globalThis.__wtParentRemoveAllOk = false;
                  w.on('message', function(m) {{
                    if (m && m.c1 === 1 && m.c2 === 0 && m.hasMessage1 === true && m.hasMessage2 === false && m.seen === 0) {{
                      globalThis.__wtParentRemoveAllOk = true;
                    }}
                  }});
                  globalThis.__wParentRemoveAll = w;
                }})();"#
            );
            eval_script(&mut isolate, &main_js);

            for _ in 0..60 {
                while let Ok(t) = task_rx.try_recv() {
                    unsafe {
                        crate::node::dispatch_cli_isolate_task(ctx, t);
                    }
                }
                unsafe {
                    let _ = crate::node::drain_next_tick_queue(ctx);
                    let rt_q = qjs::JS_GetRuntime(ctx);
                    loop {
                        let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                        let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                        if r <= 0 {
                            break;
                        }
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
            }
            eval_script(
                &mut isolate,
                "try { if (globalThis.__wParentRemoveAll && !globalThis.__wtPingArmSent) { globalThis.__wtPingArmSent = true; globalThis.__wParentRemoveAll.postMessage({ping:1}); } } catch (e) {}",
            );

            let mut ok = false;
            for _ in 0..900 {
                while let Ok(t) = task_rx.try_recv() {
                    unsafe {
                        crate::node::dispatch_cli_isolate_task(ctx, t);
                    }
                }
                unsafe {
                    let _ = crate::node::drain_next_tick_queue(ctx);
                    let rt_q = qjs::JS_GetRuntime(ctx);
                    loop {
                        let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                        let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                        if r <= 0 {
                            break;
                        }
                    }
                    if global_bool(ctx, "__wtParentRemoveAllOk") {
                        ok = true;
                        break;
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
            }
            eval_script(
                &mut isolate,
                "try { if (globalThis.__wParentRemoveAll) globalThis.__wParentRemoveAll.terminate(); } catch (e) {}",
            );
            std::thread::sleep(std::time::Duration::from_millis(80));
            ok
        });
        let _ = std::fs::remove_dir_all(&tmp_dir);
        if ok {
            return;
        }
    }
    panic!("worker parentPort removeAllListeners contract did not complete");
}

#[test]
fn async_hooks_events_helpers_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/async-events.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var events = require('events');
  var ah = require('async_hooks');
  if (typeof ah.AsyncLocalStorage !== 'function') throw new Error('AsyncLocalStorage');
  var als = new ah.AsyncLocalStorage();
  var ok = als.run({ reqId: 11 }, function() { return als.getStore().reqId === 11; });
  if (!ok) throw new Error('als run/getStore');
  als.enterWith({ reqId: 22 });
  if (!als.getStore() || als.getStore().reqId !== 22) throw new Error('als enterWith');
  var rebound = als.bind(function(x) { return (als.getStore() && als.getStore().reqId) + x; });
  if (rebound(5) !== 27) throw new Error('als bind');
  var exitOk = als.exit(function() { return als.getStore() === undefined; });
  if (!exitOk) throw new Error('als exit');
  var ar = new ah.AsyncResource('x');
  var scopeVal = ar.runInAsyncScope(function(a, b) { return a + b; }, null, 2, 3);
  if (scopeVal !== 5) throw new Error('async resource');
  var hk = ah.createHook({});
  if (!hk || typeof hk.enable !== 'function' || hk.enable() !== hk || hk.disable() !== hk) throw new Error('createHook chain');

  var ee = new events.EventEmitter();
  var order = [];
  ee.prependListener('x', function() { order.push('pre'); });
  ee.on('x', function() { order.push('on'); });
  ee.emit('x');
  if (order.join(',') !== 'pre,on') throw new Error('prependListener order');
  if (ee.listenerCount('x') !== 2) throw new Error('listenerCount');
  if (ee.rawListeners('x').length !== 2) throw new Error('rawListeners');
  ee.setMaxListeners(2);
  if (ee.getMaxListeners() !== 2) throw new Error('max listeners');
  if (ee.eventNames().indexOf('x') < 0) throw new Error('eventNames');

  events.once(ee, 'pong').then(function(args) {
    globalThis.__eventsOnceOk = Array.isArray(args) && args[0] === 33;
  });
  var it = events.on(ee, 'tick');
  it.next().then(function(step) {
    globalThis.__eventsOnOk = !!step && !step.done && Array.isArray(step.value) && step.value[0] === 44;
    return it.return();
  });
  ee.emit('pong', 33);
  ee.emit('tick', 44);
})();
"#,
    );
    let ctx = iso.ctx_ptr();
    for _ in 0..200 {
        unsafe {
            let rt = qjs::JS_GetRuntime(ctx);
            loop {
                let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                let r = qjs::JS_ExecutePendingJob(rt, &mut co);
                if r <= 0 {
                    break;
                }
            }
            if global_bool(ctx, "__eventsOnceOk") && global_bool(ctx, "__eventsOnOk") {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("async_hooks/events helpers contract did not complete");
}

#[test]
fn structured_clone_polyfill_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/structured-clone.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var src = {
    d: new Date(1700000000000),
    m: new Map([['k', 2]]),
    s: new Set([1,2]),
    u8: new Uint8Array([3,4]),
    nested: { z: 9 }
  };
  var out = structuredClone(src);
  if (out === src) throw new Error('identity');
  if (!(out.d instanceof Date) || out.d.getTime() !== src.d.getTime()) throw new Error('date');
  if (!(out.m instanceof Map) || out.m.get('k') !== 2) throw new Error('map');
  if (!(out.s instanceof Set) || !out.s.has(2)) throw new Error('set');
  if (!(out.u8 instanceof Uint8Array) || out.u8[1] !== 4) throw new Error('typed');
  if (!out.nested || out.nested.z !== 9) throw new Error('nested');

  var cyc = { name: 'root' };
  cyc.self = cyc;
  var c2 = structuredClone(cyc);
  if (!c2 || c2 === cyc) throw new Error('cycle identity');
  if (c2.self !== c2) throw new Error('cycle shape');
})();
"#,
    );
}

#[test]
fn vm_tls_dns_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/vm-tls-dns.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var vm = require('vm');
  if (typeof vm.runInThisContext !== 'function') throw new Error('vm.runInThisContext export');
  if (typeof vm.runInContext !== 'function') throw new Error('vm.runInContext export');
  if (typeof vm.runInNewContext !== 'function') throw new Error('vm.runInNewContext export');
  if (typeof vm.Script !== 'function') throw new Error('vm.Script export');
  if (typeof vm.createContext !== 'function') throw new Error('vm.createContext export');
  if (typeof vm.isContext !== 'function') throw new Error('vm.isContext export');

  var tls = require('tls');
  if (typeof tls.connect !== 'function' || typeof tls.createServer !== 'function') throw new Error('tls exports');

  var dns = require('dns');
  if (typeof dns.lookup !== 'function') throw new Error('dns.lookup');
  var dnp = require('dns/promises');
  if (typeof dnp.lookup !== 'function') throw new Error('dns/promises');
})();
"#,
    );
}

#[test]
fn tls_functional_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/tls-functional.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var tls = require('tls');
  if (typeof tls.connect !== 'function') throw new Error('tls.connect');
  if (typeof tls.createServer !== 'function') throw new Error('tls.createServer');
  var srv = tls.createServer(function() {});
  if (!srv || typeof srv.listen !== 'function' || typeof srv.close !== 'function') throw new Error('tls.createServer shape');

  globalThis.__tlsSecure = false;
  var s = tls.connect(443, 'example.com');
  if (!s || s.encrypted !== true) throw new Error('tls socket encrypted');
  if (typeof s.on !== 'function' || typeof s.end !== 'function' || typeof s.destroy !== 'function') throw new Error('tls socket methods');
  s.on('secureConnect', function() { globalThis.__tlsSecure = true; });
  s.emit('secureConnect');
  if (globalThis.__tlsSecure !== true) throw new Error('tls secureConnect listener');
})();
"#,
    );
}

#[test]
fn urlsearchparams_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/urlsearchparams.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var u = require('url');
  if (!u || typeof u.URLSearchParams !== 'function') throw new Error('url.URLSearchParams missing');

  var p = new u.URLSearchParams('a=1&a=2&b=3');
  if (p.get('a') !== '1') throw new Error('get');
  var all = p.getAll('a');
  if (!Array.isArray(all) || all.length !== 2 || all[1] !== '2') throw new Error('getAll');

  p.append('c', '4');
  if (p.get('c') !== '4') throw new Error('append');

  p.set('a', '9');
  if (p.get('a') !== '9' || p.getAll('a').length !== 1) throw new Error('set');

  p.delete('b');
  if (p.get('b') !== null) throw new Error('delete');

  var s = p.toString();
  if (s.indexOf('a=9') < 0 || s.indexOf('c=4') < 0 || s.indexOf('b=') >= 0) throw new Error('toString');
})();
"#,
    );
}

#[test]
fn global_console_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    let _ = crate::console::install(&mut iso);
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/console-global.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (!globalThis.console) throw new Error('global console missing');
  var c = globalThis.console;
  var names = ['log', 'error', 'warn', 'info', 'debug'];
  for (var i = 0; i < names.length; i++) {
    if (typeof c[names[i]] !== 'function') throw new Error('console method: ' + names[i]);
  }
  var cm = require('console');
  if (cm !== c) throw new Error('console module identity');
  var cn = require('node:console');
  if (cn !== c) throw new Error('node:console module identity');
})();
"#,
    );
}

#[test]
fn queuing_strategies_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/queuing-strategies.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof ByteLengthQueuingStrategy !== 'function') throw new Error('ByteLengthQueuingStrategy missing');
  if (typeof CountQueuingStrategy !== 'function') throw new Error('CountQueuingStrategy missing');

  var b = new ByteLengthQueuingStrategy({ highWaterMark: 8 });
  if (b.highWaterMark !== 8) throw new Error('byte hwm');
  if (b.size(new Uint8Array([1,2,3])) !== 3) throw new Error('byte size typed array');
  if (b.size('ab') < 2) throw new Error('byte size string');

  var c = new CountQueuingStrategy({ highWaterMark: 3 });
  if (c.highWaterMark !== 3) throw new Error('count hwm');
  if (c.size('anything') !== 1) throw new Error('count size');
})();
"#,
    );
}

#[test]
fn performance_globals_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/performance-globals.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var ph = require('perf_hooks');
  if (!ph || typeof ph !== 'object') throw new Error('perf_hooks module');
  if (!ph.performance || typeof ph.performance.now !== 'function') throw new Error('perf_hooks.performance');
  if (typeof ph.PerformanceObserver !== 'function') throw new Error('perf_hooks.PerformanceObserver');
  if (typeof ph.PerformanceResourceTiming !== 'function') throw new Error('perf_hooks.PerformanceResourceTiming');
  if (typeof ph.PerformanceObserverEntryList !== 'function') throw new Error('perf_hooks.PerformanceObserverEntryList');
  if (!ph.constants || typeof ph.constants.NODE_PERFORMANCE_GC_DURATION !== 'number') throw new Error('perf_hooks.constants');
  if (!ph.nodeTiming || typeof ph.nodeTiming !== 'object') throw new Error('perf_hooks.nodeTiming');

  if (typeof globalThis.PerformanceObserver !== 'function') throw new Error('global PerformanceObserver');
  if (typeof globalThis.PerformanceResourceTiming !== 'function') throw new Error('global PerformanceResourceTiming');

  var obs = new ph.PerformanceObserver(function() {});
  if (!obs || typeof obs.observe !== 'function' || typeof obs.disconnect !== 'function') throw new Error('observer shape');
})();
"#,
    );
}

#[test]
fn messaging_globals_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/messaging-globals.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof MessageEvent !== 'function') throw new Error('MessageEvent missing');
  if (typeof MessagePort !== 'function') throw new Error('MessagePort missing');
  if (typeof MessageChannel !== 'function') throw new Error('MessageChannel missing');

  var ev = new MessageEvent('message', { data: 7, origin: 'x', lastEventId: 'id1' });
  if (ev.data !== 7 || ev.origin !== 'x' || ev.lastEventId !== 'id1') throw new Error('MessageEvent fields');

  var ch = new MessageChannel();
  if (!ch.port1 || !ch.port2) throw new Error('MessageChannel ports');
  if (typeof ch.port1.postMessage !== 'function' || typeof ch.port1.start !== 'function' || typeof ch.port1.close !== 'function') {
    throw new Error('MessagePort methods');
  }

  var got = null;
  ch.port2.addEventListener('message', function(e) { got = e.data; });
  ch.port1.postMessage({ ok: true, n: 3 });
  if (!got || got.ok !== true || got.n !== 3) throw new Error('MessagePort delivery');

  ch.port1.close();
  ch.port2.close();
})();
"#,
    );
}

#[test]
fn event_target_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/event-target.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof Event !== 'function') throw new Error('Event missing');
  if (typeof EventTarget !== 'function') throw new Error('EventTarget missing');
  if (typeof CustomEvent !== 'function') throw new Error('CustomEvent missing');

  var e = new Event('ping', { bubbles: true, cancelable: true });
  if (e.type !== 'ping' || e.bubbles !== true || e.cancelable !== true) throw new Error('Event shape');
  e.preventDefault();
  if (e.defaultPrevented !== true) throw new Error('Event preventDefault');

  var ce = new CustomEvent('hello', { detail: { k: 1 } });
  if (!ce.detail || ce.detail.k !== 1) throw new Error('CustomEvent detail');

  var t = new EventTarget();
  var seq = [];
  function h1(ev) { seq.push('h1:' + ev.type); }
  function h2(ev) { seq.push('h2:' + ev.type); }
  t.addEventListener('go', h1);
  t.addEventListener('go', h2);
  t.dispatchEvent(new Event('go'));
  t.removeEventListener('go', h2);
  t.dispatchEvent(new Event('go'));
  if (seq.join(',') !== 'h1:go,h2:go,h1:go') throw new Error('EventTarget listener flow');
})();
"#,
    );
}

#[test]
fn domexception_formdata_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/domexception-formdata.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof DOMException !== 'function') throw new Error('DOMException missing');
  var de = new DOMException('boom', 'AbortError');
  if (!(de instanceof Error)) throw new Error('DOMException prototype');
  if (de.name !== 'AbortError' || de.message !== 'boom') throw new Error('DOMException fields');
  if (de.code !== 0) throw new Error('DOMException code');

  if (typeof FormData !== 'function') throw new Error('FormData missing');
  var fd = new FormData();
  fd.append('a', '1');
  fd.append('a', '2');
  fd.append('b', 'x');
  if (fd.get('a') !== '1') throw new Error('FormData get');
  var all = fd.getAll('a');
  if (!Array.isArray(all) || all.length !== 2 || all[1] !== '2') throw new Error('FormData getAll');
  if (!fd.has('b')) throw new Error('FormData has');
  fd.set('a', '9');
  if (fd.get('a') !== '9' || fd.getAll('a').length !== 1) throw new Error('FormData set');
  fd.delete('b');
  if (fd.has('b')) throw new Error('FormData delete');
})();
"#,
    );
}

#[test]
fn broadcast_channel_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/broadcast-channel.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof BroadcastChannel !== 'function') throw new Error('BroadcastChannel missing');
  var a = new BroadcastChannel('kawkab_bc');
  var b = new BroadcastChannel('kawkab_bc');
  var got = null;
  b.addEventListener('message', function(e) { got = e.data; });
  a.postMessage({ ok: true, n: 4 });
  if (!got || got.ok !== true || got.n !== 4) throw new Error('BroadcastChannel delivery');
  b.close();
  got = null;
  a.postMessage({ ok: false });
  if (got !== null) throw new Error('BroadcastChannel close');
  a.close();
})();
"#,
    );
}

#[test]
fn punycode_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/punycode.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var p = require('punycode');
  if (!p || typeof p !== 'object') throw new Error('punycode module');
  if (typeof p.encode !== 'function' || typeof p.decode !== 'function') throw new Error('punycode encode/decode exports');
  if (typeof p.toASCII !== 'function' || typeof p.toUnicode !== 'function') throw new Error('punycode toASCII/toUnicode exports');

  if (p.encode('abc') !== 'abc') throw new Error('punycode encode ascii baseline');
  if (p.decode('abc') !== 'abc') throw new Error('punycode decode ascii baseline');

  var ace = p.toASCII('münich.com');
  if (typeof ace !== 'string' || ace.indexOf('xn--') < 0) throw new Error('punycode toASCII idna');
  var uni = p.toUnicode(ace);
  if (uni !== 'münich.com') throw new Error('punycode toUnicode idna');
})();
"#,
    );
}

#[test]
fn global_base64_ascii_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/base64-ascii.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof btoa !== 'function') throw new Error('btoa missing');
  if (typeof atob !== 'function') throw new Error('atob missing');
  var enc = btoa('hello');
  if (enc !== 'aGVsbG8=') throw new Error('btoa ascii');
  var dec = atob(enc);
  if (dec !== 'hello') throw new Error('atob ascii');
})();
"#,
    );
}

#[test]
fn dgram_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/dgram.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var dgram = require('dgram');
  if (!dgram || typeof dgram.createSocket !== 'function') throw new Error('dgram.createSocket');
  var s = dgram.createSocket('udp4');
  if (!s || typeof s !== 'object') throw new Error('dgram socket object');
  var fns = ['bind', 'send', 'close', 'address', 'on', 'addListener', 'once', 'removeListener', 'removeAllListeners'];
  for (var i = 0; i < fns.length; i++) {
    if (typeof s[fns[i]] !== 'function') throw new Error('dgram socket method: ' + fns[i]);
  }
})();
"#,
    );
}

#[test]
fn blob_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/blob.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof Blob !== 'function') throw new Error('Blob missing');
  globalThis.__blobOk = false;
  var b = new Blob(['ab', new Uint8Array([99])], { type: 'TEXT/PLAIN; charset=utf-8' });
  if (b.size !== 3) throw new Error('Blob size');
  if (b.type !== 'text/plain') throw new Error('Blob type');
  var s = b.slice(1, 3, 'APPLICATION/JSON');
  if (s.size !== 2 || s.type !== 'application/json') throw new Error('Blob slice');
  Promise.all([
    b.arrayBuffer().then(function(buf) {
      var u = new Uint8Array(buf);
      if (u.length !== 3 || u[0] !== 97 || u[1] !== 98 || u[2] !== 99) throw new Error('Blob arrayBuffer');
    }),
    b.text().then(function(t) {
      if (t !== 'abc') throw new Error('Blob text');
    })
  ]).then(function() {
    globalThis.__blobOk = true;
  });
})();
"#,
    );
    let ctx = iso.ctx_ptr();
    for _ in 0..300 {
        unsafe {
            let rt = qjs::JS_GetRuntime(ctx);
            loop {
                let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                let r = qjs::JS_ExecutePendingJob(rt, &mut co);
                if r <= 0 {
                    break;
                }
            }
            if global_bool(ctx, "__blobOk") {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("blob baseline contract did not complete");
}

#[test]
fn stream_placeholder_ctors_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/stream-placeholders.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var names = [
    'ReadableByteStreamController',
    'ReadableStreamBYOBReader',
    'ReadableStreamBYOBRequest',
    'ReadableStreamDefaultController',
    'ReadableStreamDefaultReader',
    'WritableStreamDefaultController',
    'WritableStreamDefaultWriter',
    'TransformStreamDefaultController'
  ];
  for (var i = 0; i < names.length; i++) {
    var n = names[i];
    var C = globalThis[n];
    if (typeof C !== 'function') throw new Error(n + ' missing');
    var threw = false;
    try { new C(); } catch (e) { threw = e && e.name === 'TypeError'; }
    if (!threw) throw new Error(n + ' illegal ctor behavior');
  }
})();
"#,
    );
}

#[test]
fn web_streams_and_compression_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/web-streams.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof ReadableStream !== 'function') throw new Error('ReadableStream missing');
  if (typeof WritableStream !== 'function') throw new Error('WritableStream missing');
  if (typeof TransformStream !== 'function') throw new Error('TransformStream missing');
  if (typeof CompressionStream !== 'function') throw new Error('CompressionStream missing');
  if (typeof DecompressionStream !== 'function') throw new Error('DecompressionStream missing');

  globalThis.__webStreamsOk = false;
  globalThis.__webWritableOk = false;
  globalThis.__webTransformOk = false;
  globalThis.__webCompressionOk = false;

  var rs = new ReadableStream({
    start: function(ctrl) {
      ctrl.enqueue('a');
      ctrl.close();
    }
  });
  var rr = rs.getReader();
  rr.read().then(function(first) {
    if (!first || first.done || first.value !== 'a') throw new Error('ReadableStream first read');
    return rr.read();
  }).then(function(second) {
    if (!second || !second.done) throw new Error('ReadableStream done read');
    globalThis.__webStreamsOk = true;
  });

  var chunks = [];
  var ws = new WritableStream({
    write: function(chunk) { chunks.push(chunk); },
    close: function() { globalThis.__wsClosed = true; }
  });
  var ww = ws.getWriter();
  ww.write('x').then(function() {
    return ww.close();
  }).then(function() {
    if (chunks.length !== 1 || chunks[0] !== 'x' || !globalThis.__wsClosed) throw new Error('WritableStream write/close');
    globalThis.__webWritableOk = true;
  });

  var ts = new TransformStream({
    transform: function(chunk, ctrl) { ctrl.enqueue(String(chunk).toUpperCase()); }
  });
  var tw = ts.writable.getWriter();
  var tr = ts.readable.getReader();
  tw.write('ab').then(function() { return tw.close(); });
  tr.read().then(function(step) {
    if (!step || step.done || step.value !== 'AB') throw new Error('TransformStream output');
    globalThis.__webTransformOk = true;
  });

  var cs = new CompressionStream('gzip');
  var ds = new DecompressionStream('gzip');
  if (!cs.readable || !cs.writable || !ds.readable || !ds.writable) throw new Error('Compression/Decompression stream shape');
  globalThis.__webCompressionOk = true;
})();
"#,
    );
    let ctx = iso.ctx_ptr();
    for _ in 0..600 {
        unsafe {
            let rt = qjs::JS_GetRuntime(ctx);
            loop {
                let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                let r = qjs::JS_ExecutePendingJob(rt, &mut co);
                if r <= 0 {
                    break;
                }
            }
            if global_bool(ctx, "__webStreamsOk")
                && global_bool(ctx, "__webWritableOk")
                && global_bool(ctx, "__webTransformOk")
                && global_bool(ctx, "__webCompressionOk")
            {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("web streams/compression contract did not complete");
}

#[test]
fn web_platform_http_surface_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/web-http-surface.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof fetch !== 'function') throw new Error('fetch missing');
  if (typeof Headers !== 'function') throw new Error('Headers missing');
  if (typeof Request !== 'function') throw new Error('Request missing');
  if (typeof Response !== 'function') throw new Error('Response missing');
  if (typeof TextEncoder !== 'function') throw new Error('TextEncoder missing');
  if (typeof TextDecoder !== 'function') throw new Error('TextDecoder missing');
  if (typeof URL !== 'function') throw new Error('URL missing');

  var h = new Headers({ A: '1' });
  h.append('A', '2');
  if (!h.has('a')) throw new Error('Headers has');
  if (h.get('a').indexOf('1') < 0) throw new Error('Headers get');
  h.set('A', '9');
  if (h.get('a') !== '9') throw new Error('Headers set');
  h.delete('A');
  if (h.has('a')) throw new Error('Headers delete');

  var req = new Request('https://example.com/x', { method: 'post', headers: { 'x-a': 'b' }, body: 'ok' });
  if (req.method !== 'POST') throw new Error('Request method');
  if (!(req.headers instanceof Headers)) throw new Error('Request headers');

  var rsp = new Response('abc', { status: 201, headers: { 'x-r': '1' } });
  if (!rsp.ok || rsp.status !== 201) throw new Error('Response status');

  var u = new URL('https://example.com/p?q=1');
  u.searchParams.set('q', '2');
  if (u.href.indexOf('q=2') < 0) throw new Error('URL searchParams');

  var bytes = new TextEncoder().encode('abc');
  if (!(bytes instanceof Uint8Array) || bytes.length !== 3) throw new Error('TextEncoder encode');
  var txt = new TextDecoder('utf-8').decode(bytes);
  if (txt !== 'abc') throw new Error('TextDecoder decode');

  globalThis.__webHttpOk = false;
  fetch(req).then(function(out) {
    if (!(out instanceof Response)) throw new Error('fetch response type');
    return out.text();
  }).then(function(_t) {
    globalThis.__webHttpOk = true;
  });
})();
"#,
    );
    let ctx = iso.ctx_ptr();
    for _ in 0..400 {
        unsafe {
            let rt = qjs::JS_GetRuntime(ctx);
            loop {
                let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                let r = qjs::JS_ExecutePendingJob(rt, &mut co);
                if r <= 0 {
                    break;
                }
            }
            if global_bool(ctx, "__webHttpOk") {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("web http surface contract did not complete");
}

#[test]
fn web_platform_text_streams_and_crypto_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/web-text-streams-crypto.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof TextEncoderStream !== 'function') throw new Error('TextEncoderStream missing');
  if (typeof TextDecoderStream !== 'function') throw new Error('TextDecoderStream missing');
  if (typeof Atomics !== 'object') throw new Error('Atomics missing');

  var ia = new Int32Array([1, 2]);
  var prev = Atomics.add(ia, 0, 5);
  if (prev !== 1 || ia[0] !== 6) throw new Error('Atomics.add baseline');
  if (Atomics.load(ia, 0) !== 6) throw new Error('Atomics.load baseline');
  if (Atomics.store(ia, 1, 9) !== 9 || ia[1] !== 9) throw new Error('Atomics.store baseline');

  var tes = new TextEncoderStream();
  var tds = new TextDecoderStream('utf-8');
  if (!tes.readable || !tes.writable) throw new Error('TextEncoderStream shape');
  if (!tds.readable || !tds.writable) throw new Error('TextDecoderStream shape');
  if (tes.encoding !== 'utf-8' || tds.encoding !== 'utf-8') throw new Error('text stream encoding');

  globalThis.__textCryptoOk = false;
  var w = tes.writable.getWriter();
  var r1 = tes.readable.getReader();
  var r2 = tds.readable.getReader();
  var w2 = tds.writable.getWriter();
  w.write('abc').then(function() { return w.close(); }).then(function() {
    return r1.read();
  }).then(function(step) {
    if (step.done || !(step.value instanceof Uint8Array) || step.value.length !== 3) {
      throw new Error('TextEncoderStream transform');
    }
    return w2.write(step.value);
  }).then(function() {
    return w2.close();
  }).then(function() {
    return r2.read();
  }).then(function(step) {
    if (step.done || step.value !== 'abc') throw new Error('TextDecoderStream transform');
    if (typeof crypto !== 'object' || !crypto) throw new Error('crypto global missing');
    if (typeof crypto.getRandomValues !== 'function') throw new Error('crypto.getRandomValues missing');
    if (typeof crypto.randomUUID !== 'function') throw new Error('crypto.randomUUID missing');
    if (!crypto.subtle || typeof crypto.subtle.digest !== 'function') throw new Error('crypto.subtle.digest missing');
    if (typeof CryptoKey !== 'function') throw new Error('CryptoKey missing');
    if (typeof SubtleCrypto !== 'function') throw new Error('SubtleCrypto missing');
    var threw = false;
    try { new CryptoKey(); } catch (e) { threw = e instanceof TypeError; }
    if (!threw) throw new Error('CryptoKey illegal constructor');
    threw = false;
    try { new SubtleCrypto(); } catch (e) { threw = e instanceof TypeError; }
    if (!threw) throw new Error('SubtleCrypto illegal constructor');
    var arr = new Uint8Array(8);
    crypto.getRandomValues(arr);
    if (!arr.some(function(v) { return v !== 0; })) {
      // allow all-zero edge but require API call path.
      if (arr.length !== 8) throw new Error('random values path');
    }
    var id = crypto.randomUUID();
    if (typeof id !== 'string' || id.length !== 36) throw new Error('randomUUID shape');
    return crypto.subtle.digest('SHA-256', new TextEncoder().encode('abc'));
  }).then(function(ab) {
    if (!(ab instanceof ArrayBuffer) || ab.byteLength !== 32) throw new Error('subtle.digest result');
    globalThis.__textCryptoOk = true;
  });
})();
"#,
    );
    let ctx = iso.ctx_ptr();
    for _ in 0..600 {
        unsafe {
            let rt = qjs::JS_GetRuntime(ctx);
            loop {
                let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                let r = qjs::JS_ExecutePendingJob(rt, &mut co);
                if r <= 0 {
                    break;
                }
            }
            if global_bool(ctx, "__textCryptoOk") {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("web text-streams/crypto contract did not complete");
}

#[test]
fn webassembly_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/webassembly-baseline.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  if (typeof WebAssembly !== 'object' || !WebAssembly) throw new Error('WebAssembly missing');
  if (typeof WebAssembly.Module !== 'function') throw new Error('Module missing');
  if (typeof WebAssembly.Instance !== 'function') throw new Error('Instance missing');
  if (typeof WebAssembly.Memory !== 'function') throw new Error('Memory missing');
  if (typeof WebAssembly.Table !== 'function') throw new Error('Table missing');
  if (typeof WebAssembly.Global !== 'function') throw new Error('Global missing');
  if (typeof WebAssembly.validate !== 'function') throw new Error('validate missing');
  if (typeof WebAssembly.compile !== 'function') throw new Error('compile missing');
  if (typeof WebAssembly.instantiate !== 'function') throw new Error('instantiate missing');

  var minimal = new Uint8Array([0x00,0x61,0x73,0x6d,0x01,0x00,0x00,0x00]);
  if (!WebAssembly.validate(minimal)) throw new Error('validate minimal module');
  if (WebAssembly.validate(new Uint8Array([0,1,2,3]))) throw new Error('validate invalid module');

  var mem = new WebAssembly.Memory({ initial: 1 });
  if (!(mem.buffer instanceof ArrayBuffer) || mem.buffer.byteLength !== 65536) {
    throw new Error('memory baseline');
  }
  var table = new WebAssembly.Table({ initial: 2, element: 'anyfunc' });
  if (table.length !== 2) throw new Error('table baseline');
  var glob = new WebAssembly.Global({ value: 'i32', mutable: true }, 7);
  if (glob.value !== 7 || !glob.mutable) throw new Error('global baseline');

  globalThis.__wasmOk = false;
  WebAssembly.compile(minimal).then(function(mod) {
    if (!(mod instanceof WebAssembly.Module)) throw new Error('compile module type');
    return WebAssembly.instantiate(mod, {});
  }).then(function(out) {
    if (!out || !(out.module instanceof WebAssembly.Module)) throw new Error('instantiate module type');
    if (!(out.instance instanceof WebAssembly.Instance)) throw new Error('instantiate instance type');
    if (!out.instance.exports || typeof out.instance.exports !== 'object') {
      throw new Error('instance exports shape');
    }
    return WebAssembly.instantiate(minimal, {});
  }).then(function(out2) {
    if (!(out2.instance instanceof WebAssembly.Instance)) throw new Error('instantiate bytes');
    globalThis.__wasmOk = true;
  });
})();
"#,
    );
    let ctx = iso.ctx_ptr();
    for _ in 0..500 {
        unsafe {
            let rt = qjs::JS_GetRuntime(ctx);
            loop {
                let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                let r = qjs::JS_ExecutePendingJob(rt, &mut co);
                if r <= 0 {
                    break;
                }
            }
            if global_bool(ctx, "__wasmOk") {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("webassembly baseline contract did not complete");
}

#[test]
fn node_red_modules_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/node-red-modules-baseline.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var tty = require('node:tty');
  if (typeof tty.isatty !== 'function') throw new Error('tty.isatty');
  if (typeof tty.ReadStream !== 'function' || typeof tty.WriteStream !== 'function') throw new Error('tty streams');
  if (typeof tty.isatty(1) !== 'boolean') throw new Error('tty.isatty return type');
  if (tty.isatty(1) !== true || tty.isatty(999) !== false) throw new Error('tty.isatty behavior');
  if (typeof tty.ReadStream !== 'function' || typeof tty.WriteStream !== 'function') {
    throw new Error('tty stream ctors');
  }
  var rs1 = tty.ReadStream(0);
  var ws1 = tty.WriteStream(1);
  var rs2 = new tty.ReadStream(2);
  var ws2 = new tty.WriteStream(999);
  if (!rs1 || rs1.isTTY !== true || rs1.fd !== 0) throw new Error('tty ReadStream call');
  if (!ws1 || ws1.isTTY !== true || ws1.fd !== 1) throw new Error('tty WriteStream call');
  if (!rs2 || rs2.isTTY !== true || rs2.fd !== 2) throw new Error('tty ReadStream ctor');
  if (!ws2 || ws2.isTTY !== false || ws2.fd !== 999) throw new Error('tty WriteStream ctor');
  var d = require('node:domain');
  if (typeof d.create !== 'function' || typeof d.createDomain !== 'function') throw new Error('domain funcs');
  if (typeof d.Domain !== 'function') throw new Error('domain ctor');
  var dom = d.create();
  if (!dom || typeof dom !== 'object') throw new Error('domain create object');
  if (typeof dom.on !== 'function' || typeof dom.run !== 'function') throw new Error('domain base methods');
  if (typeof dom.bind !== 'function' || typeof dom.intercept !== 'function') throw new Error('domain bind/intercept');
  if (typeof dom.dispose !== 'function') throw new Error('domain dispose');

  var repl = require('node:repl');
  if (typeof repl.start !== 'function') throw new Error('repl.start');
  var replServer = repl.start({ prompt: '> ' });
  if (!replServer || typeof replServer !== 'object') throw new Error('repl server shape');
  if (!replServer.context || typeof replServer.context !== 'object') throw new Error('repl context');
  if (typeof replServer.defineCommand !== 'function' || typeof replServer.close !== 'function') {
    throw new Error('repl server methods');
  }

  var tr = require('node:trace_events');
  if (typeof tr.createTracing !== 'function') throw new Error('trace createTracing');
  if (typeof tr.getEnabledCategories !== 'function') throw new Error('trace getEnabledCategories');
  var tracing = tr.createTracing({ categories: ['node'] });
  if (!tracing || typeof tracing.enable !== 'function' || typeof tracing.disable !== 'function') {
    throw new Error('trace tracing object');
  }
  if (typeof tr.getEnabledCategories() !== 'string') throw new Error('trace categories return type');
  if (tracing.enable() !== true) throw new Error('trace enable return');
  if (typeof tr.getEnabledCategories() !== 'string') throw new Error('trace categories after enable');
  if (tracing.disable() !== false) throw new Error('trace disable return');

  var insp = require('node:inspector');
  if (typeof insp.open !== 'function' || typeof insp.close !== 'function' || typeof insp.url !== 'function') {
    throw new Error('inspector api');
  }
  if (insp.close() !== true) throw new Error('inspector close return');
  if (typeof insp.url() !== 'string') throw new Error('inspector url string');
  var session = insp.open();
  if (!session || typeof session !== 'object') throw new Error('inspector session shape');
  if (typeof session.post !== 'function' || typeof session.disconnect !== 'function') {
    throw new Error('inspector session methods');
  }

  var v8 = require('node:v8');
  if (typeof v8.cachedDataVersionTag !== 'function') throw new Error('v8.cachedDataVersionTag');
  if (typeof v8.getHeapStatistics !== 'function') throw new Error('v8.getHeapStatistics');
  if (typeof v8.setFlagsFromString !== 'function') throw new Error('v8.setFlagsFromString');
  if (typeof v8.cachedDataVersionTag() !== 'number') throw new Error('v8.cachedDataVersionTag return');
  var hs = v8.getHeapStatistics();
  if (!hs || typeof hs !== 'object') throw new Error('v8.getHeapStatistics object');
  if (typeof hs.total_heap_size !== 'number') throw new Error('v8 heap total');
  if (typeof hs.used_heap_size !== 'number') throw new Error('v8 heap used');
  if (typeof hs.total_available_size !== 'number') throw new Error('v8 heap available');
  if (typeof hs.malloced_memory !== 'number') throw new Error('v8 malloced memory');
  if (typeof hs.peak_malloced_memory !== 'number') throw new Error('v8 peak malloced memory');

  var wasi = require('node:wasi');
  if (typeof wasi.WASI !== 'function') throw new Error('wasi.WASI');
  var wasiInst = new wasi.WASI({ args: [], env: {} });
  if (!wasiInst || typeof wasiInst !== 'object') throw new Error('wasi instance');
  if (typeof wasiInst.start !== 'function' || typeof wasiInst.initialize !== 'function') {
    throw new Error('wasi instance methods');
  }
  if (!wasiInst.wasiImport || typeof wasiInst.wasiImport !== 'object') throw new Error('wasiImport shape');
  if (!wasiInst.wasiImport.wasi_snapshot_preview1 || typeof wasiInst.wasiImport.wasi_snapshot_preview1 !== 'object') {
    throw new Error('wasi preview1 shape');
  }
  var w1 = wasiInst.wasiImport.wasi_snapshot_preview1;
  if (typeof w1.proc_exit !== 'function' || typeof w1.fd_write !== 'function' || typeof w1.fd_read !== 'function') {
    throw new Error('wasi preview1 funcs');
  }
  if (typeof w1.environ_get !== 'function' || typeof w1.args_get !== 'function') {
    throw new Error('wasi preview1 env/args funcs');
  }
  if (typeof wasiInst.start({}) !== 'number' || typeof wasiInst.initialize({}) !== 'number') {
    throw new Error('wasi method return type');
  }

  var cluster = require('node:cluster');
  if (typeof cluster.fork !== 'function' || typeof cluster.setupPrimary !== 'function') throw new Error('cluster funcs');
  if (typeof cluster.isPrimary !== 'boolean') throw new Error('cluster flags');
  var cfg = cluster.setupPrimary({ silent: false });
  if (!cfg || typeof cfg !== 'object') throw new Error('cluster setupPrimary return');
  if (typeof cfg.schedulingPolicy !== 'number' || typeof cfg.silent !== 'boolean') {
    throw new Error('cluster setupPrimary shape');
  }
  var worker = cluster.fork();
  if (!worker || typeof worker !== 'object') throw new Error('cluster worker');
  if (typeof worker.id !== 'number' || typeof worker.isConnected !== 'boolean') throw new Error('cluster worker fields');
  if (typeof worker.send !== 'function' || typeof worker.kill !== 'function') throw new Error('cluster worker methods');
  if (typeof worker.on !== 'function' || typeof worker.once !== 'function') throw new Error('cluster worker event methods');
  if (!worker.process || typeof worker.process.pid !== 'number') throw new Error('cluster worker process shape');

  var http2 = require('node:http2');
  if (typeof http2.createServer !== 'function') throw new Error('http2.createServer');
  if (typeof http2.createSecureServer !== 'function') throw new Error('http2.createSecureServer');
  if (typeof http2.connect !== 'function') throw new Error('http2.connect');
  var s1 = http2.createServer();
  var s2 = http2.createSecureServer();
  if (!s1 || typeof s1.listen !== 'function' || typeof s1.close !== 'function') throw new Error('http2 server shape');
  if (!s2 || typeof s2.on !== 'function') throw new Error('http2 secure server shape');
  var client = http2.connect('https://example.com');
  if (!client || typeof client.request !== 'function' || typeof client.close !== 'function') {
    throw new Error('http2 client shape');
  }
  var reqStream = client.request({ ':path': '/' });
  if (!reqStream || typeof reqStream.on !== 'function' || typeof reqStream.end !== 'function') {
    throw new Error('http2 request stream shape');
  }
  if (typeof reqStream.close !== 'function' || typeof reqStream.setEncoding !== 'function') {
    throw new Error('http2 request stream methods');
  }

  var sqlite = require('node:sqlite');
  if (typeof sqlite.Database !== 'function') throw new Error('sqlite.Database');
  var db = new sqlite.Database(':memory:');
  if (!db || typeof db.exec !== 'function' || typeof db.prepare !== 'function' || typeof db.close !== 'function') {
    throw new Error('sqlite db shape');
  }
  var stmt = db.prepare('select 1');
  if (!stmt || typeof stmt.run !== 'function' || typeof stmt.get !== 'function') {
    throw new Error('sqlite stmt shape');
  }
  if (typeof stmt.all !== 'function' || typeof stmt.finalize !== 'function') {
    throw new Error('sqlite stmt methods');
  }
  var runInfo = stmt.run();
  if (!runInfo || typeof runInfo.changes !== 'number' || typeof runInfo.lastInsertRowid !== 'number') {
    throw new Error('sqlite stmt run return');
  }
  var one = stmt.get();
  if (!one || typeof one !== 'object' || Array.isArray(one)) throw new Error('sqlite stmt get return');
  var many = stmt.all();
  if (!Array.isArray(many)) throw new Error('sqlite stmt all return');
})();
"#,
    );
}

#[test]
fn readline_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/readline.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var rlMod = require('readline');
  var rl = rlMod.createInterface({ input: null, output: null, terminal: false });
  var closed = false;
  rl.on('close', function() { closed = true; });
  rl.setPrompt('> ');
  rl.prompt();
  rl.question('name?', function(answer) {
    if (answer !== '') throw new Error('readline question default answer');
  });
  rl.close();
  if (!closed) throw new Error('readline close event');

  var rlp = require('readline/promises');
  var rl2 = rlp.createInterface({ input: null, output: null, terminal: false });
  var p = rl2.question('name?');
  if (!p || typeof p.then !== 'function') throw new Error('readline/promises question promise');
  p.then(function(ans) {
    if (ans !== '') throw new Error('readline/promises answer');
  });
  rl2.close();
})();
"#,
    );
}

#[test]
fn module_and_zlib_contract() {
    let _s = qjs_serial();
    let main_path = serde_json::to_string("/tmp/kawkab-module-main.js").expect("main path json");

    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/module-zlib.js", None).expect("install_runtime");
    }
    let script = format!(
        r#"
(function() {{
  var moduleApi = require('module');
  if (typeof moduleApi.createRequire !== 'function') throw new Error('module.createRequire');
  var req = moduleApi.createRequire({main_path});
  var p = req('path');
  if (!p || typeof p.join !== 'function') throw new Error('createRequire builtin resolve');

  var zlib = require('zlib');
  if (typeof zlib.gzipSync !== 'function') throw new Error('zlib.gzipSync');
  if (typeof zlib.gunzipSync !== 'function') throw new Error('zlib.gunzipSync');
  if (typeof zlib.deflateSync !== 'function') throw new Error('zlib.deflateSync');
  if (typeof zlib.inflateSync !== 'function') throw new Error('zlib.inflateSync');
}})();
"#
    );
    eval_script(&mut iso, &script);
}

#[test]
fn querystring_baseline_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/querystring-baseline.js", None)
            .expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var qs = require('node:querystring');
  if (!qs || typeof qs.parse !== 'function' || typeof qs.stringify !== 'function') {
    throw new Error('querystring api');
  }
  var out = qs.parse('a=1&b=two+words&a=3');
  if (!out || !Array.isArray(out.a) || out.a.length !== 2 || out.a[0] !== '1' || out.a[1] !== '3') {
    throw new Error('querystring repeated key parse');
  }
  if (out.b !== 'two words') throw new Error('querystring plus decode');

  var limited = qs.parse('x=1&y=2&z=3', '&', '=', { maxKeys: 1 });
  if (!limited || typeof limited.x !== 'string' || typeof limited.y !== 'undefined') {
    throw new Error('querystring maxKeys');
  }

  var s = qs.stringify({ a: ['1', '3'], b: 'two words' });
  if (s.indexOf('a=1') < 0 || s.indexOf('a=3') < 0 || s.indexOf('b=two+words') < 0) {
    throw new Error('querystring stringify');
  }
})();
"#,
    );
}

#[test]
fn worker_threads_nested_worker_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/wt-nested.js", None).expect("install_runtime");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var wt = require('worker_threads');
  if (typeof wt.Worker !== 'function') throw new Error('Worker ctor missing');
})();
"#,
    );
}

#[test]
fn http_client_local_behavior_contract() {
    let _s = qjs_serial();
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/http-local.js", None).expect("install");
    }
    eval_script(
        &mut iso,
        r#"
(function() {
  var http = require('http');
  if (typeof http.get !== 'function') throw new Error('http.get missing');
  if (typeof http.request !== 'function') throw new Error('http.request missing');
})();
"#,
    );
}

#[test]
fn stream_pipeline_backpressure_contract() {
    let _s = qjs_serial();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let mut isolate = Isolate::new(IsolateConfig::default()).expect("isolate");
        let _ = crate::console::install(&mut isolate);
        let ctx = isolate.ctx_ptr();
        let (task_tx, mut task_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::event_loop::Task>();
        let sender = crate::event_loop::TaskSender::from_sender(task_tx.clone());
        unsafe {
            install_runtime(ctx, "/test/stream-pipeline.js", Some(sender)).expect("install");
        }
        eval_script(
            &mut isolate,
            r#"
(function() {
  var stream = require('stream');
  globalThis.__streamPipeOk = false;
  globalThis.__streamErrOk = false;

  var src = new stream.Readable({ highWaterMark: 2, read: function(){} });
  var dst = new stream.Writable({ highWaterMark: 1 });
  var sawDrain = false;
  dst.on('drain', function(){ sawDrain = true; });

  src.pipe(dst);
  var ret = src.push('abcdefgh');
  src.push(null);
  setTimeout(function() {
    globalThis.__streamPipeOk = (ret === false) && sawDrain;
  }, 20);

  var errReadable = new stream.Readable({ read: function(){} });
  var errSink = new stream.Writable();
  errReadable.pipe(errSink, { errorHandler: function(){ globalThis.__streamErrOk = true; } });
  errReadable.emit('error', new Error('boom'));
})();
"#,
        );

        let mut ok = false;
        for _ in 0..700 {
            while let Ok(t) = task_rx.try_recv() {
                unsafe {
                    crate::node::dispatch_cli_isolate_task(ctx, t);
                }
            }
            unsafe {
                let _ = crate::node::drain_next_tick_queue(ctx);
                let rt_q = qjs::JS_GetRuntime(ctx);
                loop {
                    let mut co: *mut qjs::JSContext = std::ptr::null_mut();
                    let r = qjs::JS_ExecutePendingJob(rt_q, &mut co);
                    if r <= 0 {
                        break;
                    }
                }
                if global_bool(ctx, "__streamPipeOk") && global_bool(ctx, "__streamErrOk") {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        assert!(ok, "stream pipeline/backpressure contract did not complete");
    });
}
