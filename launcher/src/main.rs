#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use tokio::sync::Mutex;

/// Запущенный сервер (если есть).
struct Running(Mutex<Option<server::ServerHandle>>);

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
    let (running, users, messages, logs) = match guard.as_ref() {
        Some(h) => (true, h.users(), h.message_count(), h.logs()),
        None => (false, Vec::new(), 0, Vec::new()),
    };
    Ok(Status {
        running,
        port: port_now,
        users,
        messages,
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
    let handle = server::start("0.0.0.0", port).await?;
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
