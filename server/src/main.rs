use clap::Parser;
use server::Config;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(about = "Сервер Шторки 0.0.2 (без окна)")]
struct Args {
    /// Путь к config.toml (по умолчанию — рядом с текущей папкой)
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
    /// Перекрыть адрес из конфига (0.0.0.0 — все сетевые интерфейсы)
    #[arg(long)]
    host: Option<String>,
    /// Перекрыть порт из конфига
    #[arg(long)]
    port: Option<u16>,
    /// Перекрыть папку данных из конфига (база server.db + files/)
    #[arg(long)]
    data: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let mut cfg = match Config::load_or_create(&args.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Ошибка конфига: {e}");
            std::process::exit(1);
        }
    };
    if let Some(host) = args.host {
        cfg.host = host;
    }
    if let Some(port) = args.port {
        cfg.port = port;
    }
    if let Some(data) = args.data {
        cfg.data_dir = data.to_string_lossy().to_string();
    }

    match server::start(cfg).await {
        Ok(handle) => {
            println!("Аккаунтов в базе: {}", handle.account_count());
            let _ = tokio::signal::ctrl_c().await;
            handle.stop().await;
        }
        Err(e) => {
            eprintln!("Ошибка: {e}");
            std::process::exit(1);
        }
    }
}
