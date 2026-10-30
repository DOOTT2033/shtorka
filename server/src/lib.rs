//! Ядро сервера Шторки 0.1.0. Используется и консольным бинарником, и лаунчером.
//!
//! 0.1.0:
//! - юзернейм (латиница, уникальный) и отдельное отображаемое имя;
//! - аватарки;
//! - личные сообщения и поиск пользователей по юзернейму;
//! - настройки приватности (кто может писать в личку, показывать ли онлайн);
//! - файлы и аватарки отдаются только авторизованным клиентам.

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
use shared::{
    kind_for_mime, normalize_username, AccountInfo, Attachment, AuthReq, AuthResp, ClientMsg,
    PasswordReq, ProfileReq, ServerMsg, StoredMessage, UserCard, MAX_TEXT_LEN,
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};
use tower_http::cors::CorsLayer;

pub const HISTORY_LIMIT: usize = 500;
/// Максимальный размер одного файла по умолчанию (50 МБ).
pub const MAX_UPLOAD: usize = 50 * 1024 * 1024;
/// Максимальный размер аватарки (5 МБ).
pub const MAX_AVATAR: usize = 5 * 1024 * 1024;

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
    /// Токен сессии → юзернейм.
    sessions: Mutex<HashMap<String, String>>,
    /// Кто онлайн: юзернейм → id сессии подключения.
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

    fn is_online(&self, name: &str) -> bool {
        self.online.lock().unwrap().contains_key(name)
    }

    fn card(&self, user: &db::User, viewer: &str) -> UserCard {
        let online = self.is_online(&user.name) && (user.show_online || user.name == viewer);
        UserCard {
            username: user.name.clone(),
            display_name: if user.display_name.is_empty() {
                user.name.clone()
            } else {
                user.display_name.clone()
            },
            avatar: user.avatar.clone(),
            online,
        }
    }

    fn account_info(&self, user: &db::User) -> AccountInfo {
        AccountInfo {
            username: user.name.clone(),
            display_name: user.display_name.clone(),
            avatar: user.avatar.clone(),
            created: user.created,
            dm_privacy: user.dm_privacy.clone(),
            show_online: user.show_online,
        }
    }

    fn auth_resp(&self, user: &db::User, token: String) -> AuthResp {
        AuthResp {
            token,
            username: user.name.clone(),
            display_name: user.display_name.clone(),
            avatar: user.avatar.clone(),
            created: user.created,
            dm_privacy: user.dm_privacy.clone(),
            show_online: user.show_online,
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
        .route("/avatar", post(avatar_handler).delete(avatar_delete_handler))
        .route("/register", post(register_handler))
        .route("/login", post(login_handler))
        .route("/logout", post(logout_handler))
        .route("/password", post(password_handler))
        .route("/profile", post(profile_handler))
        .route("/account", get(account_handler))
        .route("/users", get(users_handler))
        .layer(DefaultBodyLimit::max(state.max_upload.max(MAX_AVATAR)))
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

fn new_file_id(state: &AppState) -> String {
    format!(
        "{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        state.next_id.fetch_add(1, Ordering::SeqCst)
    )
}

/* ---------- Аккаунты ---------- */

fn auth_error(e: db::AuthError) -> Response {
    let (code, text) = match e {
        db::AuthError::BadName => (
            StatusCode::BAD_REQUEST,
            "Юзернейм: 3–20 символов, только латиница (a–z), цифры и _, без _ по краям",
        ),
        db::AuthError::BadPassword => (
            StatusCode::BAD_REQUEST,
            "Пароль: 6–128 символов, латиница, цифры и знаки без пробелов",
        ),
        db::AuthError::BadPrivacy => (StatusCode::BAD_REQUEST, "Некорректная настройка приватности"),
        db::AuthError::NameTaken => (StatusCode::CONFLICT, "Этот юзернейм уже занят"),
        db::AuthError::WrongCredentials => (StatusCode::UNAUTHORIZED, "Неверный пароль"),
        db::AuthError::NoUser => (StatusCode::NOT_FOUND, "Аккаунт не найден"),
    };
    (code, text).into_response()
}

async fn register_handler(State(state): State<Arc<AppState>>, Json(req): Json<AuthReq>) -> Response {
    let username = normalize_username(&req.username);
    match state.store.register(&username, &req.display_name, &req.password, now_ts()) {
        Ok(user) => {
            state.log(format!("Зарегистрирован аккаунт @{}", user.name));
            let token = state.new_token(&user.name);
            Json(state.auth_resp(&user, token)).into_response()
        }
        Err(e) => auth_error(e),
    }
}

async fn login_handler(State(state): State<Arc<AppState>>, Json(req): Json<AuthReq>) -> Response {
    let username = normalize_username(&req.username);
    match state.store.login(&username, &req.password) {
        Ok(user) => {
            let token = state.new_token(&user.name);
            Json(state.auth_resp(&user, token)).into_response()
        }
        Err(e) => auth_error(e),
    }
}

#[derive(Deserialize)]
struct LogoutReq {
    token: String,
}

async fn logout_handler(State(state): State<Arc<AppState>>, Json(req): Json<LogoutReq>) -> Response {
    let removed = state.sessions.lock().unwrap().remove(&req.token);
    if let Some(name) = removed {
        state.log(format!("@{name} вышел из аккаунта"));
    }
    "ok".into_response()
}

async fn password_handler(State(state): State<Arc<AppState>>, Json(req): Json<PasswordReq>) -> Response {
    let Some(name) = state.session_user(&req.token) else {
        return (StatusCode::UNAUTHORIZED, "Не авторизован").into_response();
    };
    match state.store.change_password(&name, &req.old, &req.new) {
        Ok(()) => {
            state.log(format!("@{name} сменил пароль"));
            "ok".into_response()
        }
        Err(e) => auth_error(e),
    }
}

async fn profile_handler(State(state): State<Arc<AppState>>, Json(req): Json<ProfileReq>) -> Response {
    let Some(name) = state.session_user(&req.token) else {
        return (StatusCode::UNAUTHORIZED, "Не авторизован").into_response();
    };
    match state.store.update_profile(
        &name,
        req.display_name.as_deref(),
        req.dm_privacy.as_deref(),
        req.show_online,
    ) {
        Ok(user) => {
            state.log(format!("@{name} обновил профиль"));
            Json(state.account_info(&user)).into_response()
        }
        Err(e) => auth_error(e),
    }
}

#[derive(Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: String,
}

async fn account_handler(State(state): State<Arc<AppState>>, Query(q): Query<TokenQuery>) -> Response {
    let Some(name) = state.session_user(&q.token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match state.store.account_info(&name) {
        Some(u) => Json(state.account_info(&u)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct UsersQuery {
    #[serde(default)]
    token: String,
    #[serde(default)]
    q: String,
}

async fn users_handler(State(state): State<Arc<AppState>>, Query(q): Query<UsersQuery>) -> Response {
    let Some(me) = state.session_user(&q.token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut users: Vec<UserCard> = state
        .store
        .search_users(&q.q, &me, 50)
        .iter()
        .map(|u| state.card(u, &me))
        .collect();
    // Сначала те, кто в сети, затем по алфавиту.
    users.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
    });
    Json(users).into_response()
}

/* ---------- WebSocket-чат ---------- */

#[derive(Deserialize)]
struct WsParams {
    username: String,
    #[serde(default)]
    token: String,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, params.username, params.token, state))
}

async fn handle_socket(socket: WebSocket, username: String, token: String, state: Arc<AppState>) {
    let name = normalize_username(&username);
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
    // Аккаунты из старых версий могли быть с кириллицей — им вход не блокируем,
    // достаточно существования аккаунта и верного токена.
    let Some(profile) = state.store.user(&name) else {
        let _ = sink
            .send(send_json(&ServerMsg::Error { text: "Аккаунт не найден".into() }))
            .await;
        return;
    };

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
        state.log(format!("@{name} переподключился, старая сессия закрыта"));
    } else {
        state.log(format!("@{name} подключился"));
        let _ = state.tx.send(ServerMsg::System {
            text: format!("{} вошёл в чат", profile.display_name),
        });
    }
    state.personal.lock().unwrap().insert(session_id, personal_tx);

    let _ = sink
        .send(send_json(&ServerMsg::Welcome {
            username: name.clone(),
            display_name: profile.display_name.clone(),
        }))
        .await;

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
                    Ok(ClientMsg::Text { text, attachment_id, to }) => {
                        handle_text(&recv_state, session_id, &recv_name, text, attachment_id, to).await;
                    }
                    Err(e) => recv_state.log(format!("Некорректный JSON от @{recv_name}: {e}")),
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
        state.log(format!("@{name} отключился"));
        let _ = state.tx.send(ServerMsg::System {
            text: format!("{} вышел из чата", profile.display_name),
        });
    }
}

async fn handle_text(
    state: &Arc<AppState>,
    session_id: u64,
    from: &str,
    text: String,
    attachment_id: Option<String>,
    to: Option<String>,
) {
    let err = |text: &str| {
        state.personal_send(session_id, ServerMsg::Error { text: text.to_string() });
    };
    let text = text.trim().to_string();
    if text.chars().count() > MAX_TEXT_LEN {
        err(&format!("Сообщение длиннее {MAX_TEXT_LEN} символов"));
        return;
    }

    let to = to.map(|t| normalize_username(&t)).filter(|t| !t.is_empty());
    if let Some(peer) = &to {
        if peer == from {
            err("Нельзя отправить сообщение самому себе");
            return;
        }
        match state.store.user(peer) {
            None => {
                err("Пользователь не найден");
                return;
            }
            Some(u) if u.dm_privacy == "nobody" => {
                err("Пользователь запретил личные сообщения");
                return;
            }
            Some(_) => {}
        }
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

    let Some(sender) = state.store.user(from) else {
        err("Аккаунт не найден");
        return;
    };
    let id = state.next_id.fetch_add(1, Ordering::SeqCst);
    let ts = now_ts();
    let message = StoredMessage {
        id,
        from: from.to_string(),
        from_name: sender.display_name.clone(),
        from_avatar: sender.avatar.clone(),
        to: to.clone(),
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
    match &to {
        Some(peer) => state.log(format!("<@{from}> → @{peer}: {text} {label}").trim_end().to_string()),
        None => state.log(format!("<@{from}> {text} {label}").trim_end().to_string()),
    }

    let out = ServerMsg::Text {
        id,
        from: from.to_string(),
        from_name: sender.display_name,
        from_avatar: sender.avatar,
        to: to.clone(),
        text,
        ts,
        attachment,
    };
    match to {
        Some(peer) => {
            // Личное сообщение видит только отправитель и получатель.
            state.personal_send(session_id, out.clone());
            let peer_session = state.online.lock().unwrap().get(&peer).copied();
            if let Some(peer_id) = peer_session {
                state.personal_send(peer_id, out);
            }
        }
        None => {
            let _ = state.tx.send(out);
        }
    }
}

async fn load_attachment(state: &Arc<AppState>, id: &str) -> Option<Attachment> {
    if !is_valid_file_id(id) {
        return None;
    }
    let meta_path = state.files_dir().join(format!("{id}.json"));
    let raw = tokio::fs::read(&meta_path).await.ok()?;
    serde_json::from_slice::<Attachment>(&raw).ok()
}

/* ---------- Файлы и аватарки ---------- */

#[derive(Deserialize)]
struct UploadParams {
    #[serde(default)]
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
    let id = new_file_id(&state);
    let attachment = Attachment { id: id.clone(), name, mime, size: body.len() as u64, kind };

    if let Err(e) = save_attachment(&state, &attachment, &body).await {
        state.log(format!("Ошибка сохранения файла: {e}"));
        return (StatusCode::INTERNAL_SERVER_ERROR, "Не удалось сохранить файл").into_response();
    }
    state.log(format!("Загружен файл {} ({} байт)", attachment.name, attachment.size));
    Json(attachment).into_response()
}

async fn avatar_handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(me) = state.session_user(&q.token) else {
        return (StatusCode::UNAUTHORIZED, "Не авторизован").into_response();
    };
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "Пустой файл").into_response();
    }
    if body.len() > MAX_AVATAR {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Аватарка больше 5 МБ").into_response();
    }
    let raw_mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mime = sanitize_mime(raw_mime);
    if !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif") {
        return (StatusCode::BAD_REQUEST, "Поддерживаются PNG, JPEG, WebP и GIF").into_response();
    }
    let id = new_file_id(&state);
    let attachment = Attachment {
        id: id.clone(),
        name: "avatar".into(),
        mime,
        size: body.len() as u64,
        kind: "image".into(),
    };
    if let Err(e) = save_attachment(&state, &attachment, &body).await {
        state.log(format!("Ошибка сохранения аватарки: {e}"));
        return (StatusCode::INTERNAL_SERVER_ERROR, "Не удалось сохранить аватарку").into_response();
    }
    if let Err(e) = state.store.set_avatar(&me, Some(&id)) {
        return auth_error(e);
    }
    state.log(format!("@{me} обновил аватарку"));
    match state.store.account_info(&me) {
        Some(u) => Json(state.account_info(&u)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn avatar_delete_handler(State(state): State<Arc<AppState>>, Query(q): Query<TokenQuery>) -> Response {
    let Some(me) = state.session_user(&q.token) else {
        return (StatusCode::UNAUTHORIZED, "Не авторизован").into_response();
    };
    if let Err(e) = state.store.set_avatar(&me, None) {
        return auth_error(e);
    }
    state.log(format!("@{me} удалил аватарку"));
    match state.store.account_info(&me) {
        Some(u) => Json(state.account_info(&u)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn save_attachment(state: &AppState, attachment: &Attachment, body: &[u8]) -> std::io::Result<()> {
    let dir = state.files_dir();
    let meta = serde_json::to_vec(attachment).unwrap();
    tokio::fs::write(dir.join(format!("{}.bin", attachment.id)), body).await?;
    tokio::fs::write(dir.join(format!("{}.json", attachment.id)), meta).await
}

async fn file_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<TokenQuery>,
) -> Response {
    if state.session_user(&q.token).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
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

#[derive(Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    token: String,
    #[serde(default)]
    peer: Option<String>,
}

async fn history_handler(State(state): State<Arc<AppState>>, Query(q): Query<HistoryQuery>) -> Response {
    let Some(me) = state.session_user(&q.token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let peer = q.peer.map(|p| normalize_username(&p)).filter(|p| !p.is_empty());
    let messages = match peer {
        Some(peer) => {
            if peer == me {
                return (StatusCode::BAD_REQUEST, "Нельзя открыть переписку с собой").into_response();
            }
            if state.store.user(&peer).is_none() {
                return StatusCode::NOT_FOUND.into_response();
            }
            state.store.recent_dm(&me, &peer, state.history_limit)
        }
        None => state.store.recent_general(state.history_limit),
    };
    Json(messages).into_response()
}

async fn health_handler() -> &'static str {
    "ok"
}
