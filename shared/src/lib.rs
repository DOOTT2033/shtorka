use serde::{Deserialize, Serialize};

/// Сообщение, которое клиент отправляет серверу.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Текст в общий чат.
    Text { text: String },
}

/// Сообщение, которое сервер рассылает клиентам.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Приветствие при подключении.
    Welcome { user: String },
    /// Новое текстовое сообщение.
    Text { id: u64, from: String, text: String, ts: u64 },
    /// Системное сообщение (вход/выход пользователя).
    System { text: String },
    /// Ошибка для клиента.
    Error { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredMessage {
    pub id: u64,
    pub from: String,
    pub text: String,
    pub ts: u64,
}
