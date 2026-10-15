//! Консольный тестовый клиент.
//! Пример: cargo run -p server --bin testclient -- --name Вася --url ws://127.0.0.1:7878
//! Каждая введённая строка уходит в чат. Выход: Ctrl+C.

use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use shared::{ClientMsg, ServerMsg};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "Тест")]
    name: String,
    #[arg(long, default_value = "ws://127.0.0.1:7878")]
    url: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let url = format!("{}/ws?name={}", args.url.trim_end_matches('/'), urlencode(&args.name));
    let (ws, _) = connect_async(&url).await.expect("Не удалось подключиться");
    let (mut sink, mut stream) = ws.split();
    println!("Подключено как {}", args.name);

    let reader = tokio::spawn(async move {
        while let Some(Ok(Message::Text(raw))) = stream.next().await {
            match serde_json::from_str::<ServerMsg>(&raw) {
                Ok(ServerMsg::Text { from, text, ts, .. }) => println!("[{ts}] {from}: {text}"),
                Ok(ServerMsg::System { text }) => println!("* {text}"),
                Ok(ServerMsg::Error { text }) => println!("! ошибка: {text}"),
                Ok(ServerMsg::Welcome { user }) => println!("Добро пожаловать, {user}"),
                Err(e) => println!("? не разобрал: {e} ({raw})"),
            }
        }
        println!("Соединение закрыто");
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let msg = serde_json::to_string(&ClientMsg::Text { text: line }).unwrap();
        if sink.send(Message::Text(msg)).await.is_err() {
            break;
        }
    }
    reader.abort();
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
