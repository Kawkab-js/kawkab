//! On-disk JS bytecode cache: key = BLAKE3(canonical path || source). `.lbc` = magic + hash + `JS_WriteObject` bytes.

use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
    sync::Arc,
};

use quickjs_sys as qjs;
use tracing::{debug, instrument, trace};

use crate::error::JsError;
use crate::ffi::js_free_value;
use crate::qjs_compat;

const LBC_MAGIC: [u8; 4] = *b"LBC\x01";
const CACHE_HASH_LEN: usize = 32;

/// Compile JS source to bytecode.
///
/// # Safety
/// `ctx` must be a valid, live JSContext on the calling thread.
#[instrument(skip(ctx, src), fields(filename, src_bytes = src.len()))]
pub fn compile(ctx: *mut qjs::JSContext, src: &[u8], filename: &str) -> Result<Arc<[u8]>, JsError> {
    use std::ffi::CString;

    let fname = CString::new(filename).unwrap_or_default();
    let flags = (qjs::JS_EVAL_TYPE_GLOBAL | qjs::JS_EVAL_FLAG_COMPILE_ONLY) as i32;

    let func_val = unsafe {
        qjs_compat::eval(
            ctx,
            src.as_ptr() as *const libc::c_char,
            src.len(),
            fname.as_ptr(),
            flags,
        )
    };

    if unsafe { qjs::JS_IsException(func_val) } {
        let exc = extract_exception_string(ctx);
        return Err(JsError::Compile(exc));
    }

    let mut out_size: usize = 0;
    let buf = unsafe {
        qjs::JS_WriteObject(
            ctx,
            &mut out_size,
            func_val,
            qjs::JS_WRITE_OBJ_BYTECODE as i32,
        )
    };

    unsafe { js_free_value(ctx, func_val) };

    if buf.is_null() || out_size == 0 {
        return Err(JsError::Bytecode(
            "JS_WriteObject produced empty output".into(),
        ));
    }

    // SAFETY: buf is a valid qjs-allocated block of `out_size` bytes.
    let slice = unsafe { std::slice::from_raw_parts(buf, out_size) };
    let bc: Arc<[u8]> = Arc::from(slice);

    // QuickJS allocates with js_malloc — free with js_free.
    unsafe { qjs::js_free(ctx, buf as *mut libc::c_void) };

    trace!(bytecode_bytes = bc.len(), "Bytecode compiled");
    Ok(bc)
}

/// Execute a previously compiled bytecode blob.
///
/// # Safety
/// `ctx` must be a valid, live JSContext on the calling thread.
/// `bc` must be a blob produced by `compile()` from the same QuickJS version.
pub unsafe fn exec(ctx: *mut qjs::JSContext, bc: &[u8]) -> Result<qjs::JSValue, JsError> {
    let func_val =
        unsafe { qjs::JS_ReadObject(ctx, bc.as_ptr(), bc.len(), qjs::JS_READ_OBJ_BYTECODE as i32) };

    if unsafe { qjs::JS_IsException(func_val) } {
        let exc = extract_exception_string(ctx);
        return Err(JsError::Bytecode(format!("JS_ReadObject failed: {exc}")));
    }

    let result = unsafe { qjs::JS_EvalFunction(ctx, func_val) };

    loop {
        let rt_ptr = unsafe { qjs::JS_GetRuntime(ctx) };
        let mut ctx_out: *mut qjs::JSContext = std::ptr::null_mut();
        let res = unsafe { qjs::JS_ExecutePendingJob(rt_ptr, &mut ctx_out) };
        if res <= 0 {
            break;
        }
    }

    if unsafe { qjs::JS_IsException(result) } {
        let exc = extract_exception_string(ctx);
        unsafe { js_free_value(ctx, result) };
        return Err(JsError::Exception {
            message: exc,
            stack: None,
        });
    }

    Ok(result)
}

pub struct DiskCache {
    dir: PathBuf,
}

impl DiskCache {
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Compute the cache key (BLAKE3 hash) for a given source.
    pub fn cache_key(canonical_path: &str, src: &[u8]) -> [u8; CACHE_HASH_LEN] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(canonical_path.as_bytes());
        hasher.update(src);
        *hasher.finalize().as_bytes()
    }

    /// Load bytecode from disk if the key matches.
    #[instrument(skip(self), fields(key = hex_prefix(key)))]
    pub fn load(&self, key: &[u8; CACHE_HASH_LEN]) -> io::Result<Option<Arc<[u8]>>> {
        let path = self.entry_path(key);
        let mut f = match fs::File::open(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(f) => f,
        };

        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;

        if buf.len() < LBC_MAGIC.len() + CACHE_HASH_LEN || &buf[..4] != &LBC_MAGIC {
            let _ = fs::remove_file(&path);
            return Ok(None);
        }

        let stored_hash = &buf[4..4 + CACHE_HASH_LEN];
        if stored_hash != key {
            let _ = fs::remove_file(&path);
            return Ok(None);
        }

        let bc_bytes = &buf[4 + CACHE_HASH_LEN..];
        debug!(bytes = bc_bytes.len(), "Bytecode cache hit");
        Ok(Some(Arc::from(bc_bytes)))
    }

    /// Persist compiled bytecode to disk (atomic write via tmp + rename).
    #[instrument(skip(self, bc), fields(key = hex_prefix(key), bc_bytes = bc.len()))]
    pub fn store(&self, key: &[u8; CACHE_HASH_LEN], bc: &[u8]) -> io::Result<()> {
        let tmp = self.dir.join(format!("{}.tmp", uuid_v4()));
        let final_path = self.entry_path(key);
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)?;
        }

        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&LBC_MAGIC)?;
            f.write_all(key)?;
            f.write_all(bc)?;
            f.flush()?;
            f.sync_data()?;
        }

        fs::rename(&tmp, &final_path)?;
        debug!("Bytecode persisted");
        Ok(())
    }

    fn entry_path(&self, key: &[u8; CACHE_HASH_LEN]) -> PathBuf {
        let hex = hex_bytes(key);
        self.dir.join(&hex[..2]).join(format!("{}.lbc", &hex[2..]))
    }
}

fn extract_exception_string(ctx: *mut qjs::JSContext) -> String {
    let exc = unsafe { qjs::JS_GetException(ctx) };
    let s_val = unsafe { qjs::JS_ToString(ctx, exc) };
    let s = if unsafe { qjs::JS_IsException(s_val) } {
        unsafe { js_free_value(ctx, s_val) };
        "<unknown exception>".to_owned()
    } else {
        let out = unsafe { crate::ffi::js_string_to_owned(ctx, s_val) };
        unsafe { js_free_value(ctx, s_val) };
        out
    };
    unsafe { js_free_value(ctx, exc) };
    s
}

fn hex_bytes(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex_prefix(b: &[u8; 32]) -> String {
    hex_bytes(&b[..4])
}

fn uuid_v4() -> String {
    use std::time::SystemTime;
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    format!("{:016x}{:016x}", t, t.wrapping_mul(6364136223846793005))
}
