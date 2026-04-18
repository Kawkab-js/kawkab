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
        for _ in 0..200 {
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
