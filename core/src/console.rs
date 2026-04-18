// `console.log` / `console.error`: thread-local `BufWriter`, string args via `JS_ToCStringLen`.

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

/// Native implementation of `console.log`.
///
/// Registered as a C-ABI function pointer via `JS_NewCFunction`.
///
/// # Safety
/// Called by the QuickJS engine on the owning thread. All pointers are valid
/// for the duration of this call. We must not retain them.
unsafe extern "C" fn native_console_log(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: i32,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    write_console_args(ctx, argc, argv, false);
    js_undefined()
}

/// Native implementation of `console.error`.
unsafe extern "C" fn native_console_error(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: i32,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    write_console_args(ctx, argc, argv, true);
    js_undefined()
}

/// Native implementation of `console.warn` (alias of `console.error`).
unsafe extern "C" fn native_console_warn(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: i32,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    write_console_args(ctx, argc, argv, true);
    js_undefined()
}

/// Write all arguments to the appropriate output stream.
///
/// Hot path: zero allocations for all-string arguments (the common case).
/// For non-string args: one JS_ToString allocation per argument.
///
/// # Safety
/// `ctx` valid, `argv` points to `argc` valid JSValues.
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

/// Inner write loop — separated so it's monomorphised once per stream type.
///
/// # Safety
/// All pointers in `args` are valid JSValues on the current thread.
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

/// Install `console.{log,error,warn,info,debug}` on the global object.
///
/// Call this once during Isolate construction.
///
/// # Safety
/// Must be called on the thread that owns `isolate`.
pub fn install(isolate: &mut crate::isolate::Isolate) -> Result<(), JsError> {
    // SAFETY: We have exclusive mutable access to the isolate (guaranteed by
    // &mut Isolate), so no other code can be executing JS right now.
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

/// Flush both stdout and stderr buffers. Call at isolate teardown.
pub fn flush_all() {
    STDOUT.with(|w| {
        let _ = w.borrow_mut().flush();
    });
    STDERR.with(|w| {
        let _ = w.borrow_mut().flush();
    });
}
