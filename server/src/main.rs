use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(about = "Сервер Шторки (без окна)")]
struct Args {
    /// Адрес для прослушивания (0.0.0.0 — все сетевые интерфейсы)
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// Порт
    #[arg(long, default_value_t = 7878)]
    port: u16,
    /// Папка для файлов
    #[arg(long, default_value = "data")]
    data: PathBuf,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    match server::start(&args.host, args.port, args.data).await {
        Ok(handle) => {
            let _ = tokio::signal::ctrl_c().await;
            handle.stop().await;
        }
        Err(e) => {
            eprintln!("Ошибка: {e}");
            std::process::exit(1);
        }
    }
}
