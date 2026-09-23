use anyhow::{Context, Result};
use clap::Parser;
use peachsh::{
    engine::Engine,
    server::{self, App},
    store::{Store, initial_settings},
};
use std::{path::PathBuf, sync::Arc};

#[derive(Parser)]
#[command(name = "peachsh", version, about = "🍑sh harness · Rust")]
struct Args {
    #[arg(long, default_value_t = 3090)]
    port: u16,
    #[arg(long, default_value = "../data-rust")]
    data_dir: PathBuf,
    #[arg(long, default_value = "../data")]
    legacy_data: PathBuf,
    #[arg(long, default_value = "../workspace")]
    workspace: PathBuf,
    #[arg(long)]
    check: bool,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.data_dir).context("无法创建数据目录")?;
    let _instance_lock = if !args.check {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(args.data_dir.join("instance.lock"))?;
        file.try_lock()
            .context("此数据目录已有一个 🍑sh 实例正在运行")?;
        Some(file)
    } else {
        None
    };
    let store = Arc::new(Store::open(&args.data_dir.join("peachsh.sqlite3"))?);
    let settings = match store.settings()? {
        Some(s) => s,
        None => initial_settings(&store, &args.legacy_data, &args.workspace)?,
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
