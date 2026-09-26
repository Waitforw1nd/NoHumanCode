use anyhow::{Context, Result};
use clap::Parser;
use peachsh::{
    engine::Engine,
    server::{self, App},
    store::{Store, initial_settings},
};
use std::{path::PathBuf, sync::Arc};

mod cli;

#[derive(Parser)]
#[command(name = "peachsh", version, about = "🍑sh harness · Rust")]
struct Args {
    #[arg(long, default_value_t = 3090, value_parser = nonzero_port)]
    port: u16,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    legacy_data: Option<PathBuf>,
    #[arg(long)]
    workspace: Option<PathBuf>,
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
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(command) = args.command {
        if args.check {
            clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--check 不能与 CLI 子命令同时使用",
            )
            .exit();
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
    let legacy_data = args.legacy_data.unwrap_or_else(|| PathBuf::from("../data"));
    let workspace = args
        .workspace
        .unwrap_or_else(|| PathBuf::from("../workspace"));
    std::fs::create_dir_all(&data_dir).context("无法创建数据目录")?;
    let _instance_lock = if !args.check {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_dir.join("instance.lock"))?;
        file.try_lock()
            .context("此数据目录已有一个 🍑sh 实例正在运行")?;
        Some(file)
    } else {
        None
    };
    let store = Arc::new(Store::open(&data_dir.join("peachsh.sqlite3"))?);
    let settings = match store.settings()? {
        Some(s) => s,
        None => initial_settings(&store, &legacy_data, &workspace)?,
    };
    settings.validate()?;
    if args.check {
        println!(
            "🍑sh {} · Rust\n数据库：正常\n项目目录：{}\n模型路由：{}\n可启动：是",
            env!("CARGO_PKG_VERSION"),
            settings.workspace,
            settings.routes.len()
        );
        return Ok(());
    }
    let interrupted = store.recover()?;
    let engine = Engine::new(store, settings.max_concurrency)?;
    let origin = format!("http://127.0.0.1:{}", args.port);
    let app = server::router(App {
        engine: engine.clone(),
        token: peachsh::domain::id(),
        origin: origin.clone(),
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, args.port))
        .await
        .context(format!(
            "无法监听端口 {}，请关闭占用程序或使用 --port 指定其他端口",
            args.port
        ))?;
    println!(
        "🍑sh harness {} · Rust\n{origin}\n恢复为中断状态的任务：{interrupted}",
        env!("CARGO_PKG_VERSION")
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            engine.cancel_all();
        })
        .await?;
    Ok(())
}
