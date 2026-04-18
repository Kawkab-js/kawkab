use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

thread_local! {
    /// `process.env.NODE_ENV` as seen by the embedder (kept in sync from JS `process.env`).
    static PACKAGE_EXPORTS_NODE_ENV: RefCell<String> = RefCell::new(
        std::env::var("NODE_ENV").unwrap_or_else(|_| "production".into()),
    );
}

/// Called when `process` exists; keeps `exports` / `imports` condition matching aligned with JS.
pub(crate) fn set_package_exports_node_env_from_process(node_env: String) {
    PACKAGE_EXPORTS_NODE_ENV.with(|c| *c.borrow_mut() = node_env);
}

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

/// Split `lodash/get` → `("lodash", Some("get"))`, `@s/pkg/x` → `("@s/pkg", Some("x"))`, `foo` → `("foo", None)`.
pub fn split_package_specifier(specifier: &str) -> Option<(String, Option<String>)> {
    if specifier.is_empty() {
        return None;
    }
    let parts: Vec<&str> = specifier.split('/').collect();
    if parts[0].starts_with('@') {
        if parts.len() < 2 {
            return None;
        }
        let pkg = format!("{}/{}", parts[0], parts[1]);
        if parts.len() == 2 {
            Some((pkg, None))
        } else {
            Some((pkg, Some(parts[2..].join("/"))))
        }
    } else if parts.len() == 1 {
        Some((parts[0].to_string(), None))
    } else {
        Some((parts[0].to_string(), Some(parts[1..].join("/"))))
    }
}

fn condition_list(kind: ModuleResolutionKind) -> Vec<String> {
    let ne = PACKAGE_EXPORTS_NODE_ENV.with(|c| c.borrow().clone());
    let ne_key = if ne == "development" {
        "development"
    } else {
        "production"
    };
    match kind {
        ModuleResolutionKind::CommonJs => vec![
            "require".into(),
            "node".into(),
            ne_key.into(),
            "default".into(),
        ],
        ModuleResolutionKind::Esm => vec![
            "import".into(),
            "module".into(),
            "node".into(),
            ne_key.into(),
            "default".into(),
        ],
    }
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

fn rel_to_path(pkg_dir: &Path, rel: &str) -> PathBuf {
    let t = rel.trim_start_matches("./");
    pkg_dir.join(t)
}

/// Resolve conditional / array export entry to a target path string (possibly containing `*`).
fn resolve_export_entry(
    entry: &Value,
    kind: ModuleResolutionKind,
    pattern_capture: Option<&str>,
) -> Option<String> {
    let resolved_str = match entry {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            for x in arr {
                if let Some(out) = resolve_export_entry(x, kind, pattern_capture) {
                    return Some(out);
                }
            }
            return None;
        }
        Value::Object(map) => {
            if map.keys().any(|k| k.starts_with('.')) {
                return None;
            }
            let conds = condition_list(kind);
            for c in &conds {
                if let Some(v) = map.get(c) {
                    if let Some(out) = resolve_export_entry(v, kind, pattern_capture) {
                        return Some(out);
                    }
                }
            }
            if let Some(v) = map.get("default") {
                return resolve_export_entry(v, kind, pattern_capture);
            }
            return None;
        }
        _ => return None,
    };

    if let Some(cap) = pattern_capture {
        if resolved_str.contains('*') {
            let parts: Vec<&str> = resolved_str.splitn(2, '*').collect();
            if parts.len() == 2 {
                let out = format!("{}{}{}", parts[0], cap, parts[1]);
                return sanitize_package_relative(&out);
            }
        }
    }
    sanitize_package_relative(&resolved_str)
}

fn export_pattern_match(pattern: &str, subpath: &str) -> Option<String> {
    if !pattern.contains('*') {
        return None;
    }
    let parts: Vec<&str> = pattern.splitn(2, '*').collect();
    let pre = parts[0];
    let post = parts.get(1).copied().unwrap_or("");
    if !subpath.starts_with(pre) {
        return None;
    }
    let mid = &subpath[pre.len()..];
    if post.is_empty() {
        return Some(mid.to_string());
    }
    if !mid.ends_with(post) {
        return None;
    }
    let cap_len = mid.len().saturating_sub(post.len());
    Some(mid[..cap_len].to_string())
}

fn best_export_match<'a>(
    exports: &'a Map<String, Value>,
    requested: &str,
) -> Option<(&'a Value, Option<String>)> {
    if let Some(v) = exports.get(requested) {
        return Some((v, None));
    }
    let mut best: Option<(&str, &'a Value, String)> = None;
    for (k, v) in exports {
        if !k.starts_with('.') || !k.contains('*') {
            continue;
        }
        if let Some(cap) = export_pattern_match(k, requested) {
            let score = k.len();
            let wins = best
                .as_ref()
                .map(|(bk, _, _)| score > bk.len())
                .unwrap_or(true);
            if wins {
                best = Some((k, v, cap));
            }
        }
    }
    best.map(|(_, v, cap)| (v, Some(cap)))
}

fn resolve_exports_for_subpath(
    exports: &Value,
    subpath: &str,
    kind: ModuleResolutionKind,
) -> Option<String> {
    let requested = if subpath.is_empty() || subpath == "." {
        ".".to_string()
    } else {
        format!("./{}", subpath.trim_start_matches('/'))
    };

    match exports {
        Value::String(s) => {
            if requested == "." {
                resolve_export_entry(&Value::String(s.clone()), kind, None)
            } else {
                None
            }
        }
        Value::Object(map) => {
            let (entry, cap) = best_export_match(map, &requested)?;
            resolve_export_entry(entry, kind, cap.as_deref())
        }
        _ => None,
    }
}

fn legacy_package_main(v: &Value, kind: ModuleResolutionKind) -> Option<String> {
    let s = match kind {
        ModuleResolutionKind::Esm => v
            .get("module")
            .and_then(|x| x.as_str())
            .or_else(|| v.get("main").and_then(|x| x.as_str())),
        ModuleResolutionKind::CommonJs => v
            .get("main")
            .and_then(|x| x.as_str())
            .or_else(|| v.get("module").and_then(|x| x.as_str())),
    };
    s.and_then(|x| sanitize_package_relative(x))
}

fn resolve_package_main_path(
    v: &Value,
    pkg_dir: &Path,
    kind: ModuleResolutionKind,
) -> Option<PathBuf> {
    if let Some(exp) = v.get("exports") {
        if !exp.is_null() {
            if let Some(rel) = resolve_exports_for_subpath(exp, ".", kind) {
                return Some(rel_to_path(pkg_dir, &rel));
            }
        }
    }
    if let Some(p) = legacy_package_main(v, kind).map(|r| rel_to_path(pkg_dir, &r)) {
        return Some(p);
    }
    let idx = pkg_dir.join("index.js");
    if idx.exists() {
        return Some(idx);
    }
    None
}

fn resolve_package_subpath_path(
    v: &Value,
    pkg_dir: &Path,
    subpath: &str,
    kind: ModuleResolutionKind,
) -> Option<PathBuf> {
    let sub = subpath.trim_matches('/');
    if sub.is_empty() {
        return resolve_package_main_path(v, pkg_dir, kind);
    }
    if let Some(exp) = v.get("exports") {
        if !exp.is_null() {
            if let Some(rel) = resolve_exports_for_subpath(exp, sub, kind) {
                return Some(rel_to_path(pkg_dir, &rel));
            }
        }
    }
    Some(pkg_dir.join(sub))
}

fn find_package_json_dir_and_value(base: &str) -> Option<(PathBuf, Value)> {
    let mut dir = PathBuf::from(base);
    if dir.is_file() || !dir.is_dir() {
        dir = dir.parent()?.to_path_buf();
    }
    loop {
        let p = dir.join("package.json");
        if let Ok(raw) = std::fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                return Some((dir, v));
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn best_import_match<'a>(
    imports: &'a Map<String, Value>,
    requested: &str,
) -> Option<(&'a Value, Option<String>)> {
    if let Some(v) = imports.get(requested) {
        return Some((v, None));
    }
    let mut best: Option<(&str, &'a Value, String)> = None;
    for (k, v) in imports {
        if !k.starts_with('#') || !k.contains('*') {
            continue;
        }
        if let Some(cap) = export_pattern_match(k, requested) {
            let score = k.len();
            let wins = best
                .as_ref()
                .map(|(bk, _, _)| score > bk.len())
                .unwrap_or(true);
            if wins {
                best = Some((k, v, cap));
            }
        }
    }
    best.map(|(_, v, cap)| (v, Some(cap)))
}

fn resolve_imports_specifier(
    base: &str,
    specifier: &str,
    kind: ModuleResolutionKind,
) -> Option<String> {
    let (pkg_dir, pkg_val) = find_package_json_dir_and_value(base)?;
    let imports = pkg_val.get("imports")?.as_object()?;
    let (entry, cap) = best_import_match(imports, specifier)?;
    let rel = resolve_export_entry(entry, kind, cap.as_deref())?;
    let path = rel_to_path(&pkg_dir, &rel);
    Some(finish_resolve_existing(path, kind))
}

fn read_package_json_value(pkg_dir: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(pkg_dir.join("package.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

fn package_info_from_value(dir: &Path, v: &Value) -> PackageJsonInfo {
    let module_type = match v.get("type").and_then(|x| x.as_str()) {
        Some("module") => SourceType::Esm,
        _ => SourceType::Cjs,
    };
    PackageJsonInfo {
        dir: dir.to_path_buf(),
        module_type,
        main: v.get("main").and_then(|x| x.as_str()).map(String::from),
        module: v.get("module").and_then(|x| x.as_str()).map(String::from),
    }
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

/// Whether `require()` should run [`crate::transpiler::transpile_ts`] before the CJS wrapper.
///
/// Plain `.js` / `.cjs` sources are already CommonJS; running SWC's `common_js` pass on them
/// breaks real-world packages (for example Express `module.exports = createApplication`).
pub fn require_should_run_transpile(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    matches!(ext, "ts" | "tsx" | "jsx")
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
            let v: Value = serde_json::from_str(&raw).ok()?;
            return Some(package_info_from_value(&dir, &v));
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Very fast ESM syntax heuristic: look for top-level `import`/`export` keywords.
fn has_esm_syntax(source: &str) -> bool {
    for line in source.lines() {
        let t = line.trim_start();
        if t.starts_with("import ") || t.starts_with("import{") {
            return true;
        }
        if t.starts_with("import(") {
            return true;
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

fn finish_resolve_existing(mut path: PathBuf, kind: ModuleResolutionKind) -> String {
    if path.is_file() {
        return path.to_string_lossy().into_owned();
    }

    if path.join("package.json").exists() {
        if let Some(raw) = read_package_json_string(&path) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(main_rel) = preferred_package_entry_from_value(&v, kind) {
                    let base = path.to_string_lossy().into_owned();
                    let rel = main_rel.trim_start_matches("./");
                    return resolve_module_path_with_kind(&base, rel, kind);
                }
            }
        }
    }

    for ext in &["js", "mjs", "cjs", "ts", "tsx", "jsx", "json"] {
        let candidate = path.with_extension(ext);
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
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
        let candidate = path.join(idx);
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    if path.extension().is_none() {
        path.set_extension("js");
    }
    path.to_string_lossy().into_owned()
}

fn read_package_json_string(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("package.json")).ok()
}

fn preferred_package_entry_from_value(v: &Value, kind: ModuleResolutionKind) -> Option<String> {
    if let Some(exp) = v.get("exports") {
        if !exp.is_null() {
            if let Some(p) = resolve_exports_for_subpath(exp, ".", kind) {
                return Some(p);
            }
        }
    }
    legacy_package_main(v, kind)
}

/// Resolve module specifier to absolute path using CommonJS `require` rules.
pub fn resolve_module_path(base: &str, request: &str) -> String {
    resolve_module_path_with_kind(base, request, ModuleResolutionKind::CommonJs)
}

/// Like [`resolve_module_path`], but selects `require`/`import` branch from `"exports"`.
pub fn resolve_module_path_with_kind(
    base: &str,
    request: &str,
    kind: ModuleResolutionKind,
) -> String {
    if request.starts_with('#') {
        if let Some(p) = resolve_imports_specifier(base, request, kind) {
            return p;
        }
        return Path::new(base).join(request).to_string_lossy().into_owned();
    }

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
        return req.to_string_lossy().into_owned();
    }

    if let Ok(raw) = std::fs::read_to_string(req.join("package.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            if let Some(main_rel) = preferred_package_entry_from_value(&v, kind) {
                let rel_trim = main_rel.trim_start_matches("./");
                return resolve_module_path_with_kind(&req.to_string_lossy(), rel_trim, kind);
            }
        }
    }

    finish_resolve_existing(req, kind)
}

fn resolve_bare_specifier(base: &str, request: &str, kind: ModuleResolutionKind) -> Option<String> {
    let (pkg_name, subpath) = split_package_specifier(request)?;
    let mut current = PathBuf::from(base);
    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }
    loop {
        let pkg_dir = current.join("node_modules").join(&pkg_name);
        if pkg_dir.is_dir() {
            let v = read_package_json_value(&pkg_dir).unwrap_or(Value::Null);
            let resolved = if let Some(ref sp) = subpath {
                if sp.is_empty() {
                    resolve_package_main_path(&v, &pkg_dir, kind)
                } else {
                    resolve_package_subpath_path(&v, &pkg_dir, sp, kind)
                }
            } else {
                resolve_package_main_path(&v, &pkg_dir, kind)
            };
            if let Some(p) = resolved {
                return Some(finish_resolve_existing(p, kind));
            }
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Walks upward from `base` to resolve a **bare** NPM specifier (`left-pad`, `@scope/pkg`, …).
pub fn resolve_npm_package(base: &std::path::Path, name: &str) -> Option<PathBuf> {
    resolve_npm_package_with_kind(base, name, ModuleResolutionKind::CommonJs)
}

pub fn resolve_npm_package_with_kind(
    base: &std::path::Path,
    name: &str,
    kind: ModuleResolutionKind,
) -> Option<PathBuf> {
    let mut current = base.to_path_buf();
    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }
    resolve_bare_specifier(&current.to_string_lossy(), name, kind).map(PathBuf::from)
}

#[allow(dead_code)] // Kept for tooling / future call sites that read `main` from raw JSON.
pub(crate) fn extract_main_from_package_json(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw).ok()?;
    v.get("main").and_then(|x| x.as_str()).map(String::from)
}

/// Minimal `"key": "value"` extractor for legacy call sites (prefer JSON parse when possible).
#[allow(dead_code)]
pub(crate) fn extract_string_field(raw: &str, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw).ok()?;
    v.get(key).and_then(|x| x.as_str()).map(String::from)
}

/// Bind CommonJS `require` and path globals used by resolver.
///
/// # Safety
/// `ctx` and `global` must be valid on the installing thread.
pub(crate) unsafe fn install_require(
    ctx: *mut quickjs_sys::JSContext,
    global: quickjs_sys::JSValue,
    entry_filename: &str,
    base_dir: &str,
) -> Result<(), String> {
    super::bind_require_and_entry_paths(ctx, global, entry_filename, base_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn write_file(path: &Path, content: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::File::create(path)
            .unwrap()
            .write_all(content.as_bytes())
            .unwrap();
    }

    #[test]
    fn split_scoped_and_subpath() {
        assert_eq!(
            split_package_specifier("@scope/pkg/foo/bar"),
            Some(("@scope/pkg".into(), Some("foo/bar".into())))
        );
        assert_eq!(
            split_package_specifier("lodash/get"),
            Some(("lodash".into(), Some("get".into())))
        );
        assert_eq!(
            split_package_specifier("axios"),
            Some(("axios".into(), None))
        );
    }

    #[test]
    fn bare_subpath_resolves_under_package() {
        let tmp = std::env::temp_dir().join(format!("kawkab_ml_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("node_modules/lodash")).unwrap();
        write_file(
            &tmp.join("node_modules/lodash/package.json"),
            r#"{"main":"lodash.js"}"#,
        );
        write_file(&tmp.join("node_modules/lodash/get.js"), "exports.x=1");
        let base = tmp.to_string_lossy();
        let p = resolve_module_path(&base, "lodash/get");
        assert!(p.ends_with("get.js"), "got {}", p);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn exports_subpath_conditional() {
        let tmp = std::env::temp_dir().join(format!("kawkab_exp_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("node_modules/left-pad")).unwrap();
        write_file(
            &tmp.join("node_modules/left-pad/package.json"),
            r#"{"exports":{".":{"require":"./c.js","import":"./e.mjs"},"./lite":"./lite.js"}}}"#,
        );
        write_file(&tmp.join("node_modules/left-pad/c.js"), "module.exports=1");
        write_file(&tmp.join("node_modules/left-pad/e.mjs"), "export default 1");
        write_file(
            &tmp.join("node_modules/left-pad/lite.js"),
            "module.exports=2",
        );
        let base = tmp.to_string_lossy();
        let cjs = resolve_module_path(&base, "left-pad/lite");
        assert!(cjs.ends_with("lite.js"), "{}", cjs);
        let esm = resolve_module_path_with_kind(&base, "left-pad/lite", ModuleResolutionKind::Esm);
        assert!(esm.ends_with("lite.js"), "{}", esm);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn exports_pattern() {
        let tmp = std::env::temp_dir().join(format!("kawkab_pat_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("node_modules/p")).unwrap();
        write_file(
            &tmp.join("node_modules/p/package.json"),
            r#"{"exports":{"./lib/*":"./dist/*.js"}}"#,
        );
        write_file(&tmp.join("node_modules/p/dist/foo.js"), "1");
        let base = tmp.to_string_lossy();
        let p = resolve_module_path(&base, "p/lib/foo");
        assert!(p.ends_with("dist/foo.js"), "{}", p);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn imports_hash_specifier() {
        let tmp = std::env::temp_dir().join(format!("kawkab_imp_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        write_file(
            &tmp.join("package.json"),
            r##"{"imports":{"#utils":"./src/utils.js","#x/*.js":"./lib/*.js"}}"##,
        );
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::create_dir_all(tmp.join("lib")).unwrap();
        write_file(&tmp.join("src/utils.js"), "1");
        write_file(&tmp.join("lib/a.js"), "1");
        let base = tmp.join("src").to_string_lossy().into_owned();
        let u = resolve_module_path_with_kind(&base, "#utils", ModuleResolutionKind::Esm);
        assert!(u.ends_with("src/utils.js"), "{}", u);
        let x = resolve_module_path_with_kind(&base, "#x/a.js", ModuleResolutionKind::Esm);
        assert!(x.ends_with("lib/a.js"), "{}", x);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn nested_conditions_resolve() {
        let v: Value = serde_json::from_str(
            r#"{"exports":{".":{"node":{"require":"./c.js","import":"./e.mjs"},"default":"./d.js"}}}"#,
        )
        .unwrap();
        let exp = v.get("exports").unwrap();
        let r = resolve_exports_for_subpath(exp, ".", ModuleResolutionKind::CommonJs).unwrap();
        assert_eq!(r, "./c.js");
        let e = resolve_exports_for_subpath(exp, ".", ModuleResolutionKind::Esm).unwrap();
        assert_eq!(e, "./e.mjs");
    }
}
