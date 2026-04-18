//! Contract checks for high-priority Node built-ins (see `docs/COMPAT_DEFINITION_OF_DONE.md`).

use crate::isolate::{Isolate, IsolateConfig};
use crate::node::install_runtime;

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
