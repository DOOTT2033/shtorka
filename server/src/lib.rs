//! Ядро сервера Шторки. Используется и консольным бинарником, и лаунчером.

use std::{
    collections::{HashSet, VecDeque},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use shared::{kind_for_mime, Attachment, ClientMsg, ServerMsg, StoredMessage};
use tokio::{
    sync::{broadcast, oneshot},
    task::JoinHandle,
};
use tower_http::cors::CorsLayer;

pub const HISTORY_LIMIT: usize = 500;
const LOG_LIMIT: usize = 300;
pub const MAX_TEXT_LEN: usize = 4000;
pub const MAX_NAME_LEN: usize = 32;
/// Максимальный размер одного файла (включая видео-кружки и голосовые).
pub const MAX_UPLOAD: usize = 50 * 1024 * 1024;

pub struct AppState {
    tx: broadcast::Sender<ServerMsg>,
    close_tx: broadcast::Sender<()>,
    history: Mutex<Vec<StoredMessage>>,
    users: Mutex<HashSet<String>>,
    logs: Mutex<VecDeque<String>>,
    next_id: AtomicU64,
    data_dir: PathBuf,
}

impl AppState {
    fn new(data_dir: PathBuf) -> Self {
        let (tx, _) = broadcast::channel(256);
        let (close_tx, _) = broadcast::channel(4);
        Self {
            tx,
            close_tx,
            history: Mutex::new(Vec::new()),
            users: Mutex::new(HashSet::new()),
            logs: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
            data_dir,
        }
    }

    fn log(&self, msg: impl Into<String>) {
        let line = format!("[{}] {}", fmt_time(now_ts()), msg.into());
        println!("{line}");
        let mut logs = self.logs.lock().unwrap();
        logs.push_back(line);
        while logs.len() > LOG_LIMIT {
            logs.pop_front();
        }
    }

    fn files_dir(&self) -> PathBuf {
        self.data_dir.join("files")
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

/// Запускает сервер на host:port. Файлы сохраняются в `data_dir/files`.
pub async fn start(host: &str, port: u16, data_dir: PathBuf) -> Result<ServerHandle, String> {
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|_| "Некорректный адрес или порт".to_string())?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Не удалось занять порт {port}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;

    let state = Arc::new(AppState::new(data_dir));
    std::fs::create_dir_all(state.files_dir())
        .map_err(|e| format!("Не удалось создать папку для файлов: {e}"))?;

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
        .route("/upload", post(upload_handler))
        .route("/files/:id", get(file_handler))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

pub fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Время HH:MM:SS по UTC+5 (Челябинск).
fn fmt_time(ts: u64) -> String {
    let local = ts + 5 * 3600;
    format!("{:02}:{:02}:{:02}", (local / 3600) % 24, (local / 60) % 60, local % 60)
}

pub fn valid_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && n.chars().count() <= MAX_NAME_LEN && !n.chars().any(|c| c.is_control())
}

/// Безопасное имя файла: без путей и управляющих символов, до 200 символов.
pub fn sanitize_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() { "file".into() } else { cleaned }
}

/// MIME-тип: только безопасные символы, иначе application/octet-stream.
pub fn sanitize_mime(raw: &str) -> String {
    let m = raw.trim().to_ascii_lowercase();
    let ok = !m.is_empty()
        && m.len() <= 100
        && m.contains('/')
        && m.chars().all(|c| c.is_ascii_alphanumeric() || "/.+-".contains(c));
    if ok { m } else { "application/octet-stream".into() }
}

fn is_valid_file_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn send_json(msg: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(msg).unwrap())
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

    let mut rx = state.tx.subscribe();
    let mut send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if sink.send(send_json(&msg)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    let recv_state = state.clone();
    let recv_name = name.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(raw) => match serde_json::from_str::<ClientMsg>(&raw) {
                    Ok(ClientMsg::Text { text, attachment_id }) => {
                        handle_text(&recv_state, &recv_name, text, attachment_id).await;
                    }
                    Err(e) => recv_state.log(format!("Некорректный JSON от {recv_name}: {e}")),
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

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

async fn handle_text(
    state: &Arc<AppState>,
    from: &str,
    text: String,
    attachment_id: Option<String>,
) {
    let text = text.trim().to_string();
    if text.chars().count() > MAX_TEXT_LEN {
        let _ = state.tx.send(ServerMsg::Error {
            text: format!("Сообщение длиннее {MAX_TEXT_LEN} символов"),
        });
        return;
    }

    // Вложение берём с диска, а не из клиентских данных — подделать метаданные нельзя.
    let attachment = match attachment_id {
        Some(id) => match load_attachment(state, &id).await {
            Some(a) => Some(a),
            None => {
                let _ = state.tx.send(ServerMsg::Error { text: "Файл не найден".into() });
                return;
            }
        },
        None => None,
    };
    if text.is_empty() && attachment.is_none() {
        return;
    }

    let id = state.next_id.fetch_add(1, Ordering::SeqCst);
    let ts = now_ts();
    {
        let mut h = state.history.lock().unwrap();
        h.push(StoredMessage {
            id,
            from: from.to_string(),
            text: text.clone(),
            ts,
            attachment: attachment.clone(),
        });
        if h.len() > HISTORY_LIMIT {
            let excess = h.len() - HISTORY_LIMIT;
            h.drain(0..excess);
        }
    }
    let label = match &attachment {
        Some(a) => format!("[{}] {}", a.kind, a.name),
        None => String::new(),
    };
    state.log(format!("<{from}> {text} {label}").trim_end().to_string());
    let _ = state.tx.send(ServerMsg::Text {
        id,
        from: from.to_string(),
        text,
        ts,
        attachment,
    });
}

async fn load_attachment(state: &Arc<AppState>, id: &str) -> Option<Attachment> {
    if !is_valid_file_id(id) {
        return None;
    }
    let meta_path = state.files_dir().join(format!("{id}.json"));
    let raw = tokio::fs::read(&meta_path).await.ok()?;
    serde_json::from_slice::<Attachment>(&raw).ok()
}

#[derive(Deserialize)]
struct UploadParams {
    name: String,
    #[serde(default)]
    mime: String,
}

async fn upload_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<UploadParams>,
    body: Bytes,
) -> Response {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "Пустой файл").into_response();
    }
    if body.len() > MAX_UPLOAD {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Файл больше 50 МБ").into_response();
    }
    let name = sanitize_file_name(&params.name);
    let mime = sanitize_mime(&params.mime);
    let kind = kind_for_mime(&mime).to_string();
    let id = format!(
        "{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        state.next_id.fetch_add(1, Ordering::SeqCst)
    );
    let attachment = Attachment { id: id.clone(), name, mime, size: body.len() as u64, kind };

    let dir = state.files_dir();
    let meta = serde_json::to_vec(&attachment).unwrap();
    let write = async {
        tokio::fs::write(dir.join(format!("{id}.bin")), &body).await?;
        tokio::fs::write(dir.join(format!("{id}.json")), &meta).await
    };
    if let Err(e) = write.await {
        state.log(format!("Ошибка сохранения файла: {e}"));
        return (StatusCode::INTERNAL_SERVER_ERROR, "Не удалось сохранить файл").into_response();
    }
    state.log(format!("Загружен файл {} ({} байт)", attachment.name, attachment.size));
    Json(attachment).into_response()
}

async fn file_handler(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(att) = load_attachment(&state, &id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let data = match tokio::fs::read(state.files_dir().join(format!("{id}.bin"))).await {
        Ok(d) => d,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    // Исполняемый или HTML-контент не отдаём как документ: только медиа показываем inline.
    let inline = matches!(att.kind.as_str(), "image" | "audio" | "video");
    let content_type = if inline { att.mime.clone() } else { "application/octet-stream".into() };
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(&content_type).unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    let disp = format!(
        "{}; filename=\"{}\"",
        if inline { "inline" } else { "attachment" },
        att.name.replace('"', "")
    );
    if let Ok(v) = HeaderValue::from_str(&disp) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    (headers, data).into_response()
}

async fn history_handler(State(state): State<Arc<AppState>>) -> Json<Vec<StoredMessage>> {
    Json(state.history.lock().unwrap().clone())
}

async fn health_handler() -> &'static str {
    "ok"
}
