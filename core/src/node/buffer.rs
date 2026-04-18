//! Native `Buffer` helpers/bootstrap (`Uint8Array` subclass) with zero-copy paths.

use std::ffi::CString;
use std::os::raw::c_int;
use std::sync::Arc;

use base64::Engine;
use quickjs_sys as qjs;

use crate::ffi::{js_free_value, js_string_to_owned, with_js_string};
use crate::qjs_compat;

/// Upper bound for `Buffer.alloc` / `Buffer.from([...])` to reduce accidental OOM.
pub(crate) const KAWKAB_MAX_BUFFER_BYTES: usize = 1usize << 30;

#[inline]
fn is_exception(value: qjs::JSValue) -> bool {
    value.tag == qjs::JS_TAG_EXCEPTION as i64
}

#[inline]
fn js_undefined() -> qjs::JSValue {
    qjs::JSValue {
        u: qjs::JSValueUnion { int32: 0 },
        tag: qjs::JS_TAG_UNDEFINED as i64,
    }
}

const BUFFER_BOOTSTRAP_SRC: &str = r##"
var __kbF = __kawkabBufferFrom;
var __kbA = __kawkabBufferAlloc;
var __kbAU = __kawkabBufferAllocUnsafe;
var __kbC = __kawkabBufferConcat;
var __kbBL = __kawkabBufferByteLength;
var __kbTS = __kawkabBufferToString;
class __kbRealBuffer extends Uint8Array {
  constructor(a, b, c) {
    if (arguments.length === 0) {
      throw new TypeError("Buffer constructor: first argument required");
    }
    if (typeof a === "number") {
      if (b !== undefined || c !== undefined) throw new TypeError("Invalid arguments");
      super(Math.max(0, Math.floor(a)));
      return;
    }
    var u = __kbF.apply(null, arguments);
    super(u.buffer, u.byteOffset, u.byteLength);
  }
}
function __kbWrap(b) {
  Object.setPrototypeOf(b, __kbRealBuffer.prototype);
  return b;
}
__kbRealBuffer.from = function from() { return __kbWrap(__kbF.apply(null, arguments)); };
__kbRealBuffer.alloc = function alloc() { return __kbWrap(__kbA.apply(null, arguments)); };
__kbRealBuffer.allocUnsafe = function allocUnsafe(n) { return __kbWrap(__kbAU(n)); };
__kbRealBuffer.allocUnsafeSlow = __kbRealBuffer.allocUnsafe;
__kbRealBuffer.concat = function concat(list, tl) { return __kbWrap(__kbC(list, tl)); };
__kbRealBuffer.byteLength = function byteLength(str, enc) { return __kbBL(str, enc); };
var KawkabBufferCtor = function KawkabBufferCtor() {
  var args = arguments;
  if (new.target === undefined) {
    if (args.length === 0) throw new TypeError("Buffer: first argument required");
    if (typeof args[0] === "number" && args.length === 1) return new __kbRealBuffer(args[0]);
    return __kbRealBuffer.from.apply(__kbRealBuffer, args);
  }
  return Reflect.construct(__kbRealBuffer, args, new.target);
};
Object.setPrototypeOf(KawkabBufferCtor, __kbRealBuffer);
KawkabBufferCtor.prototype = __kbRealBuffer.prototype;
__kbRealBuffer.isBuffer = function isBuffer(b) { return b != null && b instanceof KawkabBufferCtor; };
__kbRealBuffer.compare = function compare(a, b) {
  if (!KawkabBufferCtor.isBuffer(a) || !KawkabBufferCtor.isBuffer(b))
    throw new TypeError("Parameters must be Buffers");
  var i, ml = Math.min(a.length, b.length);
  for (i = 0; i < ml; i++) {
    if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  }
  if (a.length === b.length) return 0;
  return a.length < b.length ? -1 : 1;
};
__kbRealBuffer.prototype.equals = function equals(other) {
  if (!KawkabBufferCtor.isBuffer(other) || this.length !== other.length) return false;
  for (var i = 0; i < this.length; i++) if (this[i] !== other[i]) return false;
  return true;
};
__kbRealBuffer.prototype.toString = function toString(enc, start, end) {
  return __kbTS(this, enc, start, end);
};
__kbRealBuffer.prototype.slice = function slice(s, e) {
  return __kbWrap(Uint8Array.prototype.slice.call(this, s, e));
};
__kbRealBuffer.prototype.subarray = function subarray(s, e) {
  return __kbWrap(Uint8Array.prototype.subarray.call(this, s, e));
};
global.Buffer = KawkabBufferCtor;
globalThis.Buffer = KawkabBufferCtor;
"##;

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

/// Installs `__kawkabBuffer*` natives and runs the `Buffer` constructor bootstrap.
///
/// # Safety
/// `ctx`/`global` must be valid; frees `global` on bootstrap failure.
pub unsafe fn install(ctx: *mut qjs::JSContext, global: qjs::JSValue) -> Result<(), String> {
    install_c_fn(ctx, global, "__kawkabBufferFrom", Some(js_buffer_from), 3)?;
    install_c_fn(ctx, global, "__kawkabBufferAlloc", Some(js_buffer_alloc), 3)?;
    install_c_fn(
        ctx,
        global,
        "__kawkabBufferAllocUnsafe",
        Some(js_buffer_alloc_unsafe),
        1,
    )?;
    install_c_fn(
        ctx,
        global,
        "__kawkabBufferConcat",
        Some(js_buffer_concat),
        2,
    )?;
    install_c_fn(
        ctx,
        global,
        "__kawkabBufferByteLength",
        Some(js_buffer_byte_length),
        2,
    )?;
    install_c_fn(
        ctx,
        global,
        "__kawkabBufferToString",
        Some(js_buffer_to_string),
        4,
    )?;
    let buf_file = CString::new("buffer-bootstrap.js").map_err(|e| e.to_string())?;
    let code = CString::new(BUFFER_BOOTSTRAP_SRC.trim()).map_err(|e| e.to_string())?;
    let buf_boot = qjs_compat::eval(
        ctx,
        code.as_ptr(),
        code.as_bytes().len(),
        buf_file.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    );
    if is_exception(buf_boot) {
        let exc = qjs::JS_GetException(ctx);
        let detail = crate::ffi::js_string_to_owned(ctx, exc);
        js_free_value(ctx, exc);
        js_free_value(ctx, buf_boot);
        js_free_value(ctx, global);
        return Err(format!("buffer bootstrap failed: {detail}"));
    }
    js_free_value(ctx, buf_boot);
    Ok(())
}

fn decode_hex_bytes(s: &str) -> Result<Vec<u8>, ()> {
    let mut digits: Vec<u8> = Vec::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_hexdigit() {
            digits.push(b);
        }
    }
    if digits.len() % 2 != 0 {
        return Err(());
    }
    let mut out = Vec::with_capacity(digits.len() / 2);
    for chunk in digits.chunks(2) {
        let hi = (chunk[0] as char).to_digit(16).ok_or(())?;
        let lo = (chunk[1] as char).to_digit(16).ok_or(())?;
        out.push((hi * 16 + lo) as u8);
    }
    Ok(out)
}

pub(crate) fn string_to_buffer_bytes(s: &str, enc: &str) -> Result<Vec<u8>, String> {
    let e = enc.trim();
    match e.to_ascii_lowercase().as_str() {
        "utf8" | "utf-8" | "ucs2" => Ok(s.as_bytes().to_vec()),
        "hex" => decode_hex_bytes(s).map_err(|_| "Invalid hex encoding".to_string()),
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map_err(|_| "Invalid base64".to_string()),
        "base64url" => base64::engine::general_purpose::URL_SAFE
            .decode(s.as_bytes())
            .map_err(|_| "Invalid base64url".to_string()),
        "latin1" | "binary" | "buffer" => {
            Ok(s.chars().map(|c| (c as u32).min(255) as u8).collect())
        }
        _ => Err(format!("Unsupported Buffer encoding: {e}")),
    }
}

unsafe fn typed_array_view_byte_len(
    ctx: *mut qjs::JSContext,
    value: qjs::JSValue,
) -> Option<usize> {
    if value.tag != qjs::JS_TAG_OBJECT as i64 {
        return None;
    }
    if let Some(b) = crate::ffi::arraybuffer_bytes(ctx, value) {
        return Some(b.len());
    }
    let mut off: usize = 0;
    let mut len: usize = 0;
    let mut el: usize = 0;
    let ab = qjs::JS_GetTypedArrayBuffer(ctx, value, &mut off, &mut len, &mut el);
    if qjs::JS_IsObject(ab) {
        js_free_value(ctx, ab);
        return Some(len);
    }
    js_free_value(ctx, ab);
    None
}

unsafe fn buffer_byte_len_for_string_or_view(
    ctx: *mut qjs::JSContext,
    value: qjs::JSValue,
    enc_arg: qjs::JSValue,
    argc: c_int,
) -> Result<usize, qjs::JSValue> {
    if qjs::JS_IsString(value) {
        let s = js_string_to_owned(ctx, value);
        let enc = if argc >= 2 && !qjs::JS_IsUndefined(enc_arg) {
            js_string_to_owned(ctx, enc_arg)
        } else {
            "utf8".to_string()
        };
        return match string_to_buffer_bytes(&s, &enc) {
            Ok(bytes) => Ok(bytes.len()),
            Err(msg) => Err(crate::ffi::throw_type_error(ctx, &msg)),
        };
    }
    if let Some(n) = typed_array_view_byte_len(ctx, value) {
        return Ok(n);
    }
    Err(crate::ffi::throw_type_error(
        ctx,
        "argument must be a string, Buffer, ArrayBuffer, or DataView",
    ))
}

unsafe fn buffer_to_string_encoded(
    ctx: *mut qjs::JSContext,
    bytes: &[u8],
    enc: &str,
) -> Result<qjs::JSValue, ()> {
    let e_trim = enc.trim();
    let e = if e_trim.is_empty() { "utf8" } else { e_trim };
    let out = match e.to_ascii_lowercase().as_str() {
        "utf8" | "utf-8" => String::from_utf8_lossy(bytes).into_owned(),
        "latin1" | "binary" | "ascii" => bytes.iter().map(|&b| b as char).collect::<String>(),
        "hex" => {
            let mut s = String::with_capacity(bytes.len().saturating_mul(2));
            const HEX: &[u8; 16] = b"0123456789abcdef";
            for &b in bytes {
                s.push(HEX[(b >> 4) as usize] as char);
                s.push(HEX[(b & 0xf) as usize] as char);
            }
            s
        }
        "base64" => base64::engine::general_purpose::STANDARD.encode(bytes),
        "base64url" => base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
        "utf16le" | "ucs2" => {
            let mut u16s = Vec::with_capacity(bytes.len() / 2 + 1);
            let mut chunks = bytes.chunks_exact(2);
            for c in &mut chunks {
                u16s.push(u16::from_le_bytes([c[0], c[1]]));
            }
            let r = chunks.remainder();
            if !r.is_empty() {
                u16s.push(u16::from_le_bytes([r[0], 0]));
            }
            String::from_utf16_lossy(&u16s)
        }
        _ => return Err(()),
    };
    Ok(qjs_compat::new_string_from_str(ctx, &out))
}

pub unsafe fn buffer_uint8_from_arc(ctx: *mut qjs::JSContext, data: Arc<[u8]>) -> qjs::JSValue {
    let ab = crate::ffi::arraybuffer_from_arc(ctx, data);
    if is_exception(ab) {
        return ab;
    }
    let off = qjs_compat::new_int(ctx, 0);
    let mut argv = [ab, off, js_undefined()];
    let ta = qjs::JS_NewTypedArray(
        ctx,
        3,
        argv.as_mut_ptr(),
        qjs::JSTypedArrayEnum_JS_TYPED_ARRAY_UINT8,
    );
    js_free_value(ctx, ab);
    js_free_value(ctx, off);
    ta
}

/// Raw bytes from JS string/ArrayBuffer/typed-array or `__kawkab_buffer_data` shim.
pub unsafe fn buffer_bytes_from_value(ctx: *mut qjs::JSContext, value: qjs::JSValue) -> Vec<u8> {
    if value.tag == qjs::JS_TAG_OBJECT as i64 {
        let key = CString::new("__kawkab_buffer_data").unwrap();
        let data = qjs::JS_GetPropertyStr(ctx, value, key.as_ptr());
        if data.tag != qjs::JS_TAG_UNDEFINED as i64 {
            let out = with_js_string(ctx, data, |b| b.to_vec());
            js_free_value(ctx, data);
            return out;
        }
        js_free_value(ctx, data);
        if let Some(b) = crate::ffi::arraybuffer_bytes(ctx, value) {
            return b.to_vec();
        }
        let mut off: usize = 0;
        let mut len: usize = 0;
        let mut el: usize = 0;
        let ab = qjs::JS_GetTypedArrayBuffer(ctx, value, &mut off, &mut len, &mut el);
        if qjs::JS_IsObject(ab) {
            let mut ab_size: usize = 0;
            let ptr = qjs::JS_GetArrayBuffer(ctx, &mut ab_size, ab);
            let out = if !ptr.is_null() && off + len <= ab_size {
                std::slice::from_raw_parts(ptr.add(off), len).to_vec()
            } else {
                Vec::new()
            };
            js_free_value(ctx, ab);
            return out;
        }
        js_free_value(ctx, ab);
    }
    js_string_to_owned(ctx, value).into_bytes()
}

unsafe extern "C" fn js_buffer_from(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return buffer_uint8_from_arc(ctx, Arc::from(Vec::<u8>::new()));
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let v = args[0];

    if qjs::JS_IsNumber(v) {
        return crate::ffi::throw_type_error(
            ctx,
            "The first argument must be of type string, Buffer, ArrayBuffer, Array, or Array-like Object. Received type number",
        );
    }

    if qjs::JS_IsArray(ctx, v) != 0 {
        let len_v = qjs::JS_GetPropertyStr(ctx, v, CString::new("length").unwrap().as_ptr());
        let mut n: i32 = 0;
        let _ = qjs::JS_ToInt32(ctx, &mut n, len_v);
        js_free_value(ctx, len_v);
        let n = n.max(0) as usize;
        if n > KAWKAB_MAX_BUFFER_BYTES {
            return crate::ffi::throw_range_error(ctx, "Buffer size exceeds kawkab limit");
        }
        let mut buf = vec![0u8; n];
        for i in 0..n as u32 {
            let el = qjs::JS_GetPropertyUint32(ctx, v, i);
            let mut x: i32 = 0;
            let _ = qjs::JS_ToInt32(ctx, &mut x, el);
            js_free_value(ctx, el);
            buf[i as usize] = x.rem_euclid(256) as u8;
        }
        return buffer_uint8_from_arc(ctx, Arc::from(buf));
    }

    if qjs::JS_IsString(v) {
        let s = js_string_to_owned(ctx, v);
        let enc = if argc >= 2 {
            js_string_to_owned(ctx, args[1])
        } else {
            "utf8".to_string()
        };
        return match string_to_buffer_bytes(&s, &enc) {
            Ok(bytes) => buffer_uint8_from_arc(ctx, Arc::from(bytes)),
            Err(msg) => crate::ffi::throw_type_error(ctx, &msg),
        };
    }

    let bytes = buffer_bytes_from_value(ctx, v);
    if bytes.len() > KAWKAB_MAX_BUFFER_BYTES {
        return crate::ffi::throw_range_error(ctx, "Buffer size exceeds kawkab limit");
    }
    buffer_uint8_from_arc(ctx, Arc::from(bytes))
}

pub(crate) unsafe extern "C" fn js_buffer_alloc(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let Some(first) = args.first() else {
        return buffer_uint8_from_arc(ctx, Arc::from(Vec::<u8>::new()));
    };
    let mut size_i: i64 = 0;
    if qjs::JS_ToInt64(ctx, &mut size_i, *first) != 0 {
        return qjs::JS_GetException(ctx);
    }
    if size_i < 0 {
        return crate::ffi::throw_range_error(ctx, "Invalid array length");
    }
    let size_u = size_i as usize;
    if size_u > KAWKAB_MAX_BUFFER_BYTES {
        return crate::ffi::throw_range_error(ctx, "Buffer size exceeds kawkab limit");
    }
    let fill_byte: u8 = if argc >= 2 {
        let f = args[1];
        if qjs::JS_IsString(f) {
            let s = js_string_to_owned(ctx, f);
            s.as_bytes().first().copied().unwrap_or(0)
        } else {
            let mut x: f64 = 0.0;
            let _ = qjs::JS_ToFloat64(ctx, &mut x, f);
            (x as i32).rem_euclid(256) as u8
        }
    } else {
        0
    };
    let bytes = vec![fill_byte; size_u];
    buffer_uint8_from_arc(ctx, Arc::from(bytes))
}

unsafe extern "C" fn js_buffer_alloc_unsafe(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    let args = std::slice::from_raw_parts(argv, argc.max(0) as usize);
    let Some(first) = args.first() else {
        return buffer_uint8_from_arc(ctx, Arc::from(Vec::<u8>::new()));
    };
    let mut size_i: i64 = 0;
    if qjs::JS_ToInt64(ctx, &mut size_i, *first) != 0 {
        return qjs::JS_GetException(ctx);
    }
    if size_i < 0 {
        return crate::ffi::throw_range_error(ctx, "Invalid array length");
    }
    let size_u = size_i as usize;
    if size_u > KAWKAB_MAX_BUFFER_BYTES {
        return crate::ffi::throw_range_error(ctx, "Buffer size exceeds kawkab limit");
    }
    let bytes = vec![0u8; size_u];
    buffer_uint8_from_arc(ctx, Arc::from(bytes))
}

unsafe extern "C" fn js_buffer_byte_length(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "byteLength requires a value");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let enc = args.get(1).copied().unwrap_or_else(js_undefined);
    match buffer_byte_len_for_string_or_view(ctx, args[0], enc, argc) {
        Ok(n) => qjs::JS_NewFloat64(ctx, n as f64),
        Err(e) => e,
    }
}

unsafe extern "C" fn js_buffer_to_string(
    ctx: *mut qjs::JSContext,
    _this_val: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return crate::ffi::throw_type_error(ctx, "Buffer.prototype.toString requires a buffer");
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let buf_val = args[0];
    let enc = if argc >= 2 && !qjs::JS_IsUndefined(args[1]) {
        js_string_to_owned(ctx, args[1])
    } else {
        "utf8".to_string()
    };
    let full = buffer_bytes_from_value(ctx, buf_val);
    let mut start_i: i64 = 0;
    let mut end_i: i64 = full.len() as i64;
    if argc >= 3 && !qjs::JS_IsUndefined(args[2]) {
        let _ = qjs::JS_ToInt64(ctx, &mut start_i, args[2]);
    }
    if argc >= 4 && !qjs::JS_IsUndefined(args[3]) {
        let _ = qjs::JS_ToInt64(ctx, &mut end_i, args[3]);
    }
    let fl = full.len() as i64;
    let start = start_i.clamp(0, fl) as usize;
    let mut end = end_i.clamp(0, fl) as usize;
    if end < start {
        end = start;
    }
    let slice = full.get(start..end).unwrap_or(&[]);
    match buffer_to_string_encoded(ctx, slice, &enc) {
        Ok(v) => v,
        Err(()) => crate::ffi::throw_type_error(ctx, "Unknown encoding"),
    }
}

unsafe extern "C" fn js_buffer_concat(
    ctx: *mut qjs::JSContext,
    _this: qjs::JSValue,
    argc: c_int,
    argv: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return buffer_uint8_from_arc(ctx, Arc::from(Vec::<u8>::new()));
    }
    let args = std::slice::from_raw_parts(argv, argc as usize);
    let list = args[0];
    let len_key = CString::new("length").unwrap();
    let len_val = qjs::JS_GetPropertyStr(ctx, list, len_key.as_ptr());
    let mut len: i32 = 0;
    let _ = qjs::JS_ToInt32(ctx, &mut len, len_val);
    js_free_value(ctx, len_val);
    let mut out: Vec<u8> = Vec::new();
    for i in 0..(len.max(0) as u32) {
        let item = qjs::JS_GetPropertyUint32(ctx, list, i);
        let chunk = buffer_bytes_from_value(ctx, item);
        js_free_value(ctx, item);
        if out.len().saturating_add(chunk.len()) > KAWKAB_MAX_BUFFER_BYTES {
            return crate::ffi::throw_range_error(ctx, "Buffer size exceeds kawkab limit");
        }
        out.extend_from_slice(&chunk);
    }
    if argc >= 2 && !qjs::JS_IsUndefined(args[1]) {
        let mut hint: i64 = 0;
        if qjs::JS_ToInt64(ctx, &mut hint, args[1]) == 0 && hint >= 0 {
            let h = hint as usize;
            if h < out.len() {
                out.truncate(h);
            }
        }
    }
    buffer_uint8_from_arc(ctx, Arc::from(out))
}
