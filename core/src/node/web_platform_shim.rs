//! Web platform globals (Blob, streams, compression, messaging) for Node-style embedding.

use std::ffi::CString;
use std::io::Read;
use std::os::raw::c_int;
use std::sync::Arc;

use quickjs_sys as qjs;

use crate::ffi::{arraybuffer_from_arc, js_free_value, js_string_to_owned, throw_type_error};
use crate::qjs_compat;

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

unsafe extern "C" fn js_web_compress(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return throw_type_error(
            ctx,
            "__kawkabWebCompress(format, data) requires 2 arguments",
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let format = js_string_to_owned(ctx, args[0]);
    let input = super::buffer::buffer_bytes_from_value(ctx, args[1]);
    let result: Result<Vec<u8>, String> = match format.as_str() {
        "gzip" => {
            let mut enc = flate2::read::GzEncoder::new(
                std::io::Cursor::new(input),
                flate2::Compression::default(),
            );
            let mut v = Vec::new();
            match enc.read_to_end(&mut v) {
                Ok(_) => Ok(v),
                Err(e) => Err(format!("gzip compress: {e}")),
            }
        }
        "deflate" => {
            let mut enc = flate2::read::DeflateEncoder::new(
                std::io::Cursor::new(input),
                flate2::Compression::default(),
            );
            let mut v = Vec::new();
            match enc.read_to_end(&mut v) {
                Ok(_) => Ok(v),
                Err(e) => Err(format!("deflate compress: {e}")),
            }
        }
        _ => Err(format!("unsupported compression format: {format}")),
    };
    match result {
        Ok(v) => unsafe { arraybuffer_from_arc(ctx, Arc::from(v.into_boxed_slice())) },
        Err(e) => throw_type_error(ctx, &e),
    }
}

unsafe extern "C" fn js_web_decompress(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 2 {
        return throw_type_error(
            ctx,
            "__kawkabWebDecompress(format, data) requires 2 arguments",
        );
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let format = js_string_to_owned(ctx, args[0]);
    let input = super::buffer::buffer_bytes_from_value(ctx, args[1]);
    let result: Result<Vec<u8>, String> = match format.as_str() {
        "gzip" => {
            let mut dec = flate2::read::GzDecoder::new(std::io::Cursor::new(input));
            let mut v = Vec::new();
            match dec.read_to_end(&mut v) {
                Ok(_) => Ok(v),
                Err(e) => Err(format!("gzip decompress: {e}")),
            }
        }
        "deflate" => {
            let mut dec = flate2::read::DeflateDecoder::new(std::io::Cursor::new(input));
            let mut v = Vec::new();
            match dec.read_to_end(&mut v) {
                Ok(_) => Ok(v),
                Err(e) => Err(format!("deflate decompress: {e}")),
            }
        }
        _ => Err(format!("unsupported decompression format: {format}")),
    };
    match result {
        Ok(v) => unsafe { arraybuffer_from_arc(ctx, Arc::from(v.into_boxed_slice())) },
        Err(e) => throw_type_error(ctx, &e),
    }
}

/// Installs `Blob`, `FormData`, web streams, `CompressionStream`, messaging globals, and natives
/// `__kawkabWebCompress` / `__kawkabWebDecompress`. Idempotent via `__kawkabPrimedWebPlatform`.
///
/// # Safety
/// `ctx` and `global` must be the realm global; call after [`super::buffer::install`].
pub unsafe fn install(ctx: *mut qjs::JSContext, global: qjs::JSValue) -> Result<(), String> {
    let key = CString::new("__kawkabPrimedWebPlatform").map_err(|e| e.to_string())?;
    let existing = qjs::JS_GetPropertyStr(ctx, global, key.as_ptr());
    let already = !qjs::JS_IsUndefined(existing);
    js_free_value(ctx, existing);
    if already {
        return Ok(());
    }

    install_c_fn(ctx, global, "__kawkabWebCompress", Some(js_web_compress), 2)?;
    install_c_fn(
        ctx,
        global,
        "__kawkabWebDecompress",
        Some(js_web_decompress),
        2,
    )?;

    let src = include_str!("web_platform_shim.js");
    let c_src = CString::new(src).map_err(|e| e.to_string())?;
    let file = CString::new("kawkab:web-platform-shim").map_err(|e| e.to_string())?;
    let val = qjs_compat::eval(
        ctx,
        c_src.as_ptr(),
        c_src.as_bytes().len(),
        file.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    );
    if super::is_exception(val) {
        let exc = qjs::JS_GetException(ctx);
        let detail = js_string_to_owned(ctx, exc);
        js_free_value(ctx, exc);
        js_free_value(ctx, val);
        return Err(format!("web platform shim eval failed: {detail}"));
    }
    js_free_value(ctx, val);

    let flag = qjs::JS_NewBool(ctx, true);
    qjs::JS_SetPropertyStr(ctx, global, key.as_ptr(), flag);
    Ok(())
}
