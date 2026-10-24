//! Консольный тестовый клиент: регистрация/вход, затем каждая строка уходит в чат.
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use shared::{AuthReq, AuthResp, ClientMsg, ServerMsg};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "Тест")]
    name: String,
    #[arg(long, default_value = "1234")]
    password: String,
    #[arg(long, default_value = "http://127.0.0.1:7878")]
    url: String,
    /// Если аккаунта нет — зарегистрировать, а не только входить.
    #[arg(long)]
    register: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let base = args.url.trim_end_matches('/').replace("ws://", "http://").replace("wss://", "https://");

    // Вход; при неудаче — регистрация (или сразу регистрация по флагу).
    let mut resp = post_auth(&base, "login", &args.name, &args.password).await;
    if resp.is_none() && !args.register {
        println!("Вход не удался, пробую зарегистрироваться…");
        resp = post_auth(&base, "register", &args.name, &args.password).await;
    }
    if args.register && resp.is_none() {
        resp = post_auth(&base, "register", &args.name, &args.password).await;
    }
    let AuthResp { token, name } = match resp {
        Some(r) => r,
        None => {
            eprintln!("Не удалось войти или зарегистрироваться");
            std::process::exit(1);
        }
    };

    let ws_base = args
        .url
        .trim_end_matches('/')
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let url = format!("{ws_base}/ws?name={}&token={}", urlencode(&name), urlencode(&token));
    let (ws, _) = connect_async(&url).await.expect("Не удалось подключиться");
    let (mut sink, mut stream) = ws.split();
    println!("Подключено как {name}");

    let reader = tokio::spawn(async move {
        while let Some(Ok(Message::Text(raw))) = stream.next().await {
            match serde_json::from_str::<ServerMsg>(&raw) {
                Ok(ServerMsg::Text { from, text, ts, attachment, .. }) => {
                    let att = attachment.map(|a| format!(" [{}: {}]", a.kind, a.name)).unwrap_or_default();
                    println!("[{ts}] {from}: {text}{att}");
                }
                Ok(ServerMsg::System { text }) => println!("* {text}"),
                Ok(ServerMsg::Error { text }) => println!("! ошибка: {text}"),
                Ok(ServerMsg::Kick { text }) => println!("! сессия закрыта: {text}"),
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
        let msg = serde_json::to_string(&ClientMsg::Text { text: line, attachment_id: None }).unwrap();
        if sink.send(Message::Text(msg)).await.is_err() {
            break;
        }
    }
    reader.abort();
}

async fn post_auth(base: &str, path: &str, name: &str, password: &str) -> Option<AuthResp> {
    let body = serde_json::to_string(&AuthReq { name: name.into(), password: password.into() }).unwrap();
    let req = ureq::post(&format!("{base}/{path}")).set("content-type", "application/json");
    req.send_bytes(body.as_bytes()).ok()?.into_json::<AuthResp>().ok()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
