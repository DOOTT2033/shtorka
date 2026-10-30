use futures_util::{stream::SplitSink, stream::SplitStream, SinkExt, StreamExt};
use shared::{
    AccountInfo, Attachment, AuthReq, AuthResp, ClientMsg, ProfileReq, ServerMsg, StoredMessage, UserCard,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type Sink = SplitSink<Ws, Message>;
type Stream = SplitStream<Ws>;

fn temp_dir(tag: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!(
        "shtorka-test-{tag}-{}-{}-{seq}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn start_in(dir: PathBuf) -> (server::ServerHandle, String) {
    let cfg = server::Config {
        host: "127.0.0.1".into(),
        port: 0,
        data_dir: dir.to_string_lossy().to_string(),
        ..Default::default()
    };
    let h = server::start(cfg).await.unwrap();
    let base = format!("http://{}", h.addr);
    (h, base)
}

async fn start() -> (server::ServerHandle, String) {
    start_in(temp_dir("api")).await
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

fn auth_body(username: &str, password: &str, display_name: &str) -> String {
    serde_json::to_string(&AuthReq {
        username: username.into(),
        password: password.into(),
        display_name: display_name.into(),
    })
    .unwrap()
}

fn register_blocking(base: &str, username: &str, display: &str, password: &str) -> (u16, Option<AuthResp>) {
    match ureq::post(&format!("{base}/register"))
        .set("content-type", "application/json")
        .send_bytes(auth_body(username, password, display).as_bytes())
    {
        Ok(r) => (200, r.into_json::<AuthResp>().ok()),
        Err(ureq::Error::Status(code, _)) => (code, None),
        Err(_) => (0, None),
    }
}

fn login_blocking(base: &str, username: &str, password: &str) -> (u16, Option<AuthResp>) {
    match ureq::post(&format!("{base}/login"))
        .set("content-type", "application/json")
        .send_bytes(auth_body(username, password, "").as_bytes())
    {
        Ok(r) => (200, r.into_json::<AuthResp>().ok()),
        Err(ureq::Error::Status(code, _)) => (code, None),
        Err(_) => (0, None),
    }
}

fn get_status(url: &str) -> u16 {
    match ureq::get(url).call() {
        Ok(_) => 200,
        Err(ureq::Error::Status(code, _)) => code,
        Err(_) => 0,
    }
}

fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> (u16, Option<T>) {
    match ureq::get(url).call() {
        Ok(r) => (200, r.into_json::<T>().ok()),
        Err(ureq::Error::Status(code, _)) => (code, None),
        Err(_) => (0, None),
    }
}

fn post_profile(base: &str, req: &ProfileReq) -> (u16, Option<AccountInfo>) {
    let body = serde_json::to_string(req).unwrap();
    match ureq::post(&format!("{base}/profile"))
        .set("content-type", "application/json")
        .send_bytes(body.as_bytes())
    {
        Ok(r) => (200, r.into_json::<AccountInfo>().ok()),
        Err(ureq::Error::Status(code, _)) => (code, None),
        Err(_) => (0, None),
    }
}

async fn connect(base: &str, username: &str, display: &str) -> (Sink, Stream, AuthResp) {
    let (code, auth) = blocking({
        let b = base.to_string();
        let u = username.to_string();
        let d = display.to_string();
        move || register_blocking(&b, &u, &d, "123456")
    })
    .await;
    assert_eq!(code, 200, "регистрация {username}");
    let auth = auth.unwrap();
    let host = base.trim_start_matches("http://");
    let url = format!(
        "ws://{host}/ws?username={}&token={}",
        urlencode(&auth.username),
        urlencode(&auth.token)
    );
    let (ws, _) = connect_async(&url).await.unwrap();
    let (sink, stream) = ws.split();
    (sink, stream, auth)
}

async fn recv_until(stream: &mut Stream, pred: impl Fn(&ServerMsg) -> bool) -> ServerMsg {
    loop {
        let next = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .expect("таймаут ожидания сообщения");
        let Some(Ok(Message::Text(raw))) = next else {
            panic!("соединение закрыто или пришёл не текст");
        };
        if let Ok(msg) = serde_json::from_str::<ServerMsg>(&raw) {
            if pred(&msg) {
                return msg;
            }
        }
    }
}

async fn send_text(sink: &mut Sink, text: &str, to: Option<&str>) {
    let msg = serde_json::to_string(&ClientMsg::Text {
        text: text.into(),
        attachment_id: None,
        to: to.map(|s| s.to_string()),
    })
    .unwrap();
    sink.send(Message::Text(msg)).await.unwrap();
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
async fn health_ok() {
    let (h, base) = start().await;
    let body = blocking(move || ureq::get(&format!("{base}/health")).call().unwrap().into_string().unwrap()).await;
    assert_eq!(body, "ok");
    h.stop().await;
}

#[tokio::test]
async fn register_login_and_account() {
    let (h, base) = start().await;

    let (code, auth) = blocking({
        let b = base.clone();
        move || register_blocking(&b, "vasya", "Вася", "123456")
    })
    .await;
    assert_eq!(code, 200);
    let auth = auth.expect("токен после регистрации");
    assert_eq!(auth.username, "vasya");
    assert_eq!(auth.display_name, "Вася");
    assert_eq!(auth.dm_privacy, "everyone");
    assert!(auth.show_online);
    assert!(!auth.token.is_empty());

    // Регистр юзернейма нормализуется, повторная регистрация — конфликт.
    let (code, _) = blocking({
        let b = base.clone();
        move || register_blocking(&b, "VaSyA", "Другой", "654321")
    })
    .await;
    assert_eq!(code, 409);

    // Кириллица в юзернейме запрещена.
    let (code, _) = blocking({
        let b = base.clone();
        move || register_blocking(&b, "вася", "Вася", "123456")
    })
    .await;
    assert_eq!(code, 400);

    // Короткий пароль запрещён.
    let (code, _) = blocking({
        let b = base.clone();
        move || register_blocking(&b, "petya", "Петя", "12345")
    })
    .await;
    assert_eq!(code, 400);

    // Неверный пароль — 401, неизвестный — 404.
    let (code, _) = blocking({
        let b = base.clone();
        move || login_blocking(&b, "vasya", "wrong1")
    })
    .await;
    assert_eq!(code, 401);
    let (code, _) = blocking({
        let b = base.clone();
        move || login_blocking(&b, "nobody", "123456")
    })
    .await;
    assert_eq!(code, 404);

    // Вход без учёта регистра выдаёт новый токен.
    let (code, auth2) = blocking({
        let b = base.clone();
        move || login_blocking(&b, "VASYA", "123456")
    })
    .await;
    assert_eq!(code, 200);
    let auth2 = auth2.unwrap();
    assert_ne!(auth2.token, auth.token);
    assert_eq!(auth2.display_name, "Вася");

    // /account отдаёт профиль по токену.
    let b = base.clone();
    let t = auth.token.clone();
    let info = blocking(move || {
        let (code, info) = get_json::<AccountInfo>(&format!("{b}/account?token={t}"));
        (code, info)
    })
    .await;
    assert_eq!(info.0, 200);
    assert_eq!(info.1.unwrap().username, "vasya");

    h.stop().await;
}

#[tokio::test]
async fn profile_privacy_and_logout() {
    let (h, base) = start().await;
    let (_, auth) = blocking({
        let b = base.clone();
        move || register_blocking(&b, "lena", "Лена", "123456")
    })
    .await;
    let auth = auth.unwrap();

    let req = ProfileReq {
        token: auth.token.clone(),
        display_name: Some("Лена К.".into()),
        dm_privacy: Some("nobody".into()),
        show_online: Some(false),
    };
    let (code, info) = blocking({
        let b = base.clone();
        move || post_profile(&b, &req)
    })
    .await;
    assert_eq!(code, 200);
    let info = info.unwrap();
    assert_eq!(info.display_name, "Лена К.");
    assert_eq!(info.dm_privacy, "nobody");
    assert!(!info.show_online);

    // Некорректная приватность.
    let req = ProfileReq {
        token: auth.token.clone(),
        display_name: None,
        dm_privacy: Some("friends".into()),
        show_online: None,
    };
    let (code, _) = blocking({
        let b = base.clone();
        move || post_profile(&b, &req)
    })
    .await;
    assert_eq!(code, 400);

    // Пустое имя отображается как юзернейм.
    let req = ProfileReq {
        token: auth.token.clone(),
        display_name: Some("   ".into()),
        dm_privacy: None,
        show_online: None,
    };
    let (code, info) = blocking({
        let b = base.clone();
        move || post_profile(&b, &req)
    })
    .await;
    assert_eq!(code, 200);
    assert_eq!(info.unwrap().display_name, "lena");

    // Logout убивает токен.
    let body = serde_json::json!({ "token": auth.token }).to_string();
    let b = base.clone();
    let code = blocking(move || {
        match ureq::post(&format!("{b}/logout"))
            .set("content-type", "application/json")
            .send_bytes(body.as_bytes())
        {
            Ok(_) => 200,
            Err(ureq::Error::Status(code, _)) => code,
            Err(_) => 0,
        }
    })
    .await;
    assert_eq!(code, 200);
    let b = base.clone();
    let t = auth.token.clone();
    let code = blocking(move || get_status(&format!("{b}/account?token={t}"))).await;
    assert_eq!(code, 401);

    h.stop().await;
}

#[tokio::test]
async fn users_search_and_presence() {
    let (h, base) = start().await;
    let (mut alice_sink, mut alice_stream, alice) = connect(&base, "alice", "Алиса").await;
    let (_bob_sink, _bob_stream, _bob) = connect(&base, "bob", "Боб").await;
    let (_c_sink, _c_stream, _c) = connect(&base, "carol", "Кэрол").await;

    let b = base.clone();
    let t = alice.token.clone();
    let (code, users) = blocking(move || {
        get_json::<Vec<UserCard>>(&format!("{b}/users?token={t}&q=bo"))
    })
    .await;
    assert_eq!(code, 200);
    let users = users.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].username, "bob");
    assert_eq!(users[0].display_name, "Боб");
    assert!(users[0].online);

    // Себя в поиске не показываем.
    let b = base.clone();
    let t = alice.token.clone();
    let (_, users) = blocking(move || get_json::<Vec<UserCard>>(&format!("{b}/users?token={t}&q=alice"))).await;
    assert!(users.unwrap().is_empty());

    // bob скрывает онлайн — для alice он офлайн.
    let (_, bob) = blocking({
        let b = base.clone();
        move || login_blocking(&b, "bob", "123456")
    })
    .await;
    let req = ProfileReq {
        token: bob.unwrap().token,
        display_name: None,
        dm_privacy: None,
        show_online: Some(false),
    };
    let (code, _) = blocking({
        let b = base.clone();
        move || post_profile(&b, &req)
    })
    .await;
    assert_eq!(code, 200);

    let b = base.clone();
    let t = alice.token.clone();
    let (_, users) = blocking(move || get_json::<Vec<UserCard>>(&format!("{b}/users?token={t}&q=bob"))).await;
    assert!(!users.unwrap()[0].online);

    // Без токена список недоступен.
    let b = base.clone();
    let code = blocking(move || get_status(&format!("{b}/users?q=bob"))).await;
    assert_eq!(code, 401);

    // alice остаётся в сети: ей приходит ответ на сообщение.
    send_text(&mut alice_sink, "привет", None).await;
    recv_until(&mut alice_stream, |m| matches!(m, ServerMsg::Text { text, .. } if text == "привет")).await;
    h.stop().await;
}

#[tokio::test]
async fn general_chat_text_and_attachment() {
    let (h, base) = start().await;
    let (mut sink, mut stream, auth) = connect(&base, "vasya", "Вася").await;

    let b = base.clone();
    let t = auth.token.clone();
    let att = blocking(move || {
        ureq::post(&format!("{b}/upload?token={t}&name=cat.jpg&mime=image%2Fjpeg"))
            .send_bytes(b"\xff\xd8\xff")
            .unwrap()
            .into_json::<Attachment>()
            .unwrap()
    })
    .await;

    let msg = serde_json::to_string(&ClientMsg::Text {
        text: "смотри".into(),
        attachment_id: Some(att.id.clone()),
        to: None,
    })
    .unwrap();
    sink.send(Message::Text(msg)).await.unwrap();

    let got = recv_until(&mut stream, |m| matches!(m, ServerMsg::Text { attachment: Some(_), .. })).await;
    match got {
        ServerMsg::Text { text, attachment: Some(a), from, from_name, to, .. } => {
            assert_eq!(text, "смотри");
            assert_eq!(a.kind, "image");
            assert_eq!(a.name, "cat.jpg");
            assert_eq!(from, "vasya");
            assert_eq!(from_name, "Вася");
            assert!(to.is_none());
        }
        _ => unreachable!(),
    }

    // Поддельный id вложения: ошибка приходит персонально отправителю.
    let fake = serde_json::to_string(&ClientMsg::Text {
        text: "x".into(),
        attachment_id: Some("nope".into()),
        to: None,
    })
    .unwrap();
    sink.send(Message::Text(fake)).await.unwrap();
    recv_until(&mut stream, |m| matches!(m, ServerMsg::Error { .. })).await;

    // История общего чата содержит вложение.
    let b = base.clone();
    let t = auth.token.clone();
    let (code, hist) = blocking(move || {
        get_json::<Vec<StoredMessage>>(&format!("{b}/history?token={t}"))
    })
    .await;
    assert_eq!(code, 200);
    let hist = hist.unwrap();
    assert!(hist.iter().any(|m| m.attachment.as_ref().map(|a| a.name == "cat.jpg").unwrap_or(false)));
    assert!(hist.iter().all(|m| m.to.is_none()));

    h.stop().await;
}

#[tokio::test]
async fn dm_flow_and_privacy() {
    let (h, base) = start().await;
    let (mut a_sink, mut a_stream, a) = connect(&base, "alice", "Алиса").await;
    let (_b_sink, mut b_stream, b) = connect(&base, "bob", "Боб").await;
    let (_c_sink, mut c_stream, _c) = connect(&base, "carol", "Кэрол").await;

    send_text(&mut a_sink, "только для боба", Some("bob")).await;

    // Отправитель получает эхо.
    recv_until(&mut a_stream, |m| matches!(m, ServerMsg::Text { to: Some(p), .. } if p == "bob")).await;
    // Получатель — сообщение.
    let got = recv_until(&mut b_stream, |m| matches!(m, ServerMsg::Text { .. })).await;
    match got {
        ServerMsg::Text { text, from, to, .. } => {
            assert_eq!(text, "только для боба");
            assert_eq!(from, "alice");
            assert_eq!(to.as_deref(), Some("bob"));
        }
        _ => unreachable!(),
    }
    // Третий участник не получает личных сообщений (системные — можно).
    let mut leaked = false;
    loop {
        match tokio::time::timeout(Duration::from_millis(300), c_stream.next()).await {
            Err(_) => break,
            Ok(Some(Ok(Message::Text(raw)))) => {
                if matches!(serde_json::from_str::<ServerMsg>(&raw), Ok(ServerMsg::Text { .. })) {
                    leaked = true;
                    break;
                }
            }
            Ok(_) => break,
        }
    }
    assert!(!leaked, "личное сообщение утекло третьему пользователю");

    // История переписки с обеих сторон.
    for (who, token, peer) in [
        ("alice", a.token.clone(), "bob"),
        ("bob", b.token.clone(), "alice"),
    ] {
        let bse = base.clone();
        let t = token.clone();
        let (code, hist) = blocking(move || {
            get_json::<Vec<StoredMessage>>(&format!("{bse}/history?token={t}&peer={peer}"))
        })
        .await;
        assert_eq!(code, 200, "история для {who}");
        let hist = hist.unwrap();
        assert_eq!(hist.len(), 1, "история для {who}");
        assert_eq!(hist[0].text, "только для боба");
        assert_eq!(hist[0].to.as_deref(), Some("bob"));
    }

    // В общую историю личное сообщение не попадает.
    let b = base.clone();
    let t = a.token.clone();
    let (_, hist) = blocking(move || get_json::<Vec<StoredMessage>>(&format!("{b}/history?token={t}"))).await;
    assert!(hist.unwrap().is_empty());

    // Боб запрещает личку — сообщение отклоняется.
    let (_, b_auth) = blocking({
        let b = base.clone();
        move || login_blocking(&b, "bob", "123456")
    })
    .await;
    let req = ProfileReq {
        token: b_auth.unwrap().token,
        display_name: None,
        dm_privacy: Some("nobody".into()),
        show_online: None,
    };
    let (code, _) = blocking({
        let b = base.clone();
        move || post_profile(&b, &req)
    })
    .await;
    assert_eq!(code, 200);

    send_text(&mut a_sink, "ещё одно", Some("bob")).await;
    let err = recv_until(&mut a_stream, |m| matches!(m, ServerMsg::Error { .. })).await;
    match err {
        ServerMsg::Error { text } => assert!(text.contains("запретил")),
        _ => unreachable!(),
    }

    // Себе писать нельзя.
    send_text(&mut a_sink, "себе", Some("alice")).await;
    let err = recv_until(&mut a_stream, |m| matches!(m, ServerMsg::Error { .. })).await;
    match err {
        ServerMsg::Error { text } => assert!(text.contains("самому себе")),
        _ => unreachable!(),
    }

    h.stop().await;
}

#[tokio::test]
async fn upload_and_file_auth() {
    let (h, base) = start().await;
    let (_, auth) = blocking({
        let b = base.clone();
        move || register_blocking(&b, "files", "Файлы", "123456")
    })
    .await;
    let token = auth.unwrap().token;

    let b = base.clone();
    let t = token.clone();
    let meta = blocking(move || {
        ureq::post(&format!("{b}/upload?token={t}&name=..%2F..%2Fhi.txt&mime=text%2Fplain"))
            .send_bytes(b"hello world")
            .unwrap()
            .into_json::<Attachment>()
            .unwrap()
    })
    .await;
    assert_eq!(meta.name, "hi.txt");
    assert_eq!(meta.kind, "file");

    // Без токена файл не отдаётся.
    let b = base.clone();
    let id = meta.id.clone();
    let code = blocking(move || get_status(&format!("{b}/files/{id}"))).await;
    assert_eq!(code, 401);

    let b = base.clone();
    let id = meta.id.clone();
    let t = token.clone();
    let (ctype, disp, bytes) = blocking(move || {
        let r = ureq::get(&format!("{b}/files/{id}?token={t}")).call().unwrap();
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

    // Невалидный id — 404 (но с токеном).
    let b = base.clone();
    let t = token.clone();
    let code = blocking(move || get_status(&format!("{b}/files/deadbeef?token={t}"))).await;
    assert_eq!(code, 404);

    h.stop().await;
}

#[tokio::test]
async fn avatar_upload_and_remove() {
    let (h, base) = start().await;
    let (_, auth) = blocking({
        let b = base.clone();
        move || register_blocking(&b, "avatar", "Аватар", "123456")
    })
    .await;
    let token = auth.unwrap().token;

    let b = base.clone();
    let t = token.clone();
    let info = blocking(move || {
        ureq::post(&format!("{b}/avatar?token={t}"))
            .set("content-type", "image/png")
            .send_bytes(b"\x89PNG")
            .unwrap()
            .into_json::<AccountInfo>()
            .unwrap()
    })
    .await;
    let avatar_id = info.avatar.clone().expect("id аватарки");
    assert!(!avatar_id.is_empty());

    // Аватарка доступна по токену.
    let b = base.clone();
    let t = token.clone();
    let id = avatar_id.clone();
    let (code, ctype) = blocking(move || {
        let r = ureq::get(&format!("{b}/files/{id}?token={t}")).call().unwrap();
        (200, r.header("content-type").unwrap().to_string())
    })
    .await;
    assert_eq!(code, 200);
    assert_eq!(ctype, "image/png");

    // Не картинка — отказ.
    let b = base.clone();
    let t = token.clone();
    let code = blocking(move || {
        match ureq::post(&format!("{b}/avatar?token={t}"))
            .set("content-type", "text/html")
            .send_bytes(b"<html>")
        {
            Ok(_) => 200,
            Err(ureq::Error::Status(code, _)) => code,
            Err(_) => 0,
        }
    })
    .await;
    assert_eq!(code, 400);

    // Удаление.
    let b = base.clone();
    let t = token.clone();
    let info = blocking(move || {
        ureq::delete(&format!("{b}/avatar?token={t}"))
            .call()
            .unwrap()
            .into_json::<AccountInfo>()
            .unwrap()
    })
    .await;
    assert!(info.avatar.is_none());

    h.stop().await;
}

#[tokio::test]
async fn ws_requires_auth() {
    let (h, _base) = start().await;
    let url = format!("ws://{}/ws?username=vasya&token=nope", h.addr);
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
    let (_, auth) = blocking({
        let b = base.clone();
        move || register_blocking(&b, "twin", "Двойник", "123456")
    })
    .await;
    let token = auth.unwrap().token;
    let url1 = format!("ws://{}/ws?username=twin&token={}", h.addr, urlencode(&token));
    let (ws1, _) = connect_async(&url1).await.unwrap();
    let (_s1, mut st1) = ws1.split();

    let url2 = format!("ws://{}/ws?username=twin&token={}", h.addr, urlencode(&token));
    let (ws2, _) = connect_async(&url2).await.unwrap();
    let (_s2, mut st2) = ws2.split();

    let kicked = recv_until(&mut st1, |m| matches!(m, ServerMsg::Kick { .. })).await;
    assert!(matches!(kicked, ServerMsg::Kick { .. }));

    let welcome = recv_until(&mut st2, |m| matches!(m, ServerMsg::Welcome { .. })).await;
    match welcome {
        ServerMsg::Welcome { username, display_name } => {
            assert_eq!(username, "twin");
            assert_eq!(display_name, "Двойник");
        }
        _ => unreachable!(),
    }
    h.stop().await;
}

#[tokio::test]
async fn history_survives_restart() {
    let dir = temp_dir("persist");
    let (h, base) = start_in(dir.clone()).await;
    let (_, auth) = blocking({
        let b = base.clone();
        move || register_blocking(&b, "arch", "Архив", "123456")
    })
    .await;
    let token = auth.unwrap().token;

    let url = format!("ws://{}/ws?username=arch&token={}", h.addr, urlencode(&token));
    let (ws, _) = connect_async(&url).await.unwrap();
    let (mut sink, _st) = ws.split();
    send_text(&mut sink, "сохрани меня", None).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.stop().await;

    // Перезапуск на той же папке данных.
    let (h2, base2) = start_in(dir).await;
    let (_, auth2) = blocking({
        let b = base2.clone();
        move || login_blocking(&b, "arch", "123456")
    })
    .await;
    let token2 = auth2.unwrap().token;
    let b = base2.clone();
    let (_, hist) = blocking(move || {
        get_json::<Vec<StoredMessage>>(&format!("{b}/history?token={token2}"))
    })
    .await;
    assert!(hist.unwrap().iter().any(|m| m.text == "сохрани меня"));
    assert_eq!(h2.account_count(), 1);
    h2.stop().await;
}

#[test]
fn sanitizers_and_validation() {
    assert_eq!(server::sanitize_file_name("../../etc/passwd"), "passwd");
    assert_eq!(server::sanitize_file_name("C:\\x\\a.png"), "a.png");
    assert_eq!(server::sanitize_file_name("   "), "file");
    assert_eq!(server::sanitize_mime("text/html; charset=x"), "application/octet-stream");
    assert_eq!(server::sanitize_mime("video/webm;codecs=vp8,opus"), "application/octet-stream");
    assert_eq!(server::sanitize_mime("video/webm"), "video/webm");

    assert!(shared::valid_username("good_name"));
    assert!(!shared::valid_username("Плохое"));
    assert!(!shared::valid_username("admin"));
    assert!(shared::valid_password("123456"));
    assert!(!shared::valid_password("12345"));
}
