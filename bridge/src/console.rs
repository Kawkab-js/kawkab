use kawkab_core::error::JsError;
use std::io::Write;

pub fn install(isolate: &mut kawkab_core::isolate::Isolate) -> Result<(), JsError> {
    let ctx = isolate.ctx_ptr();
    if ctx.is_null() {
        return Err(JsError::Runtime(
            "bridge console install received null context".to_string(),
        ));
    }
    Ok(())
}

pub fn flush_all() {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}
