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

fn main() -> anyhow::Result<()> {
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

    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(
            cli.workers.unwrap_or_else(num_cpus::get).saturating_sub(1).max(1),
        )
        .thread_name("kawkab-io")
        .enable_all()
        .build()?;

    let rt_handle = rt.handle().clone();

    if cli.build_snapshot {
        let file = cli.file.as_ref().ok_or_else(|| {
            anyhow::anyhow!("--build-snapshot requires --file")
        })?;
        return build_snapshot(file, &cli.snapshot_out, &cli);
    }

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

    let scheduler = Scheduler::spawn(worker_count, config, rt_handle.clone())?;

    info!(
        elapsed_ms = boot.elapsed().as_millis(),
        "Kawkab ready (cold-start complete)"
    );

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
        prewarm:    false,
    };

    let mut isolate = Isolate::new(config)?;
    let mut builder = SnapshotBuilder::new();

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
