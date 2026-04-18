use std::path::Path;

use quickjs_sys as qjs;

pub struct SnapshotBuilder {
    scripts: Vec<SnapshotScript>,
}

#[derive(Debug, Clone)]
struct SnapshotScript {
    name: String,
    filename: String,
    source_hash: String,
    source_len: usize,
}

impl SnapshotBuilder {
    pub fn new() -> Self {
        Self {
            scripts: Vec::new(),
        }
    }

    /// Registers script metadata into the experimental snapshot manifest.
    ///
    /// # Safety
    /// The provided `JSContext` pointer is currently unused by this experimental
    /// implementation, but callers must pass a valid context pointer for
    /// forward-compatibility with future snapshot integration.
    pub unsafe fn add_script(
        &mut self,
        ctx: *mut qjs::JSContext,
        name: impl Into<String>,
        src: &[u8],
        filename: &str,
    ) -> Result<(), SnapshotError> {
        if ctx.is_null() {
            return Err(SnapshotError::InvalidContext);
        }
        let hash = blake3::hash(src);
        self.scripts.push(SnapshotScript {
            name: name.into(),
            filename: filename.to_string(),
            source_hash: hash.to_hex().to_string(),
            source_len: src.len(),
        });
        Ok(())
    }

    pub fn write_to(&self, path: impl AsRef<Path>) -> Result<(), SnapshotError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(SnapshotError::Io)?;
        }
        let mut out = String::from("{\n  \"format\": \"kawkab-snapshot-manifest-v1\",\n  \"experimental\": true,\n  \"scripts\": [\n");
        for (idx, s) in self.scripts.iter().enumerate() {
            if idx > 0 {
                out.push_str(",\n");
            }
            out.push_str(&format!(
                "    {{\"name\":\"{}\",\"filename\":\"{}\",\"sourceHash\":\"{}\",\"sourceLen\":{}}}",
                escape_json(&s.name),
                escape_json(&s.filename),
                s.source_hash,
                s.source_len
            ));
        }
        out.push_str("\n  ]\n}\n");
        std::fs::write(path, out).map_err(SnapshotError::Io)?;
        Ok(())
    }
}

impl Default for SnapshotBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum SnapshotError {
    InvalidContext,
    Io(std::io::Error),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::InvalidContext => write!(f, "invalid QuickJS context pointer"),
            SnapshotError::Io(err) => write!(f, "snapshot I/O error: {err}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
