use crate::qjs_compat;
use quickjs_sys as qjs;
use std::{ffi::CStr, os::raw::c_int, slice, sync::Arc};

/// Mirror QuickJS `JS_FreeValue`: decrement refcount, call `__JS_FreeValue` at zero.
///
/// # Safety
/// Valid `ctx`; caller holds a refcount share of `v`.
#[inline]
pub unsafe fn js_free_value(ctx: *mut qjs::JSContext, v: qjs::JSValue) {
    if v.tag < 0 {
        let p = v.u.ptr as *mut qjs::JSRefCountHeader;
        (*p).ref_count -= 1;
        if (*p).ref_count <= 0 {
            qjs::__JS_FreeValue(ctx, v);
        }
    }
}

/// Mirror QuickJS `JS_DupValue` for ref-counted tags.
///
/// # Safety
/// `v` must be valid.
#[inline]
pub unsafe fn js_dup_value(v: qjs::JSValue) -> qjs::JSValue {
    if v.tag < 0 {
        let p = v.u.ptr as *mut qjs::JSRefCountHeader;
        (*p).ref_count += 1;
    }
    v
}

/// Borrow UTF-8 string bytes for the duration of `f` (via `JS_ToCStringLen`).
///
/// # Safety
/// Valid `ctx` and `val` on this thread; do not retain the slice past `f`.
pub unsafe fn with_js_string<F, R>(ctx: *mut qjs::JSContext, val: qjs::JSValue, f: F) -> R
where
    F: FnOnce(&[u8]) -> R,
{
    let mut len: c_int = 0;
    let ptr = qjs_compat::to_c_string_len(ctx, &mut len, val);
    let bytes = if ptr.is_null() {
        b"" as &[u8]
    } else {
        // SAFETY: ptr is valid for `len` bytes; we release it after `f`.
        slice::from_raw_parts(ptr as *const u8, len.max(0) as usize)
    };

    let result = f(bytes);

    if !ptr.is_null() {
        qjs::JS_FreeCString(ctx, ptr);
    }
    result
}

/// Copy a JS string into an owned `String`.
///
/// # Safety
/// Same as [`with_js_string`].
pub unsafe fn js_string_to_owned(ctx: *mut qjs::JSContext, val: qjs::JSValue) -> String {
    with_js_string(ctx, val, |b| String::from_utf8_lossy(b).into_owned())
}

/// View an ArrayBuffer's bytes; `None` if `val` is not an ArrayBuffer.
///
/// # Safety
/// Slice borrows QJS memory; valid while isolate access stays pinned to this thread.
pub unsafe fn arraybuffer_bytes<'a>(
    ctx: *mut qjs::JSContext,
    val: qjs::JSValue,
) -> Option<&'a [u8]> {
    let mut size: usize = 0;
    let ptr = qjs::JS_GetArrayBuffer(ctx, &mut size, val);
    if ptr.is_null() {
        None
    } else {
        Some(slice::from_raw_parts(ptr, size))
    }
}

/// `JS_NewArrayBufferCopy` — always copies `data` into the heap object.
///
/// # Safety
/// Valid `ctx` on this thread.
pub unsafe fn arraybuffer_from_slice(ctx: *mut qjs::JSContext, data: &[u8]) -> qjs::JSValue {
    qjs::JS_NewArrayBufferCopy(ctx, data.as_ptr(), data.len())
}

/// ArrayBuffer backed by `Arc<[u8]>`; `free_arc_cb` drops the `Arc` when JS collects the buffer.
///
/// # Safety
/// Valid `ctx` on this thread.
pub unsafe fn arraybuffer_from_arc(ctx: *mut qjs::JSContext, data: Arc<[u8]>) -> qjs::JSValue {
    let len = data.len();
    let buf = data.as_ptr() as *mut u8;
    let opaque = Box::into_raw(Box::new(data)) as *mut std::os::raw::c_void;

    qjs::JS_NewArrayBuffer(ctx, buf, len, Some(free_arc_cb), opaque, 0)
}

/// Called by QuickJS when an `ArrayBuffer` created via [`arraybuffer_from_arc`] is freed.
pub unsafe extern "C" fn free_arc_cb(
    _rt: *mut qjs::JSRuntime,
    opaque: *mut std::os::raw::c_void,
    _ptr: *mut std::os::raw::c_void,
) {
    // SAFETY: opaque was created via Box::into_raw in arraybuffer_from_arc.
    let _ = Box::from_raw(opaque as *mut Arc<[u8]>);
}

/// Like [`arraybuffer_from_arc`], exposing only `[start..start+len)` while retaining the full `Arc`.
///
/// # Safety
/// Valid `ctx`; `start + len <= data.len()`.
pub unsafe fn arraybuffer_from_arc_slice(
    ctx: *mut qjs::JSContext,
    data: Arc<[u8]>,
    start: usize,
    len: usize,
) -> qjs::JSValue {
    if len == 0 {
        return arraybuffer_from_slice(ctx, &[]);
    }
    if start.saturating_add(len) > data.len() {
        return arraybuffer_from_slice(ctx, &[]);
    }
    let buf = data.as_ptr().wrapping_add(start) as *mut u8;
    let opaque = Box::into_raw(Box::new(data)) as *mut std::os::raw::c_void;
    qjs::JS_NewArrayBuffer(ctx, buf, len, Some(free_arc_cb), opaque, 0)
}

/// Set `obj[name]` to a native `JSCFunction` (`JS_SetPropertyStr` takes `f`).
///
/// # Safety
/// Valid `ctx`/`obj`; `func` matches the QuickJS C callback ABI.
pub unsafe fn set_function(
    ctx: *mut qjs::JSContext,
    obj: qjs::JSValue,
    name: &CStr,
    func: qjs::JSCFunction,
    length: i32, // number of formal parameters (for .length property)
) {
    let f = qjs::JS_NewCFunction2(
        ctx,
        func,
        name.as_ptr(),
        length,
        qjs::JSCFunctionEnum_JS_CFUNC_generic,
        0,
    );
    qjs::JS_SetPropertyStr(ctx, obj, name.as_ptr(), f);
}

/// `obj[name] = value` (`JS_SetPropertyStr` consumes `value`).
///
/// # Safety
/// Valid `ctx`, `obj`, `value`.
pub unsafe fn set_property_str(
    ctx: *mut qjs::JSContext,
    obj: qjs::JSValue,
    name: &CStr,
    value: qjs::JSValue,
) {
    qjs::JS_SetPropertyStr(ctx, obj, name.as_ptr(), value);
}

/// Throw `TypeError` and return the exception sentinel.
///
/// # Safety
/// Valid `ctx` on this thread.
#[inline]
pub unsafe fn throw_type_error(ctx: *mut qjs::JSContext, msg: &str) -> qjs::JSValue {
    let c = std::ffi::CString::new(msg).unwrap_or_default();
    qjs::JS_ThrowTypeError(ctx, c.as_ptr())
}

/// Throw `RangeError`.
///
/// # Safety
/// Same as [`throw_type_error`].
#[inline]
pub unsafe fn throw_range_error(ctx: *mut qjs::JSContext, msg: &str) -> qjs::JSValue {
    let c = std::ffi::CString::new(msg).unwrap_or_default();
    qjs::JS_ThrowRangeError(ctx, c.as_ptr())
}
