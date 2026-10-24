//! Хранилище Шторки: SQLite-база для аккаунтов и истории сообщений.
//! База лежит в `data/server.db`, вложения — файлами в `data/files` (как раньше).

use std::path::Path;
use std::sync::Mutex;

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rusqlite::{params, Connection, OptionalExtension};
use shared::{Attachment, StoredMessage};

pub const MIN_PASSWORD_LEN: usize = 4;
pub const MAX_PASSWORD_LEN: usize = 128;

/// Публичная информация об аккаунте.
#[derive(Debug, Clone)]
pub struct User {
    pub name: String,
    pub created: u64,
}

/// Ошибки работы с аккаунтами.
#[derive(Debug, PartialEq)]
pub enum AuthError {
    NameTaken,
    BadName,
    BadPassword,
    WrongCredentials,
    NoUser,
}

fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| AuthError::BadPassword)
}

fn verify_password(password: &str, hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(p) => p,
        Err(_) => return false,
    };
    Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok()
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(db_path: &Path) -> Result<Self, String> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("Не удалось создать папку базы: {e}"))?;
        }
        let conn = Connection::open(db_path)
            .map_err(|e| format!("Не удалось открыть базу {}: {e}", db_path.display()))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS users (
                name       TEXT PRIMARY KEY,
                pass_hash  TEXT NOT NULL,
                created    INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id         INTEGER PRIMARY KEY,
                from_user  TEXT NOT NULL,
                text       TEXT NOT NULL,
                attachment TEXT,
                ts         INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_ts ON messages (id);
            ",
        )
        .map_err(|e| format!("Не удалось создать таблицы: {e}"))?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /* ---------- Аккаунты ---------- */

    pub fn register(&self, name: &str, password: &str, now: u64) -> Result<(), AuthError> {
        if !crate::valid_name(name) {
            return Err(AuthError::BadName);
        }
        if password.len() < MIN_PASSWORD_LEN || password.len() > MAX_PASSWORD_LEN {
            return Err(AuthError::BadPassword);
        }
        let hash = hash_password(password)?;
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM users WHERE name = ?1)", params![name], |r| r.get(0))
            .map_err(|_| AuthError::NameTaken)?;
        if exists {
            return Err(AuthError::NameTaken);
        }
        conn.execute(
            "INSERT INTO users (name, pass_hash, created) VALUES (?1, ?2, ?3)",
            params![name, hash, now],
        )
        .map(|_| ())
        .map_err(|_| AuthError::NameTaken)
    }

    pub fn login(&self, name: &str, password: &str) -> Result<User, AuthError> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, u64)> = conn
            .query_row(
                "SELECT pass_hash, created FROM users WHERE name = ?1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| AuthError::WrongCredentials)?;
        match row {
            Some((hash, created)) if verify_password(password, &hash) => Ok(User { name: name.to_string(), created }),
            Some(_) => Err(AuthError::WrongCredentials),
            None => Err(AuthError::NoUser),
        }
    }

    pub fn change_password(&self, name: &str, old: &str, new: &str) -> Result<(), AuthError> {
        if new.len() < MIN_PASSWORD_LEN || new.len() > MAX_PASSWORD_LEN {
            return Err(AuthError::BadPassword);
        }
        let conn = self.conn.lock().unwrap();
        let hash: Option<String> = conn
            .query_row("SELECT pass_hash FROM users WHERE name = ?1", params![name], |r| r.get(0))
            .optional()
            .map_err(|_| AuthError::NoUser)?;
        let hash = hash.ok_or(AuthError::NoUser)?;
        if !verify_password(old, &hash) {
            return Err(AuthError::WrongCredentials);
        }
        let new_hash = hash_password(new)?;
        conn.execute("UPDATE users SET pass_hash = ?1 WHERE name = ?2", params![new_hash, name])
            .map(|_| ())
            .map_err(|_| AuthError::NoUser)
    }

    pub fn account_info(&self, name: &str) -> Option<User> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT name, created FROM users WHERE name = ?1", params![name], |r| {
            Ok(User { name: r.get(0)?, created: r.get(1)? })
        })
        .optional()
        .ok()
        .flatten()
    }

    pub fn user_count(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap_or(0)
    }

    /* ---------- Сообщения ---------- */

    pub fn insert_message(&self, m: &StoredMessage) -> Result<(), String> {
        let attachment = m
            .attachment
            .as_ref()
            .map(|a| serde_json::to_string(a).unwrap_or_default());
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (id, from_user, text, attachment, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![m.id, m.from, m.text, attachment, m.ts],
        )
        .map(|_| ())
        .map_err(|e| format!("Не удалось сохранить сообщение: {e}"))
    }

    /// Последние `limit` сообщений в хронологическом порядке.
    pub fn recent_messages(&self, limit: usize) -> Vec<StoredMessage> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT id, from_user, text, attachment, ts FROM messages ORDER BY id DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![limit as i64], |r| {
            let attachment_json: Option<String> = r.get(3)?;
            Ok(StoredMessage {
                id: r.get(0)?,
                from: r.get(1)?,
                text: r.get(2)?,
                ts: r.get(4)?,
                attachment: attachment_json
                    .and_then(|j| serde_json::from_str::<Attachment>(&j).ok()),
            })
        });
        match rows {
            Ok(iter) => {
                let mut v: Vec<StoredMessage> = iter.filter_map(|r| r.ok()).collect();
                v.reverse(); // запрос шёл в обратном порядке
                v
            }
            Err(_) => Vec::new(),
        }
    }

    pub fn message_count(&self) -> u64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap_or(0)
    }

    /// Следующий свободный id сообщения.
    pub fn next_message_id(&self) -> u64 {
        let conn = self.conn.lock().unwrap();
        let max: Option<u64> = conn
            .query_row("SELECT MAX(id) FROM messages", [], |r| r.get(0))
            .ok()
            .flatten();
        max.unwrap_or(0) + 1
    }
}
