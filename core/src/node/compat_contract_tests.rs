//! Contract checks for high-priority Node built-ins (see `docs/COMPAT_DEFINITION_OF_DONE.md`).

use std::ffi::CString;

use quickjs_sys as qjs;

use crate::ffi::js_free_value;
use crate::isolate::{Isolate, IsolateConfig};
use crate::node::install_runtime;

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

#[test]
fn isolate_eval_smoke() {
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    eval_script(&mut iso, "var __t = 1 + 1; if (__t !== 2) throw new Error('eval');");
}

#[test]
fn install_runtime_smoke() {
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/smoke.js", None).expect("install_runtime");
    }
    eval_script(&mut iso, "var __smoke = 1; if (__smoke !== 1) throw new Error('smoke');");
}

fn eval_script(iso: &mut Isolate, src: &str) {
    let v = iso
        .eval(src.as_bytes(), "compat_contract_test.js")
        .unwrap_or_else(|e| panic!("JS eval failed: {e:?}"));
    unsafe {
        crate::ffi::js_free_value(iso.ctx_ptr(), v);
    }
}

#[test]
fn priority_builtins_green_contract() {
    let tmp = std::env::temp_dir().join(format!("kawkab_priority_contract_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"ok").expect("temp file");

    let tmp_json = serde_json::to_string(tmp.to_str().expect("utf8 temp path"))
        .expect("json encode path");

    let prev_child = std::env::var("KAWKAB_ALLOW_CHILD_PROCESS").ok();
    std::env::set_var("KAWKAB_ALLOW_CHILD_PROCESS", "1");

    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/contract.js", None).expect("install_runtime");
    }

    eval_script(
        &mut iso,
        &format!(
            r#"
(function() {{
  var fs = require('fs');
  var path = require('path');
  var buf = require('buffer');
  var url = require('url');
  var crypto = require('crypto');
  var stream = require('stream');
  var net = require('net');
  var wt = require('worker_threads');
  var timers = require('timers');
  var tmp = {tmp_json};

  var p = path.join('x', 'y');
  if (p.indexOf('y') < 0) throw new Error('path.join');

  var txt = fs.readFileSync(tmp);
  if (txt.length !== 2 || txt.toString() !== 'ok') throw new Error('fs.readFileSync');

  var b = buf.Buffer.from('ab', 'utf8');
  if (b.length !== 2) throw new Error('Buffer');
  var util = require('util');
  if (!util.types.isBuffer(b)) throw new Error('isBuffer');

  var u = new url.URL('http://a/b?c=1');
  if (u.searchParams.get('c') !== '1') throw new Error('URL');

  var h = crypto.createHash('sha256').update('x').digest('hex');
  if (h.length !== 64) throw new Error('crypto');

  if (typeof process.cwd !== 'function') throw new Error('process.cwd');

  var fired = false;
  timers.setTimeout(function() {{ fired = true; }}, 0);
  if (!fired) throw new Error('timers.setTimeout');

  var r = new stream.Readable({{ read: function() {{}} }});
  r.push('z');
  r.push(null);

  if (typeof net.createServer !== 'function') throw new Error('net.createServer');

  if (wt.isMainThread !== true) throw new Error('worker_threads.isMainThread');
  if (typeof wt.Worker !== 'function') throw new Error('worker_threads.Worker');
}})();
"#,
            tmp_json = tmp_json
        ),
    );

    eval_script(
        &mut iso,
        r#"
(function() {
  var http = require('http');
  var okHttp = false;
  http.get('http://example.com', function(res) {
    okHttp = res.statusCode === 200;
  });
  if (!okHttp) throw new Error('http.get');
})();
"#,
    );

    eval_script(
        &mut iso,
        r#"
(function() {
  var https = require('https');
  var okHttps = false;
  https.get('https://example.com', function(res) {
    okHttps = res.statusCode === 200;
  });
  if (!okHttps) throw new Error('https.get');
})();
"#,
    );

    eval_script(
        &mut iso,
        r#"
(function() {
  var cp = require('child_process');
  var out = cp.execSync('echo kawkab_child_ok');
  if (String(out).indexOf('kawkab_child_ok') < 0) throw new Error('child_process.execSync');
})();
"#,
    );

    let _ = std::fs::remove_file(&tmp);

    match prev_child {
        Some(v) => std::env::set_var("KAWKAB_ALLOW_CHILD_PROCESS", v),
        None => std::env::remove_var("KAWKAB_ALLOW_CHILD_PROCESS"),
    }
}

#[test]
fn worker_threads_roundtrip() {
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
                if global_bool(ctx, "__wtOk") {
                    ok = true;
                    break;
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
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
fn web_platform_and_builtins_baseline_contract() {
    let mut iso = Isolate::new(IsolateConfig::default()).expect("isolate");
    unsafe {
        install_runtime(iso.ctx_ptr(), "/test/web_platform_contract.js", None).expect("install");
    }

    eval_script(
        &mut iso,
        r#"
(function() {
  var b = new Blob(['hi'], { type: 'text/plain; charset=utf-8' });
  if (b.size !== 2 || b.type !== 'text/plain') throw new Error('Blob');
  var fd = new FormData();
  fd.append('k', 'v');
  if (fd.get('k') !== 'v' || !fd.has('k')) throw new Error('FormData');
  var et = new EventTarget();
  var fired = false;
  et.addEventListener('e', function(ev) { if (ev.type !== 'e') throw new Error('bad type'); fired = true; });
  et.dispatchEvent(new Event('e'));
  if (!fired) throw new Error('EventTarget');
  var ce = new CustomEvent('c', { detail: 7 });
  if (ce.detail !== 7) throw new Error('CustomEvent');
  var de = new DOMException('x', 'NotFoundError');
  if (de.name !== 'NotFoundError') throw new Error('DOMException');

  var rs = new ReadableStream({
    start: function(c) { c.enqueue(new Uint8Array([1,2])); c.close(); }
  });
  var reader = rs.getReader();
  var step = reader.read();
  if (!step || typeof step.then !== 'function') throw new Error('read promise');
  var ws = new WritableStream();
  var w = ws.getWriter();
  var wc = w.close();
  if (!wc || typeof wc.then !== 'function') throw new Error('writable');

  var ts = new TransformStream({
    transform: function(chunk, ctrl) { ctrl.enqueue(chunk); },
    flush: function(ctrl) { ctrl.enqueue(new Uint8Array([9])); }
  });
  var tw = ts.writable.getWriter();
  tw.write(new Uint8Array([3]));
  tw.close();

  var cs = new CompressionStream('gzip');
  if (!cs.readable || !cs.writable) throw new Error('CompressionStream');
  var ds = new DecompressionStream('gzip');
  if (!ds.readable || !ds.writable) throw new Error('DecompressionStream');

  var ch = new MessageChannel();
  var got = false;
  ch.port2.addEventListener('message', function(ev) { if (ev.data === 1) got = true; });
  ch.port1.postMessage(1);
  if (!got) throw new Error('MessageChannel sync flag');

  var bcHit = false;
  var bc1 = new BroadcastChannel('t');
  var bc2 = new BroadcastChannel('t');
  bc2.addEventListener('message', function(ev) { if (ev.data === 2) bcHit = true; });
  bc1.postMessage(2);
  if (!bcHit) throw new Error('BroadcastChannel');

  var util = require('util');
  if (!util.types.isString('a')) throw new Error('isString');
  if (!util.types.isNumber(3)) throw new Error('isNumber');
  if (!util.types.isBoolean(true)) throw new Error('isBoolean');
  if (!util.types.isObject({})) throw new Error('isObject');
  if (!util.types.isFunction(function(){})) throw new Error('isFunction');
  var ab = new ArrayBuffer(0);
  if (!util.types.isArrayBuffer(ab)) throw new Error('isArrayBuffer');
  if (util.types.isNull(null) !== true) throw new Error('isNull');
  if (util.types.isUndefined(undefined) !== true) throw new Error('isUndefined');
  if (util.types.isRegExp(/x/) !== true) throw new Error('isRegExp');
  if (util.types.isBuffer(Buffer.from('x')) !== true) throw new Error('isBuffer');

  var qs = require('querystring');
  if (typeof qs.escape !== 'function' || typeof qs.unescape !== 'function') throw new Error('querystring legacy');
  var qdup = qs.parse('a=1&a=2');
  if (!Array.isArray(qdup.a) || qdup.a.length !== 2) throw new Error('qs array');

  var SD = require('string_decoder').StringDecoder;
  var dec3 = new SD('utf8');
  var euro3 = dec3.write(Buffer.from([0xe2])) + dec3.write(Buffer.from([0x82, 0xac]));
  if (euro3 !== '\u20ac') throw new Error('StringDecoder');

  var os = require('os');
  if (typeof os.arch !== 'function' || typeof os.cpus !== 'function') throw new Error('os');
  if (!Array.isArray(os.cpus()) || os.cpus().length < 1) throw new Error('os.cpus');
  if (typeof os.totalmem() !== 'number') throw new Error('os.totalmem');
  if (typeof os.EOL !== 'string' || os.EOL.length < 1) throw new Error('os.EOL');
  var la = os.loadavg();
  if (!Array.isArray(la) || la.length !== 3) throw new Error('loadavg');
  if (typeof os.networkInterfaces !== 'function' || typeof os.networkInterfaces() !== 'object') throw new Error('networkInterfaces');

  var ph = require('perf_hooks');
  if (typeof ph.PerformanceObserver !== 'function') throw new Error('PerformanceObserver');
  var obs = new ph.PerformanceObserver(function() {});
  if (typeof obs.observe !== 'function') throw new Error('observe');
  if (typeof ph.PerformanceMark !== 'function' || typeof ph.PerformanceEntry !== 'function') throw new Error('perf_hooks types');
  if (typeof PerformanceMark !== 'function') throw new Error('global PerformanceMark');

  var tp = require('timers/promises');
  if (typeof tp.setInterval !== 'function') throw new Error('timers/promises.setInterval');

  var test = require('test');
  if (typeof test.before !== 'function' || typeof test.after !== 'function') throw new Error('test hooks');
  if (typeof test.beforeEach !== 'function' || typeof test.afterEach !== 'function') throw new Error('test each hooks');

  var bqs = new ByteLengthQueuingStrategy({ highWaterMark: 8 });
  if (typeof bqs.size !== 'function') throw new Error('ByteLengthQueuingStrategy');
  var cqs = new CountQueuingStrategy({ highWaterMark: 2 });
  if (cqs.size() !== 1) throw new Error('CountQueuingStrategy');
})();
"#,
    );

    eval_script(
        &mut iso,
        r#"
(function() {
  globalThis.__webPlatRoundtrip = false;
  globalThis.__webPlatRoundtripErr = '';
  var input = new Uint8Array([1, 2, 3, 4]);
  var cs = new CompressionStream('gzip');
  var w = cs.writable.getWriter();
  w.write(input);
  w.close();
  cs.readable.getReader().read().then(function(r) {
    if (r.done || !r.value) throw new Error('compress read');
    var ds = new DecompressionStream('gzip');
    var w2 = ds.writable.getWriter();
    w2.write(r.value);
    w2.close();
    return ds.readable.getReader().read();
  }).then(function(r) {
    if (r.done || !r.value || r.value.length !== 4) throw new Error('decompress');
    globalThis.__webPlatRoundtrip = true;
  }).catch(function(e) {
    globalThis.__webPlatRoundtripErr = e && e.message ? String(e.message) : String(e);
  });
})();
"#,
    );

    unsafe fn drain_microtasks(ctx: *mut qjs::JSContext, rounds: u32) {
        let rt = qjs::JS_GetRuntime(ctx);
        for _ in 0..rounds {
            let mut co: *mut qjs::JSContext = std::ptr::null_mut();
            let r = qjs::JS_ExecutePendingJob(rt, &mut co);
            if r <= 0 {
                break;
            }
        }
    }

    let ctx = iso.ctx_ptr();
    let mut ok = false;
    for _ in 0..400 {
        unsafe {
            drain_microtasks(ctx, 32);
        }
        if unsafe { global_bool(ctx, "__webPlatRoundtrip") } {
            ok = true;
            break;
        }
        let g = unsafe { qjs::JS_GetGlobalObject(ctx) };
        let key = CString::new("__webPlatRoundtripErr").unwrap();
        let errv = unsafe { qjs::JS_GetPropertyStr(ctx, g, key.as_ptr()) };
        unsafe {
            crate::ffi::js_free_value(ctx, g);
        }
        let has_err = unsafe {
            if qjs::JS_IsString(errv) {
                let s = crate::ffi::js_string_to_owned(ctx, errv);
                crate::ffi::js_free_value(ctx, errv);
                !s.is_empty()
            } else {
                crate::ffi::js_free_value(ctx, errv);
                false
            }
        };
        if has_err {
            let g2 = unsafe { qjs::JS_GetGlobalObject(ctx) };
            let errv2 = unsafe { qjs::JS_GetPropertyStr(ctx, g2, key.as_ptr()) };
            let msg = unsafe {
                let m = crate::ffi::js_string_to_owned(ctx, errv2);
                crate::ffi::js_free_value(ctx, errv2);
                crate::ffi::js_free_value(ctx, g2);
                m
            };
            panic!("web platform gzip round-trip: {msg}");
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(ok, "web platform gzip round-trip did not complete");
}
