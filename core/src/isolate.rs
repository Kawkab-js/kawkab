use crate::error::JsError;
use crate::qjs_compat;
use quickjs_sys as qjs;
use std::ptr;

#[derive(Debug, Clone)]
pub struct IsolateConfig {
    pub heap_size: usize,
    pub stack_size: usize,
    pub strict: bool,
    pub prewarm: bool,
}

impl Default for IsolateConfig {
    fn default() -> Self {
        Self {
            heap_size: 32 * 1024 * 1024,
            stack_size: 256 * 1024,
            strict: true,
            prewarm: true,
        }
    }
}

pub struct Isolate {
    rt: *mut qjs::JSRuntime,
    ctx: *mut qjs::JSContext,
}

// SAFETY: QuickJS Runtime/Context can be Send/Sync if we ensure single-threaded access.
// The Scheduler pins each Isolate to a single thread, so this is safe.
unsafe impl Send for Isolate {}
unsafe impl Sync for Isolate {}

impl Isolate {
    pub fn new(config: IsolateConfig) -> Result<Self, JsError> {
        unsafe {
            let rt = qjs::JS_NewRuntime();
            if rt.is_null() {
                return Err(JsError::Runtime(
                    "Failed to create QuickJS runtime".to_string(),
                ));
            }

            qjs_compat::set_memory_limit(rt, config.heap_size);
            let ctx = qjs::JS_NewContext(rt);
            if ctx.is_null() {
                qjs::JS_FreeRuntime(rt);
                return Err(JsError::Runtime(
                    "Failed to create QuickJS context".to_string(),
                ));
            }

            qjs_compat::set_max_stack_size(ctx, config.stack_size);

            // Install default console if prewarm is true
            if config.prewarm {
                // We'll let the scheduler handle specialized bindings,
                // but we initialize the basic context here.
            }

            Ok(Self { rt, ctx })
        }
    }

    pub fn ctx_ptr(&self) -> *mut qjs::JSContext {
        self.ctx
    }

    pub fn eval(&mut self, src: &[u8], filename: &str) -> Result<qjs::JSValue, JsError> {
        let c_filename = std::ffi::CString::new(filename)
            .map_err(|_| JsError::Runtime("Invalid filename".to_string()))?;
        let flags = qjs::JS_EVAL_TYPE_GLOBAL as i32;

        // Ensure the source is null-terminated or use the length-based Eval.
        // JS_Eval takes a length, so null-termination is not strictly required by the API,
        // but it's good practice.
        unsafe {
            let val = qjs_compat::eval(
                self.ctx,
                src.as_ptr() as *const i8,
                src.len(),
                c_filename.as_ptr(),
                flags,
            );

            if self.is_exception(val) {
                let _exc = qjs::JS_GetException(self.ctx);
                qjs::__JS_FreeValue(self.ctx, val); // Free the exception tag
                return Err(JsError::Js("Execution failed".to_string()));
            }

            Ok(val)
        }
    }

    /// Stringify a JS value for host output and release the `JSValue`.
    pub fn stringify_js_value(&mut self, v: qjs::JSValue) -> String {
        unsafe {
            let s = crate::ffi::js_string_to_owned(self.ctx, v);
            crate::ffi::js_free_value(self.ctx, v);
            s
        }
    }

    pub fn run_pending_jobs(&mut self) -> Result<bool, JsError> {
        unsafe {
            let mut ctx_out: *mut qjs::JSContext = ptr::null_mut();
            let res = qjs::JS_ExecutePendingJob(self.rt, &mut ctx_out);
            if res < 0 {
                return Err(JsError::Js("Pending job failed".to_string()));
            }
            Ok(res > 0)
        }
    }

    /// Reserved for bytecode cache integration; not wired yet.
    pub fn eval_bytecode(&mut self, _key: &str) -> Result<qjs::JSValue, JsError> {
        Err(JsError::Runtime(
            "eval_bytecode is not implemented for this build".to_string(),
        ))
    }

    /// Runs `http.createServer` listener: `handler.call(this_val, req, res)`.
    ///
    /// Does not take ownership of `handler`, `req`, or `res` (caller frees). On JS
    /// exception, returns [`JsError::Js`] with a short message.
    pub unsafe fn dispatch_http_request(
        &mut self,
        handler: qjs::JSValue,
        this_val: qjs::JSValue,
        req: qjs::JSValue,
        res: qjs::JSValue,
    ) -> Result<(), JsError> {
        crate::node::http_invoke_handler(self.ctx, handler, this_val, req, res)
    }

    fn is_exception(&self, val: qjs::JSValue) -> bool {
        val.tag == qjs::JS_TAG_EXCEPTION as i64
    }
}

impl Drop for Isolate {
    fn drop(&mut self) {
        // Native `JS_NewCFunction*` objects retain `JS_DupContext` refs. Calling
        // `JS_FreeContext` once only decrements and returns early while those
        // functions are reachable, and `JS_FreeRuntime` then aborts when `gc_obj_list`
        // is non-empty. A correct refcount-ordered teardown belongs in a dedicated
        // shutdown path; short-lived CLI runs rely on process exit to reclaim memory.
        let _ = (self.rt, self.ctx);
    }
}
