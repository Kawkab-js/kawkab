use std::{
    cell::RefCell,
    ffi::CString,
    io::{BufWriter, Write},
};

use quickjs_sys as qjs;

use crate::ffi::{js_free_value, set_function, with_js_string};
use crate::error::JsError;

#[inline]
fn js_undefined() -> qjs::JSValue {
    qjs::JSValue {
        u: qjs::JSValueUnion { int32: 0 },
        tag: qjs::JS_TAG_UNDEFINED as i64,
    }
}

const STDOUT_BUF_SIZE: usize = 8 * 1024;

thread_local! {
    static STDOUT: RefCell<BufWriter<std::io::Stdout>> = RefCell::new(
        BufWriter::with_capacity(STDOUT_BUF_SIZE, std::io::stdout())
    );
    static STDERR: RefCell<BufWriter<std::io::Stderr>> = RefCell::new(
        BufWriter::with_capacity(STDOUT_BUF_SIZE, std::io::stderr())
    );
}

/// `console.log` QuickJS native callback.
///
/// # Safety
/// Called by QuickJS on the isolate thread; pointers valid only for this call.
unsafe extern "C" fn native_console_log(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: i32,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    write_console_args(ctx, argc, argv, false);
    js_undefined()
}

/// `console.error` QuickJS native callback.
unsafe extern "C" fn native_console_error(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: i32,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    write_console_args(ctx, argc, argv, true);
    js_undefined()
}

/// `console.warn` QuickJS native callback (stderr path like `console.error`).
unsafe extern "C" fn native_console_warn(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: i32,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    write_console_args(ctx, argc, argv, true);
    js_undefined()
}

/// Print `argc` values to stdout or stderr (strings fast-path; else `JS_ToString` per arg).
///
/// # Safety
/// Valid `ctx` and `argv`/`argc` from QuickJS.
unsafe fn write_console_args(
    ctx: *mut qjs::JSContext,
    argc: i32,
    argv: *mut qjs::JSValue,
    is_err: bool,
) {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);

    if is_err {
        STDERR.with(|w| {
            let mut w = w.borrow_mut();
            write_args_to(ctx, args, &mut *w);
        });
    } else {
        STDOUT.with(|w| {
            let mut w = w.borrow_mut();
            write_args_to(ctx, args, &mut *w);
        });
    }
}

/// Inner loop (monomorphised once per `W`).
///
/// # Safety
/// `args` are live `JSValue`s on this thread.
unsafe fn write_args_to<W: Write>(ctx: *mut qjs::JSContext, args: &[qjs::JSValue], out: &mut W) {
    for (i, &arg) in args.iter().enumerate() {
        if i > 0 {
            // SAFETY: single-byte write, infallible.
            let _ = out.write_all(b" ");
        }

        if arg.tag == qjs::JS_TAG_STRING as i64 {
            with_js_string(ctx, arg, |bytes| {
                let _ = out.write_all(bytes);
            });
        } else {
            let str_val = qjs::JS_ToString(ctx, arg);
            if !qjs::JS_IsException(str_val) {
                with_js_string(ctx, str_val, |bytes| {
                    let _ = out.write_all(bytes);
                });
                js_free_value(ctx, str_val);
            } else {
                qjs::JS_GetException(ctx);
                let _ = out.write_all(b"[object Object]");
            }
        }
    }

    let _ = out.write_all(b"\n");

    let _ = out.flush();
}

/// Install `console.{log,error,warn,info,debug}` on `globalThis`.
///
/// # Safety
/// Call on the thread that owns `isolate`.
pub fn install(isolate: &mut crate::isolate::Isolate) -> Result<(), JsError> {
    // SAFETY: `&mut Isolate` — no concurrent JS on this thread.
    unsafe {
        let ctx = isolate.ctx_ptr();

        let global = qjs::JS_GetGlobalObject(ctx);

        let console = qjs::JS_NewObject(ctx);

        macro_rules! bind {
            ($obj:expr, $name:literal, $fn:expr, $len:expr) => {{
                let c = CString::new($name).unwrap();
                set_function(ctx, $obj, &c, $fn, $len);
            }};
        }

        bind!(console, "log", Some(native_console_log), 1);
        bind!(console, "info", Some(native_console_log), 1); // alias
        bind!(console, "debug", Some(native_console_log), 1); // alias
        bind!(console, "error", Some(native_console_error), 1);
        bind!(console, "warn", Some(native_console_warn), 1);

        let key = CString::new("console").unwrap();
        qjs::JS_SetPropertyStr(ctx, global, key.as_ptr(), console);

        js_free_value(ctx, global);
    }

    Ok(())
}

/// Flush thread-local stdout/stderr `BufWriter`s.
pub fn flush_all() {
    STDOUT.with(|w| {
        let _ = w.borrow_mut().flush();
    });
    STDERR.with(|w| {
        let _ = w.borrow_mut().flush();
    });
}
