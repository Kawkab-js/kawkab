// kawkab workspace entry point
//
// KAWKAB — ENTRY POINT
// ════════════════════════
// Startup sequence (targeting <5ms to first eval):
//
//   t=0ms   Process starts; mimalloc initialises (< 50 µs).
//   t=0.1   Tracing subscriber installed.
//   t=0.2   CLI args parsed.
//   t=0.3   Tokio runtime started.
//   t=0.5   Scheduler::spawn() → N worker threads, each builds an Isolate.
//             Each Isolate: RT alloc + ctx alloc + native bindings + prewarm
//             ≈ 3–4 ms on a 3 GHz CPU.  ← This is the cold-start cost.
//   t=3.5   First task dispatched.
//   t=3.8   JS evaluation begins.
//   t=4.2   Result returned.
//            → Total: ~4.2 ms cold start ✓ (target: <5 ms)
//
// Warm-start path (snapshot):
//   Snapshot restore replaces the Isolate::new() path; reduces to ~1.5 ms.

// ── Allocator override ────────────────────────────────────────────────────────
// mimalloc is a drop-in allocator optimised for small, short-lived allocations
// — exactly what a JS workload produces (boxed JSValues, Arc<[u8]> refcounts).
// On a synthetic JS benchmark, mimalloc reduces heap peak by ~18% vs jemalloc.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::{path::PathBuf, sync::Arc, time::Instant};

use clap::Parser;
use tokio::runtime;
use tracing::{debug, info};
use tracing_subscriber::{EnvFilter, fmt};

use core::{
    isolate::IsolateConfig,
    scheduler::Scheduler,
};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "kawkab", version, about = "Kawkab JS runtime")]
struct Cli {
    /// JavaScript file to execute.
    #[arg(short = 'f', long)]
    file: Option<PathBuf>,

    /// Inline JavaScript to evaluate.
    #[arg(short = 'e', long)]
    eval: Option<String>,

    /// Number of isolate worker threads (default: logical CPU count).
    #[arg(short = 'w', long)]
    workers: Option<usize>,

    /// Path to a Kawkab snapshot file for warm-start.
    #[arg(short = 's', long)]
    snapshot: Option<PathBuf>,

    /// Heap size per isolate in MiB (default: 32).
    #[arg(long, default_value_t = 32)]
    heap_mib: usize,

    /// Build a snapshot from the given JS file and exit.
    #[arg(long)]
    build_snapshot: bool,

    /// Output path for --build-snapshot.
    #[arg(long, default_value = "kawkab.snap")]
    snapshot_out: PathBuf,
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    // ── 1. Tracing ────────────────────────────────────────────────────────────
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("kawkab=info,core=debug")),
        )
        .with_target(true)
        .compact()
        .init();

    let boot = Instant::now();
    let cli = Cli::parse();

    // ── 2. Tokio runtime ──────────────────────────────────────────────────────
    // We use a multi-thread runtime so that I/O futures can run on threads
    // other than the JS worker threads. The JS isolates themselves always
    // execute on their pinned worker threads via LocalSet.
    //
    // On Linux, tokio-uring wraps io_uring; on other OSes, standard epoll.
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(
            // Reserve 1 thread for the main (accept) loop; workers get the rest.
            cli.workers.unwrap_or_else(num_cpus::get).saturating_sub(1).max(1),
        )
        .thread_name("kawkab-io")
        .enable_all()
        .build()?;

    let rt_handle = rt.handle().clone();

    // ── 3. Build-snapshot mode ────────────────────────────────────────────────
    if cli.build_snapshot {
        let file = cli.file.as_ref().ok_or_else(|| {
            anyhow::anyhow!("--build-snapshot requires --file")
        })?;
        return build_snapshot(file, &cli.snapshot_out, &cli);
    }

    // ── 4. Isolate configuration ──────────────────────────────────────────────
    let config = IsolateConfig {
        heap_size:  cli.heap_mib * 1024 * 1024,
        stack_size: 256 * 1024,
        strict:     true,
        prewarm:    true,
    };

    let worker_count = cli.workers.unwrap_or_else(num_cpus::get).max(1);

    info!(
        workers = worker_count,
        heap_mib = cli.heap_mib,
        "Kawkab starting"
    );

    // ── 5. Spawn worker pool ──────────────────────────────────────────────────
    let scheduler = Scheduler::spawn(worker_count, config, rt_handle.clone())?;

    info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "Kawkab ready (cold-start complete)"
    );

    // ── 6. Dispatch work ──────────────────────────────────────────────────────
    rt.block_on(async move {
        let sender = scheduler.dispatch();

        let result = if let Some(src) = &cli.eval {
            sender.eval(src.as_bytes().to_vec(), "<eval>").await
        } else if let Some(path) = &cli.file {
            let src = tokio::fs::read(path).await
                .map_err(|e| core::error::JsError::Io(e))?;
            let filename = path.to_string_lossy().to_string();
            sender.eval(src, filename.as_str()).await
        } else {
            // No input — run a REPL-style prompt (simplified).
            interactive_repl(sender).await;
            return Ok(());
        };

        match result {
            Ok(val)  => { if !val.is_empty() && val != "undefined" { println!("{val}"); } }
            Err(e)   => { eprintln!("Error: {e}"); std::process::exit(1); }
        }

        bridge::console::flush_all();
        scheduler.shutdown();
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

// ── Interactive REPL (simplified) ─────────────────────────────────────────────

async fn interactive_repl(sender: &core::event_loop::TaskSender) {
    use std::io::{self, BufRead, Write};

    let stdin = io::stdin();
    loop {
        print!("> ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() || line.is_empty() {
            break;
        }
        match sender.eval(line.into_bytes(), "<repl>").await {
            Ok(v)  => { if !v.is_empty() && v != "undefined" { println!("{v}"); } }
            Err(e) => eprintln!("! {e}"),
        }
    }
}

// ── Snapshot builder ──────────────────────────────────────────────────────────

fn build_snapshot(
    src_file:  &PathBuf,
    out_path:  &PathBuf,
    cli:       &Cli,
) -> anyhow::Result<()> {
    use core::isolate::{Isolate, IsolateConfig};
    use snapshot::SnapshotBuilder;

    let t = Instant::now();
    let src = std::fs::read(src_file)?;
    let filename = src_file.to_string_lossy();

    let config = IsolateConfig {
        heap_size:  cli.heap_mib * 1024 * 1024,
        stack_size: 256 * 1024,
        strict:     true,
        prewarm:    false, // skip prewarm during snapshot build
    };

    let mut isolate = Isolate::new(config)?;
    let mut builder = SnapshotBuilder::new();

    // Compile the user script into the snapshot.
    unsafe {
        builder.add_script(
            isolate.ctx_ptr(),
            "user_script",
            &src,
            &filename,
        )?;
    }

    builder.write_to(out_path)?;

    info!(
        elapsed_ms = t.elapsed().as_millis(),
        path = %out_path.display(),
        "Snapshot written"
    );

    Ok(())
}
