use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

use quickjs_sys as qjs;

#[inline]
pub unsafe fn eval(
    ctx: *mut qjs::JSContext,
    input: *const c_char,
    input_len: usize,
    filename: *const c_char,
    eval_flags: c_int,
) -> qjs::JSValue {
    qjs::JS_Eval(ctx, input, input_len, filename, eval_flags)
}

#[inline]
pub unsafe fn set_memory_limit(rt: *mut qjs::JSRuntime, limit: usize) {
    qjs::JS_SetMemoryLimit(rt, limit);
}

#[inline]
pub unsafe fn set_max_stack_size(ctx: *mut qjs::JSContext, stack_size: usize) {
    let rt = qjs::JS_GetRuntime(ctx);
    qjs::JS_SetMaxStackSize(rt, stack_size);
}

#[inline]
pub unsafe fn new_int(ctx: *mut qjs::JSContext, value: i64) -> qjs::JSValue {
    if value < i32::MIN as i64 {
        return qjs::JS_NewInt32(ctx, i32::MIN);
    }
    if value > i32::MAX as i64 {
        return qjs::JS_NewInt32(ctx, i32::MAX);
    }
    qjs::JS_NewInt32(ctx, value as i32)
}

#[inline]
pub unsafe fn to_c_string_len(
    ctx: *mut qjs::JSContext,
    plen: *mut c_int,
    val: qjs::JSValue,
) -> *const c_char {
    let mut out_len: usize = 0;
    let ptr = qjs::JS_ToCStringLen2(ctx, &mut out_len as *mut usize, val, 0);
    if !plen.is_null() {
        *plen = out_len as c_int;
    }
    ptr
}

#[inline]
pub unsafe fn new_string_from_cstr(ctx: *mut qjs::JSContext, value: *const c_char) -> qjs::JSValue {
    if value.is_null() {
        return qjs::JS_NewStringLen(ctx, b"".as_ptr() as *const c_char, 0);
    }
    let s = CStr::from_ptr(value).to_bytes();
    qjs::JS_NewStringLen(ctx, s.as_ptr() as *const c_char, s.len())
}

#[inline]
pub unsafe fn new_string_from_str(ctx: *mut qjs::JSContext, value: &str) -> qjs::JSValue {
    let c = std::ffi::CString::new(value).unwrap_or_default();
    new_string_from_cstr(ctx, c.as_ptr())
}
