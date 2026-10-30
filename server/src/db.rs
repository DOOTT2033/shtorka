//! Хранилище Шторки: SQLite-база для аккаунтов и истории сообщений.
//! База лежит в `data/server.db`, вложения — файлами в `data/files`.

use std::path::Path;
use std::sync::Mutex;

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rusqlite::{params, Connection, OptionalExtension};
use shared::{normalize_display_name, valid_password, valid_privacy, valid_username, Attachment, StoredMessage};

/// Публичная информация об аккаунте.
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub name: String,
    pub display_name: String,
    pub avatar: Option<String>,
    pub dm_privacy: String,
    pub show_online: bool,
    pub created: u64,
}

/// Ошибки работы с аккаунтами.
#[derive(Debug, PartialEq)]
pub enum AuthError {
    NameTaken,
    BadName,
    BadPassword,
    BadPrivacy,
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

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let name: String = row.get(1).map_err(|e| e.to_string())?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<(), String> {
    if has_column(conn, table, column)? {
        return Ok(());
    }
    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))
        .map_err(|e| format!("Не удалось обновить таблицу {table}: {e}"))
}

const SELECT_MESSAGES: &str = "SELECT m.id, m.from_user,
        COALESCE(NULLIF(u.display_name, ''), m.from_user),
        u.avatar, m.to_user, m.text, m.attachment, m.ts
 FROM messages m LEFT JOIN users u ON u.name = m.from_user";

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

        // Миграции со схемы 0.0.2 (без потери аккаунтов и истории).
        add_column(&conn, "users", "display_name", "TEXT NOT NULL DEFAULT ''")?;
        add_column(&conn, "users", "avatar", "TEXT")?;
        add_column(&conn, "users", "dm_privacy", "TEXT NOT NULL DEFAULT 'everyone'")?;
        add_column(&conn, "users", "show_online", "INTEGER NOT NULL DEFAULT 1")?;
        add_column(&conn, "messages", "to_user", "TEXT")?;
        conn.execute_batch(
            "
            UPDATE users SET display_name = name WHERE display_name = '';
            CREATE INDEX IF NOT EXISTS idx_messages_dm ON messages (to_user, id);
            ",
        )
        .map_err(|e| format!("Не удалось обновить данные: {e}"))?;

        Ok(Self { conn: Mutex::new(conn) })
    }

    /* ---------- Аккаунты ---------- */

    pub fn register(
        &self,
        name: &str,
        display_name: &str,
        password: &str,
        now: u64,
    ) -> Result<User, AuthError> {
        if !valid_username(name) {
            return Err(AuthError::BadName);
        }
        if !valid_password(password) {
            return Err(AuthError::BadPassword);
        }
        let display = normalize_display_name(display_name, name);
        let hash = hash_password(password)?;
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM users WHERE name = ?1)", params![name], |r| r.get(0))
            .map_err(|_| AuthError::NameTaken)?;
        if exists {
            return Err(AuthError::NameTaken);
        }
        conn.execute(
            "INSERT INTO users (name, pass_hash, created, display_name) VALUES (?1, ?2, ?3, ?4)",
            params![name, hash, now, display],
        )
        .map_err(|_| AuthError::NameTaken)?;
        Ok(User {
            name: name.to_string(),
            display_name: display,
            avatar: None,
            dm_privacy: "everyone".into(),
            show_online: true,
            created: now,
        })
    }

    pub fn login(&self, name: &str, password: &str) -> Result<User, AuthError> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, String, Option<String>, String, i64, u64)> = conn
            .query_row(
                "SELECT pass_hash, display_name, avatar, dm_privacy, show_online, created
                 FROM users WHERE name = ?1",
                params![name],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| AuthError::WrongCredentials)?;
        match row {
            Some((hash, display_name, avatar, dm_privacy, show_online, created))
                if verify_password(password, &hash) =>
            {
                Ok(User {
                    name: name.to_string(),
                    display_name: if display_name.is_empty() { name.to_string() } else { display_name },
                    avatar,
                    dm_privacy,
                    show_online: show_online != 0,
                    created,
                })
            }
            Some(_) => Err(AuthError::WrongCredentials),
            None => Err(AuthError::NoUser),
        }
    }

    pub fn change_password(&self, name: &str, old: &str, new: &str) -> Result<(), AuthError> {
        if !valid_password(new) {
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

    pub fn update_profile(
        &self,
        name: &str,
        display_name: Option<&str>,
        dm_privacy: Option<&str>,
        show_online: Option<bool>,
    ) -> Result<User, AuthError> {
        if let Some(p) = dm_privacy {
            if !valid_privacy(p) {
                return Err(AuthError::BadPrivacy);
            }
        }
        let conn = self.conn.lock().unwrap();
        if let Some(raw) = display_name {
            let display = normalize_display_name(raw, name);
            conn.execute("UPDATE users SET display_name = ?1 WHERE name = ?2", params![display, name])
                .map_err(|_| AuthError::NoUser)?;
        }
        if let Some(p) = dm_privacy {
            conn.execute("UPDATE users SET dm_privacy = ?1 WHERE name = ?2", params![p, name])
                .map_err(|_| AuthError::NoUser)?;
        }
        if let Some(v) = show_online {
            conn.execute(
                "UPDATE users SET show_online = ?1 WHERE name = ?2",
                params![if v { 1 } else { 0 }, name],
            )
            .map_err(|_| AuthError::NoUser)?;
        }
        Self::user_locked(&conn, name).ok_or(AuthError::NoUser)
    }

    pub fn set_avatar(&self, name: &str, avatar: Option<&str>) -> Result<(), AuthError> {
        let conn = self.conn.lock().unwrap();
        let changed = conn
            .execute("UPDATE users SET avatar = ?1 WHERE name = ?2", params![avatar, name])
            .map_err(|_| AuthError::NoUser)?;
        if changed == 0 { Err(AuthError::NoUser) } else { Ok(()) }
    }

    fn user_locked(conn: &Connection, name: &str) -> Option<User> {
        conn.query_row(
            "SELECT name, display_name, avatar, dm_privacy, show_online, created
             FROM users WHERE name = ?1",
            params![name],
            |r| {
                Ok(User {
                    name: r.get(0)?,
                    display_name: r.get(1)?,
                    avatar: r.get(2)?,
                    dm_privacy: r.get(3)?,
                    show_online: r.get::<_, i64>(4)? != 0,
                    created: r.get(5)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn user(&self, name: &str) -> Option<User> {
        let conn = self.conn.lock().unwrap();
        Self::user_locked(&conn, name)
    }

    pub fn account_info(&self, name: &str) -> Option<User> {
        self.user(name)
    }

    pub fn user_count(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap_or(0)
    }

    /// Ищет пользователей по юзернейму и отображаемому имени.
    /// Пустой запрос — список всех (до `limit`), начиная с тех, кто в сети.
    pub fn search_users(&self, query: &str, exclude: &str, limit: usize) -> Vec<User> {
        let q = query.trim();
        let pattern = format!("%{}%", escape_like(q));
        let conn = self.conn.lock().unwrap();
        let sql = "SELECT name, display_name, avatar, dm_privacy, show_online, created
                   FROM users
                   WHERE name <> ?2 AND (name LIKE ?1 ESCAPE '\\' OR display_name LIKE ?1 ESCAPE '\\')
                   ORDER BY name LIMIT ?3";
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![pattern, exclude, limit as i64], |r| {
            Ok(User {
                name: r.get(0)?,
                display_name: r.get(1)?,
                avatar: r.get(2)?,
                dm_privacy: r.get(3)?,
                show_online: r.get::<_, i64>(4)? != 0,
                created: r.get(5)?,
            })
        });
        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /* ---------- Сообщения ---------- */

    pub fn insert_message(&self, m: &StoredMessage) -> Result<(), String> {
        let attachment = m
            .attachment
            .as_ref()
            .map(|a| serde_json::to_string(a).unwrap_or_default());
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (id, from_user, to_user, text, attachment, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![m.id, m.from, m.to, m.text, attachment, m.ts],
        )
        .map(|_| ())
        .map_err(|e| format!("Не удалось сохранить сообщение: {e}"))
    }

    /// Последние `limit` сообщений общего чата в хронологическом порядке.
    pub fn recent_general(&self, limit: usize) -> Vec<StoredMessage> {
        let conn = self.conn.lock().unwrap();
        self.recent_locked(
            &conn,
            &format!("{SELECT_MESSAGES} WHERE m.to_user IS NULL ORDER BY m.id DESC LIMIT ?1"),
            params![limit as i64],
        )
    }

    /// Последние `limit` сообщений личной переписки `a` ↔ `b`.
    pub fn recent_dm(&self, a: &str, b: &str, limit: usize) -> Vec<StoredMessage> {
        let conn = self.conn.lock().unwrap();
        self.recent_locked(
            &conn,
            &format!(
                "{SELECT_MESSAGES}
                 WHERE (m.from_user = ?1 AND m.to_user = ?2)
                    OR (m.from_user = ?2 AND m.to_user = ?1)
                 ORDER BY m.id DESC LIMIT ?3"
            ),
            params![a, b, limit as i64],
        )
    }

    fn recent_locked(&self, conn: &Connection, sql: &str, args: impl rusqlite::Params) -> Vec<StoredMessage> {
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let map = |r: &rusqlite::Row<'_>| {
            let attachment_json: Option<String> = r.get(6)?;
            Ok(StoredMessage {
                id: r.get(0)?,
                from: r.get(1)?,
                from_name: r.get(2)?,
                from_avatar: r.get(3)?,
                to: r.get(4)?,
                text: r.get(5)?,
                ts: r.get(7)?,
                attachment: attachment_json.and_then(|j| serde_json::from_str::<Attachment>(&j).ok()),
            })
        };
        let result = match stmt.query_map(args, map) {
            Ok(iter) => {
                let mut v: Vec<StoredMessage> = iter.filter_map(|r| r.ok()).collect();
                v.reverse(); // запрос шёл в обратном порядке
                v
            }
            Err(_) => Vec::new(),
        };
        result
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

fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
