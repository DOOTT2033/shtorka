use serde::{Deserialize, Serialize};

/// Вложение: файл, фото, голосовое или видео-кружок.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// image | audio | video | file
    pub kind: String,
}

/// Сообщение, которое клиент отправляет серверу.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Текст и/или вложение (вложение сначала загружается через POST /upload).
    Text {
        #[serde(default)]
        text: String,
        #[serde(default)]
        attachment_id: Option<String>,
    },
}

/// Сообщение, которое сервер рассылает клиентам.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome { user: String },
    Text {
        id: u64,
        from: String,
        text: String,
        ts: u64,
        #[serde(default)]
        attachment: Option<Attachment>,
    },
    System { text: String },
    Error { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredMessage {
    pub id: u64,
    pub from: String,
    pub text: String,
    pub ts: u64,
    #[serde(default)]
    pub attachment: Option<Attachment>,
}

/// Определяет тип вложения по MIME-типу.
pub fn kind_for_mime(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("video/") {
        "video"
    } else {
        "file"
    }
}
