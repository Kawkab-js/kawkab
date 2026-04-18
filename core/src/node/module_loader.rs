use std::path::{Path, PathBuf};

use quickjs_sys as qjs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Esm,
    Cjs,
    Json,
}

/// Whether resolution prefers the `require` or `import` condition in `package.json` `"exports"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleResolutionKind {
    CommonJs,
    Esm,
}

/// Information extracted from the nearest `package.json` above a given path.
#[derive(Debug, Clone)]
pub struct PackageJsonInfo {
    /// Directory containing the `package.json`.
    pub dir: PathBuf,
    /// Resolved module type based on the `"type"` field.
    pub module_type: SourceType,
    /// `"main"` field value (relative path).
    pub main: Option<String>,
    /// `"module"` field (ESM entry point).
    pub module: Option<String>,
}

/// Determine the source type for a file path + its raw source content.
///
/// Rules (in priority order):
/// 1. `.cjs` → CJS
/// 2. `.mjs` → ESM
/// 3. `.json` → JSON
/// 4. Nearest `package.json` `"type"` field
/// 5. Scan source for top-level `import`/`export` → ESM; else CJS
pub fn detect_source_type(path: &str, source: &str) -> SourceType {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match ext {
        "cjs" => return SourceType::Cjs,
        "mjs" => return SourceType::Esm,
        "json" => return SourceType::Json,
        _ => {}
    }

    if let Some(pkg) = find_nearest_package_json(path) {
        if pkg.module_type == SourceType::Esm {
            return SourceType::Esm;
        }
    }

    if has_esm_syntax(source) {
        SourceType::Esm
    } else {
        SourceType::Cjs
    }
}

/// Walk upward from `path` to find the nearest `package.json`.
pub fn find_nearest_package_json(path: &str) -> Option<PackageJsonInfo> {
    let mut dir = PathBuf::from(path);
    if dir.is_file() || !dir.is_dir() {
        dir = dir.parent()?.to_path_buf();
    }

    loop {
        let pkg_path = dir.join("package.json");
        if pkg_path.exists() {
            let raw = std::fs::read_to_string(&pkg_path).ok()?;
            return Some(parse_package_json(&dir, &raw));
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn parse_package_json(dir: &Path, raw: &str) -> PackageJsonInfo {
    PackageJsonInfo {
        dir: dir.to_path_buf(),
        module_type: extract_type_field(raw),
        main: extract_main_from_package_json(raw),
        module: extract_string_field(raw, "module"),
    }
}

fn extract_type_field(raw: &str) -> SourceType {
    match extract_string_field(raw, "type").as_deref() {
        Some("module") => SourceType::Esm,
        _ => SourceType::Cjs,
    }
}

/// Minimal `"key": "value"` extractor (no full JSON parser needed).
pub(crate) fn extract_string_field(raw: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let key_pos = raw.find(&needle)?;
    let after = &raw[key_pos + needle.len()..];
    let colon = after.find(':')?;
    let val = after[colon + 1..].trim_start();
    if val.starts_with('"') {
        let val2 = &val[1..];
        let end = val2.find('"')?;
        Some(val2[..end].to_string())
    } else {
        None
    }
}

/// `"exports": "./path"` only (no surrounding object).
fn extract_exports_simple_string(raw: &str) -> Option<String> {
    let key_pos = raw.find("\"exports\"")?;
    let after = &raw[key_pos + 9..];
    let colon = after.find(':')?;
    let val = after[colon + 1..].trim_start();
    if val.starts_with('"') {
        let val2 = &val[1..];
        let end = val2.find('"')?;
        return Some(val2[..end].to_string());
    }
    None
}

fn sanitize_package_relative(rel: &str) -> Option<String> {
    if rel.is_empty() || rel.contains("..") {
        return None;
    }
    if Path::new(rel).is_absolute() {
        return None;
    }
    Some(rel.to_string())
}

/// Content inside the outer `{` … `}` (exclusive of braces). `input` must start with `{`.
fn extract_brace_object_body(input: &str) -> Option<&str> {
    let b = input.as_bytes();
    if b.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i = (i + 2).min(b.len());
                        continue;
                    }
                    if b[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'{' => {
                if depth == 0 && i == 0 {
                    depth = 1;
                } else {
                    depth += 1;
                }
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&input[1..i]);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

fn object_value_after_key<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{}\"", key);
    let pos = body.find(&needle)?;
    let after = &body[pos + needle.len()..];
    let colon = after.find(':')?;
    Some(after[colon + 1..].trim_start())
}

fn take_json_string(raw: &str) -> Option<String> {
    let s = raw.trim_start();
    if !s.starts_with('"') {
        return None;
    }
    let v2 = &s[1..];
    let end = v2.find('"')?;
    Some(v2[..end].to_string())
}

/// Read `"exports"` and resolve the `"."` entry for `require` vs `import` conditions.
fn parse_exports_dot_path(raw: &str, kind: ModuleResolutionKind) -> Option<String> {
    let key_pos = raw.find("\"exports\"")?;
    let after = &raw[key_pos + 9..];
    let colon = after.find(':')?;
    let val = after[colon + 1..].trim_start();
    if val.starts_with('"') {
        return take_json_string(val).and_then(|p| sanitize_package_relative(&p));
    }
    if !val.starts_with('{') {
        return None;
    }
    let exports_body = extract_brace_object_body(val)?;
    let dot_rest = object_value_after_key(exports_body, ".")?;
    let dot_rest = dot_rest.trim_start();
    if dot_rest.starts_with('"') {
        return take_json_string(dot_rest);
    }
    if !dot_rest.starts_with('{') {
        return None;
    }
    let cond_body = extract_brace_object_body(dot_rest)?;
    let pick = |k: &str| object_value_after_key(cond_body, k).and_then(take_json_string);
    let path = match kind {
        ModuleResolutionKind::CommonJs => pick("require")
            .or_else(|| pick("default"))
            .or_else(|| pick("import")),
        ModuleResolutionKind::Esm => pick("import")
            .or_else(|| pick("default"))
            .or_else(|| pick("require")),
    };
    path.and_then(|p| sanitize_package_relative(&p))
}

/// Entry file relative to package root: `exports` (conditional or string) > `module` > `main`.
fn preferred_package_entry(raw: &str, kind: ModuleResolutionKind) -> Option<String> {
    if let Some(p) = parse_exports_dot_path(raw, kind) {
        return Some(p);
    }
    if let Some(p) = extract_exports_simple_string(raw) {
        return sanitize_package_relative(&p);
    }
    if let Some(p) = extract_string_field(raw, "module") {
        if let Some(s) = sanitize_package_relative(&p) {
            return Some(s);
        }
    }
    extract_main_from_package_json(raw).and_then(|p| sanitize_package_relative(&p))
}

/// Very fast ESM syntax heuristic: look for top-level `import`/`export` keywords.
/// This avoids a full parse for the common case.
fn has_esm_syntax(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim_start();
        if t.starts_with("import ") || t.starts_with("import{") || t.starts_with("import(") {
            if !t.starts_with("import(") {
                return true;
            }
        }
        if t.starts_with("export ")
            || t.starts_with("export{")
            || t.starts_with("export*")
            || t.starts_with("export default")
        {
            return true;
        }
    }
    false
}

/// Resolve module specifier to absolute path using CommonJS `require` rules.
///
/// For ESM `import`, use [`resolve_module_path_with_kind`] + [`ModuleResolutionKind::Esm`].
pub fn resolve_module_path(base: &str, request: &str) -> String {
    resolve_module_path_with_kind(base, request, ModuleResolutionKind::CommonJs)
}

/// Like [`resolve_module_path`], but selects `require`/`import` branch from `"exports"`.
pub fn resolve_module_path_with_kind(
    base: &str,
    request: &str,
    kind: ModuleResolutionKind,
) -> String {
    if !request.starts_with('.') && !request.starts_with('/') {
        if let Some(bare) = resolve_bare_specifier(base, request, kind) {
            return bare;
        }
    }

    let req = if request.starts_with('/') {
        PathBuf::from(request)
    } else {
        Path::new(base).join(request)
    };

    let ext = req.extension().and_then(|e| e.to_str()).unwrap_or("");
    if matches!(ext, "js" | "mjs" | "cjs" | "json" | "ts" | "tsx" | "jsx") {
        return req.to_string_lossy().to_string();
    }

    let pkg = req.join("package.json");
    if let Ok(raw) = std::fs::read_to_string(&pkg) {
        if let Some(main_rel) = preferred_package_entry(&raw, kind) {
            return resolve_module_path_with_kind(&req.to_string_lossy(), &main_rel, kind);
        }
    }

    for ext in &["js", "mjs", "cjs", "ts", "tsx", "jsx", "json"] {
        let candidate = req.with_extension(ext);
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }

    for idx in &[
        "index.js",
        "index.mjs",
        "index.cjs",
        "index.ts",
        "index.tsx",
        "index.jsx",
        "index.json",
    ] {
        let candidate = req.join(idx);
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }

    req.with_extension("js").to_string_lossy().to_string()
}

/// Walks upward from `base` to resolve a **bare** NPM specifier (`left-pad`, `@scope/pkg`, …).
/// Uses the same rules as [`resolve_module_path`] (including `package.json` / extensions).
pub fn resolve_npm_package(base: &std::path::Path, name: &str) -> Option<PathBuf> {
    let mut current = base.to_path_buf();
    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }
    resolve_bare_specifier(
        &current.to_string_lossy(),
        name,
        ModuleResolutionKind::CommonJs,
    )
    .map(PathBuf::from)
}

fn resolve_bare_specifier(base: &str, request: &str, kind: ModuleResolutionKind) -> Option<String> {
    let mut current = PathBuf::from(base);
    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }
    loop {
        let nm = current.join("node_modules").join(request);
        if nm.exists() {
            return Some(resolve_module_path_with_kind(
                &current.to_string_lossy(),
                &nm.to_string_lossy(),
                kind,
            ));
        }
        if !current.pop() {
            break;
        }
    }
    None
}

pub(crate) fn extract_main_from_package_json(raw: &str) -> Option<String> {
    extract_string_field(raw, "main")
}

/// Bind CommonJS `require` and path globals used by resolver.
///
/// # Safety
/// `ctx` and `global` must be valid on the installing thread.
pub(crate) unsafe fn install_require(
    ctx: *mut qjs::JSContext,
    global: qjs::JSValue,
    entry_filename: &str,
    base_dir: &str,
) -> Result<(), String> {
    super::bind_require_and_entry_paths(ctx, global, entry_filename, base_dir)
}
