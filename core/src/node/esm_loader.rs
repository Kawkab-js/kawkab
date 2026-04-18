//! QuickJS native ESM loader via `JS_SetModuleLoaderFunc` (ESM/CJS/JSON + CJS↔ESM bridge).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::Path;

use quickjs_sys as qjs;

use crate::error::JsError;
use crate::ffi::{js_dup_value, js_free_value, js_string_to_owned};
use crate::node::module_loader::{
    detect_source_type, resolve_module_path_with_kind, ModuleResolutionKind, SourceType,
};
use crate::qjs_compat;

thread_local! {
    /// Canonical path -> module namespace value.
    static ESM_NS_CACHE: RefCell<HashMap<String, qjs::JSValue>> =
        RefCell::new(HashMap::new());

    /// Canonical path -> bridged CJS exports value.
    static CJS_NS_CACHE: RefCell<HashMap<String, qjs::JSValue>> =
        RefCell::new(HashMap::new());
}

/// Normalize specifier to canonical absolute path (`js_malloc` C string).
unsafe extern "C" fn normalize_module(
    ctx: *mut qjs::JSContext,
    module_base_name: *const c_char,
    module_name: *const c_char,
    _opaque: *mut c_void,
) -> *mut c_char {
    let name = CStr::from_ptr(module_name)
        .to_string_lossy()
        .into_owned();

    let resolved = if name.starts_with('/') {
        name.clone()
    } else {
        let base = if module_base_name.is_null() {
            ".".to_string()
        } else {
            CStr::from_ptr(module_base_name).to_string_lossy().into_owned()
        };

        let base_dir = if Path::new(&base).is_file() || base.contains('.') {
            Path::new(&base)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".to_string())
        } else {
            base
        };

        let path = Path::new(&base_dir).join(&name);
        match std::fs::canonicalize(&path) {
            Ok(abs) => abs.to_string_lossy().into_owned(),
            Err(_) => resolve_module_path_with_kind(
                &base_dir,
                &name,
                ModuleResolutionKind::Esm,
            ),
        }
    };

    let c_resolved = CString::new(resolved).unwrap_or_else(|_| CString::new(".").unwrap());
    let bytes = c_resolved.as_bytes_with_nul();
    let ptr = qjs::js_malloc(ctx, bytes.len()) as *mut c_char;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, ptr, bytes.len());
    }
    ptr
}

/// Load module by canonical path and return `JSModuleDef*`.
unsafe extern "C" fn load_module(
    ctx: *mut qjs::JSContext,
    module_name: *const c_char,
    _opaque: *mut c_void,
) -> *mut qjs::JSModuleDef {
    let path = CStr::from_ptr(module_name)
        .to_string_lossy()
        .into_owned();

    if path.ends_with(".json") {
        return load_json_module(ctx, &path);
    }

    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            let msg = CString::new(format!("Cannot resolve module '{}': {}", path, e))
                .unwrap_or_default();
            qjs::JS_ThrowReferenceError(ctx, msg.as_ptr());
            return std::ptr::null_mut();
        }
    };

    let src_type = detect_source_type(&path, &source);

    match src_type {
        SourceType::Json => load_json_module(ctx, &path),
        SourceType::Cjs => load_cjs_as_esm_module(ctx, &path, &source),
        SourceType::Esm => load_esm_module(ctx, &path, &source),
    }
}

unsafe fn load_esm_module(
    ctx: *mut qjs::JSContext,
    path: &str,
    source: &str,
) -> *mut qjs::JSModuleDef {
    let mut js_source = match crate::transpiler::strip_types_only(source, path) {
        Ok(s) => s,
        Err(e) => {
            let msg = CString::new(format!("ESM transpile error '{}': {}", path, e))
                .unwrap_or_default();
            qjs::JS_ThrowSyntaxError(ctx, msg.as_ptr());
            return std::ptr::null_mut();
        }
    };
    js_source.push('\n');

    let c_path = CString::new(path).unwrap_or_default();
    let flags = (qjs::JS_EVAL_TYPE_MODULE | qjs::JS_EVAL_FLAG_COMPILE_ONLY) as i32;

    let func_val = qjs::JS_Eval(
        ctx,
        js_source.as_ptr() as *const c_char,
        js_source.len(),
        c_path.as_ptr(),
        flags,
    );

    if func_val.tag == qjs::JS_TAG_EXCEPTION as i64 {
        return std::ptr::null_mut();
    }

    let m = func_val.u.ptr as *mut qjs::JSModuleDef;
    if !m.is_null() {
        install_import_meta(ctx, m, path);
    }

    // Do not `js_free` func_val here; QuickJS owns module lifetime after compile.
    m
}

/// Evaluate CJS and expose `module.exports` as synthetic ESM namespace.
unsafe fn load_cjs_as_esm_module(
    ctx: *mut qjs::JSContext,
    path: &str,
    source: &str,
) -> *mut qjs::JSModuleDef {
    let cached = CJS_NS_CACHE.with(|c| c.borrow().get(path).copied());
    let exports_obj = if let Some(cached_val) = cached {
        js_dup_value(cached_val)
    } else {
        let js_source = match crate::transpiler::transpile_ts(source, path) {
            Ok(s) => s,
            Err(e) => {
                let msg = CString::new(format!("CJS transpile error '{}': {}", path, e))
                    .unwrap_or_default();
                qjs::JS_ThrowSyntaxError(ctx, msg.as_ptr());
                return std::ptr::null_mut();
            }
        };

        let cjs_exports = eval_cjs_source(ctx, &js_source, path);
        if cjs_exports.tag == qjs::JS_TAG_EXCEPTION as i64 {
            return std::ptr::null_mut();
        }

        CJS_NS_CACHE.with(|c| {
            c.borrow_mut().insert(path.to_string(), js_dup_value(cjs_exports));
        });
        cjs_exports
    };

    build_synthetic_cjs_module(ctx, path, exports_obj)
}

/// Execute CJS wrapper and return final `module.exports`.
unsafe fn eval_cjs_source(
    ctx: *mut qjs::JSContext,
    js_source: &str,
    path: &str,
) -> qjs::JSValue {
    let global = qjs::JS_GetGlobalObject(ctx);
    let module_obj = qjs::JS_NewObject(ctx);
    let exports_obj = qjs::JS_NewObject(ctx);

    let exports_key = CString::new("exports").unwrap();
    qjs::JS_SetPropertyStr(ctx, module_obj, exports_key.as_ptr(), js_dup_value(exports_obj));

    let dir = Path::new(path)
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy()
        .to_string();
    let wrapper = format!(
        "(function(exports, require, module, __filename, __dirname) {{\n{}\n}})",
        js_source
    );

    let c_path = CString::new(path).unwrap_or_default();
    let func_val = qjs::JS_Eval(
        ctx,
        wrapper.as_ptr() as *const c_char,
        wrapper.len(),
        c_path.as_ptr(),
        qjs::JS_EVAL_TYPE_GLOBAL as i32,
    );

    if func_val.tag == qjs::JS_TAG_EXCEPTION as i64 {
        js_free_value(ctx, exports_obj);
        js_free_value(ctx, module_obj);
        js_free_value(ctx, global);
        return func_val; // propagate exception
    }

    let require_key = CString::new("require").unwrap();
    let require_fn = qjs::JS_GetPropertyStr(ctx, global, require_key.as_ptr());

    let filename_val = qjs_compat::new_string_from_str(ctx, path);
    let dirname_val = qjs_compat::new_string_from_str(ctx, &dir);

    let exports_arg = js_dup_value(exports_obj);
    let module_arg  = js_dup_value(module_obj);

    let mut args = [exports_arg, require_fn, module_arg, filename_val, dirname_val];
    let ret = qjs::JS_Call(ctx, func_val, global, 5, args.as_mut_ptr());

    js_free_value(ctx, func_val);
    js_free_value(ctx, exports_arg);
    js_free_value(ctx, module_arg);
    js_free_value(ctx, require_fn);
    js_free_value(ctx, filename_val);
    js_free_value(ctx, dirname_val);

    if ret.tag == qjs::JS_TAG_EXCEPTION as i64 {
        js_free_value(ctx, exports_obj);
        js_free_value(ctx, module_obj);
        js_free_value(ctx, global);
        return ret;
    }
    js_free_value(ctx, ret);

    let final_exports = qjs::JS_GetPropertyStr(ctx, module_obj, exports_key.as_ptr());

    js_free_value(ctx, exports_obj);
    js_free_value(ctx, module_obj);
    js_free_value(ctx, global);

    final_exports
}

/// Build synthetic module: `default` + enumerable named exports from `exports_obj`.
unsafe fn build_synthetic_cjs_module(
    ctx: *mut qjs::JSContext,
    path: &str,
    exports_obj: qjs::JSValue,
) -> *mut qjs::JSModuleDef {
    let mut ptab: *mut qjs::JSPropertyEnum = std::ptr::null_mut();
    let mut plen: u32 = 0;
    let flags = (qjs::JS_GPN_STRING_MASK | qjs::JS_GPN_ENUM_ONLY) as i32;
    qjs::JS_GetOwnPropertyNames(ctx, &mut ptab, &mut plen, exports_obj, flags);

    let prop_names: Vec<String> = (0..plen as usize)
        .map(|i| {
            let atom = (*ptab.add(i)).atom;
            let js_str = qjs::JS_AtomToString(ctx, atom);
            let name = js_string_to_owned(ctx, js_str);
            js_free_value(ctx, js_str);
            name
        })
        .filter(|n: &String| !n.is_empty())
        .collect();

    if !ptab.is_null() {
        qjs::JS_FreePropertyEnum(ctx, ptab, plen);
    }

    let c_path = CString::new(path).unwrap_or_default();

    unsafe extern "C" fn noop_init(
        _ctx: *mut qjs::JSContext,
        _m: *mut qjs::JSModuleDef,
    ) -> i32 {
        0
    }

    let m = qjs::JS_NewCModule(ctx, c_path.as_ptr(), Some(noop_init));
    if m.is_null() {
        js_free_value(ctx, exports_obj);
        return std::ptr::null_mut();
    }

    let default_key = CString::new("default").unwrap();
    qjs::JS_AddModuleExport(ctx, m, default_key.as_ptr());
    for name in &prop_names {
        if let Ok(cn) = CString::new(name.clone()) {
            qjs::JS_AddModuleExport(ctx, m, cn.as_ptr());
        }
    }

    qjs::JS_SetModuleExport(ctx, m, default_key.as_ptr(), js_dup_value(exports_obj));
    for name in &prop_names {
        if name == "default" { continue; }
        if let Ok(cn) = CString::new(name.clone()) {
            let val = qjs::JS_GetPropertyStr(ctx, exports_obj, cn.as_ptr());
            qjs::JS_SetModuleExport(ctx, m, cn.as_ptr(), val);
        }
    }

    js_free_value(ctx, exports_obj);
    m
}

unsafe fn load_json_module(ctx: *mut qjs::JSContext, path: &str) -> *mut qjs::JSModuleDef {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            let msg = CString::new(format!("Cannot read JSON '{}': {}", path, e)).unwrap_or_default();
            qjs::JS_ThrowReferenceError(ctx, msg.as_ptr());
            return std::ptr::null_mut();
        }
    };

    let c_path = CString::new(path).unwrap_or_default();
    let parsed = qjs::JS_ParseJSON(
        ctx,
        source.as_ptr() as *const c_char,
        source.len(),
        c_path.as_ptr(),
    );

    if parsed.tag == qjs::JS_TAG_EXCEPTION as i64 {
        return std::ptr::null_mut();
    }

    build_synthetic_cjs_module(ctx, path, parsed)
}

/// Install `import.meta.url`, `.filename`, `.dirname`, and `.resolve`.
pub unsafe fn install_import_meta(ctx: *mut qjs::JSContext, m: *mut qjs::JSModuleDef, path: &str) {
    let meta = qjs::JS_GetImportMeta(ctx, m);
    if meta.tag == qjs::JS_TAG_EXCEPTION as i64 || meta.tag == qjs::JS_TAG_UNDEFINED as i64 {
        return;
    }

    let url = format!("file://{}", path);
    let url_val = qjs_compat::new_string_from_str(ctx, &url);
    let url_key = CString::new("url").unwrap();
    qjs::JS_SetPropertyStr(ctx, meta, url_key.as_ptr(), url_val);

    let filename_val = qjs_compat::new_string_from_str(ctx, path);
    let filename_key = CString::new("filename").unwrap();
    qjs::JS_SetPropertyStr(ctx, meta, filename_key.as_ptr(), filename_val);

    let dir = Path::new(path)
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy()
        .to_string();
    let dirname_val = qjs_compat::new_string_from_str(ctx, &dir);
    let dirname_key = CString::new("dirname").unwrap();
    qjs::JS_SetPropertyStr(ctx, meta, dirname_key.as_ptr(), dirname_val);

    install_import_meta_resolve(ctx, meta, &dir);

    js_free_value(ctx, meta);
}

/// Attach `import.meta.resolve`.
unsafe fn install_import_meta_resolve(ctx: *mut qjs::JSContext, meta: qjs::JSValue, base_dir: &str) {
    let base_dir_val = qjs_compat::new_string_from_str(ctx, base_dir);
    let resolve_fn = qjs::JS_NewCFunctionData(
        ctx,
        Some(js_import_meta_resolve),
        1,
        0,
        1,
        &mut [base_dir_val] as *mut qjs::JSValue,
    );
    let resolve_key = CString::new("resolve").unwrap();
    qjs::JS_SetPropertyStr(ctx, meta, resolve_key.as_ptr(), resolve_fn);
}

/// Native `import.meta.resolve(specifier)`.
unsafe extern "C" fn js_import_meta_resolve(
    ctx: *mut qjs::JSContext,
    _this_val: qjs::JSValue,
    argc: i32,
    argv: *mut qjs::JSValue,
    _magic: i32,
    data: *mut qjs::JSValue,
) -> qjs::JSValue {
    if argc < 1 {
        return qjs::JS_ThrowTypeError(ctx, b"resolve(specifier) requires primary argument\0".as_ptr() as *const i8);
    }
    let specifier = js_string_to_owned(ctx, *argv);
    let base_dir = js_string_to_owned(ctx, *data);

    let resolved = resolve_module_path_with_kind(
        &base_dir,
        &specifier,
        ModuleResolutionKind::Esm,
    );
    let url = format!("file://{}", resolved);
    qjs_compat::new_string_from_str(ctx, &url)
}

/// Register module loader callbacks on runtime.
pub unsafe fn install_module_loader(rt: *mut qjs::JSRuntime) {
    qjs::JS_SetModuleLoaderFunc(
        rt,
        Some(normalize_module),
        Some(load_module),
        std::ptr::null_mut(),
    );
}

/// Evaluate ESM entry file; returns eval result/namespace JSValue.
pub unsafe fn eval_esm_entry(
    ctx: *mut qjs::JSContext,
    path: &str,
) -> Result<qjs::JSValue, JsError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| JsError::Runtime(format!("Cannot read '{}': {}", path, e)))?;

    let mut js_source = crate::transpiler::strip_types_only(&source, path)
        .map_err(|e| JsError::Runtime(format!("Transpile error '{}': {}", path, e)))?;
    js_source.push('\n');

    let c_path = CString::new(path).unwrap_or_default();

    let flags = (qjs::JS_EVAL_TYPE_MODULE | qjs::JS_EVAL_FLAG_COMPILE_ONLY) as i32;
    let func_val = qjs::JS_Eval(
        ctx,
        js_source.as_ptr() as *const c_char,
        js_source.len(),
        c_path.as_ptr(),
        flags,
    );

    if func_val.tag == qjs::JS_TAG_EXCEPTION as i64 {
        let exc = qjs::JS_GetException(ctx);
        let msg = js_string_to_owned(ctx, exc);
        
        let line_val = qjs::JS_GetPropertyStr(ctx, exc, b"lineNumber\0".as_ptr() as *const i8);
        let line = if line_val.tag == qjs::JS_TAG_INT as i64 { line_val.u.int32 } else { -1 };
        js_free_value(ctx, line_val);

        let file_val = qjs::JS_GetPropertyStr(ctx, exc, b"fileName\0".as_ptr() as *const i8);
        let file_name = if qjs::JS_IsString(file_val) { js_string_to_owned(ctx, file_val) } else { "unknown".to_string() };
        js_free_value(ctx, file_val);

        js_free_value(ctx, exc);
        return Err(JsError::Compile(format!("ESM compile error in '{}' at line {}: {}", file_name, line, msg)));
    }

    let m = func_val.u.ptr as *mut qjs::JSModuleDef;
    if !m.is_null() {
        install_import_meta(ctx, m, path);
    }

    let eval_val = qjs::JS_EvalFunction(ctx, func_val);

    if eval_val.tag == qjs::JS_TAG_EXCEPTION as i64 {
        let exc = qjs::JS_GetException(ctx);
        let msg = js_string_to_owned(ctx, exc);
        js_free_value(ctx, exc);
        return Err(JsError::Js(format!("ESM eval '{}': {}", path, msg)));
    }

    Ok(eval_val)
}

/// CJS `require()` path for ESM target; returns plain object of exports.
pub unsafe fn require_esm_as_cjs(
    ctx: *mut qjs::JSContext,
    path: &str,
    base_dir: &str,
) -> qjs::JSValue {
    let cached = ESM_NS_CACHE.with(|c| c.borrow().get(path).copied());
    if let Some(ns) = cached {
        return js_dup_value(ns);
    }

    let c_base = CString::new(base_dir).unwrap_or_default();
    let c_path = CString::new(path).unwrap_or_default();

    let ns_val = qjs::JS_LoadModule(ctx, c_base.as_ptr(), c_path.as_ptr());

    if ns_val.tag == qjs::JS_TAG_EXCEPTION as i64 {
        return ns_val; // propagate
    }
    if ns_val.tag == qjs::JS_TAG_UNDEFINED as i64 {
        return ns_val;
    }

    let out = qjs::JS_NewObject(ctx);

    let mut ptab: *mut qjs::JSPropertyEnum = std::ptr::null_mut();
    let mut plen: u32 = 0;
    let flags = (qjs::JS_GPN_STRING_MASK | qjs::JS_GPN_ENUM_ONLY) as i32;
    qjs::JS_GetOwnPropertyNames(ctx, &mut ptab, &mut plen, ns_val, flags);

    let mut has_default = false;
    for i in 0..plen as usize {
        let atom = (*ptab.add(i)).atom;
        let js_str = qjs::JS_AtomToString(ctx, atom);
        let name = js_string_to_owned(ctx, js_str);
        js_free_value(ctx, js_str);
        if name.is_empty() { continue; }

        let val = qjs::JS_GetPropertyStr(
            ctx,
            ns_val,
            CString::new(name.clone()).unwrap_or_default().as_ptr(),
        );
        if name == "default" {
            has_default = true;
        }
        let c_name = CString::new(name.clone()).unwrap_or_default();
        qjs::JS_SetPropertyStr(ctx, out, c_name.as_ptr(), val);
    }

    if !ptab.is_null() {
        qjs::JS_FreePropertyEnum(ctx, ptab, plen);
    }

    if has_default {
        let default_key = CString::new("default").unwrap();
        let default_val = qjs::JS_GetPropertyStr(ctx, out, default_key.as_ptr());
        if default_val.tag != qjs::JS_TAG_UNDEFINED as i64 {
            let esm_flag_key = CString::new("__esModule").unwrap();
            qjs::JS_SetPropertyStr(ctx, out, esm_flag_key.as_ptr(), qjs::JS_NewBool(ctx, true));
        }
        js_free_value(ctx, default_val);
    }

    ESM_NS_CACHE.with(|c| {
        c.borrow_mut().insert(path.to_string(), js_dup_value(out));
    });

    js_free_value(ctx, ns_val);
    out
}

/// Free and clear per-isolate ESM/CJS bridge caches.
pub unsafe fn clear_module_caches(ctx: *mut qjs::JSContext) {
    ESM_NS_CACHE.with(|c| {
        for (_, v) in c.borrow_mut().drain() {
            js_free_value(ctx, v);
        }
    });
    CJS_NS_CACHE.with(|c| {
        for (_, v) in c.borrow_mut().drain() {
            js_free_value(ctx, v);
        }
    });
}
