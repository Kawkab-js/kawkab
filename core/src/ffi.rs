// bridge ffi primitives
//
// ZERO-COPY FFI PRIMITIVES
// ════════════════════════
// Rules for every function in this module:
//   * Never clone a JS string if we only need to read it (use JS_ToCStringLen).
//   * Never allocate an intermediate Vec — pass slices directly to consumers.
//   * Always free JSValues we don't own (QuickJS is ref-counted).
//   * Tag all unsafe blocks with a // SAFETY: comment.

use crate::qjs_compat;
use quickjs_sys as qjs;
use std::{ffi::CStr, os::raw::c_int, slice, sync::Arc};

// ── Reference Counting ────────────────────────────────────────────────────────

/// Proper Rust equivalent of the C inline `JS_FreeValue`.
///
/// `__JS_FreeValue` is the *raw* free (called when ref_count already hit 0).
/// The C inline first decrements ref_count and only calls `__JS_FreeValue`
/// when it reaches 0. We must do the same to avoid the
/// `free_object: ref_count == 0` assertion crash.
///
/// # Safety
/// `ctx` must be valid. `v` must be a JSValue whose ref_count we own a share of.
#[inline]
pub unsafe fn js_free_value(ctx: *mut qjs::JSContext, v: qjs::JSValue) {
    // Tags < 0 are ref-counted (objects, strings, symbols, etc.).
    if v.tag < 0 {
        // Every ref-counted QJS heap object is at least ref_count large at the start.
        let p = v.u.ptr as *mut qjs::JSRefCountHeader;
        (*p).ref_count -= 1;
        if (*p).ref_count <= 0 {
            qjs::__JS_FreeValue(ctx, v);
        }
    }
}

/// Proper Rust equivalent of the C inline `JS_DupValue`.
///
/// Increments ref_count if the value is ref-counted.
///
/// # Safety
/// `v` must be a valid JSValue.
#[inline]
pub unsafe fn js_dup_value(v: qjs::JSValue) -> qjs::JSValue {
    if v.tag < 0 {
        let p = v.u.ptr as *mut qjs::JSRefCountHeader;
        (*p).ref_count += 1;
    }
    v
}

// ── String extraction ─────────────────────────────────────────────────────────

/// Borrow the UTF-8 bytes of a JS string value without allocating.
///
/// `f` receives a `&[u8]` slice that is valid only for the duration of the
/// call. Do not store the slice beyond `f`.
///
/// # Safety
/// `ctx` and `val` must be valid for the current thread.
pub unsafe fn with_js_string<F, R>(ctx: *mut qjs::JSContext, val: qjs::JSValue, f: F) -> R
where
    F: FnOnce(&[u8]) -> R,
{
    let mut len: c_int = 0;
    // JS_ToCStringLen returns a pointer into QJS's internal string storage
    // (or a small stack buffer for short strings). No heap allocation occurs
    // for ASCII strings below ~64 bytes — the common case for log lines.
    // cesu8=0: return standard UTF-8 (not CESU-8).
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

/// Extract a JS string to an owned `String`. Use only when ownership is needed.
///
/// # Safety
/// Same as `with_js_string`.
pub unsafe fn js_string_to_owned(ctx: *mut qjs::JSContext, val: qjs::JSValue) -> String {
    with_js_string(ctx, val, |b| String::from_utf8_lossy(b).into_owned())
}

// ── ArrayBuffer access ────────────────────────────────────────────────────────

/// Access the raw bytes of a JS ArrayBuffer without copying.
///
/// Returns `None` if `val` is not an ArrayBuffer.
///
/// # Safety
/// The returned slice borrows QuickJS's GC-managed memory. The GC will not
/// collect `val`'s buffer while you hold a Rust reference to the Isolate,
/// because the Isolate is !Send and the GC only runs when you call into QJS.
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

/// Create a JS ArrayBuffer from a Rust byte slice without copying if the
/// slice lives long enough.
///
/// When `data` is backed by an `Arc<[u8]>` handed in from the I/O layer we
/// use `JS_NewArrayBufferCopy` (one allocation inside QJS). For truly
/// zero-copy paths we'd need a custom JS_NewArrayBuffer with a free callback
/// that decrements the Arc ref-count — see `arraybuffer_from_arc`.
///
/// # Safety
/// `ctx` must be valid on the current thread.
pub unsafe fn arraybuffer_from_slice(ctx: *mut qjs::JSContext, data: &[u8]) -> qjs::JSValue {
    qjs::JS_NewArrayBufferCopy(ctx, data.as_ptr(), data.len())
}

/// Create a JS ArrayBuffer from an `Arc<[u8]>` without copying.
///
/// The `Arc` is boxed into the ArrayBuffer `opaque` handle and freed by
/// `free_arc_cb` when the buffer is collected (same lifetime model as a
/// typical `free_arc_buffer` hook for shared Rust backing stores).
///
/// The `Arc` is stashed in the ArrayBuffer's `opaque` field and is
/// automatically decremented when the ArrayBuffer is garbage collected.
///
/// # Safety
/// `ctx` must be valid on the current thread.
pub unsafe fn arraybuffer_from_arc(ctx: *mut qjs::JSContext, data: Arc<[u8]>) -> qjs::JSValue {
    let len = data.len();
    let buf = data.as_ptr() as *mut u8;
    // We must Box the Arc because Arc<[u8]> is a wide pointer (2 words),
    // but the `opaque` field in JS_NewArrayBuffer is a thin pointer (1 word).
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

/// `ArrayBuffer` backed by a subslice of `data`; the **entire** `Arc` is kept alive until the buffer is freed.
///
/// # Safety
/// `ctx` must be valid. `start + len` must be within `data.len()`.
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
        // Should not happen when called from the HTTP stack; avoid UB if it does.
        return arraybuffer_from_slice(ctx, &[]);
    }
    let buf = data.as_ptr().wrapping_add(start) as *mut u8;
    let opaque = Box::into_raw(Box::new(data)) as *mut std::os::raw::c_void;
    qjs::JS_NewArrayBuffer(ctx, buf, len, Some(free_arc_cb), opaque, 0)
}

// ── Property setters ──────────────────────────────────────────────────────────

/// Install a native Rust function as `obj.name` in JavaScript.
///
/// # Safety
/// `ctx` and `obj` must be valid. `func` must be a valid C function pointer
/// with the QuickJS callback signature.
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
    // JS_SetPropertyStr takes ownership of `f`; do NOT free it.
    qjs::JS_SetPropertyStr(ctx, obj, name.as_ptr(), f);
}

/// Set `obj.name = value` (takes ownership of `value`).
///
/// # Safety
/// `ctx`, `obj`, `value` must be valid.
pub unsafe fn set_property_str(
    ctx: *mut qjs::JSContext,
    obj: qjs::JSValue,
    name: &CStr,
    value: qjs::JSValue,
) {
    // JS_SetPropertyStr takes ownership of `value`.
    qjs::JS_SetPropertyStr(ctx, obj, name.as_ptr(), value);
}

// ── Error helpers ─────────────────────────────────────────────────────────────

/// Throw a JS TypeError and return JS_EXCEPTION (the sentinel JSValue).
///
/// # Safety
/// `ctx` must be valid on the current thread.
#[inline]
pub unsafe fn throw_type_error(ctx: *mut qjs::JSContext, msg: &str) -> qjs::JSValue {
    let c = std::ffi::CString::new(msg).unwrap_or_default();
    qjs::JS_ThrowTypeError(ctx, c.as_ptr())
}

/// Throw a JS RangeError.
///
/// # Safety
/// Same as `throw_type_error`.
#[inline]
pub unsafe fn throw_range_error(ctx: *mut qjs::JSContext, msg: &str) -> qjs::JSValue {
    let c = std::ffi::CString::new(msg).unwrap_or_default();
    qjs::JS_ThrowRangeError(ctx, c.as_ptr())
}
