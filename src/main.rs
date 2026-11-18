use anyhow::{Context, Result};
use clap::Parser;
use coda::{
    engine::Engine,
    server::{self, App},
    store::{Store, fresh_settings, initial_settings},
};
use std::{future::IntoFuture, path::PathBuf, sync::Arc, time::Duration};

mod cli;

#[derive(Parser)]
#[command(
    name = "coda",
    version,
    about = "Coda CLI · Native AI Coding Workbench"
)]
struct Args {
    #[arg(long, default_value_t = 3090, value_parser = nonzero_port)]
    port: u16,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Explicitly import legacy settings/credentials on first initialization only.
    /// Without this option a new database starts with no routes or account.
    #[arg(long)]
    legacy_data: Option<PathBuf>,
    #[arg(long)]
    workspace: Option<PathBuf>,
    /// Validate startup; may create/migrate the database and initialize settings.
    /// This is not a read-only check. Legacy import requires --legacy-data.
    #[arg(long)]
    check: bool,
    #[arg(long)]
    json: bool,
    #[command(subcommand)]
    command: Option<cli::Command>,
}

fn nonzero_port(value: &str) -> std::result::Result<u16, String> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| "端口必须在 1..=65535".to_owned())
}

fn main() -> Result<()> {
    // Declare the lock before the runtime so unwinding also tears the runtime
    // down first. Startup/run errors must not admit another Host while old
    // blocking work is still being drained.
    let mut instance_lock = None;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(&mut instance_lock));
    // Bound runtime teardown on exit.
    let instance_lock = teardown_runtime(runtime, instance_lock, Duration::from_secs(1));
    if result.is_err() {
        std::mem::forget(instance_lock);
    }
    result
}

fn teardown_runtime(
    runtime: tokio::runtime::Runtime,
    instance_lock: Option<std::fs::File>,
    timeout: Duration,
) -> Option<std::fs::File> {
    runtime.shutdown_timeout(timeout);
    instance_lock
}

async fn run(instance_lock: &mut Option<std::fs::File>) -> Result<()> {
    let args = Args::try_parse().unwrap_or_else(|error| {
        if error.use_stderr() && std::env::args_os().any(|arg| arg == "host") {
            clap::Error::raw(
                clap::error::ErrorKind::ValueValidation,
                "Host 命令参数无效；秘密只能通过隐藏提示或重定向 stdin 输入，请使用 --help",
            )
            .exit();
        }
        error.exit()
    });

    if let Some(command) = args.command
        && !matches!(command, cli::Command::Serve)
    {
        if args.check {
            clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--check 不能与 CLI 子命令同时使用",
            )
            .exit();
        }
        if command.is_local_workbench() {
            if args.legacy_data.is_some() {
                clap::Error::raw(
                    clap::error::ErrorKind::ArgumentConflict,
                    "--legacy-data 不能与本地工作台子命令同时使用",
                )
                .exit();
            }
            if !command.valid_ids() {
                clap::Error::raw(clap::error::ErrorKind::ValueValidation, "命令参数无效").exit();
            }
            if let Err(error) = cli::execute_local(
                instance_lock,
                args.data_dir,
                args.workspace,
                args.json,
                command,
            )
            .await
            {
                error.print(args.json);
                std::process::exit(1);
            }
            return Ok(());
        }

        if args.data_dir.is_some() || args.legacy_data.is_some() || args.workspace.is_some() {
            clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "CLI 子命令不能与服务数据路径参数同时使用",
            )
            .exit();
        }
        if !command.valid_ids() {
            clap::Error::raw(clap::error::ErrorKind::ValueValidation, "命令参数无效").exit();
        }
        if let Err(error) = cli::execute(args.port, args.json, command).await {
            error.print(args.json);
            std::process::exit(1);
        }
        return Ok(());
    }

    if args.json {
        clap::Error::raw(
            clap::error::ErrorKind::MissingSubcommand,
            "--json 需要 CLI 子命令",
        )
        .exit();
    }

    let data_dir = args
        .data_dir
        .unwrap_or_else(|| PathBuf::from("../data-rust"));
    let workspace = args
        .workspace
        .unwrap_or_else(|| PathBuf::from("../workspace"));
    std::fs::create_dir_all(&data_dir).context("无法创建数据目录")?;
    let store = Arc::new(Store::open(&data_dir.join("coda.sqlite3"))?);
    *instance_lock = Some(store.instance_lock_handle()?);

    let settings = match store.settings()? {
        Some(s) => s,
        None => match args.legacy_data.as_deref() {
            Some(legacy_data) => initial_settings(&store, legacy_data, &workspace)?,
            None => fresh_settings(&store, &workspace)?,
        },
    };
    settings.validate()?;
    if args.check {
        println!(
            "Coda CLI {} · Rust\n数据库：正常\n项目目录：{}\n模型路由：{}\n可启动：是",
            env!("CARGO_PKG_VERSION"),
            settings.workspace,
            settings.routes.len()
        );
        return Ok(());
    }

    let (engine, interrupted) = Engine::new_recovered(store, settings.max_concurrency)?;
    let origin = format!("http://127.0.0.1:{}", args.port);
    let app = server::router(App {
        engine: engine.clone(),
        token: coda::domain::id(),
        origin: origin.clone(),
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, args.port))
        .await
        .context(format!(
            "无法监听端口 {}，请关闭占用程序或使用 --port 指定其他端口",
            args.port
        ))?;
    println!(
        "Coda Host {} · Rust\n{origin}\n恢复为中断状态的任务：{interrupted}",
        env!("CARGO_PKG_VERSION")
    );
    let http_stop = tokio_util::sync::CancellationToken::new();
    let serving = axum::serve(listener, app)
        .with_graceful_shutdown(http_stop.clone().cancelled_owned())
        .into_future();
    tokio::pin!(serving);
    let (completed, signal) = tokio::select! {
        result = &mut serving => (Some(result), Ok(())),
        signal = tokio::signal::ctrl_c() => (None, signal),
    };
    engine.request_shutdown();
    http_stop.cancel();
    let timeout = Duration::from_secs(30);
    let (http, shutdown) = tokio::join!(
        async {
            match completed {
                Some(result) => result.context("HTTP 服务退出失败"),
                None => tokio::time::timeout(timeout, &mut serving)
                    .await
                    .context("HTTP 关闭超时；结果 unknown")?
                    .context("HTTP 服务退出失败"),
            }
        },
        engine.shutdown(timeout),
    );
    anyhow::ensure!(
        shutdown.settled(),
        "Host 关闭结果 unknown：{} 个后台工作未收束，持久化或工作失败={}，外部结果未知={}，超时={}",
        shutdown.remaining_workers,
        shutdown.persistence_or_worker_failure,
        shutdown.external_outcome_unknown,
        shutdown.timed_out
    );
    http?;
    signal.context("无法等待关闭信号")?;
    Ok(())
}

#[cfg(test)]
mod audit_shutdown_tests {
    use super::*;
    use std::sync::mpsc;

    struct TeardownStarted(mpsc::Sender<()>);
    impl Drop for TeardownStarted {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    #[test]
    fn instance_lock_covers_runtime_teardown_after_run_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("instance.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        lock.try_lock().unwrap();
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        let (teardown_tx, teardown_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let _guard = TeardownStarted(teardown_tx);
            let _ = ready_tx.send(());
            std::future::pending::<()>().await;
        });
        runtime.block_on(ready_rx).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let blocking = runtime.spawn_blocking(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
            let _ = finished_tx.send(());
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let teardown = std::thread::spawn(move || {
            let result: Result<()> = Err(anyhow::anyhow!("synthetic run failure"));
            let lock = teardown_runtime(runtime, Some(lock), Duration::from_secs(5));
            drop(lock);
            let _ = stopped_tx.send(result);
        });
        teardown_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let during_teardown = contender.try_lock();
        let still_draining = stopped_rx.try_recv().is_err();
        let _ = release_tx.send(());
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let result = stopped_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        teardown.join().unwrap();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(blocking)
            .unwrap();
        assert!(
            during_teardown.is_err(),
            "another Host acquired the lock during teardown"
        );
        assert!(
            still_draining,
            "teardown returned while the blocking writer was held"
        );
        assert_eq!(result.unwrap_err().to_string(), "synthetic run failure");
        contender.try_lock().unwrap();
    }

    #[test]
    fn instance_lock_survives_teardown_timeout_until_owner_releases_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("instance.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        lock.try_lock().unwrap();
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let blocking = runtime.spawn_blocking(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
            let _ = finished_tx.send(());
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let retained = teardown_runtime(runtime, Some(lock), Duration::from_millis(20));
        assert!(
            finished_rx.try_recv().is_err(),
            "the writer must still be held"
        );
        let while_writer_alive = contender.try_lock();
        let _ = release_tx.send(());
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(blocking)
            .unwrap();
        assert!(
            while_writer_alive.is_err(),
            "timeout released the instance lock"
        );
        assert!(
            contender.try_lock().is_err(),
            "teardown must return the original locked handle"
        );
        drop(retained);
        contender.try_lock().unwrap();
    }
}
