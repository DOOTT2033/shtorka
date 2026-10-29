#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

/// Запущенный сервер (если есть).
struct Running(Mutex<Option<server::ServerHandle>>);

/// Проверяет, что в папку можно писать (при необходимости создаёт её).
fn writable_dir(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".shtorka-write-test");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    if ok {
        let _ = std::fs::remove_file(&probe);
    }
    ok
}

/// Где хранить config.toml и data.
///
/// Портативная сборка: рядом с exe. Если exe лежит в Program Files (установщик),
/// туда писать нельзя — тогда `%LOCALAPPDATA%\Shtorka\server`.
fn storage_dirs(exe_dir: &Path) -> (PathBuf, PathBuf) {
    if writable_dir(exe_dir) {
        return (exe_dir.to_path_buf(), exe_dir.join("data"));
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Shtorka")
        .join("server");
    (base.clone(), base.join("data"))
}

#[derive(Serialize)]
struct Address {
    /// Понятная подпись: Radmin VPN, Локальная сеть и т.д.
    label: String,
    /// Имя сетевого адаптера.
    iface: String,
    /// Адрес для подключения клиента, например ws://192.168.1.5:7878
    url: String,
}

#[derive(Serialize)]
struct Status {
    running: bool,
    port: u16,
    users: Vec<String>,
    messages: usize,
    /// Аккаунтов в базе (None — сервер остановлен).
    accounts: Option<usize>,
    logs: Vec<String>,
    addresses: Vec<Address>,
}

/// Подпись адреса по адаптеру и диапазону IP.
fn classify(iface: &str, ip: &std::net::Ipv4Addr) -> String {
    let name = iface.to_lowercase();
    let o = ip.octets();
    if name.contains("radmin") || o[0] == 26 {
        "Radmin VPN".into()
    } else if o[0] == 127 {
        "Этот ПК".into()
    } else if o[0] == 192 && o[1] == 168 || o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) {
        "Локальная сеть".into()
    } else {
        "Другой адаптер".into()
    }
}

fn list_addresses(port: u16) -> Vec<Address> {
    let mut out = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if let std::net::IpAddr::V4(ip) = iface.ip() {
                // 169.254.x.x — автоматический адрес без сети, клиенту не нужен.
                if ip.octets()[0] == 169 && ip.octets()[1] == 254 {
                    continue;
                }
                out.push(Address {
                    label: classify(&iface.name, &ip),
                    iface: iface.name.clone(),
                    url: format!("ws://{ip}:{port}"),
                });
            }
        }
    }
    // Сначала «Radmin VPN» и «Локальная сеть», «Этот ПК» — в конце.
    let rank = |l: &str| match l {
        "Radmin VPN" => 0,
        "Локальная сеть" => 1,
        "Другой адаптер" => 2,
        _ => 3,
    };
    out.sort_by_key(|a| rank(&a.label));
    out
}

#[tauri::command]
async fn status(state: tauri::State<'_, Running>, port: Option<u16>) -> Result<Status, String> {
    let guard = state.0.lock().await;
    let port_now = match guard.as_ref() {
        Some(h) => h.addr.port(),
        None => port.unwrap_or(7878),
    };
    let (running, users, messages, accounts, logs) = match guard.as_ref() {
        Some(h) => (true, h.users(), h.message_count(), Some(h.account_count()), h.logs()),
        None => (false, Vec::new(), 0, None, Vec::new()),
    };
    Ok(Status {
        running,
        port: port_now,
        users,
        messages,
        accounts,
        logs,
        addresses: list_addresses(port_now),
    })
}

#[tauri::command]
async fn start_server(state: tauri::State<'_, Running>, port: u16) -> Result<(), String> {
    let mut guard = state.0.lock().await;
    if guard.is_some() {
        return Err("Сервер уже запущен".into());
    }
    if port < 1024 {
        return Err("Порт должен быть от 1024 до 65535".into());
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    // Портативно — рядом с exe; после установки в Program Files — в %LOCALAPPDATA%.
    let (cfg_dir, data_dir) = storage_dirs(&exe_dir);
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("Не удалось создать папку данных {}: {e}", data_dir.display()))?;
    let mut cfg = server::Config::load_or_create(&cfg_dir.join("config.toml")).unwrap_or_default();
    cfg.host = "0.0.0.0".into();
    cfg.port = port;
    cfg.data_dir = data_dir.to_string_lossy().to_string();
    let handle = server::start(cfg).await?;
    *guard = Some(handle);
    Ok(())
}

#[tauri::command]
async fn stop_server(state: tauri::State<'_, Running>) -> Result<(), String> {
    let handle = state.0.lock().await.take();
    match handle {
        Some(h) => {
            h.stop().await;
            Ok(())
        }
        None => Err("Сервер не запущен".into()),
    }
}

#[tauri::command]
async fn public_ip() -> Result<String, String> {
    tokio::task::spawn_blocking(|| {
        ureq::get("https://api.ipify.org")
            .timeout(std::time::Duration::from_secs(8))
            .call()
            .map_err(|e| format!("Не удалось узнать внешний IP: {e}"))?
            .into_string()
            .map(|s| s.trim().to_string())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .manage(Running(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![status, start_server, stop_server, public_ip])
        .run(tauri::generate_context!())
        .expect("ошибка запуска лаунчера");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "shtorka-launcher-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writable_dir_detects_writable_folder() {
        let dir = temp_dir("writable");
        assert!(writable_dir(&dir));
        // Пробный файл не должен оставаться.
        assert!(!dir.join(".shtorka-write-test").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn writable_dir_rejects_path_under_file() {
        let dir = temp_dir("blocked");
        let file = dir.join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        assert!(!writable_dir(&file.join("data")));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn portable_mode_keeps_data_next_to_exe() {
        let dir = temp_dir("portable");
        let (cfg_dir, data_dir) = storage_dirs(&dir);
        assert_eq!(cfg_dir, dir);
        assert_eq!(data_dir, dir.join("data"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn installed_mode_falls_back_to_local_appdata() {
        let dir = temp_dir("installed");
        let file = dir.join("exe-dir-is-a-file");
        std::fs::write(&file, b"x").unwrap();
        let (cfg_dir, data_dir) = storage_dirs(&file);
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap();
        assert_eq!(cfg_dir, base.join("Shtorka").join("server"));
        assert_eq!(data_dir, base.join("Shtorka").join("server").join("data"));
        std::fs::remove_dir_all(dir).ok();
    }
}
