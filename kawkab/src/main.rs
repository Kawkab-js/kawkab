use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use kawkab_core::ffi::{js_free_value, js_string_to_owned};
use quickjs_sys as qjs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngineMode {
    Auto,
    QuickJs,
    Node,
}

struct RuntimeCliArgs {
    file: PathBuf,
    engine: EngineMode,
    verbose: bool,
}

enum CliArgs {
    Runtime(RuntimeCliArgs),
    Pm(pm::PmCommand),
}

struct LoadedSource {
    exec_src: Vec<u8>,
    cache_material: Vec<u8>,
    /// Whether the entry file should be loaded as native ESM.
    is_esm: bool,
}

/// Version bump invalidates on-disk bytecode when cache key scheme changes.
const TS_CACHE_SALT: &str = "kawkab-bytecode-v10";

fn append_exec_fingerprint(key_material: &mut Vec<u8>, exec_src: &[u8]) {
    key_material.extend_from_slice(b":exec:");
    key_material.extend_from_slice(blake3::hash(exec_src).as_bytes());
}

fn main() -> anyhow::Result<()> {
    let cli = parse_args()?;
    match cli {
        CliArgs::Pm(command) => {
            let cwd = std::env::current_dir()?;
            pm::execute_command(&cwd, command)
        }
        CliArgs::Runtime(cli) => {
            let file = cli.file;
            let loaded = load_source_for_runtime(&file)?;
            let filename = file.to_string_lossy().to_string();
            match cli.engine {
                EngineMode::Node => run_with_node(filename.as_str()),
                EngineMode::QuickJs => run_with_quickjs(&loaded, &filename, false, cli.verbose),
                EngineMode::Auto => run_with_quickjs(&loaded, &filename, true, cli.verbose),
            }
        }
    }
}

fn load_source_for_runtime(file: &PathBuf) -> anyhow::Result<LoadedSource> {
    let raw = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let filename = file.to_string_lossy().to_string();

    let src_type = kawkab_core::node::module_loader::detect_source_type(&filename, &raw);
    let is_json = src_type == kawkab_core::node::module_loader::SourceType::Json;
    let is_esm = src_type == kawkab_core::node::module_loader::SourceType::Esm;

    if is_json {
        let exec_src = raw.into_bytes();
        let mut cache_material = exec_src.clone();
        append_exec_fingerprint(&mut cache_material, &exec_src);
        return Ok(LoadedSource {
            exec_src,
            cache_material,
            is_esm: false,
        });
    }

    if is_esm {
        let js = kawkab_core::transpiler::strip_types_only(&raw, &filename)
            .with_context(|| format!("failed to transpile ESM {}", file.display()))?;

        if std::env::var("KAWKAB_DEBUG").is_ok() {
            eprintln!("--- DEBUG ESM JS ---");
            eprintln!("{}", js);
            eprintln!("-------------------");
        }
        let mut key_material = Vec::with_capacity(TS_CACHE_SALT.len() + raw.len() + 48);
        key_material.extend_from_slice(TS_CACHE_SALT.as_bytes());
        key_material.extend_from_slice(b":esm:");
        key_material.extend_from_slice(raw.as_bytes());
        let exec_src = js.into_bytes();
        append_exec_fingerprint(&mut key_material, &exec_src);
        return Ok(LoadedSource {
            exec_src,
            cache_material: key_material,
            is_esm: true,
        });
    }

    let ext = file
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default();
    let js = if matches!(ext, "ts" | "tsx" | "jsx") {
        kawkab_core::transpiler::transpile_ts(&raw, &filename)
            .with_context(|| format!("failed to transpile {}", file.display()))?
    } else {
        raw.clone()
    };

    if std::env::var("KAWKAB_DEBUG").is_ok() {
        eprintln!("--- DEBUG CJS JS ---");
        eprintln!("{}", js);
        eprintln!("-------------------");
    }
    let mut key_material = Vec::with_capacity(TS_CACHE_SALT.len() + raw.len() + 48);
    key_material.extend_from_slice(TS_CACHE_SALT.as_bytes());
    key_material.extend_from_slice(b":cjs:");
    key_material.extend_from_slice(raw.as_bytes());
    let exec_src = js.into_bytes();
    append_exec_fingerprint(&mut key_material, &exec_src);
    Ok(LoadedSource {
        exec_src,
        cache_material: key_material,
        is_esm: false,
    })
}

fn parse_args() -> anyhow::Result<CliArgs> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I>(args: I) -> anyhow::Result<CliArgs>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    let raw_args = args.into_iter().map(Into::into).collect::<Vec<String>>();
    if raw_args.is_empty() {
        anyhow::bail!(
            "usage: kawkab --file <path-to-script.(js|jsx|ts|tsx)> [--engine auto|quickjs|node] \
             | kawkab install|add|remove|update|run|outdated|why|doctor"
        );
    }

    if let Some(pm) = parse_pm_command(&raw_args)? {
        return Ok(CliArgs::Pm(pm));
    }

    let mut args = raw_args.into_iter();
    let mut file: Option<PathBuf> = None;
    let mut engine = EngineMode::Auto;
    let mut verbose = false;

    while let Some(arg) = args.next() {
        if arg == "--file" || arg == "-f" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing value for --file"))?;
            file = Some(PathBuf::from(value));
        } else if arg == "--engine" {
            let value = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing value for --engine"))?;
            engine = match value.as_str() {
                "auto" => EngineMode::Auto,
                "quickjs" => EngineMode::QuickJs,
                "node" => EngineMode::Node,
                _ => anyhow::bail!("invalid --engine value: {value} (use auto|quickjs|node)"),
            };
        } else if arg == "--verbose" || arg == "-v" {
            verbose = true;
        }
    }

    let Some(file) = file else {
        anyhow::bail!(
            "usage: kawkab --file <path-to-script.(js|jsx|ts|tsx)> [--engine auto|quickjs|node]"
        );
    };
    Ok(CliArgs::Runtime(RuntimeCliArgs {
        file,
        engine,
        verbose,
    }))
}

fn parse_pm_command(args: &[String]) -> anyhow::Result<Option<pm::PmCommand>> {
    match args[0].as_str() {
        "install" | "i" => Ok(Some(pm::PmCommand::Install)),
        "add" => {
            let Some(name) = args.get(1) else {
                anyhow::bail!("usage: kawkab add <package[@range]> [--dev|--peer|--optional]");
            };
            let (pkg_name, range) = split_package_and_range(name);
            let section = if args.iter().any(|a| a == "--dev") {
                pm::manifest::DependencySection::DevDependencies
            } else if args.iter().any(|a| a == "--peer") {
                pm::manifest::DependencySection::PeerDependencies
            } else if args.iter().any(|a| a == "--optional") {
                pm::manifest::DependencySection::OptionalDependencies
            } else {
                pm::manifest::DependencySection::Dependencies
            };
            Ok(Some(pm::PmCommand::Add {
                name: pkg_name,
                range,
                section,
            }))
        }
        "remove" | "rm" => {
            let Some(name) = args.get(1) else {
                anyhow::bail!("usage: kawkab remove <package>");
            };
            Ok(Some(pm::PmCommand::Remove { name: name.clone() }))
        }
        "update" | "up" => {
            let strategy = if args.iter().any(|a| a == "--patch") {
                pm::UpdateStrategy::Patch
            } else if args.iter().any(|a| a == "--minor") {
                pm::UpdateStrategy::Minor
            } else {
                pm::UpdateStrategy::Latest
            };
            Ok(Some(pm::PmCommand::Update { strategy }))
        }
        "run" => {
            let Some(script) = args.get(1) else {
                anyhow::bail!("usage: kawkab run <script> [args...]");
            };
            Ok(Some(pm::PmCommand::Run {
                script: script.clone(),
                args: args.iter().skip(2).cloned().collect(),
            }))
        }
        "outdated" => Ok(Some(pm::PmCommand::Outdated)),
        "why" => {
            let Some(name) = args.get(1) else {
                anyhow::bail!(
                    "usage: kawkab why <package> [--json] [--pretty=false] [--json-schema]"
                );
            };
            let json = args.iter().any(|a| a == "--json");
            let pretty = !args.iter().any(|a| a == "--pretty=false");
            let schema = args.iter().any(|a| a == "--json-schema");
            Ok(Some(pm::PmCommand::Why {
                name: name.clone(),
                json,
                pretty,
                schema,
            }))
        }
        "doctor" => {
            let json = args.iter().any(|a| a == "--json");
            let pretty = !args.iter().any(|a| a == "--pretty=false");
            Ok(Some(pm::PmCommand::Doctor { json, pretty }))
        }
        _ => Ok(None),
    }
}

fn split_package_and_range(value: &str) -> (String, String) {
    if value.starts_with('@') {
        if let Some(at) = value[1..].find('@') {
            let split = at + 1;
            let name = value[..split + 1].to_string();
            let range = value[split + 2..].to_string();
            if range.is_empty() {
                return (name, "^latest".to_string());
            }
            return (name, range);
        }
        return (value.to_string(), "^latest".to_string());
    }
    if let Some(split) = value.find('@') {
        let name = value[..split].to_string();
        let range = value[split + 1..].to_string();
        if range.is_empty() {
            return (name, "^latest".to_string());
        }
        return (name, range);
    }
    (value.to_string(), "^latest".to_string())
}

fn run_with_quickjs(
    loaded: &LoadedSource,
    filename: &str,
    allow_node_fallback: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create Tokio runtime")?;

    let result = rt.block_on(async move {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(run_quickjs_inner(
                loaded,
                filename,
                allow_node_fallback,
                verbose,
            ))
            .await
    });

    result
}

async fn run_quickjs_inner(
    loaded: &LoadedSource,
    filename: &str,
    allow_node_fallback: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    use kawkab_core::{
        event_loop::TaskSender,
        isolate::{Isolate, IsolateConfig},
    };

    let mut isolate =
        Isolate::new(IsolateConfig::default()).context("failed to create QuickJS isolate")?;
    kawkab_core::console::install(&mut isolate).context("failed to install console")?;
    let ctx = isolate.ctx_ptr();

    let filename_abs = std::fs::canonicalize(filename)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| filename.to_string());
    let filename = &filename_abs;

    let _rt_handle = tokio::runtime::Handle::current();
    let (task_tx, mut task_rx) =
        tokio::sync::mpsc::unbounded_channel::<kawkab_core::event_loop::Task>();
    let sender = TaskSender::from_sender(task_tx.clone());

    unsafe {
        kawkab_core::node::install_runtime(ctx, filename, Some(sender))
            .map_err(|e| anyhow::anyhow!(e))?;
    }

    let eval_val = if loaded.is_esm {
        match unsafe { kawkab_core::node::eval_esm_entry(ctx, filename) } {
            Ok(v) => v,
            Err(e) => {
                if allow_node_fallback {
                    if verbose {
                        eprintln!("[auto] QuickJS ESM failed: {e}");
                        eprintln!("[auto] Switching to Node.js engine...");
                    }
                    if run_with_node(filename).is_ok() {
                        return Ok(());
                    }
                }
                anyhow::bail!("{e}");
            }
        }
    } else {
        let v = eval_with_bytecode_cache(
            ctx,
            &loaded.exec_src,
            &loaded.cache_material,
            filename,
            verbose,
        )
        .with_context(|| format!("failed to run {filename}"))?;

        if js_is_exception(v) {
            let err = unsafe { js_exception_string(ctx) };
            unsafe { js_free_value(ctx, v) };
            if allow_node_fallback {
                if verbose {
                    eprintln!("[auto] QuickJS failed: {err}");
                    eprintln!("[auto] Switching to Node.js engine...");
                }
                if run_with_node(filename).is_ok() {
                    return Ok(());
                }
            }
            anyhow::bail!("JavaScript error: {err}");
        }
        v
    };

    loop {
        loop {
            let rt_ptr = unsafe { qjs::JS_GetRuntime(ctx) };
            let mut ctx_out: *mut qjs::JSContext = std::ptr::null_mut();
            let res = unsafe { qjs::JS_ExecutePendingJob(rt_ptr, &mut ctx_out) };
            if res <= 0 {
                break;
            }
        }

        let mut got_task = false;
        let mut pending_http = Vec::<(u64, tokio::net::TcpStream)>::new();
        loop {
            match task_rx.try_recv() {
                Ok(kawkab_core::event_loop::Task::HttpConnection { server_id, stream }) => {
                    got_task = true;
                    pending_http.push((server_id, stream));
                }
                Ok(task) => {
                    got_task = true;
                    handle_cli_task(ctx, task);
                }
                Err(_) => break,
            }
        }
        for (server_id, stream) in pending_http {
            unsafe {
                let _ = kawkab_core::node::dispatch_http_connection(ctx, server_id, stream).await;
            }
        }

        if got_task {
            continue;
        }

        let pending = kawkab_core::node::PENDING_ASYNC_TIMERS
            .load(std::sync::atomic::Ordering::Relaxed)
            + kawkab_core::node::PENDING_HOST_ASYNC.load(std::sync::atomic::Ordering::Relaxed);
        if pending == 0 {
            break;
        }

        match task_rx.recv().await {
            Some(kawkab_core::event_loop::Task::HttpConnection { server_id, stream }) => unsafe {
                let _ = kawkab_core::node::dispatch_http_connection(ctx, server_id, stream).await;
            },
            Some(task) => handle_cli_task(ctx, task),
            None => break,
        }
    }

    unsafe { js_free_value(ctx, eval_val) };

    kawkab_core::console::flush_all();
    unsafe {
        kawkab_core::node::clear_module_caches(ctx);
    }

    Ok(())
}

/// CLI path: run one event-loop task on the QuickJS context.
fn handle_cli_task(ctx: *mut qjs::JSContext, task: kawkab_core::event_loop::Task) {
    unsafe {
        kawkab_core::node::dispatch_cli_isolate_task(ctx, task);
    }
}

/// Ensure compiled CJS sees `Buffer` by seeding it from `globalThis` before bytecode compile.
fn cjs_bytecode_source(exec_src: &[u8]) -> Vec<u8> {
    let mut out = b"var Buffer = globalThis.Buffer;\n".to_vec();
    out.extend_from_slice(exec_src);
    out
}

fn eval_with_bytecode_cache(
    ctx: *mut qjs::JSContext,
    exec_src: &[u8],
    cache_material: &[u8],
    filename: &str,
    verbose: bool,
) -> anyhow::Result<qjs::JSValue> {
    let skip_disk = std::env::var("KAWKAB_SKIP_BYTECODE")
        .map(|v| v == "1")
        .unwrap_or(false);

    if !skip_disk {
        let cache_dir = bytecode_cache_dir();
        let disk = kawkab_core::bytecode::DiskCache::new(&cache_dir)
            .with_context(|| format!("failed to init bytecode cache at {}", cache_dir.display()))?;

        let canonical = canonical_for_key(filename);
        let key = kawkab_core::bytecode::DiskCache::cache_key(&canonical, cache_material);
        if let Some(bc) = disk
            .load(&key)
            .context("failed to read bytecode cache entry")?
        {
            if verbose {
                eprintln!("[quickjs] bytecode cache hit");
            }
            let v = unsafe { kawkab_core::bytecode::exec(ctx, &bc) }
                .map_err(|e| anyhow::anyhow!("bytecode exec failed: {e}"))?;
            return Ok(v);
        }

        if verbose {
            eprintln!("[quickjs] bytecode cache miss -> compile");
        }
        let src = cjs_bytecode_source(exec_src);
        let bc = kawkab_core::bytecode::compile(ctx, &src, filename)
            .map_err(|e| anyhow::anyhow!("bytecode compile failed: {e}"))?;
        let _ = disk.store(&key, &bc);
        let v = unsafe { kawkab_core::bytecode::exec(ctx, &bc) }
            .map_err(|e| anyhow::anyhow!("bytecode exec failed: {e}"))?;
        return Ok(v);
    }

    if verbose {
        eprintln!("[quickjs] bytecode disk cache skipped (KAWKAB_SKIP_BYTECODE=1)");
    }
    let src = cjs_bytecode_source(exec_src);
    let bc = kawkab_core::bytecode::compile(ctx, &src, filename)
        .map_err(|e| anyhow::anyhow!("bytecode compile failed: {e}"))?;
    let v = unsafe { kawkab_core::bytecode::exec(ctx, &bc) }
        .map_err(|e| anyhow::anyhow!("bytecode exec failed: {e}"))?;
    Ok(v)
}

fn canonical_for_key(filename: &str) -> String {
    std::fs::canonicalize(filename)
        .unwrap_or_else(|_| PathBuf::from(filename))
        .to_string_lossy()
        .to_string()
}

fn bytecode_cache_dir() -> PathBuf {
    if let Ok(v) = std::env::var("KAWKAB_CACHE_DIR") {
        return PathBuf::from(v);
    }
    if let Ok(home) = std::env::var("HOME") {
        return Path::new(&home)
            .join(".cache")
            .join("kawkab")
            .join("bytecode");
    }
    std::env::temp_dir().join("kawkab-bytecode")
}

unsafe fn js_exception_string(ctx: *mut qjs::JSContext) -> String {
    let exc = qjs::JS_GetException(ctx);
    let out = js_string_to_owned(ctx, exc);
    js_free_value(ctx, exc);
    out
}

fn js_is_exception(value: qjs::JSValue) -> bool {
    value.tag == qjs::JS_TAG_EXCEPTION as i64
}

fn run_with_node(filename: &str) -> anyhow::Result<()> {
    let output = Command::new("node")
        .arg(filename)
        .output()
        .map_err(|e| anyhow::anyhow!("node engine unavailable: {e}"))?;

    if !output.stdout.is_empty() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!("node engine failed with status {}", output.status)
    }
}
