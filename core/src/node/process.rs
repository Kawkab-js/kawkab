//! `process.stdout` / `process.stderr` backed by `BufWriter` over the host streams.
//! Other `process.*` fields are still assembled in [`super::install_runtime`].

use std::cell::RefCell;
use std::ffi::CString;
use std::io::{self, BufWriter, Write};
use std::os::raw::c_int;

use quickjs_sys as qjs;

use crate::node::buffer::buffer_bytes_from_value;
use crate::qjs_compat;

thread_local! {
    static PROC_STDOUT: RefCell<BufWriter<io::Stdout>> =
        RefCell::new(BufWriter::new(io::stdout()));
    static PROC_STDERR: RefCell<BufWriter<io::Stderr>> =
        RefCell::new(BufWriter::new(io::stderr()));
}

#[inline]
fn is_exception(value: qjs::JSValue) -> bool {
    value.tag == qjs::JS_TAG_EXCEPTION as i64
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

/// Attaches `stdout` / `stderr` objects with `write` / `writeSync` (buffered host I/O).
///
/// # Safety
/// `ctx` and `process` must be valid QuickJS handles.
pub unsafe fn install_stdio(ctx: *mut qjs::JSContext, process: qjs::JSValue) -> Result<(), String> {
    let stdout = qjs::JS_NewObject(ctx);
    install_obj_fn(ctx, stdout, "writeSync", Some(js_stdout_write_sync), 1)?;
    install_obj_fn(ctx, stdout, "write", Some(js_stdout_write), 3)?;
    qjs::JS_SetPropertyStr(
        ctx,
        process,
        CString::new("stdout").unwrap().as_ptr(),
        stdout,
    );

    let stderr = qjs::JS_NewObject(ctx);
    install_obj_fn(ctx, stderr, "writeSync", Some(js_stderr_write_sync), 1)?;
    install_obj_fn(ctx, stderr, "write", Some(js_stderr_write), 3)?;
    qjs::JS_SetPropertyStr(
        ctx,
        process,
        CString::new("stderr").unwrap().as_ptr(),
        stderr,
    );

    Ok(())
}

unsafe extern "C" fn js_stdout_write_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs_compat::new_int(ctx, 0);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let bytes = buffer_bytes_from_value(ctx, args[0]);
    match PROC_STDOUT.with(|b| b.borrow_mut().write_all(&bytes)) {
        Ok(()) => {
            let _ = PROC_STDOUT.with(|b| b.borrow_mut().flush());
            qjs_compat::new_int(ctx, bytes.len() as i64)
        }
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("process.stdout.writeSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

unsafe extern "C" fn js_stderr_write_sync(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs_compat::new_int(ctx, 0);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let bytes = buffer_bytes_from_value(ctx, args[0]);
    match PROC_STDERR.with(|b| b.borrow_mut().write_all(&bytes)) {
        Ok(()) => {
            let _ = PROC_STDERR.with(|b| b.borrow_mut().flush());
            qjs_compat::new_int(ctx, bytes.len() as i64)
        }
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("process.stderr.writeSync failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    }
}

/// `write(chunk[, encoding][, callback])` — synchronous write; optional callback invoked with no args on success.
unsafe extern "C" fn js_stdout_write(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs_compat::new_int(ctx, 0);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let callback = if argc >= 2 && qjs::JS_IsFunction(ctx, args[(argc - 1) as usize]) != 0 {
        Some(args[(argc - 1) as usize])
    } else {
        None
    };
    let chunk = args[0];
    let bytes = buffer_bytes_from_value(ctx, chunk);
    let ret = match PROC_STDOUT.with(|b| b.borrow_mut().write_all(&bytes)) {
        Ok(()) => {
            let _ = PROC_STDOUT.with(|b| b.borrow_mut().flush());
            qjs_compat::new_int(ctx, bytes.len() as i64)
        }
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("process.stdout.write failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    };
    if !is_exception(ret) {
        if let Some(cb) = callback {
            let r = qjs::JS_Call(ctx, cb, this, 0, std::ptr::null_mut());
            crate::ffi::js_free_value(ctx, r);
        }
    }
    ret
}

unsafe extern "C" fn js_stderr_write(
    ctx: *mut qjs::JSContext,
    this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs_compat::new_int(ctx, 0);
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let callback = if argc >= 2 && qjs::JS_IsFunction(ctx, args[(argc - 1) as usize]) != 0 {
        Some(args[(argc - 1) as usize])
    } else {
        None
    };
    let chunk = args[0];
    let bytes = buffer_bytes_from_value(ctx, chunk);
    let ret = match PROC_STDERR.with(|b| b.borrow_mut().write_all(&bytes)) {
        Ok(()) => {
            let _ = PROC_STDERR.with(|b| b.borrow_mut().flush());
            qjs_compat::new_int(ctx, bytes.len() as i64)
        }
        Err(e) => qjs::JS_ThrowTypeError(
            ctx,
            CString::new(format!("process.stderr.write failed: {e}"))
                .unwrap_or_default()
                .as_ptr(),
        ),
    };
    if !is_exception(ret) {
        if let Some(cb) = callback {
            let r = qjs::JS_Call(ctx, cb, this, 0, std::ptr::null_mut());
            crate::ffi::js_free_value(ctx, r);
        }
    }
    ret
}
