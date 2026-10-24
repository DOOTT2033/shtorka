//! Ядро сервера Шторки 0.0.2. Используется и консольным бинарником, и лаунчером.
//!
//! Новое в 0.0.2:
//! - аккаунты (регистрация/вход по паролю, argon2, токены сессий);
//! - история сообщений и аккаунты хранятся в SQLite (`data/server.db`);
//! - файлы по-прежнему в `data/files`;
//! - конфиг `config.toml` (порт, лимиты, папка данных);
//! - ошибки уходят персонально отправителю, а не в общий чат;
//! - повторный вход под тем же аккаунтом вытесняет старую сессию.

pub mod db;

use std::{
    collections::{HashMap, VecDeque},
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
use rand::RngCore;
use serde::Deserialize;
use shared::{kind_for_mime, AccountInfo, Attachment, AuthReq, AuthResp, ClientMsg, PasswordReq, ServerMsg, StoredMessage};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};
use tower_http::cors::CorsLayer;

pub const HISTORY_LIMIT: usize = 500;
pub const MAX_TEXT_LEN: usize = 4000;
pub const MAX_NAME_LEN: usize = 32;
/// Максимальный размер одного файла по умолчанию (50 МБ).
pub const MAX_UPLOAD: usize = 50 * 1024 * 1024;

/* ---------- Конфиг ---------- */

/// Настройки сервера. Читаются из `config.toml`, параметры командной строки их перекрывают.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// Адрес прослушивания: 0.0.0.0 — все интерфейсы.
    pub host: String,
    pub port: u16,
    /// Папка данных: база `server.db` и вложения `files/`.
    pub data_dir: String,
    /// Максимальный размер загрузки, МБ.
    pub max_upload_mb: usize,
    /// Сколько последних сообщений отдаёт /history.
    pub history_limit: usize,
    /// Название сервера (приветствие в логах).
    pub server_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 7878,
            data_dir: "data".into(),
            max_upload_mb: 50,
            history_limit: HISTORY_LIMIT,
            server_name: "Шторка".into(),
        }
    }
}

impl Config {
    /// Читает конфиг из `path`. Если файла нет — создаёт файл с настройками по умолчанию.
    pub fn load_or_create(path: &std::path::Path) -> Result<Self, String> {
        if path.exists() {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| format!("Не удалось прочитать {}: {e}", path.display()))?;
            toml::from_str(&raw).map_err(|e| format!("Ошибка в {}: {e}", path.display()))
        } else {
            let cfg = Self::default();
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).ok();
            }
            let text = toml::to_string_pretty(&cfg).unwrap_or_default();
            std::fs::write(path, format!("# Настройки сервера Шторки\n{text}"))
                .map_err(|e| format!("Не удалось записать {}: {e}", path.display()))?;
            Ok(cfg)
        }
    }

    pub fn data_path(&self) -> PathBuf {
        PathBuf::from(&self.data_dir)
    }

    pub fn max_upload(&self) -> usize {
        self.max_upload_mb.saturating_mul(1024 * 1024).max(1024 * 1024)
    }
}

/* ---------- Состояние ---------- */

pub struct AppState {
    store: db::Store,
    tx: broadcast::Sender<ServerMsg>,
    close_tx: broadcast::Sender<()>,
    /// Токен сессии → имя пользователя.
    sessions: Mutex<HashMap<String, String>>,
    /// Кто онлайн: имя → id сессии подключения.
    online: Mutex<HashMap<String, u64>>,
    /// Персональные каналы: id сессии → отправитель.
    personal: Mutex<HashMap<u64, mpsc::UnboundedSender<ServerMsg>>>,
    logs: Mutex<VecDeque<String>>,
    next_id: AtomicU64,
    next_session: AtomicU64,
    data_dir: PathBuf,
    history_limit: usize,
    max_upload: usize,
}

impl AppState {
    fn new(cfg: &Config) -> Result<Self, String> {
        let data_dir = cfg.data_path();
        std::fs::create_dir_all(data_dir.join("files"))
            .map_err(|e| format!("Не удалось создать папку для файлов: {e}"))?;
        let store = db::Store::open(&data_dir.join("server.db"))?;
        let next_id = store.next_message_id();
        let (tx, _) = broadcast::channel(256);
        let (close_tx, _) = broadcast::channel(4);
        Ok(Self {
            store,
            tx,
            close_tx,
            sessions: Mutex::new(HashMap::new()),
            online: Mutex::new(HashMap::new()),
            personal: Mutex::new(HashMap::new()),
            logs: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(next_id),
            next_session: AtomicU64::new(1),
            data_dir,
            history_limit: cfg.history_limit,
            max_upload: cfg.max_upload(),
        })
    }

    fn log(&self, msg: impl Into<String>) {
        let line = format!("[{}] {}", fmt_time(now_ts()), msg.into());
        println!("{line}");
        let mut logs = self.logs.lock().unwrap();
        logs.push_back(line);
        while logs.len() > 300 {
            logs.pop_front();
        }
    }

    fn files_dir(&self) -> PathBuf {
        self.data_dir.join("files")
    }

    fn session_user(&self, token: &str) -> Option<String> {
        self.sessions.lock().unwrap().get(token).cloned()
    }

    fn new_token(&self, name: &str) -> String {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        self.sessions.lock().unwrap().insert(token.clone(), name.to_string());
        token
    }

    fn personal_send(&self, session_id: u64, msg: ServerMsg) {
        if let Some(tx) = self.personal.lock().unwrap().get(&session_id) {
            let _ = tx.send(msg);
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
        let mut v: Vec<String> = self.state.online.lock().unwrap().keys().cloned().collect();
        v.sort();
        v
    }

    pub fn message_count(&self) -> usize {
        self.state.store.message_count() as usize
    }

    pub fn account_count(&self) -> usize {
        self.state.store.user_count()
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

/// Запускает сервер по конфигу `cfg`. Файлы сохраняются в `cfg.data_dir/files`.
pub async fn start(cfg: Config) -> Result<ServerHandle, String> {
    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port)
        .parse()
        .map_err(|_| "Некорректный адрес или порт".to_string())?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Не удалось занять порт {}: {e}", cfg.port))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;

    let name = cfg.server_name.clone();
    let state = Arc::new(AppState::new(&cfg)?);

    let app = router(state.clone());
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let serve_state = state.clone();
    let task = tokio::spawn(async move {
        serve_state.log(format!("Сервер «{name}» запущен на {addr}"));
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
        .route("/register", post(register_handler))
        .route("/login", post(login_handler))
        .route("/password", post(password_handler))
        .route("/account", get(account_handler))
        .layer(DefaultBodyLimit::max(state.max_upload))
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

/* ---------- Аккаунты ---------- */

fn auth_error(e: db::AuthError) -> Response {
    let (code, text) = match e {
        db::AuthError::BadName => (StatusCode::BAD_REQUEST, "Некорректное имя"),
        db::AuthError::BadPassword => (StatusCode::BAD_REQUEST, "Пароль должен быть от 4 до 128 символов"),
        db::AuthError::NameTaken => (StatusCode::CONFLICT, "Имя уже занято"),
        db::AuthError::WrongCredentials => (StatusCode::UNAUTHORIZED, "Неверный пароль"),
        db::AuthError::NoUser => (StatusCode::NOT_FOUND, "Аккаунт не найден"),
    };
    (code, text).into_response()
}

async fn register_handler(State(state): State<Arc<AppState>>, Json(req): Json<AuthReq>) -> Response {
    let name = req.name.trim().to_string();
    match state.store.register(&name, &req.password, now_ts()) {
        Ok(()) => {
            state.log(format!("Зарегистрирован аккаунт {name}"));
            Json(AuthResp { token: state.new_token(&name), name }).into_response()
        }
        Err(e) => auth_error(e),
    }
}

async fn login_handler(State(state): State<Arc<AppState>>, Json(req): Json<AuthReq>) -> Response {
    let name = req.name.trim().to_string();
    match state.store.login(&name, &req.password) {
        Ok(_) => Json(AuthResp { token: state.new_token(&name), name }).into_response(),
        Err(e) => auth_error(e),
    }
}

async fn password_handler(State(state): State<Arc<AppState>>, Json(req): Json<PasswordReq>) -> Response {
    let Some(name) = state.session_user(&req.token) else {
        return (StatusCode::UNAUTHORIZED, "Не авторизован").into_response();
    };
    match state.store.change_password(&name, &req.old, &req.new) {
        Ok(()) => {
            state.log(format!("{name} сменил пароль"));
            "ok".into_response()
        }
        Err(e) => auth_error(e),
    }
}

#[derive(Deserialize)]
struct TokenQuery {
    token: String,
}

async fn account_handler(State(state): State<Arc<AppState>>, Query(q): Query<TokenQuery>) -> Response {
    let Some(name) = state.session_user(&q.token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match state.store.account_info(&name) {
        Some(u) => Json(AccountInfo { name: u.name, created: u.created }).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/* ---------- WebSocket-чат ---------- */

#[derive(Deserialize)]
struct WsParams {
    name: String,
    token: String,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, params.name, params.token, state))
}

async fn handle_socket(socket: WebSocket, name: String, token: String, state: Arc<AppState>) {
    let name = name.trim().to_string();
    let (mut sink, mut stream) = socket.split();

    // Проверяем токен: имя в сокете должно совпадать с именем токена.
    match state.session_user(&token) {
        Some(t) if t == name => {}
        _ => {
            let _ = sink
                .send(send_json(&ServerMsg::Error { text: "Не авторизован. Войдите заново.".into() }))
                .await;
            return;
        }
    }
    if !valid_name(&name) {
        let _ = sink.send(send_json(&ServerMsg::Error { text: "Некорректное имя".into() })).await;
        return;
    }

    let session_id = state.next_session.fetch_add(1, Ordering::SeqCst);
    let (personal_tx, mut personal_rx) = mpsc::unbounded_channel::<ServerMsg>();

    // Если аккаунт уже в сети — вытесняем старую сессию (быстрое переподключение).
    let replaced = {
        let mut online = state.online.lock().unwrap();
        online.insert(name.clone(), session_id)
    };
    if let Some(old_id) = replaced {
        state.personal_send(old_id, ServerMsg::Kick { text: "Выполнен вход с другого устройства".into() });
        state.personal.lock().unwrap().remove(&old_id);
        state.log(format!("{name} переподключился, старая сессия закрыта"));
    } else {
        state.log(format!("{name} подключился"));
        let _ = state.tx.send(ServerMsg::System { text: format!("{name} вошёл в чат") });
    }
    state.personal.lock().unwrap().insert(session_id, personal_tx);

    let _ = sink.send(send_json(&ServerMsg::Welcome { user: name.clone() })).await;

    let mut rx = state.tx.subscribe();
    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Ok(msg) => {
                        if sink.send(send_json(&msg)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
                personal = personal_rx.recv() => match personal {
                    Some(msg) => {
                        if sink.send(send_json(&msg)).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
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
                        handle_text(&recv_state, session_id, &recv_name, text, attachment_id).await;
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

    // Убираем из «онлайн» только если это НАША сессия (нас могли уже вытеснить).
    let was_current = {
        let mut online = state.online.lock().unwrap();
        match online.get(&name) {
            Some(id) if *id == session_id => {
                online.remove(&name);
                true
            }
            _ => false,
        }
    };
    state.personal.lock().unwrap().remove(&session_id);
    if was_current {
        state.log(format!("{name} отключился"));
        let _ = state.tx.send(ServerMsg::System { text: format!("{name} вышел из чата") });
    }
}

async fn handle_text(
    state: &Arc<AppState>,
    session_id: u64,
    from: &str,
    text: String,
    attachment_id: Option<String>,
) {
    let err = |text: &str| {
        state.personal_send(session_id, ServerMsg::Error { text: text.to_string() });
    };
    let text = text.trim().to_string();
    if text.chars().count() > MAX_TEXT_LEN {
        err(&format!("Сообщение длиннее {MAX_TEXT_LEN} символов"));
        return;
    }

    // Вложение берём с диска, а не из клиентских данных — подделать метаданные нельзя.
    let attachment = match attachment_id {
        Some(id) => match load_attachment(state, &id).await {
            Some(a) => Some(a),
            None => {
                err("Файл не найден");
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
    let message = StoredMessage {
        id,
        from: from.to_string(),
        text: text.clone(),
        ts,
        attachment: attachment.clone(),
    };
    // Сначала диск, потом рассылка: после перезапуска сообщение не потеряется.
    if let Err(e) = state.store.insert_message(&message) {
        state.log(e);
        err("Не удалось сохранить сообщение");
        return;
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

/* ---------- Файлы ---------- */

#[derive(Deserialize)]
struct UploadParams {
    token: String,
    name: String,
    #[serde(default)]
    mime: String,
}

async fn upload_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<UploadParams>,
    body: Bytes,
) -> Response {
    if state.session_user(&params.token).is_none() {
        return (StatusCode::UNAUTHORIZED, "Не авторизован").into_response();
    }
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "Пустой файл").into_response();
    }
    if body.len() > state.max_upload {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Файл больше лимита").into_response();
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

/* ---------- История ---------- */

async fn history_handler(State(state): State<Arc<AppState>>, Query(q): Query<TokenQuery>) -> Response {
    if state.session_user(&q.token).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(state.store.recent_messages(state.history_limit)).into_response()
}

async fn health_handler() -> &'static str {
    "ok"
}
