use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "Сервер мессенджера (без окна)")]
struct Args {
    /// Адрес для прослушивания (0.0.0.0 — все сетевые интерфейсы)
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// Порт
    #[arg(long, default_value_t = 7878)]
    port: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    match server::start(&args.host, args.port).await {
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
