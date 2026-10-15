const $ = (id) => document.getElementById(id);
const LS = "messenger.client";

let ws = null;
let me = "";
let serverUrl = ""; // ws://host:port
let seen = new Set();
let reconnectTimer = null;
let manualClose = false;

function normalizeServer(raw) {
  let s = raw.trim();
  if (!s) throw "Введите адрес сервера";
  if (!/^(wss?|https?):\/\//.test(s)) s = "ws://" + s;
  s = s.replace(/^http/, "ws");
  s = s.replace(/\/+$/, "");
  const u = new URL(s);
  if (!u.host) throw "Некорректный адрес";
  return `${u.protocol}//${u.host}`;
}

function httpBase(ws) {
  return ws.replace(/^ws/, "http");
}

function timeOf(ts) {
  const d = new Date(ts * 1000);
  return d.toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
}

function scrollDown() {
  const m = $("messages");
  m.scrollTop = m.scrollHeight;
}

function addLine(cls, text) {
  const d = document.createElement("div");
  d.className = cls;
  d.textContent = text;
  $("messages").appendChild(d);
  scrollDown();
}

function addMessage(id, from, text, ts) {
  if (id != null) {
    if (seen.has(id)) return;
    seen.add(id);
  }
  const mine = from === me;
  const d = document.createElement("div");
  d.className = "msg" + (mine ? " mine" : "");
  const who = document.createElement("div");
  who.className = "who";
  who.textContent = from;
  const body = document.createElement("div");
  body.textContent = text;
  const meta = document.createElement("div");
  meta.className = "meta";
  meta.textContent = timeOf(ts);
  d.append(who, body, meta);
  $("messages").appendChild(d);
  scrollDown();
}

function setStatus(ok, text) {
  const el = $("status");
  el.textContent = (ok ? "● " : "● ") + text;
  el.className = "status " + (ok ? "on" : "bad");
  $("send").disabled = !ok;
}

async function loadHistory() {
  try {
    const r = await fetch(httpBase(serverUrl) + "/history");
    const list = await r.json();
    for (const m of list) addMessage(m.id, m.from, m.text, m.ts);
  } catch {
    addLine("err-line", "Не удалось загрузить историю");
  }
}

function connect() {
  clearTimeout(reconnectTimer);
  if (ws) { manualClose = true; ws.close(); }
  manualClose = false;
  setStatus(false, "подключение…");
  const url = `${serverUrl}/ws?name=${encodeURIComponent(me)}`;
  const sock = new WebSocket(url);
  ws = sock;

  sock.onopen = () => {};
  sock.onmessage = (ev) => {
    let m;
    try { m = JSON.parse(ev.data); } catch { return; }
    switch (m.type) {
      case "welcome":
        setStatus(true, "в сети");
        $("sub").textContent = serverUrl.replace(/^ws:\/\//, "") + " · вы: " + me;
        break;
      case "text":
        addMessage(m.id, m.from, m.text, m.ts);
        break;
      case "system":
        addLine("sys", m.text);
        break;
      case "error":
        addLine("err-line", m.text);
        if (m.text.includes("занято") || m.text.includes("Некорректное")) {
          manualClose = true;
          sock.close();
          logout(m.text);
        }
        break;
    }
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    setStatus(false, "нет связи");
    if (!manualClose && !$("chat").classList.contains("hidden")) {
      reconnectTimer = setTimeout(connect, 3000);
    }
  };
  sock.onerror = () => {};
}

function showChat() {
  $("login").classList.add("hidden");
  $("chat").classList.remove("hidden");
  $("title").textContent = "Общий чат";
  $("input").focus();
}

function logout(errText = "") {
  manualClose = true;
  clearTimeout(reconnectTimer);
  if (ws) { const s = ws; ws = null; s.close(); }
  seen = new Set();
  $("messages").innerHTML = "";
  $("chat").classList.add("hidden");
  $("login").classList.remove("hidden");
  $("login-error").textContent = errText;
  setStatus(false, "офлайн");
}

async function enter() {
  $("login-error").textContent = "";
  let url;
  try {
    url = normalizeServer($("server").value);
  } catch (e) {
    $("login-error").textContent = String(e);
    return;
  }
  const name = $("name").value.trim();
  if (!name) {
    $("login-error").textContent = "Введите имя";
    return;
  }
  serverUrl = url;
  me = name;
  localStorage.setItem(LS, JSON.stringify({ server: $("server").value.trim(), name }));
  seen = new Set();
  $("messages").innerHTML = "";
  showChat();
  await loadHistory();
  connect();
}

function send() {
  const text = $("input").value.trim();
  if (!text || !ws || ws.readyState !== WebSocket.OPEN) return;
  ws.send(JSON.stringify({ type: "text", text }));
  $("input").value = "";
  $("input").style.height = "auto";
}

$("connect").onclick = enter;
$("name").addEventListener("keydown", (e) => { if (e.key === "Enter") enter(); });
$("server").addEventListener("keydown", (e) => { if (e.key === "Enter") $("name").focus(); });
$("form").addEventListener("submit", (e) => { e.preventDefault(); send(); });
$("input").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); }
});
$("input").addEventListener("input", (e) => {
  e.target.style.height = "auto";
  e.target.style.height = Math.min(e.target.scrollHeight, 140) + "px";
});
$("logout").onclick = () => logout();

// Восстановить последние адрес и имя.
try {
  const saved = JSON.parse(localStorage.getItem(LS) || "null");
  if (saved) {
    $("server").value = saved.server || "";
    $("name").value = saved.name || "";
  }
} catch {}
if (!$("server").value) $("server").value = "127.0.0.1:7878";
