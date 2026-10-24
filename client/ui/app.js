// Шторка — клиент 0.0.2. Аккаунты, настройки, уведомления, сообщения, файлы, записи, темы.
const $ = (id) => document.getElementById(id);
const LS = "shtorka.client";
const THEME_KEY = "shtorka.theme";
const NOTIF_KEY = "shtorka.notif";
const MAX_UPLOAD = 50 * 1024 * 1024;
const THEME_NAMES = { dark: "Тёмная", light: "Светлая", auto: "Авто" };

let ws = null;
let me = "";
let token = "";
let authMode = "login"; // login | register
let serverUrl = "";
let httpUrl = "";
let seen = new Set();
let reconnectTimer = null;
let manualClose = false;
let lastDay = "";
let lastFrom = "";

let rec = null;
let recKind = null;
let recChunks = [];
let recCancelled = false;
let recStart = 0;
let recTimer = null;
let camStream = null;

/* ---------- Темы ---------- */

function effectiveTheme(mode) {
  if (mode === "auto") {
    return matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }
  return mode;
}

function applyTheme(mode) {
  const eff = effectiveTheme(mode);
  document.documentElement.dataset.theme = eff;
  const name = THEME_NAMES[mode] || THEME_NAMES.dark;
  document.querySelectorAll(".theme-name").forEach((el) => (el.textContent = name));
  $("theme-btn").title = "Тема: " + name;
  $("theme-btn").textContent = eff === "light" ? "☀" : "☾";
}

function currentMode() {
  return localStorage.getItem(THEME_KEY) || "dark";
}

function cycleTheme() {
  const order = ["dark", "light", "auto"];
  const next = order[(order.indexOf(currentMode()) + 1) % order.length];
  localStorage.setItem(THEME_KEY, next);
  document.documentElement.classList.add("theme-anim");
  applyTheme(next);
  setTimeout(() => document.documentElement.classList.remove("theme-anim"), 400);
}

matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => {
  if (currentMode() === "auto") applyTheme("auto");
});

/* ---------- Вспомогательные ---------- */

function toast(text, isErr = false) {
  const el = $("toast");
  el.textContent = text;
  el.classList.toggle("err", isErr);
  el.classList.add("show");
  clearTimeout(el._t);
  el._t = setTimeout(() => el.classList.remove("show"), 2600);
}

function normalizeServer(raw) {
  let s = raw.trim();
  if (!s) throw "Введите адрес сервера";
  if (!/^(wss?|https?):\/\//.test(s)) s = "ws://" + s;
  s = s.replace(/^http/, "ws").replace(/\/+$/, "");
  const u = new URL(s);
  if (!u.host) throw "Некорректный адрес";
  return `${u.protocol}//${u.host}`;
}

function dayKeyOf(date) {
  return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
}

function dayLabel(date) {
  const now = new Date();
  const yest = new Date(now);
  yest.setDate(now.getDate() - 1);
  if (dayKeyOf(date) === dayKeyOf(now)) return "Сегодня";
  if (dayKeyOf(date) === dayKeyOf(yest)) return "Вчера";
  return date.toLocaleDateString("ru-RU", { day: "numeric", month: "long" });
}

function timeOf(date) {
  return date.toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
}

function fmtSize(n) {
  if (n < 1024) return n + " Б";
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " КБ";
  return (n / 1024 / 1024).toFixed(1) + " МБ";
}

function fmtDuration(ms) {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function hueOf(name) {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}

function initials(name) {
  return name.trim().split(/\s+/).map((w) => w[0]).join("").slice(0, 2).toUpperCase();
}

function scrollDown() {
  const m = $("messages");
  m.scrollTop = m.scrollHeight;
}

function addSystem(text, cls = "sys") {
  const d = document.createElement("div");
  d.className = cls + " anim";
  d.textContent = text;
  $("list").appendChild(d);
  lastFrom = "";
  scrollDown();
}

function kindTitle(kind) {
  return { image: "Фото", audio: "Голосовое", video: "Видео", file: "Файл" }[kind] || "Файл";
}

/* ---------- Сообщения ---------- */

function attachmentEl(a, isNote) {
  const url = `${httpUrl}/files/${encodeURIComponent(a.id)}`;
  if (a.kind === "image") {
    const img = new Image();
    img.src = url;
    img.alt = a.name;
    img.className = "media-img";
    img.loading = "lazy";
    img.onload = scrollDown;
    return img;
  }
  if (a.kind === "audio") {
    const w = document.createElement("div");
    w.className = "audio";
    const au = document.createElement("audio");
    au.controls = true;
    au.preload = "metadata";
    au.src = url;
    w.appendChild(au);
    return w;
  }
  if (a.kind === "video") {
    const v = document.createElement("video");
    v.src = url;
    v.preload = "metadata";
    v.playsInline = true;
    if (isNote) {
      v.className = "note";
      v.loop = true;
      v.addEventListener("click", () => (v.paused ? v.play() : v.pause()));
    } else {
      v.className = "media-video";
      v.controls = true;
    }
    v.onloadedmetadata = scrollDown;
    return v;
  }
  const link = document.createElement("a");
  link.className = "file";
  link.href = url;
  link.download = a.name;
  const ico = document.createElement("span");
  ico.className = "file-ico";
  ico.innerHTML = '<svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true"><path d="M6 3h8l4 4v14H6z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/><path d="M14 3v4h4" fill="none" stroke="currentColor" stroke-width="2"/></svg>';
  const meta = document.createElement("span");
  meta.className = "file-meta";
  const b = document.createElement("b");
  b.textContent = a.name;
  const small = document.createElement("small");
  small.textContent = fmtSize(a.size);
  meta.append(b, small);
  link.append(ico, meta);
  return link;
}

function renderMessage(m) {
  const date = new Date(m.ts * 1000);
  const mine = m.from === me;
  const key = dayKeyOf(date);
  if (key !== lastDay) {
    lastDay = key;
    const d = document.createElement("div");
    d.className = "day";
    d.textContent = dayLabel(date);
    $("list").appendChild(d);
    lastFrom = "";
  }

  const row = document.createElement("div");
  row.className = "row anim " + (mine ? "out" : "in");
  if (!mine) {
    const av = document.createElement("div");
    av.className = "avatar";
    av.style.setProperty("--h", hueOf(m.from));
    av.textContent = initials(m.from);
    row.appendChild(av);
  }

  const bubble = document.createElement("div");
  bubble.className = "bubble";
  if (!mine && lastFrom !== m.from) {
    const who = document.createElement("div");
    who.className = "who";
    who.textContent = m.from;
    bubble.appendChild(who);
  }
  if (m.attachment) {
    const isNote = m.attachment.name.startsWith("Видеокружок");
    bubble.appendChild(attachmentEl(m.attachment, isNote));
  }
  if (m.text) {
    const t = document.createElement("div");
    t.className = "text";
    t.textContent = m.text;
    bubble.appendChild(t);
  }
  const meta = document.createElement("div");
  meta.className = "meta";
  meta.textContent = timeOf(date);
  bubble.appendChild(meta);

  row.appendChild(bubble);
  $("list").appendChild(row);
  lastFrom = m.from;

  const preview = m.attachment ? `[${kindTitle(m.attachment.kind)}] ${m.text}`.trim() : m.text;
  $("ci-last").textContent = (mine ? "Вы: " : m.from + ": ") + preview;
  $("ci-time").textContent = timeOf(date);
  scrollDown();
}

function addMessage(m) {
  if (m.id != null) {
    if (seen.has(m.id)) return;
    seen.add(m.id);
  }
  renderMessage(m);
}

function setStatus(ok, text) {
  const el = $("status");
  el.textContent = text;
  el.className = "status " + (ok ? "on" : "off");
  $("send").disabled = !ok;
}

async function loadHistory() {
  try {
    const r = await fetch(`${httpUrl}/history?token=${encodeURIComponent(token)}`);
    if (!r.ok) throw new Error(String(r.status));
    const list = await r.json();
    for (const m of list) addMessage(m);
  } catch {
    addSystem("Не удалось загрузить историю", "errline");
  }
}

/* ---------- Уведомления ---------- */

function notifsEnabled() {
  return localStorage.getItem(NOTIF_KEY) === "1";
}

async function enableNotifs() {
  if (!("Notification" in window)) {
    toast("Уведомления не поддерживаются", true);
    return false;
  }
  if (Notification.permission === "granted") return true;
  if (Notification.permission === "denied") {
    toast("Уведомления запрещены в настройках системы", true);
    return false;
  }
  return (await Notification.requestPermission()) === "granted";
}

function notifyNew(m) {
  if (!notifsEnabled() || !("Notification" in window) || Notification.permission !== "granted") return;
  if (!document.hidden && document.hasFocus()) return;
  if (m.from === me) return;
  const preview = m.attachment ? `[${kindTitle(m.attachment.kind)}] ${m.text}`.trim() : m.text;
  try {
    const n = new Notification(m.from, { body: preview || "Новое сообщение", tag: "shtorka-" + m.id });
    n.onclick = () => window.focus();
  } catch (e) { /* вебвью может не дать — не страшно */ }
}

/* ---------- Соединение ---------- */

function connect() {
  clearTimeout(reconnectTimer);
  if (ws) {
    manualClose = true;
    ws.close();
  }
  manualClose = false;
  setStatus(false, "подключение…");
  const sock = new WebSocket(`${serverUrl}/ws?name=${encodeURIComponent(me)}&token=${encodeURIComponent(token)}`);
  ws = sock;

  sock.onmessage = (ev) => {
    let m;
    try {
      m = JSON.parse(ev.data);
    } catch {
      return;
    }
    switch (m.type) {
      case "welcome":
        setStatus(true, "в сети");
        $("sub").textContent = serverUrl.replace(/^ws:\/\//, "") + " · " + me;
        break;
      case "text":
        addMessage(m);
        notifyNew(m);
        break;
      case "system":
        addSystem(m.text);
        break;
      case "kick":
        manualClose = true;
        sock.close();
        logout(m.text);
        toast(m.text, true);
        break;
      case "error":
        addSystem(m.text, "errline");
        if (m.text.includes("авториз")) {
          manualClose = true;
          sock.close();
          logout("Сессия устарела. Войдите заново.");
        }
        break;
    }
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    setStatus(false, "нет связи");
    if (!manualClose && !$("app").classList.contains("hidden")) {
      reconnectTimer = setTimeout(connect, 3000);
    }
  };
}

function showApp() {
  $("login").classList.add("hidden");
  $("app").classList.remove("hidden");
  $("me-label").textContent = "Вы: " + me;
  $("input").focus();
}

function logout(errText = "") {
  manualClose = true;
  clearTimeout(reconnectTimer);
  if (ws) {
    const s = ws;
    ws = null;
    s.close();
  }
  stopRecording(true);
  seen = new Set();
  lastDay = "";
  lastFrom = "";
  token = "";
  $("list").innerHTML = "";
  $("ci-last").textContent = "Пока пусто";
  $("ci-time").textContent = "";
  $("app").classList.add("hidden");
  $("settings").classList.add("hidden");
  $("login").classList.remove("hidden");
  $("password").value = "";
  $("login-error").textContent = errText;
  setStatus(false, "офлайн");
  const saved = JSON.parse(localStorage.getItem(LS) || "null");
  if (saved) localStorage.setItem(LS, JSON.stringify({ server: saved.server || "", name: saved.name || "" }));
}

/* ---------- Настройки ---------- */

function openSettings() {
  $("set-name").textContent = me;
  $("set-server").textContent = serverUrl.replace(/^wss?:\/\//, "");
  $("pw-old").value = "";
  $("pw-new").value = "";
  $("pw-msg").textContent = "";
  $("pw-msg").classList.remove("err");
  $("notif-toggle").checked = notifsEnabled() && "Notification" in window && Notification.permission === "granted";
  $("settings").classList.remove("hidden");
}

function closeSettings() {
  $("settings").classList.add("hidden");
}

async function changePassword() {
  const oldPw = $("pw-old").value;
  const newPw = $("pw-new").value;
  const msg = $("pw-msg");
  msg.classList.remove("err");
  if (!oldPw || !newPw) {
    msg.textContent = "Заполните оба поля";
    msg.classList.add("err");
    return;
  }
  try {
    const r = await fetch(`${httpUrl}/password`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token, old: oldPw, new: newPw }),
    });
    const text = await r.text();
    if (!r.ok) {
      msg.textContent = text || "Ошибка " + r.status;
      msg.classList.add("err");
      return;
    }
    msg.textContent = "Пароль изменён";
    $("pw-old").value = "";
    $("pw-new").value = "";
  } catch {
    msg.textContent = "Сервер не отвечает";
    msg.classList.add("err");
  }
}

function saveSession() {
  localStorage.setItem(LS, JSON.stringify({ server: $("server").value.trim(), name: me, token }));
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
  const password = $("password").value;
  if (!name) {
    $("login-error").textContent = "Введите имя";
    return;
  }
  if (!password) {
    $("login-error").textContent = "Введите пароль";
    return;
  }

  const http = url.replace(/^ws/, "http");
  $("connect").disabled = true;
  try {
    const r = await fetch(`${http}/${authMode}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name, password }),
    });
    if (!r.ok) {
      const text = await r.text();
      $("login-error").textContent = text || "Ошибка " + r.status;
      return;
    }
    const auth = await r.json();
    serverUrl = url;
    httpUrl = http;
    me = auth.name;
    token = auth.token;
    saveSession();
  } catch (e) {
    $("login-error").textContent = "Сервер не отвечает";
    return;
  } finally {
    $("connect").disabled = false;
  }

  seen = new Set();
  lastDay = "";
  lastFrom = "";
  $("list").innerHTML = "";
  showApp();
  await loadHistory();
  connect();
}

/* ---------- Отправка ---------- */

function sendPayload(payload) {
  if (!ws || ws.readyState !== WebSocket.OPEN) {
    toast("Нет подключения к серверу", true);
    return false;
  }
  ws.send(JSON.stringify({ type: "text", ...payload }));
  return true;
}

function sendText() {
  const text = $("input").value.trim();
  if (!text) return;
  if (sendPayload({ text })) {
    $("input").value = "";
    $("input").style.height = "auto";
  }
}

function baseMime(type) {
  return (type || "application/octet-stream").split(";")[0].trim();
}

function setBusy(on) {
  $("busy").classList.toggle("hidden", !on);
}

async function upload(blob, name, mime) {
  if (blob.size > MAX_UPLOAD) throw new Error("Файл больше 50 МБ");
  setBusy(true);
  try {
    const url = `${httpUrl}/upload?token=${encodeURIComponent(token)}&name=${encodeURIComponent(name)}&mime=${encodeURIComponent(baseMime(mime))}`;
    const r = await fetch(url, { method: "POST", body: blob });
    if (!r.ok) throw new Error(await r.text());
    return await r.json();
  } finally {
    setBusy(false);
  }
}

async function sendFile(file, caption = "") {
  try {
    const att = await upload(file, file.name || "file", file.type);
    return sendPayload({ text: caption, attachment_id: att.id });
  } catch (e) {
    toast(String(e.message || e), true);
    return false;
  }
}

/* ---------- Запись голосовых и видео ---------- */

function pickMime(kind) {
  const list =
    kind === "audio"
      ? ["audio/webm;codecs=opus", "audio/webm", "audio/ogg;codecs=opus", "audio/mp4"]
      : ["video/webm;codecs=vp8,opus", "video/webm", "video/mp4"];
  return list.find((t) => window.MediaRecorder && MediaRecorder.isTypeSupported(t)) || "";
}

function stopTracks() {
  if (camStream) camStream.getTracks().forEach((t) => t.stop());
  camStream = null;
}

async function startRecording(kind) {
  if (rec) return;
  if (!ws || ws.readyState !== WebSocket.OPEN) {
    toast("Нет подключения к серверу", true);
    return;
  }
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia(
      kind === "audio"
        ? { audio: true }
        : { audio: true, video: { width: 360, height: 360, facingMode: "user" } }
    );
  } catch (e) {
    toast("Нет доступа к " + (kind === "audio" ? "микрофону" : "камере") + ": " + e.message, true);
    return;
  }
  camStream = stream;
  recKind = kind;
  recChunks = [];
  recCancelled = false;
  const mime = pickMime(kind);
  try {
    rec = new MediaRecorder(stream, mime ? { mimeType: mime } : undefined);
  } catch (e) {
    toast("Запись не поддерживается: " + e.message, true);
    stopTracks();
    return;
  }
  rec.ondataavailable = (e) => {
    if (e.data && e.data.size) recChunks.push(e.data);
  };
  rec.onstop = onRecStop;
  rec.start(250);
  recStart = Date.now();
  $("rec-time").textContent = "0:00";
  recTimer = setInterval(() => ($("rec-time").textContent = fmtDuration(Date.now() - recStart)), 250);
  $("rec-label").textContent = kind === "audio" ? "Голосовое" : "Видео-кружок";
  $("rec-bar").classList.remove("hidden");
  $("voice").classList.toggle("rec", kind === "audio");
  $("note").classList.toggle("rec", kind === "video");
  if (kind === "video") {
    const pv = $("note-preview");
    pv.srcObject = stream;
    pv.classList.remove("hidden");
    pv.play().catch(() => {});
  }
}

function stopRecording(cancel) {
  if (!rec) return;
  recCancelled = cancel;
  if (rec.state !== "inactive") rec.stop();
}

function onRecStop() {
  const kind = recKind;
  const type = baseMime(rec.mimeType) || (kind === "audio" ? "audio/webm" : "video/webm");
  const blob = new Blob(recChunks, { type });
  const cancelled = recCancelled;
  cleanupRecording();
  if (cancelled || blob.size === 0) return;
  const name = kind === "audio" ? "Голосовое.webm" : "Видеокружок.webm";
  upload(blob, name, type)
    .then((att) => sendPayload({ text: "", attachment_id: att.id }))
    .catch((e) => toast(String(e.message || e), true));
}

function cleanupRecording() {
  clearInterval(recTimer);
  recTimer = null;
  stopTracks();
  rec = null;
  recKind = null;
  const pv = $("note-preview");
  pv.classList.add("hidden");
  pv.srcObject = null;
  $("rec-bar").classList.add("hidden");
  $("voice").classList.remove("rec");
  $("note").classList.remove("rec");
}

function toggleRecording(kind) {
  if (rec && recKind === kind) {
    stopRecording(false);
  } else if (rec) {
    toast("Сначала завершите текущую запись", true);
  } else {
    startRecording(kind);
  }
}

/* ---------- Обработчики ---------- */

$("connect").onclick = enter;
$("name").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("password").focus();
});
$("password").addEventListener("keydown", (e) => {
  if (e.key === "Enter") enter();
});
$("server").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("name").focus();
});
$("mode-toggle").onclick = () => {
  authMode = authMode === "login" ? "register" : "login";
  $("connect").textContent = authMode === "login" ? "Войти" : "Зарегистрироваться";
  $("mode-toggle").innerHTML =
    authMode === "login"
      ? 'Нет аккаунта? <b>Зарегистрироваться</b>'
      : 'Уже есть аккаунт? <b>Войти</b>';
  $("login-error").textContent = "";
};
$("settings-btn").onclick = openSettings;
$("settings-close").onclick = closeSettings;
$("settings").addEventListener("click", (e) => {
  if (e.target === $("settings")) closeSettings();
});
$("pw-save").onclick = changePassword;
$("settings-logout").onclick = () => logout();
$("notif-toggle").addEventListener("change", async (e) => {
  if (e.target.checked) {
    const ok = await enableNotifs();
    e.target.checked = ok;
    localStorage.setItem(NOTIF_KEY, ok ? "1" : "0");
    if (ok) toast("Уведомления включены");
  } else {
    localStorage.setItem(NOTIF_KEY, "0");
  }
});
$("form").addEventListener("submit", (e) => {
  e.preventDefault();
  sendText();
});
$("input").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    sendText();
  }
});
$("input").addEventListener("input", (e) => {
  e.target.style.height = "auto";
  e.target.style.height = Math.min(e.target.scrollHeight, 140) + "px";
});
$("attach").onclick = () => $("file-input").click();
$("file-input").addEventListener("change", async (e) => {
  const files = Array.from(e.target.files || []);
  e.target.value = "";
  const caption = $("input").value.trim();
  for (const f of files) {
    const ok = await sendFile(f, caption);
    if (ok) $("input").value = "";
  }
});
$("voice").onclick = () => toggleRecording("audio");
$("note").onclick = () => toggleRecording("video");
$("rec-send").onclick = () => stopRecording(false);
$("rec-cancel").onclick = () => stopRecording(true);
$("logout").onclick = () => logout();
$("logout-top").onclick = () => logout();
$("theme-btn").onclick = cycleTheme;
$("login-theme").onclick = cycleTheme;

$("input").addEventListener("paste", (e) => {
  const item = Array.from((e.clipboardData && e.clipboardData.items) || []).find((i) => i.type.startsWith("image/"));
  if (!item) return;
  e.preventDefault();
  const file = item.getAsFile();
  if (!file) return;
  const ext = (file.type.split("/")[1] || "png");
  sendFile(new File([file], "Изображение." + ext, { type: file.type }), $("input").value.trim()).then((ok) => {
    if (ok) $("input").value = "";
  });
});

applyTheme(currentMode());
(async () => {
  let saved = null;
  try {
    saved = JSON.parse(localStorage.getItem(LS) || "null");
  } catch (e) {}
  if (saved) {
    $("server").value = saved.server || "";
    $("name").value = saved.name || "";
    // Автовход по сохранённой сессии: проверяем токен через /account.
    if (saved.token && saved.server && saved.name) {
      try {
        const http = normalizeServer(saved.server).replace(/^ws/, "http");
        const r = await fetch(`${http}/account?token=${encodeURIComponent(saved.token)}`);
        if (r.ok) {
          const info = await r.json();
          serverUrl = normalizeServer(saved.server);
          httpUrl = http;
          me = info.name;
          token = saved.token;
          showApp();
          await loadHistory();
          connect();
          return;
        }
      } catch (e) { /* сервер недоступен — покажем форму входа */ }
    }
  }
  if (!$("server").value) $("server").value = "127.0.0.1:7878";
  $("password").focus();
})();