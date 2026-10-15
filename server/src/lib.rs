//! Ядро сервера мессенджера. Используется и консольным бинарником, и лаунчером.

use std::{
    collections::{HashSet, VecDeque},
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use shared::{ClientMsg, ServerMsg, StoredMessage};
use tokio::{sync::{broadcast, oneshot}, task::JoinHandle};
use tower_http::cors::CorsLayer;

const HISTORY_LIMIT: usize = 500;
const LOG_LIMIT: usize = 300;
const MAX_TEXT_LEN: usize = 4000;
const MAX_NAME_LEN: usize = 32;

pub struct AppState {
    tx: broadcast::Sender<ServerMsg>,
    /// Сигнал закрыть все соединения при остановке сервера.
    close_tx: broadcast::Sender<()>,
    history: Mutex<Vec<StoredMessage>>,
    users: Mutex<HashSet<String>>,
    logs: Mutex<VecDeque<String>>,
    next_id: AtomicU64,
}

impl AppState {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        let (close_tx, _) = broadcast::channel(4);
        Self {
            tx,
            close_tx,
            history: Mutex::new(Vec::new()),
            users: Mutex::new(HashSet::new()),
            logs: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
        }
    }

    fn log(&self, msg: impl Into<String>) {
        let ts = now_ts();
        let line = format!("[{}] {}", fmt_time(ts), msg.into());
        println!("{line}");
        let mut logs = self.logs.lock().unwrap();
        logs.push_back(line);
        while logs.len() > LOG_LIMIT {
            logs.pop_front();
        }
    }
}

/// Работающий сервер. Остановка — через `stop()`.
pub struct ServerHandle {
    pub addr: SocketAddr,
    state: Arc<AppState>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl ServerHandle {
    pub fn users(&self) -> Vec<String> {
        let mut v: Vec<String> = self.state.users.lock().unwrap().iter().cloned().collect();
        v.sort();
        v
    }

    pub fn message_count(&self) -> usize {
        self.state.history.lock().unwrap().len()
    }

    pub fn logs(&self) -> Vec<String> {
        self.state.logs.lock().unwrap().iter().cloned().collect()
    }

    pub async fn stop(mut self) {
        self.state.log("Остановка сервера");
        let _ = self.state.close_tx.send(());
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

/// Запускает сервер на host:port (0.0.0.0 — все интерфейсы).
pub async fn start(host: &str, port: u16) -> Result<ServerHandle, String> {
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|_| "Некорректный адрес или порт".to_string())?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Не удалось занять порт {port}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;

    let state = Arc::new(AppState::new());
    let app = router(state.clone());
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let serve_state = state.clone();
    let task = tokio::spawn(async move {
        serve_state.log(format!("Сервер запущен на {addr}"));
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(ServerHandle { addr, state, shutdown: Some(shutdown_tx), task })
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/history", get(history_handler))
        .route("/health", get(health_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Время HH:MM:SS по UTC+5 (Екатеринбург/Челябинск). Простая запасная реализация без зависимостей.
fn fmt_time(ts: u64) -> String {
    let local = ts + 5 * 3600;
    format!("{:02}:{:02}:{:02}", (local / 3600) % 24, (local / 60) % 60, local % 60)
}

fn valid_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && n.chars().count() <= MAX_NAME_LEN && !n.chars().any(|c| c.is_control())
}

#[derive(Deserialize)]
struct WsParams {
    name: String,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, params.name, state))
}

fn send_json(msg: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(msg).unwrap())
}

async fn handle_socket(socket: WebSocket, name: String, state: Arc<AppState>) {
    let name = name.trim().to_string();
    let (mut sink, mut stream) = socket.split();

    if !valid_name(&name) {
        let _ = sink.send(send_json(&ServerMsg::Error { text: "Некорректное имя".into() })).await;
        return;
    }
    if !state.users.lock().unwrap().insert(name.clone()) {
        let _ = sink.send(send_json(&ServerMsg::Error { text: "Имя уже занято".into() })).await;
        return;
    }

    state.log(format!("{name} подключился"));
    let _ = state.tx.send(ServerMsg::System { text: format!("{name} вошёл в чат") });
    let _ = sink.send(send_json(&ServerMsg::Welcome { user: name.clone() })).await;

    // Рассылка всех сообщений этому клиенту.
    let mut rx = state.tx.subscribe();
    let mut send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if sink.send(send_json(&msg)).await.is_err() {
                break;
            }
        }
    });

    // Чтение сообщений от клиента.
    let recv_state = state.clone();
    let recv_name = name.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(raw) => match serde_json::from_str::<ClientMsg>(&raw) {
                    Ok(ClientMsg::Text { text }) => {
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        if text.chars().count() > MAX_TEXT_LEN {
                            let _ = recv_state.tx.send(ServerMsg::Error {
                                text: format!("Сообщение длиннее {MAX_TEXT_LEN} символов"),
                            });
                            continue;
                        }
                        let id = recv_state.next_id.fetch_add(1, Ordering::SeqCst);
                        let ts = now_ts();
                        {
                            let mut h = recv_state.history.lock().unwrap();
                            h.push(StoredMessage { id, from: recv_name.clone(), text: text.clone(), ts });
                            if h.len() > HISTORY_LIMIT {
                                let excess = h.len() - HISTORY_LIMIT;
                                h.drain(0..excess);
                            }
                        }
                        recv_state.log(format!("<{recv_name}> {text}"));
                        let _ = recv_state.tx.send(ServerMsg::Text { id, from: recv_name.clone(), text, ts });
                    }
                    Err(e) => recv_state.log(format!("Некорректный JSON от {recv_name}: {e}")),
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Ждём, пока закроется клиент, пропадёт канал рассылки или сервер остановится.
    let mut close_rx = state.close_tx.subscribe();
    tokio::select! {
        _ = &mut send_task => {}
        _ = &mut recv_task => {}
        _ = close_rx.recv() => {}
    }
    send_task.abort();
    recv_task.abort();

    state.users.lock().unwrap().remove(&name);
    state.log(format!("{name} отключился"));
    let _ = state.tx.send(ServerMsg::System { text: format!("{name} вышел из чата") });
}

async fn history_handler(State(state): State<Arc<AppState>>) -> Json<Vec<StoredMessage>> {
    Json(state.history.lock().unwrap().clone())
}

async fn health_handler() -> &'static str {
    "ok"
}
