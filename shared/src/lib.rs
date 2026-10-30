//! Общие типы клиента и сервера Шторки 0.1.0.
//!
//! 0.1.0: отдельные юзернейм и отображаемое имя, аватарки, личные сообщения,
//! настройки приватности, поиск пользователей.

use serde::{Deserialize, Serialize};

/// Минимальная длина юзернейма.
pub const USERNAME_MIN: usize = 3;
/// Максимальная длина юзернейма.
pub const USERNAME_MAX: usize = 20;
/// Максимальная длина отображаемого имени.
pub const DISPLAY_NAME_MAX: usize = 40;
/// Минимальная длина пароля.
pub const MIN_PASSWORD_LEN: usize = 6;
/// Максимальная длина пароля.
pub const MAX_PASSWORD_LEN: usize = 128;
/// Максимальная длина сообщения.
pub const MAX_TEXT_LEN: usize = 4000;

/// Юзернеймы, которые нельзя занимать.
pub const RESERVED_USERNAMES: &[&str] = &[
    "admin",
    "administrator",
    "moderator",
    "owner",
    "root",
    "server",
    "support",
    "system",
    "shtorka",
    "help",
];

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
    /// `to` — юзернейм получателя для личного сообщения; пусто — общий чат.
    Text {
        #[serde(default)]
        text: String,
        #[serde(default)]
        attachment_id: Option<String>,
        #[serde(default)]
        to: Option<String>,
    },
}

/// Сообщение, которое сервер рассылает клиентам.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        username: String,
        display_name: String,
    },
    Text {
        id: u64,
        /// Юзернейм отправителя.
        from: String,
        /// Отображаемое имя отправителя.
        #[serde(default)]
        from_name: String,
        #[serde(default)]
        from_avatar: Option<String>,
        /// Юзернейм получателя для личного сообщения, иначе null.
        #[serde(default)]
        to: Option<String>,
        text: String,
        ts: u64,
        #[serde(default)]
        attachment: Option<Attachment>,
    },
    System {
        text: String,
    },
    Error {
        text: String,
    },
    /// Сессия закрыта сервером (например, вход с того же аккаунта с другого устройства).
    Kick {
        text: String,
    },
}

/// Запрос регистрации или входа.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthReq {
    /// Юзернейм: латиница, цифры и `_`, без учёта регистра.
    pub username: String,
    pub password: String,
    /// Отображаемое имя (только при регистрации, можно не указывать).
    #[serde(default)]
    pub display_name: String,
}

/// Ответ сервера на регистрацию/вход: токен сессии и профиль.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResp {
    pub token: String,
    pub username: String,
    pub display_name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    pub created: u64,
    /// everyone | nobody — кто может писать в личку.
    pub dm_privacy: String,
    /// Показывать ли другим, что пользователь в сети.
    pub show_online: bool,
}

/// Публичная информация об аккаунте.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub username: String,
    pub display_name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    pub created: u64,
    pub dm_privacy: String,
    pub show_online: bool,
}

/// Смена пароля (только для авторизованного пользователя).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasswordReq {
    pub token: String,
    pub old: String,
    pub new: String,
}

/// Частичное обновление профиля.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileReq {
    pub token: String,
    #[serde(default)]
    pub display_name: Option<String>,
    /// everyone | nobody
    #[serde(default)]
    pub dm_privacy: Option<String>,
    #[serde(default)]
    pub show_online: Option<bool>,
}

/// Карточка пользователя для поиска и списка чатов.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserCard {
    pub username: String,
    pub display_name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    pub online: bool,
}

/// Сохранённое сообщение (история).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredMessage {
    pub id: u64,
    pub from: String,
    #[serde(default)]
    pub from_name: String,
    #[serde(default)]
    pub from_avatar: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    pub text: String,
    pub ts: u64,
    #[serde(default)]
    pub attachment: Option<Attachment>,
}

/// Нормализует юзернейм: обрезает пробелы и приводит к нижнему регистру.
pub fn normalize_username(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// Юзернейм: 3–20 символов, только латиница в нижнем регистре, цифры и `_`,
/// без `_` в начале и конце, не из списка зарезервированных.
pub fn valid_username(name: &str) -> bool {
    let len = name.chars().count();
    (USERNAME_MIN..=USERNAME_MAX).contains(&len)
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with('_')
        && !name.ends_with('_')
        && !RESERVED_USERNAMES.contains(&name)
}

/// Пароль: 6–128 печатных ASCII-символов (латиница, цифры, символы).
pub fn valid_password(password: &str) -> bool {
    (MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&password.len())
        && password.chars().all(|c| c.is_ascii_graphic())
}

/// Приводит отображаемое имя в порядок: без управляющих символов, до 40 символов.
/// Пустое имя заменяется юзернеймом.
pub fn normalize_display_name(raw: &str, username: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(DISPLAY_NAME_MAX)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        username.to_string()
    } else {
        cleaned.to_string()
    }
}

/// Проверяет настройку «кто может писать в личку».
pub fn valid_privacy(value: &str) -> bool {
    matches!(value, "everyone" | "nobody")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        assert!(valid_username("vasya"));
        assert!(valid_username("vasya_2007"));
        assert!(valid_username("a1b"));
        assert!(!valid_username("VaSyA"));
        assert!(!valid_username("вася"));
        assert!(!valid_username("_vasya"));
        assert!(!valid_username("vasya_"));
        assert!(!valid_username("va"));
        assert!(!valid_username("admin"));
        assert!(!valid_username("vas ya"));
        assert!(!valid_username(""));
    }

    #[test]
    fn passwords() {
        assert!(valid_password("123456"));
        assert!(valid_password("Pa$$w0rd!"));
        assert!(!valid_password("12345"));
        assert!(!valid_password("пароль"));
        assert!(!valid_password("pass word"));
    }

    #[test]
    fn display_names() {
        assert_eq!(normalize_display_name("  Вася  ", "vasya"), "Вася");
        assert_eq!(normalize_display_name("", "vasya"), "vasya");
        assert_eq!(normalize_display_name("   ", "vasya"), "vasya");
        assert_eq!(normalize_display_name("a\nb", "vasya"), "ab");
        assert_eq!(normalize_display_name(&"x".repeat(60), "vasya").chars().count(), 40);
    }

    #[test]
    fn normalize_usernames() {
        assert_eq!(normalize_username("  VaSya "), "vasya");
    }
}
