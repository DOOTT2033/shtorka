use futures_util::{SinkExt, StreamExt};
use shared::{AuthReq, AuthResp, ClientMsg, ServerMsg, StoredMessage};
use std::path::PathBuf;
use tokio_tungstenite::{connect_async, tungstenite::Message};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "shtorka-test-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn start_in(dir: PathBuf) -> (server::ServerHandle, String) {
    let mut cfg = server::Config::default();
    cfg.host = "127.0.0.1".into();
    cfg.port = 0;
    cfg.data_dir = dir.to_string_lossy().to_string();
    let h = server::start(cfg).await.unwrap();
    let base = format!("http://{}", h.addr);
    (h, base)
}

async fn start() -> (server::ServerHandle, String) {
    start_in(temp_dir("api")).await
}

fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = T> {
    async move { tokio::task::spawn_blocking(f).await.unwrap() }
}

fn register_blocking(base: &str, name: &str, password: &str) -> (u16, Option<AuthResp>) {
    let body = serde_json::to_string(&AuthReq { name: name.into(), password: password.into() }).unwrap();
    match ureq::post(&format!("{base}/register")).set("content-type", "application/json").send_bytes(body.as_bytes()) {
        Ok(r) => (200, r.into_json::<AuthResp>().ok()),
        Err(ureq::Error::Status(code, _)) => (code, None),
        Err(_) => (0, None),
    }
}

fn login_blocking(base: &str, name: &str, password: &str) -> (u16, Option<AuthResp>) {
    let body = serde_json::to_string(&AuthReq { name: name.into(), password: password.into() }).unwrap();
    match ureq::post(&format!("{base}/login")).set("content-type", "application/json").send_bytes(body.as_bytes()) {
        Ok(r) => (200, r.into_json::<AuthResp>().ok()),
        Err(ureq::Error::Status(code, _)) => (code, None),
        Err(_) => (0, None),
    }
}

#[tokio::test]
async fn health_ok() {
    let (h, base) = start().await;
    let body = blocking(move || ureq::get(&format!("{base}/health")).call().unwrap().into_string().unwrap()).await;
    assert_eq!(body, "ok");
    h.stop().await;
}

#[tokio::test]
async fn register_login_and_account() {
    let (h, base) = start().await;

    let (code, auth) = blocking({ let b = base.clone(); move || register_blocking(&b, "Вася", "1234") }).await;
    assert_eq!(code, 200);
    let auth = auth.expect("токен после регистрации");
    assert_eq!(auth.name, "Вася");
    assert!(!auth.token.is_empty());

    // Повторная регистрация того же имени — конфликт.
    let (code, _) = blocking({ let b = base.clone(); move || register_blocking(&b, "Вася", "9999") }).await;
    assert_eq!(code, 409);

    // Неверный пароль — 401.
    let (code, _) = blocking({ let b = base.clone(); move || login_blocking(&b, "Вася", "wrong") }).await;
    assert_eq!(code, 401);

    // Нет такого аккаунта — 404.
    let (code, _) = blocking({ let b = base.clone(); move || login_blocking(&b, "Петя", "1234") }).await;
    assert_eq!(code, 404);

    // Вход с верным паролем выдаёт новый токен.
    let (code, auth2) = blocking({ let b = base.clone(); move || login_blocking(&b, "Вася", "1234") }).await;
    assert_eq!(code, 200);
    assert!(!auth2.unwrap().token.is_empty());

    // /account отдаёт профиль по токену.
    let b = base.clone();
    let t = auth.token.clone();
    let info = blocking(move || {
        ureq::get(&format!("{b}/account?token={t}")).call().unwrap().into_json::<shared::AccountInfo>().unwrap()
    })
    .await;
    assert_eq!(info.name, "Вася");

    h.stop().await;
}

#[tokio::test]
async fn change_password_flow() {
    let (h, base) = start().await;
    let (_, auth) = blocking({ let b = base.clone(); move || register_blocking(&b, "Лена", "1234") }).await;
    let auth = auth.unwrap();

    // Неверный старый пароль.
    let b = base.clone();
    let t = auth.token.clone();
    let code = blocking(move || {
        let body = serde_json::to_string(&shared::PasswordReq { token: t, old: "0000".into(), new: "5678".into() }).unwrap();
        match ureq::post(&format!("{b}/password")).set("content-type", "application/json").send_bytes(body.as_bytes()) {
            Ok(_) => 200,
            Err(ureq::Error::Status(c, _)) => c,
            Err(_) => 0,
        }
    })
    .await;
    assert_eq!(code, 401);

    // Корректная смена.
    let b = base.clone();
    let t = auth.token.clone();
    let code = blocking(move || {
        let body = serde_json::to_string(&shared::PasswordReq { token: t, old: "1234".into(), new: "5678".into() }).unwrap();
        match ureq::post(&format!("{b}/password")).set("content-type", "application/json").send_bytes(body.as_bytes()) {
            Ok(_) => 200,
            Err(ureq::Error::Status(c, _)) => c,
            Err(_) => 0,
        }
    })
    .await;
    assert_eq!(code, 200);

    // Старый пароль больше не подходит, новый — работает.
    let (code, _) = blocking({ let b = base.clone(); move || login_blocking(&b, "Лена", "1234") }).await;
    assert_eq!(code, 401);
    let (code, _) = blocking({ let b = base.clone(); move || login_blocking(&b, "Лена", "5678") }).await;
    assert_eq!(code, 200);
    h.stop().await;
}

#[tokio::test]
async fn upload_and_download_roundtrip() {
    let (h, base) = start().await;
    let (_, auth) = blocking({ let b = base.clone(); move || register_blocking(&b, "Файл", "1234") }).await;
    let token = auth.unwrap().token;
    let b = base.clone();
    let t = token.clone();
    let meta = blocking(move || {
        let data = b"hello world".to_vec();
        let resp = ureq::post(&format!("{b}/upload?token={t}&name=..%2F..%2Fhi.txt&mime=text%2Fplain"))
            .send_bytes(&data)
            .unwrap();
        resp.into_json::<shared::Attachment>().unwrap()
    })
    .await;
    // Имя очищено от путей, тип файла — file.
    assert_eq!(meta.name, "hi.txt");
    assert_eq!(meta.kind, "file");
    assert_eq!(meta.size, 11);

    let b = base.clone();
    let id = meta.id.clone();
    let (ctype, disp, bytes) = blocking(move || {
        let r = ureq::get(&format!("{b}/files/{id}")).call().unwrap();
        let ctype = r.header("content-type").unwrap().to_string();
        let disp = r.header("content-disposition").unwrap().to_string();
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut r.into_reader(), &mut buf).unwrap();
        (ctype, disp, buf)
    })
    .await;
    assert_eq!(bytes, b"hello world");
    assert_eq!(ctype, "application/octet-stream");
    assert!(disp.starts_with("attachment"));
    h.stop().await;
}

#[tokio::test]
async fn upload_requires_token() {
    let (h, base) = start().await;
    let b = base.clone();
    let status = blocking(move || {
        match ureq::post(&format!("{b}/upload?token=nope&name=a.txt").as_str()).send_bytes(b"x") {
            Ok(_) => 200,
            Err(ureq::Error::Status(code, _)) => code,
            Err(_) => 0,
        }
    })
    .await;
    assert_eq!(status, 401);
    h.stop().await;
}

#[tokio::test]
async fn image_is_served_inline() {
    let (h, base) = start().await;
    let (_, auth) = blocking({ let b = base.clone(); move || register_blocking(&b, "Фото", "1234") }).await;
    let token = auth.unwrap().token;
    let b = base.clone();
    let t = token.clone();
    let meta = blocking(move || {
        ureq::post(&format!("{b}/upload?token={t}&name=p.png&mime=image%2Fpng"))
            .send_bytes(&[0x89, b'P', b'N', b'G'])
            .unwrap()
            .into_json::<shared::Attachment>()
            .unwrap()
    })
    .await;
    assert_eq!(meta.kind, "image");
    let b = base.clone();
    let id = meta.id.clone();
    let ctype = blocking(move || {
        ureq::get(&format!("{b}/files/{id}")).call().unwrap().header("content-type").unwrap().to_string()
    })
    .await;
    assert_eq!(ctype, "image/png");
    h.stop().await;
}

#[tokio::test]
async fn bad_ids_and_unknown_files_are_rejected() {
    let (h, base) = start().await;
    let b = base.clone();
    let status = blocking(move || match ureq::get(&format!("{b}/files/..%2Fsecret")).call() {
        Ok(_) => 200,
        Err(ureq::Error::Status(code, _)) => code,
        Err(_) => 0,
    })
    .await;
    assert_eq!(status, 404);
    let b = base.clone();
    let status = blocking(move || match ureq::get(&format!("{b}/files/deadbeef")).call() {
        Ok(_) => 200,
        Err(ureq::Error::Status(code, _)) => code,
        Err(_) => 0,
    })
    .await;
    assert_eq!(status, 404);
    h.stop().await;
}

#[tokio::test]
async fn empty_upload_rejected() {
    let (h, base) = start().await;
    let (_, auth) = blocking({ let b = base.clone(); move || register_blocking(&b, "Пусто", "1234") }).await;
    let token = auth.unwrap().token;
    let b = base.clone();
    let t = token.clone();
    let status = blocking(move || {
        match ureq::post(&format!("{b}/upload?token={t}&name=a").as_str()).send_bytes(&[]) {
            Ok(_) => 200,
            Err(ureq::Error::Status(code, _)) => code,
            Err(_) => 0,
        }
    })
    .await;
    assert_eq!(status, 400);
    h.stop().await;
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Регистрация + подключение по WebSocket. Возвращает split-сокет.
async fn connect_user(
    h: &server::ServerHandle,
    base: &str,
    name: &str,
) -> (
    futures_util::stream::SplitSink<Ws, Message>,
    futures_util::stream::SplitStream<Ws>,
) {
    let (_, auth) = blocking({ let b = base.to_string(); let n = name.to_string(); move || register_blocking(&b, &n, "1234") }).await;
    let auth = auth.unwrap();
    let url = format!("ws://{}/ws?name={}&token={}", h.addr, urlencode(name), urlencode(&auth.token));
    let (ws, _) = connect_async(&url).await.unwrap();
    ws.split()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn ws_text_and_attachment_flow() {
    let (h, base) = start().await;
    let (mut sink, mut stream) = connect_user(&h, &base, "Вася").await;

    // Загружаем картинку и отправляем её в чат.
    let b = base.clone();
    let (_, auth) = blocking({ let b = b.clone(); move || register_blocking(&b, "Вася2", "1234") }).await;
    // Для загрузки можно любой токен — берём второй аккаунт.
    let token = auth.unwrap().token;
    let att = blocking(move || {
        ureq::post(&format!("{b}/upload?token={token}&name=cat.jpg&mime=image%2Fjpeg"))
            .send_bytes(b"\xff\xd8\xff")
            .unwrap()
            .into_json::<shared::Attachment>()
            .unwrap()
    })
    .await;

    let msg = serde_json::to_string(&ClientMsg::Text {
        text: "смотри".into(),
        attachment_id: Some(att.id.clone()),
    })
    .unwrap();
    sink.send(Message::Text(msg)).await.unwrap();

    // Ждём своё сообщение с вложением.
    let mut got = None;
    while let Some(Ok(Message::Text(raw))) = stream.next().await {
        if let Ok(ServerMsg::Text { text, attachment: Some(a), .. }) = serde_json::from_str(&raw) {
            got = Some((text, a));
            break;
        }
    }
    let (text, a) = got.expect("сообщение с вложением не пришло");
    assert_eq!(text, "смотри");
    assert_eq!(a.kind, "image");
    assert_eq!(a.name, "cat.jpg");

    // Поддельный id вложения: ошибка приходит персонально отправителю.
    let fake = serde_json::to_string(&ClientMsg::Text { text: "x".into(), attachment_id: Some("nope".into()) }).unwrap();
    sink.send(Message::Text(fake)).await.unwrap();
    let mut err = false;
    while let Some(Ok(Message::Text(raw))) = stream.next().await {
        if let Ok(ServerMsg::Error { .. }) = serde_json::from_str(&raw) {
            err = true;
            break;
        }
    }
    assert!(err);

    // История содержит вложение.
    let (_, auth2) = blocking({ let b = base.clone(); move || register_blocking(&b, "История", "1234") }).await;
    let token = auth2.unwrap().token;
    let b = base.clone();
    let hist = blocking(move || {
        ureq::get(&format!("{b}/history?token={token}")).call().unwrap().into_json::<Vec<StoredMessage>>().unwrap()
    })
    .await;
    assert!(hist.iter().any(|m| m.attachment.as_ref().map(|a| a.name == "cat.jpg").unwrap_or(false)));
    h.stop().await;
}

#[tokio::test]
async fn ws_requires_auth() {
    let (h, _base) = start().await;
    let url = format!("ws://{}/ws?name=Вася&token=nope", h.addr);
    let (ws, _) = connect_async(&url).await.unwrap();
    let (_s, mut st) = ws.split();
    let mut rejected = false;
    while let Some(Ok(Message::Text(raw))) = st.next().await {
        if let Ok(ServerMsg::Error { text }) = serde_json::from_str(&raw) {
            rejected = text.contains("авториз");
            break;
        }
    }
    assert!(rejected);
    h.stop().await;
}

#[tokio::test]
async fn same_account_replaces_old_session() {
    let (h, base) = start().await;
    let (_, auth) = blocking({ let b = base.clone(); move || register_blocking(&b, "Двойник", "1234") }).await;
    let token = auth.unwrap().token;
    let url1 = format!("ws://{}/ws?name=Двойник&token={}", h.addr, urlencode(&token));
    let (ws1, _) = connect_async(&url1).await.unwrap();
    let (_s1, mut st1) = ws1.split();

    let url2 = format!("ws://{}/ws?name=Двойник&token={}", h.addr, urlencode(&token));
    let (ws2, _) = connect_async(&url2).await.unwrap();
    let (_s2, mut st2) = ws2.split();

    // Старая сессия получает Kick.
    let mut kicked = false;
    while let Some(Ok(Message::Text(raw))) = st1.next().await {
        if let Ok(ServerMsg::Kick { .. }) = serde_json::from_str(&raw) {
            kicked = true;
            break;
        }
    }
    assert!(kicked);

    // Новая сессия работает: Welcome.
    let mut welcomed = false;
    while let Some(Ok(Message::Text(raw))) = st2.next().await {
        if let Ok(ServerMsg::Welcome { .. }) = serde_json::from_str(&raw) {
            welcomed = true;
            break;
        }
    }
    assert!(welcomed);
    h.stop().await;
}

#[tokio::test]
async fn history_survives_restart() {
    let dir = temp_dir("persist");
    let (h, base) = start_in(dir.clone()).await;
    let (_, auth) = blocking({ let b = base.clone(); move || register_blocking(&b, "Архив", "1234") }).await;
    let token = auth.unwrap().token;

    let url = format!("ws://{}/ws?name=Архив&token={}", h.addr, urlencode(&token));
    let (ws, _) = connect_async(&url).await.unwrap();
    let (mut sink, _st) = ws.split();
    let msg = serde_json::to_string(&ClientMsg::Text { text: "сохрани меня".into(), attachment_id: None }).unwrap();
    sink.send(Message::Text(msg)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    h.stop().await;

    // Перезапуск на той же папке данных.
    let (h2, base2) = start_in(dir).await;
    let (_, auth2) = blocking({ let b = base2.clone(); move || login_blocking(&b, "Архив", "1234") }).await;
    let token2 = auth2.unwrap().token;
    let b = base2.clone();
    let hist = blocking(move || {
        ureq::get(&format!("{b}/history?token={token2}")).call().unwrap().into_json::<Vec<StoredMessage>>().unwrap()
    })
    .await;
    assert!(hist.iter().any(|m| m.text == "сохрани меня"));
    assert_eq!(h2.account_count(), 1);
    h2.stop().await;
}

#[test]
fn sanitizers() {
    assert_eq!(server::sanitize_file_name("../../etc/passwd"), "passwd");
    assert_eq!(server::sanitize_file_name("C:\\x\\a.png"), "a.png");
    assert_eq!(server::sanitize_file_name("   "), "file");
    assert_eq!(server::sanitize_mime("text/html; charset=x"), "application/octet-stream");
    assert_eq!(server::sanitize_mime("video/webm;codecs=vp8,opus"), "application/octet-stream");
    assert_eq!(server::sanitize_mime("video/webm"), "video/webm");
}
