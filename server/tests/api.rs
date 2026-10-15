use futures_util::{SinkExt, StreamExt};
use shared::{ClientMsg, ServerMsg};
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

async fn start() -> (server::ServerHandle, String) {
    let h = server::start("127.0.0.1", 0, temp_dir("api")).await.unwrap();
    let base = format!("http://{}", h.addr);
    (h, base)
}

fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = T> {
    async move { tokio::task::spawn_blocking(f).await.unwrap() }
}

#[tokio::test]
async fn health_ok() {
    let (h, base) = start().await;
    let body = blocking(move || ureq::get(&format!("{base}/health")).call().unwrap().into_string().unwrap()).await;
    assert_eq!(body, "ok");
    h.stop().await;
}

#[tokio::test]
async fn upload_and_download_roundtrip() {
    let (h, base) = start().await;
    let b = base.clone();
    let meta = blocking(move || {
        let data = b"hello world".to_vec();
        let resp = ureq::post(&format!("{b}/upload?name=..%2F..%2Fhi.txt&mime=text%2Fplain"))
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
    // Не-медиа отдаём как octet-stream и attachment, чтобы не исполнялось в браузере.
    assert_eq!(ctype, "application/octet-stream");
    assert!(disp.starts_with("attachment"));
    h.stop().await;
}

#[tokio::test]
async fn image_is_served_inline() {
    let (h, base) = start().await;
    let b = base.clone();
    let meta = blocking(move || {
        ureq::post(&format!("{b}/upload?name=p.png&mime=image%2Fpng"))
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
    let b = base.clone();
    let status = blocking(move || match ureq::post(&format!("{b}/upload?name=a")).send_bytes(&[]) {
        Ok(_) => 200,
        Err(ureq::Error::Status(code, _)) => code,
        Err(_) => 0,
    })
    .await;
    assert_eq!(status, 400);
    h.stop().await;
}

#[tokio::test]
async fn ws_text_and_attachment_flow() {
    let (h, base) = start().await;
    let ws_url = format!("ws://{}/ws?name=Вася", h.addr);
    let (ws, _) = connect_async(&ws_url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    // Загружаем картинку и отправляем её в чат.
    let b = base.clone();
    let att = blocking(move || {
        ureq::post(&format!("{b}/upload?name=cat.jpg&mime=image%2Fjpeg"))
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

    // Поддельный id вложения не принимается.
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
    let b = base.clone();
    let hist = blocking(move || ureq::get(&format!("{b}/history")).call().unwrap().into_string().unwrap()).await;
    assert!(hist.contains(&att.id) || hist.contains("cat.jpg"));
    h.stop().await;
}

#[tokio::test]
async fn duplicate_name_rejected() {
    let (h, _base) = start().await;
    let url = format!("ws://{}/ws?name=Петя", h.addr);
    let (_ws1, _) = connect_async(&url).await.unwrap();
    let (ws2, _) = connect_async(&url).await.unwrap();
    let (_s, mut st) = ws2.split();
    let mut rejected = false;
    while let Some(Ok(Message::Text(raw))) = st.next().await {
        if let Ok(ServerMsg::Error { text }) = serde_json::from_str(&raw) {
            rejected = text.contains("занято");
            break;
        }
    }
    assert!(rejected);
    h.stop().await;
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
