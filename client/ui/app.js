// Шторка 0.1 — клиент: профиль, аватарки, личные сообщения, поиск, настройки.
"use strict";

const $ = (id) => document.getElementById(id);
const SESSION_KEY = "shtorka.session";
const SETTINGS_KEY = "shtorka.settings";
const RECENT_KEY = "shtorka.recent";
const MAX_UPLOAD = 50 * 1024 * 1024;
const MAX_AVATAR = 5 * 1024 * 1024;
const MAX_WALL = 3.5 * 1024 * 1024;
const THEME_NAMES = { dark: "Тёмная", light: "Светлая", auto: "Авто" };

const DEFAULT_SETTINGS = {
  theme: "dark",
  accent: "blue",
  wallpaper: "aurora",
  wallpaperData: "",
  font: "normal",
  notif: false,
  sound: true,
};

let settings = loadSettings();
let state = freshState();
let authMode = "login";
let presenceTimer = null;
let searchTimer = null;
let openSeq = 0;

/* Запись голосовых/видео */
let rec = null;
let recKind = null;
let recChunks = [];
let recCancelled = false;
let recStart = 0;
let recTimer = null;
let camStream = null;

/* ---------- Состояние и хранилища ---------- */

function freshState() {
  return {
    server: "",
    http: "",
    token: "",
    me: null,
    ws: null,
    online: false,
    reconnectTimer: null,
    manualClose: false,
    peer: null,
    history: new Map(),
    peers: new Map(),
    unread: new Map(),
    users: new Map(),
    seen: new Set(),
    searchResults: [],
  };
}

function loadSettings() {
  try {
    return Object.assign({}, DEFAULT_SETTINGS, JSON.parse(localStorage.getItem(SETTINGS_KEY) || "{}"));
  } catch {
    return Object.assign({}, DEFAULT_SETTINGS);
  }
}

function saveSettings() {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
  } catch {
    toast("Не удалось сохранить настройки (мало места)", true);
  }
}

function loadSession() {
  try {
    const s = JSON.parse(localStorage.getItem(SESSION_KEY) || "null");
    return s && s.server && s.username && s.token ? s : null;
  } catch {
    return null;
  }
}

function saveSession() {
  if (!state.me) return;
  localStorage.setItem(
    SESSION_KEY,
    JSON.stringify({ server: state.server, username: state.me.username, token: state.token })
  );
}

function clearSession() {
  localStorage.removeItem(SESSION_KEY);
  localStorage.removeItem(RECENT_KEY);
}

function loadRecent() {
  try {
    const list = JSON.parse(localStorage.getItem(RECENT_KEY) || "[]");
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

function saveRecent() {
  const list = [...state.peers.values()]
    .filter((p) => p.ts)
    .sort((a, b) => b.ts - a.ts)
    .slice(0, 30)
    .map((p) => ({ username: p.username, ts: p.ts, preview: p.preview || "" }));
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(list));
  } catch {}
}

/* ---------- Тема и оформление ---------- */

function effectiveTheme(mode) {
  if (mode === "auto") {
    return matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }
  return mode;
}

function applySettings() {
  const root = document.documentElement;
  root.dataset.theme = effectiveTheme(settings.theme);
  root.dataset.accent = settings.accent || "blue";
  root.dataset.wallpaper = settings.wallpaper || "aurora";
  root.dataset.font = settings.font || "normal";
  if (settings.wallpaper === "custom" && settings.wallpaperData) {
    root.style.setProperty("--custom-wall", `url("${settings.wallpaperData}")`);
  }
  const name = THEME_NAMES[settings.theme] || THEME_NAMES.dark;
  document.querySelectorAll(".theme-name").forEach((el) => (el.textContent = name));
  $("theme-btn").title = `Тема: ${name}`;
  document.querySelectorAll("[data-theme-choice]").forEach((b) => {
    b.classList.toggle("active", b.dataset.themeChoice === settings.theme);
  });
  document.querySelectorAll("[data-accent]").forEach((b) => {
    b.classList.toggle("active", b.dataset.accent === settings.accent);
  });
  document.querySelectorAll("[data-wallpaper]").forEach((b) => {
    b.classList.toggle("active", b.dataset.wallpaper === settings.wallpaper);
  });
  document.querySelectorAll("[data-font]").forEach((b) => {
    b.classList.toggle("active", b.dataset.font === settings.font);
  });
  $("notif-toggle").checked = !!settings.notif;
  $("sound-toggle").checked = !!settings.sound;
}

function setTheme(mode) {
  settings.theme = mode;
  saveSettings();
  applySettings();
}

function cycleTheme() {
  const order = ["dark", "light", "auto"];
  setTheme(order[(order.indexOf(settings.theme) + 1) % order.length]);
}

matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => {
  if (settings.theme === "auto") applySettings();
});

/* ---------- Вспомогательные ---------- */

function toast(text, isErr = false) {
  const el = $("toast");
  el.textContent = text;
  el.classList.toggle("err", isErr);
  el.classList.add("show");
  clearTimeout(el._t);
  el._t = setTimeout(() => el.classList.remove("show"), 2800);
}

function normalizeServer(raw) {
  let s = String(raw || "").trim();
  if (!s) throw new Error("Введите адрес сервера");
  if (!/^(wss?|https?):\/\//.test(s)) s = "ws://" + s;
  s = s.replace(/^http/, "ws").replace(/\/+$/, "");
  const u = new URL(s);
  if (!u.host) throw new Error("Некорректный адрес");
  return `${u.protocol}//${u.host}`;
}

function fileUrl(id) {
  return `${state.http}/files/${encodeURIComponent(id)}?token=${encodeURIComponent(state.token)}`;
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
  for (const c of name || "?") h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}

function initials(name) {
  const parts = String(name || "?").trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return "?";
  const s = parts.length > 1 ? parts[0][0] + parts[1][0] : parts[0].slice(0, 2);
  return s.toUpperCase();
}

function shortHost() {
  return state.server.replace(/^wss?:\/\//, "");
}

function scrollDown(force = false) {
  const m = $("messages");
  const near = m.scrollHeight - m.scrollTop - m.clientHeight < 160;
  if (force || near) m.scrollTop = m.scrollHeight;
}

function kindTitle(kind) {
  return { image: "Фото", audio: "Голосовое", video: "Видео", file: "Файл" }[kind] || "Файл";
}

function previewOf(m) {
  const att = m.attachment ? `[${kindTitle(m.attachment.kind)}] ` : "";
  return (att + (m.text || "")).trim() || "Вложение";
}

function messageKey(m) {
  if (!m.to) return "";
  return m.from === state.me.username ? m.to : m.from;
}

/* ---------- Аватарки ---------- */

function fillAvatar(el, name) {
  el.textContent = initials(name);
  el.style.setProperty("--h", hueOf(name));
}

function makeAvatar(card, cls = "") {
  const el = document.createElement("div");
  el.className = "avatar" + (cls ? " " + cls : "");
  const name = (card && (card.display_name || card.username)) || "?";
  if (card && card.avatar) {
    const img = document.createElement("img");
    img.alt = "";
    img.src = fileUrl(card.avatar);
    img.onerror = () => {
      img.remove();
      fillAvatar(el, name);
    };
    el.appendChild(img);
  } else {
    fillAvatar(el, name);
  }
  return el;
}

function applyAvatarTo(el, card, name) {
  el.innerHTML = "";
  el.className = "avatar";
  const label = name || (card && (card.display_name || card.username)) || "?";
  if (card && card.avatar) {
    const img = document.createElement("img");
    img.alt = "";
    img.src = fileUrl(card.avatar);
    img.onerror = () => {
      img.remove();
      fillAvatar(el, label);
    };
    el.appendChild(img);
  } else {
    fillAvatar(el, label);
  }
}

/* ---------- Пользователи и чаты ---------- */

function userCard(username) {
  return (
    state.users.get(username) ||
    (state.peers.get(username) && state.peers.get(username).user) || {
      username,
      display_name: username,
      avatar: null,
      online: false,
    }
  );
}

function ensurePeer(username) {
  if (!state.peers.has(username)) {
    const card = userCard(username);
    state.peers.set(username, {
      username,
      user: card,
      ts: 0,
      preview: "",
    });
  }
  return state.peers.get(username);
}

function onlineCount() {
  let n = state.online ? 1 : 0;
  for (const [name, u] of state.users) {
    if (name !== (state.me && state.me.username) && u.online) n++;
  }
  return n;
}

async function refreshUsers() {
  if (!state.token) return;
  try {
    const r = await fetch(`${state.http}/users?token=${encodeURIComponent(state.token)}&q=`);
    if (!r.ok) return;
    const list = await r.json();
    for (const u of list) {
      state.users.set(u.username, u);
      const p = state.peers.get(u.username);
      if (p) p.user = u;
    }
    renderChats();
    updateChatHeader();
  } catch {}
}

/* ---------- Сессия ---------- */

function showApp() {
  $("login").classList.add("hidden");
  $("app").classList.remove("hidden");
  updateMeUi();
}

function updateMeUi() {
  if (!state.me) return;
  $("me-name").textContent = state.me.display_name || state.me.username;
  $("me-user").textContent = "@" + state.me.username;
  applyAvatarTo($("me-avatar"), state.me);
}

async function startSession(me, serverUrl) {
  state = freshState();
  state.me = me;
  state.token = me.token;
  state.server = normalizeServer(serverUrl);
  state.http = state.server.replace(/^ws/, "http");
  saveSession();

  $("password").value = "";
  $("login-error").textContent = "";
  showApp();
  renderChats();

  // Восстанавливаем недавние переписки (без сети — из localStorage).
  for (const item of loadRecent()) {
    if (!item.username) continue;
    const p = ensurePeer(item.username);
    p.ts = item.ts || 0;
    p.preview = item.preview || "";
  }
  renderChats();

  await refreshUsers();
  await openChat(null, true);
  connect();

  clearInterval(presenceTimer);
  presenceTimer = setInterval(() => {
    if (state.token) refreshUsers();
  }, 25000);
}

async function enter() {
  $("login-error").textContent = "";
  let url;
  try {
    url = normalizeServer($("server").value);
  } catch (e) {
    $("login-error").textContent = e.message;
    return;
  }
  const username = $("username").value.trim();
  const password = $("password").value;
  if (!username) {
    $("login-error").textContent = "Введите юзернейм";
    return;
  }
  if (authMode === "register" && !/^[a-zA-Z0-9_]{3,20}$/.test(username)) {
    $("login-error").textContent = "Юзернейм: 3–20 символов, латиница, цифры и _";
    return;
  }
  if (authMode === "register" && password.length < 6) {
    $("login-error").textContent = "Пароль должен быть не короче 6 символов";
    return;
  }
  if (!password) {
    $("login-error").textContent = "Введите пароль";
    return;
  }

  const http = url.replace(/^ws/, "http");
  const btn = $("connect");
  btn.disabled = true;
  try {
    const r = await fetch(`${http}/${authMode}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        username,
        password,
        display_name: authMode === "register" ? $("display").value.trim() : "",
      }),
    });
    if (!r.ok) {
      $("login-error").textContent = (await r.text()) || "Ошибка " + r.status;
      return;
    }
    const auth = await r.json();
    await startSession(auth, url);
  } catch {
    $("login-error").textContent = "Сервер не отвечает";
  } finally {
    btn.disabled = false;
  }
}

function logout(errText = "") {
  state.manualClose = true;
  clearTimeout(state.reconnectTimer);
  clearInterval(presenceTimer);
  const token = state.token;
  if (state.ws) {
    try {
      state.ws.close();
    } catch {}
  }
  if (token) {
    fetch(`${state.http}/logout`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token }),
    }).catch(() => {});
  }
  stopRecording(true);
  clearSession();
  state = freshState();
  $("app").classList.add("hidden");
  $("settings").classList.add("hidden");
  $("login").classList.remove("hidden");
  $("list").innerHTML = "";
  $("password").value = "";
  $("login-error").textContent = errText;
  setStatus(false, "офлайн");
  document.title = "Шторка";
  renderChats();
  updateChatHeader();
  $("username").focus();
}

/* ---------- Соединение ---------- */

function connect() {
  clearTimeout(state.reconnectTimer);
  if (state.ws) {
    state.manualClose = true;
    try {
      state.ws.close();
    } catch {}
  }
  if (!state.token) return;
  state.manualClose = false;
  setStatus(false, "подключение…");

  const sock = new WebSocket(
    `${state.server}/ws?username=${encodeURIComponent(state.me.username)}&token=${encodeURIComponent(state.token)}`
  );
  state.ws = sock;

  sock.onopen = () => {
    if (sock !== state.ws) return;
    state.online = true;
    setStatus(true, "в сети");
    updateChatHeader();
  };

  sock.onmessage = (ev) => {
    let m;
    try {
      m = JSON.parse(ev.data);
    } catch {
      return;
    }
    switch (m.type) {
      case "welcome":
        if (m.username) state.me.username = m.username;
        if (m.display_name) state.me.display_name = m.display_name;
        updateMeUi();
        state.online = true;
        setStatus(true, "в сети");
        updateChatHeader();
        break;
      case "text":
        receiveMessage(m);
        break;
      case "system":
        if (state.peer === null) addSystem(m.text);
        break;
      case "kick":
        state.manualClose = true;
        try {
          sock.close();
        } catch {}
        logout(m.text || "Сессия закрыта");
        toast(m.text || "Выполнен вход с другого устройства", true);
        break;
      case "error":
        if (String(m.text || "").includes("авториз")) {
          state.manualClose = true;
          try {
            sock.close();
          } catch {}
          logout("Сессия устарела. Войдите заново.");
          return;
        }
        toast(m.text, true);
        addSystem(m.text, "errline");
        break;
    }
  };

  sock.onclose = () => {
    if (state.ws !== sock) return;
    state.online = false;
    setStatus(false, "нет связи");
    updateChatHeader();
    if (!state.manualClose && state.token && !$("app").classList.contains("hidden")) {
      state.reconnectTimer = setTimeout(connect, 3000);
    }
  };
}

function setStatus(ok, text) {
  const el = $("status");
  el.textContent = text;
  el.className = "status " + (ok ? "on" : "off");
  $("send").disabled = !ok;
  $("voice").disabled = !ok;
  $("note").disabled = !ok;
  $("attach").disabled = !ok;
}

function receiveMessage(m) {
  if (m.id != null) {
    if (state.seen.has(m.id)) return;
    state.seen.add(m.id);
  }
  const key = messageKey(m);
  if (!state.history.has(key)) state.history.set(key, []);
  state.history.get(key).push(m);

  if (key) {
    const p = ensurePeer(key);
    p.ts = m.ts;
    p.preview = (m.from === state.me.username ? "Вы: " : "") + previewOf(m);
    p.user = p.user || userCard(key);
    if (m.from !== state.me.username) {
      const known = state.users.get(m.from) || {};
      const card = Object.assign({}, known, {
        username: m.from,
        display_name: m.from_name || known.display_name || m.from,
        avatar: m.from_avatar || known.avatar || null,
        online: true,
      });
      state.users.set(m.from, card);
      p.user = card;
    }
  }

  saveRecent();

  if (key === (state.peer || "")) {
    renderMessageToDom(m, true);
    markRead(key);
  } else {
    state.unread.set(key, (state.unread.get(key) || 0) + 1);
  }
  renderChats();
  refreshTitle();
  maybeNotify(m, key);
}

function currentKey() {
  return state.peer || "";
}

/* ---------- История и рендер ---------- */

async function fetchHistory(peer) {
  const q = peer ? `&peer=${encodeURIComponent(peer)}` : "";
  const r = await fetch(`${state.http}/history?token=${encodeURIComponent(state.token)}${q}`);
  if (!r.ok) throw new Error((await r.text()) || String(r.status));
  return await r.json();
}

async function openChat(peer, force = false) {
  state.peer = peer || null;
  const key = currentKey();
  const seq = ++openSeq;
  closeMobileSide();
  clearSearch();

  renderChats();
  updateChatHeader();
  markRead(key);

  if (!state.history.has(key) || force) {
    try {
      const list = await fetchHistory(peer);
      if (seq !== openSeq) return;
      const existing = state.history.get(key) || [];
      const merged = mergeMessages(list, existing);
      state.history.set(key, merged);
      for (const m of merged) if (m.id != null) state.seen.add(m.id);
    } catch (e) {
      if (seq === openSeq) {
        addSystem("Не удалось загрузить историю: " + (e.message || e), "errline");
      }
      return;
    }
  }

  renderMessages();
  markRead(key);
  renderChats();
  updateChatHeader();
  $("input").focus();
  scrollDown(true);
}

function mergeMessages(a, b) {
  const map = new Map();
  for (const m of a) map.set(m.id, m);
  for (const m of b) if (!map.has(m.id)) map.set(m.id, m);
  return [...map.values()].sort((x, y) => x.id - y.id);
}

function markRead(key) {
  if (state.unread.get(key)) {
    state.unread.delete(key);
    refreshTitle();
  }
}

function refreshTitle() {
  let total = 0;
  for (const n of state.unread.values()) total += n;
  document.title = total ? `(${total}) Шторка` : "Шторка";
}

function addSystem(text, cls = "sys") {
  const list = $("list");
  const empty = list.querySelector(".empty-note");
  if (empty) empty.remove();
  const d = document.createElement("div");
  d.className = cls + " anim";
  d.textContent = text;
  list.appendChild(d);
  scrollDown();
}

function renderMessages() {
  const list = $("list");
  list.innerHTML = "";
  const msgs = state.history.get(currentKey()) || [];
  if (!msgs.length) {
    const empty = document.createElement("div");
    empty.className = "empty-note";
    if (state.peer) {
      const card = userCard(state.peer);
      empty.innerHTML = "";
      const b = document.createElement("b");
      b.textContent = card.display_name || state.peer;
      empty.appendChild(b);
      empty.appendChild(document.createTextNode("Начните переписку — сообщения видны только вам двоим."));
    } else {
      const b = document.createElement("b");
      b.textContent = "Пока тихо";
      empty.appendChild(b);
      empty.appendChild(document.createTextNode("Напишите первое сообщение в общий чат."));
    }
    list.appendChild(empty);
    return;
  }
  let lastDay = "";
  let lastFrom = "";
  for (const m of msgs) {
    lastDay = renderMessageToDom(m, false, lastDay, lastFrom).lastDay;
    lastFrom = m.from;
  }
}

function renderMessageToDom(m, append, lastDayInit = "", lastFromInit = "") {
  const list = $("list");
  const empty = list.querySelector(".empty-note");
  if (empty) empty.remove();

  const date = new Date(m.ts * 1000);
  const mine = m.from === state.me.username;
  const key = dayKeyOf(date);
  let lastDay = lastDayInit || "";
  let lastFrom = lastFromInit || "";

  if (append) {
    // Для дозаписи берём состояние из последнего сообщения в списке.
    const rows = list.querySelectorAll(".row");
    const last = rows[rows.length - 1];
    if (last && last.dataset.day) lastDay = last.dataset.day;
    if (last && last.dataset.from) lastFrom = last.dataset.from;
  }

  if (key !== lastDay) {
    const d = document.createElement("div");
    d.className = "day";
    d.textContent = dayLabel(date);
    list.appendChild(d);
    lastDay = key;
    lastFrom = "";
  }

  const row = document.createElement("div");
  row.className = "row anim " + (mine ? "out" : "in");
  row.dataset.day = key;
  row.dataset.from = m.from;

  if (!mine) {
    const card = state.users.get(m.from) || {
      username: m.from,
      display_name: m.from_name || m.from,
      avatar: m.from_avatar || null,
    };
    row.appendChild(makeAvatar(card));
  }

  const body = document.createElement("div");
  body.className = "row-body";
  const bubble = document.createElement("div");
  bubble.className = "bubble";

  if (!mine && lastFrom !== m.from) {
    const who = document.createElement("div");
    who.className = "who";
    who.textContent = m.from_name || m.from;
    bubble.appendChild(who);
  }
  if (m.attachment) bubble.appendChild(attachmentEl(m.attachment));
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
  body.appendChild(bubble);
  row.appendChild(body);
  list.appendChild(row);

  if (append) scrollDown();
  return { lastDay, lastFrom: m.from };
}

function attachmentEl(a) {
  const url = fileUrl(a.id);
  if (a.kind === "image") {
    const img = new Image();
    img.src = url;
    img.alt = a.name;
    img.className = "media-img";
    img.loading = "lazy";
    img.onclick = () => openLightbox(url);
    img.onload = () => scrollDown();
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
    if ((a.name || "").startsWith("Видеокружок")) {
      v.className = "note";
      v.loop = true;
      v.addEventListener("click", () => (v.paused ? v.play() : v.pause()));
    } else {
      v.className = "media-video";
      v.controls = true;
    }
    v.onloadedmetadata = () => scrollDown();
    return v;
  }
  const link = document.createElement("a");
  link.className = "file";
  link.href = url;
  link.download = a.name;
  const ico = document.createElement("span");
  ico.className = "file-ico";
  ico.innerHTML =
    '<svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true"><path d="M6 3h8l4 4v14H6z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/><path d="M14 3v4h4" fill="none" stroke="currentColor" stroke-width="2"/></svg>';
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

function openLightbox(src) {
  $("lightbox-img").src = src;
  $("lightbox").classList.remove("hidden");
}

/* ---------- Список чатов ---------- */

function renderChats() {
  const list = $("chat-list");
  if (!list) return;
  list.innerHTML = "";

  const general = document.createElement("button");
  general.className = "chat-item" + (state.peer === null ? " active" : "");
  const gAvatar = document.createElement("div");
  gAvatar.className = "avatar";
  gAvatar.style.setProperty("--h", 210);
  gAvatar.innerHTML =
    '<svg viewBox="0 0 32 32" width="24" height="24" aria-hidden="true"><path d="M10 8v9c0 3 1.4 5.2 3 6.2M16 8v11c0 2.6 1 4.7 2.6 6.2M22 8v9c0 3-1.4 5.2-3 6.2" fill="none" stroke="#fff" stroke-width="2.4" stroke-linecap="round"/></svg>';
  const gBody = document.createElement("div");
  gBody.className = "ci-body";
  const gTop = document.createElement("div");
  gTop.className = "ci-top";
  const gName = document.createElement("span");
  gName.className = "ci-name";
  gName.textContent = "Общий чат";
  const gState = document.createElement("span");
  gState.className = "ci-state";
  const gUnread = state.unread.get("") || 0;
  if (gUnread) {
    const badge = document.createElement("span");
    badge.className = "ci-badge";
    badge.textContent = gUnread > 99 ? "99+" : String(gUnread);
    gState.appendChild(badge);
  } else {
    const gDot = document.createElement("span");
    gDot.className = "dot" + (state.online ? " on" : "");
    gState.appendChild(gDot);
  }
  gTop.append(gName, gState);
  const gLast = document.createElement("div");
  gLast.className = "ci-last";
  const lastGeneral = state.history.get("") || [];
  gLast.textContent = lastGeneral.length ? previewOf(lastGeneral[lastGeneral.length - 1]) : "Пока пусто";
  gBody.append(gTop, gLast);
  general.append(gAvatar, gBody);
  general.onclick = () => openChat(null);
  list.appendChild(general);

  const peers = [...state.peers.values()].sort((a, b) => (b.ts || 0) - (a.ts || 0));
  for (const p of peers) {
    const card = state.users.get(p.username) || p.user || userCard(p.username);
    const btn = document.createElement("button");
    btn.className = "chat-item" + (state.peer === p.username ? " active" : "");
    btn.appendChild(makeAvatar(card));
    const body = document.createElement("div");
    body.className = "ci-body";
    const top = document.createElement("div");
    top.className = "ci-top";
    const name = document.createElement("span");
    name.className = "ci-name";
    name.textContent = card.display_name || p.username;
    const right = document.createElement("span");
    right.className = "ci-state";
    const unread = state.unread.get(p.username) || 0;
    if (unread) {
      const badge = document.createElement("span");
      badge.className = "ci-badge";
      badge.textContent = unread > 99 ? "99+" : String(unread);
      right.appendChild(badge);
    } else if (card.online) {
      const dot = document.createElement("span");
      dot.className = "dot on";
      right.appendChild(dot);
    }
    top.append(name, right);
    const last = document.createElement("div");
    last.className = "ci-last";
    const msgs = state.history.get(p.username) || [];
    last.textContent = p.preview || (msgs.length ? previewOf(msgs[msgs.length - 1]) : "@" + p.username);
    body.append(top, last);
    btn.append(body);
    btn.onclick = () => openChat(p.username);
    list.appendChild(btn);
  }
}

function updateChatHeader() {
  if (!state.me) return;
  const avatar = $("chat-avatar");
  avatar.innerHTML = "";
  if (state.peer === null) {
    $("chat-title").textContent = "Общий чат";
    $("chat-sub").textContent = `${onlineCount()} в сети · ${shortHost()}`;
    const g = document.createElement("div");
    g.className = "avatar";
    g.style.setProperty("--h", 210);
    g.innerHTML =
      '<svg viewBox="0 0 32 32" width="22" height="22" aria-hidden="true"><path d="M10 8v9c0 3 1.4 5.2 3 6.2M16 8v11c0 2.6 1 4.7 2.6 6.2M22 8v9c0 3-1.4 5.2-3 6.2" fill="none" stroke="#fff" stroke-width="2.4" stroke-linecap="round"/></svg>';
    avatar.appendChild(g);
  } else {
    const card = userCard(state.peer);
    $("chat-title").textContent = card.display_name || state.peer;
    $("chat-sub").textContent = `@${state.peer} · ${card.online ? "в сети" : "не в сети"}`;
    avatar.appendChild(makeAvatar(card));
  }
}

/* ---------- Поиск ---------- */

function clearSearch() {
  $("search").value = "";
  $("search-clear").classList.add("hidden");
  $("search-panel").classList.add("hidden");
  $("chat-list").classList.remove("hidden");
  state.searchResults = [];
}

async function runSearch(q) {
  if (!state.token) return;
  try {
    const r = await fetch(
      `${state.http}/users?token=${encodeURIComponent(state.token)}&q=${encodeURIComponent(q)}`
    );
    if (!r.ok) return;
    state.searchResults = await r.json();
    renderSearch(q);
  } catch {}
}

function renderSearch(q) {
  const panel = $("search-panel");
  panel.innerHTML = "";
  $("chat-list").classList.add("hidden");
  panel.classList.remove("hidden");

  const title = document.createElement("div");
  title.className = "search-title";
  title.textContent = q ? `Поиск: ${q}` : "Все пользователи";
  panel.appendChild(title);

  if (!state.searchResults.length) {
    const empty = document.createElement("div");
    empty.className = "search-empty";
    empty.textContent = q ? "Никого не нашли" : "На сервере пока нет других пользователей";
    panel.appendChild(empty);
    return;
  }

  for (const u of state.searchResults) {
    const btn = document.createElement("button");
    btn.className = "chat-item";
    btn.appendChild(makeAvatar(u));
    const body = document.createElement("div");
    body.className = "ci-body";
    const top = document.createElement("div");
    top.className = "ci-top";
    const name = document.createElement("span");
    name.className = "ci-name";
    name.textContent = u.display_name || u.username;
    const stateEl = document.createElement("span");
    stateEl.className = "ci-state";
    if (u.online) {
      const dot = document.createElement("span");
      dot.className = "dot on";
      stateEl.appendChild(dot);
    }
    top.append(name, stateEl);
    const last = document.createElement("div");
    last.className = "ci-last";
    last.textContent = "@" + u.username;
    body.append(top, last);
    btn.appendChild(body);
    btn.onclick = () => {
      clearSearch();
      const p = ensurePeer(u.username);
      p.user = u;
      state.users.set(u.username, u);
      openChat(u.username);
    };
    panel.appendChild(btn);
  }
}

/* ---------- Отправка ---------- */

function sendPayload(payload) {
  if (!state.ws || state.ws.readyState !== WebSocket.OPEN) {
    toast("Нет подключения к серверу", true);
    return false;
  }
  const msg = { type: "text", text: "", attachment_id: null, to: state.peer || null };
  Object.assign(msg, payload);
  state.ws.send(JSON.stringify(msg));
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

function setBusy(on) {
  $("busy").classList.toggle("hidden", !on);
}

function baseMime(type) {
  return (type || "application/octet-stream").split(";")[0].trim();
}

async function upload(blob, name, mime) {
  if (blob.size > MAX_UPLOAD) throw new Error("Файл больше 50 МБ");
  setBusy(true);
  try {
    const url = `${state.http}/upload?token=${encodeURIComponent(state.token)}&name=${encodeURIComponent(
      name
    )}&mime=${encodeURIComponent(baseMime(mime))}`;
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

/* ---------- Запись ---------- */

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
  if (!state.ws || state.ws.readyState !== WebSocket.OPEN) {
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
    toast(`Нет доступа к ${kind === "audio" ? "микрофону" : "камере"}: ${e.message}`, true);
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
  if (rec && recKind === kind) stopRecording(false);
  else if (rec) toast("Сначала завершите текущую запись", true);
  else startRecording(kind);
}

/* ---------- Уведомления и звук ---------- */

function soundOn() {
  return settings.sound && (document.hidden || !document.hasFocus());
}

function beep() {
  try {
    const Ctx = window.AudioContext || window.webkitAudioContext;
    if (!Ctx) return;
    if (!beep.ctx) beep.ctx = new Ctx();
    const ctx = beep.ctx;
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.value = 920;
    gain.gain.setValueAtTime(0.0001, ctx.currentTime);
    gain.gain.exponentialRampToValueAtTime(0.12, ctx.currentTime + 0.01);
    gain.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + 0.16);
    osc.connect(gain).connect(ctx.destination);
    osc.start();
    osc.stop(ctx.currentTime + 0.18);
  } catch {}
}

function maybeNotify(m, key) {
  if (m.from === state.me.username) return;
  const away = document.hidden || !document.hasFocus();
  if (key === currentKey() && !away) return;
  if (settings.sound) beep();
  if (!settings.notif || !("Notification" in window) || Notification.permission !== "granted") return;
  try {
    const n = new Notification(m.from_name || m.from, {
      body: previewOf(m),
      tag: "shtorka-" + m.id,
      silent: true,
    });
    n.onclick = () => {
      window.focus();
      if (key) openChat(key);
    };
  } catch {}
}

async function enableNotifications() {
  if (!("Notification" in window)) {
    toast("Уведомления не поддерживаются", true);
    return false;
  }
  if (Notification.permission === "granted") return true;
  if (Notification.permission === "denied") {
    toast("Уведомления запрещены в настройках Windows", true);
    return false;
  }
  return (await Notification.requestPermission()) === "granted";
}

/* ---------- Настройки ---------- */

function fillProfile() {
  if (!state.me) return;
  const me = state.me;
  applyAvatarTo($("avatar-btn"), me);
  $("avatar-btn").classList.add("avatar-xl");
  $("profile-display").textContent = me.display_name || me.username;
  $("profile-username").textContent = "@" + me.username;
  $("profile-username-row").textContent = "@" + me.username;
  $("account-username").textContent = "@" + me.username;
  $("account-server").textContent = shortHost();
  $("display-name").value = me.display_name || "";
  $("dm-privacy").value = me.dm_privacy === "nobody" ? "nobody" : "everyone";
  $("show-online").checked = me.show_online !== false;
  $("profile-created").textContent = me.created
    ? "На сервере с " + new Date(me.created * 1000).toLocaleDateString("ru-RU")
    : "";
  $("profile-msg").textContent = "";
  $("privacy-msg").textContent = "";
  $("pw-msg").textContent = "";
  $("pw-old").value = "";
  $("pw-new").value = "";
}

function openSettings(tab = "profile") {
  if (!state.me) return;
  fillProfile();
  switchTab(tab);
  $("settings").classList.remove("hidden");
}

function closeSettings() {
  $("settings").classList.add("hidden");
}

function switchTab(name) {
  document.querySelectorAll(".set-tab").forEach((b) => b.classList.toggle("active", b.dataset.tab === name));
  document.querySelectorAll(".set-panel").forEach((p) => p.classList.toggle("active", p.dataset.panel === name));
}

async function apiProfile(payload) {
  const r = await fetch(`${state.http}/profile`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(Object.assign({ token: state.token }, payload)),
  });
  if (!r.ok) throw new Error((await r.text()) || "Ошибка " + r.status);
  return await r.json();
}

async function saveProfile() {
  const msg = $("profile-msg");
  msg.classList.remove("err");
  msg.textContent = "";
  try {
    const info = await apiProfile({ display_name: $("display-name").value });
    Object.assign(state.me, info, { token: state.token });
    if (state.users.has(state.me.username)) {
      state.users.set(state.me.username, Object.assign(state.users.get(state.me.username), info));
    }
    fillProfile();
    updateMeUi();
    renderChats();
    msg.textContent = "Сохранено";
    toast("Профиль обновлён");
  } catch (e) {
    msg.textContent = e.message || String(e);
    msg.classList.add("err");
  }
}

async function uploadAvatar(file) {
  if (!file) return;
  if (file.size > MAX_AVATAR) {
    toast("Аватарка больше 5 МБ", true);
    return;
  }
  if (!/^image\/(png|jpeg|webp|gif)$/.test(file.type)) {
    toast("Поддерживаются PNG, JPEG, WebP и GIF", true);
    return;
  }
  try {
    const r = await fetch(`${state.http}/avatar?token=${encodeURIComponent(state.token)}`, {
      method: "POST",
      headers: { "content-type": file.type },
      body: file,
    });
    if (!r.ok) throw new Error((await r.text()) || "Ошибка " + r.status);
    const info = await r.json();
    Object.assign(state.me, info, { token: state.token });
    fillProfile();
    updateMeUi();
    renderChats();
    renderMessages();
    toast("Аватарка обновлена");
  } catch (e) {
    toast(String(e.message || e), true);
  }
}

async function removeAvatar() {
  try {
    const r = await fetch(`${state.http}/avatar?token=${encodeURIComponent(state.token)}`, {
      method: "DELETE",
    });
    if (!r.ok) throw new Error((await r.text()) || "Ошибка " + r.status);
    const info = await r.json();
    Object.assign(state.me, info, { token: state.token });
    fillProfile();
    updateMeUi();
    renderChats();
    renderMessages();
  } catch (e) {
    toast(String(e.message || e), true);
  }
}

async function savePrivacy() {
  const msg = $("privacy-msg");
  msg.classList.remove("err");
  try {
    const info = await apiProfile({
      dm_privacy: $("dm-privacy").value,
      show_online: $("show-online").checked,
    });
    Object.assign(state.me, info, { token: state.token });
    msg.textContent = "Сохранено";
    toast("Приватность обновлена");
  } catch (e) {
    msg.textContent = e.message || String(e);
    msg.classList.add("err");
  }
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
  if (newPw.length < 6) {
    msg.textContent = "Новый пароль — минимум 6 символов";
    msg.classList.add("err");
    return;
  }
  try {
    const r = await fetch(`${state.http}/password`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token: state.token, old: oldPw, new: newPw }),
    });
    if (!r.ok) throw new Error((await r.text()) || "Ошибка " + r.status);
    msg.textContent = "Пароль изменён";
    $("pw-old").value = "";
    $("pw-new").value = "";
  } catch (e) {
    msg.textContent = e.message || String(e);
    msg.classList.add("err");
  }
}

function pickWallpaper(file) {
  if (!file) return;
  if (!/^image\/(png|jpeg|webp)$/.test(file.type)) {
    toast("Обои: PNG, JPEG или WebP", true);
    return;
  }
  if (file.size > MAX_WALL) {
    toast("Картинка больше 3,5 МБ", true);
    return;
  }
  const reader = new FileReader();
  reader.onload = () => {
    settings.wallpaper = "custom";
    settings.wallpaperData = String(reader.result || "");
    saveSettings();
    applySettings();
  };
  reader.readAsDataURL(file);
}

/* ---------- Мобильная панель ---------- */

function closeMobileSide() {
  $("side").classList.remove("open");
}

/* ---------- Обработчики ---------- */

$("connect").onclick = enter;
$("password").addEventListener("keydown", (e) => {
  if (e.key === "Enter") enter();
});
$("username").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("password").focus();
});
$("server").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("username").focus();
});
$("mode-toggle").onclick = () => {
  authMode = authMode === "login" ? "register" : "login";
  const register = authMode === "register";
  $("display-field").classList.toggle("hidden", !register);
  $("connect").textContent = register ? "Зарегистрироваться" : "Войти";
  $("mode-toggle").innerHTML = register
    ? 'Уже есть аккаунт? <b>Войти</b>'
    : 'Нет аккаунта? <b>Зарегистрироваться</b>';
  $("password").autocomplete = register ? "new-password" : "current-password";
  $("login-error").textContent = "";
};

$("settings-btn").onclick = () => openSettings("profile");
$("settings-close").onclick = closeSettings;
$("settings").addEventListener("click", (e) => {
  if (e.target === $("settings")) closeSettings();
});
document.querySelectorAll(".set-tab").forEach((b) => {
  b.onclick = () => switchTab(b.dataset.tab);
});

$("theme-btn").onclick = cycleTheme;
$("login-theme").onclick = cycleTheme;

document.querySelectorAll("[data-theme-choice]").forEach((b) => {
  b.onclick = () => setTheme(b.dataset.themeChoice);
});
document.querySelectorAll("[data-accent]").forEach((b) => {
  b.onclick = () => {
    settings.accent = b.dataset.accent;
    saveSettings();
    applySettings();
  };
});
document.querySelectorAll("[data-wallpaper]").forEach((b) => {
  b.onclick = () => {
    if (b.dataset.wallpaper === "custom") {
      $("wall-input").click();
      return;
    }
    settings.wallpaper = b.dataset.wallpaper;
    saveSettings();
    applySettings();
  };
});
document.querySelectorAll("[data-font]").forEach((b) => {
  b.onclick = () => {
    settings.font = b.dataset.font;
    saveSettings();
    applySettings();
  };
});
$("wall-input").addEventListener("change", (e) => {
  const f = e.target.files && e.target.files[0];
  e.target.value = "";
  pickWallpaper(f);
});

$("avatar-change").onclick = $("avatar-btn").onclick = () => $("avatar-input").click();
$("avatar-input").addEventListener("change", (e) => {
  const f = e.target.files && e.target.files[0];
  e.target.value = "";
  uploadAvatar(f);
});
$("avatar-remove").onclick = removeAvatar;
$("profile-save").onclick = saveProfile;
$("privacy-save").onclick = savePrivacy;
$("pw-save").onclick = changePassword;
$("settings-logout").onclick = () => logout();

$("notif-toggle").addEventListener("change", async (e) => {
  if (e.target.checked) {
    const ok = await enableNotifications();
    e.target.checked = ok;
    settings.notif = ok;
    if (ok) toast("Уведомления включены");
  } else {
    settings.notif = false;
  }
  saveSettings();
});
$("sound-toggle").addEventListener("change", (e) => {
  settings.sound = e.target.checked;
  saveSettings();
  if (settings.sound) beep();
});
$("notif-test").onclick = async () => {
  const ok = await enableNotifications();
  if (!ok) return;
  try {
    new Notification("Шторка", { body: "Уведомления работают" });
  } catch {
    toast("Система не показала уведомление", true);
  }
};

$("logout").onclick = () => logout();
$("search").addEventListener("input", (e) => {
  const q = e.target.value.trim();
  $("search-clear").classList.toggle("hidden", !q);
  clearTimeout(searchTimer);
  if (!q) {
    $("search-panel").classList.add("hidden");
    $("chat-list").classList.remove("hidden");
    return;
  }
  searchTimer = setTimeout(() => runSearch(q), 220);
});
$("search").addEventListener("focus", () => {
  if (!$("search").value.trim()) {
    runSearch("");
  }
});
$("search-clear").onclick = () => clearSearch();

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

$("input").addEventListener("paste", (e) => {
  const item = Array.from((e.clipboardData && e.clipboardData.items) || []).find((i) =>
    i.type.startsWith("image/")
  );
  if (!item) return;
  e.preventDefault();
  const file = item.getAsFile();
  if (!file) return;
  const ext = file.type.split("/")[1] || "png";
  sendFile(new File([file], "Изображение." + ext, { type: file.type }), $("input").value.trim()).then(
    (ok) => {
      if (ok) $("input").value = "";
    }
  );
});

$("menu-btn").onclick = () => $("side").classList.toggle("open");
$("lightbox").onclick = () => {
  $("lightbox").classList.add("hidden");
  $("lightbox-img").src = "";
};
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    $("lightbox").classList.add("hidden");
    closeSettings();
    closeMobileSide();
  }
});

/* ---------- Запуск ---------- */

applySettings();
renderChats();
updateChatHeader();
setStatus(false, "офлайн");

(async function boot() {
  const saved = loadSession();
  if (saved) {
    $("server").value = saved.server.replace(/^wss?:\/\//, "");
    $("username").value = saved.username;
    try {
      const http = normalizeServer(saved.server).replace(/^ws/, "http");
      const r = await fetch(`${http}/account?token=${encodeURIComponent(saved.token)}`);
      if (r.ok) {
        const me = await r.json();
        me.token = saved.token;
        await startSession(me, saved.server);
        return;
      }
    } catch {}
  }
  if (!$("server").value) $("server").value = "127.0.0.1:7878";
  $("username").focus();
})();
