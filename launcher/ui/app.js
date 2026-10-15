const { invoke } = window.__TAURI__.core;

const $ = (id) => document.getElementById(id);
let running = false;
let busy = false;

function toast(text, isErr = false) {
  let el = document.querySelector(".toast");
  if (!el) {
    el = document.createElement("div");
    el.className = "toast";
    document.body.appendChild(el);
  }
  el.textContent = text;
  el.classList.toggle("err", isErr);
  el.classList.add("show");
  clearTimeout(el._t);
  el._t = setTimeout(() => el.classList.remove("show"), 2200);
}

function tagFor(label) {
  if (label === "Radmin VPN") return ["tag radmin", label];
  if (label === "Локальная сеть") return ["tag lan", label];
  return ["tag pc", label];
}

function renderAddresses(list, port) {
  const ul = $("addresses");
  ul.innerHTML = "";
  if (!list.length) {
    ul.innerHTML = '<li class="addr-empty">Сетевые адреса не найдены</li>';
    return;
  }
  for (const a of list) {
    const li = document.createElement("li");
    const [cls, text] = tagFor(a.label);
    const url = running ? a.url : a.url.replace(/:\d+$/, `:${port}`);
    li.innerHTML = `<div class="addr-main"><span class="${cls}">${text}</span>` +
      `<span class="addr-url"></span></div>`;
    li.querySelector(".addr-url").textContent = url;
    const btn = document.createElement("button");
    btn.className = "ghost small";
    btn.textContent = "Копировать";
    btn.disabled = !running;
    btn.onclick = async () => {
      try {
        await navigator.clipboard.writeText(url);
        toast("Скопировано: " + url);
      } catch {
        toast("Не удалось скопировать", true);
      }
    };
    li.appendChild(btn);
    if (a.label === "Этот ПК") li.classList.add("muted");
    ul.appendChild(li);
  }
}

function renderUsers(users) {
  const ul = $("users");
  ul.innerHTML = "";
  $("count").textContent = users.length;
  if (!users.length) {
    ul.innerHTML = '<li class="empty">Никого нет</li>';
    return;
  }
  for (const u of users) {
    const li = document.createElement("li");
    li.textContent = u;
    ul.appendChild(li);
  }
}

function renderLogs(logs) {
  const pre = $("logs");
  const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 20;
  pre.textContent = logs.join("\n");
  if (atBottom) pre.scrollTop = pre.scrollHeight;
}

function setRunning(isRunning) {
  running = isRunning;
  $("state").className = "pill " + (isRunning ? "on" : "off");
  $("state-text").textContent = isRunning ? "Работает" : "Остановлен";
  $("toggle").textContent = isRunning ? "Остановить" : "Запустить";
  $("toggle").className = isRunning ? "danger" : "primary";
  $("port").disabled = isRunning;
}

async function refresh() {
  try {
    const port = Number($("port").value) || 7878;
    const s = await invoke("status", { port: running ? null : port });
    if (s.running !== running) setRunning(s.running);
    if (!running && document.activeElement !== $("port")) $("port").value = s.port;
    renderAddresses(s.addresses, s.port);
    renderUsers(s.users);
    $("msgs").textContent = s.messages;
    renderLogs(s.logs);
  } catch (e) {
    console.error(e);
  }
}

$("toggle").onclick = async () => {
  if (busy) return;
  busy = true;
  $("toggle").disabled = true;
  $("message").textContent = "";
  try {
    if (running) {
      await invoke("stop_server");
      toast("Сервер остановлен");
    } else {
      const port = Number($("port").value);
      if (!Number.isInteger(port) || port < 1024 || port > 65535) {
        throw "Порт должен быть от 1024 до 65535";
      }
      await invoke("start_server", { port });
      toast("Сервер запущен на порту " + port);
    }
  } catch (e) {
    $("message").textContent = String(e);
    toast(String(e), true);
  } finally {
    busy = false;
    $("toggle").disabled = false;
    await refresh();
  }
};

$("pubBtn").onclick = async () => {
  const box = $("public");
  box.classList.remove("hidden");
  box.textContent = "Определяю…";
  try {
    const ip = await invoke("public_ip");
    const port = running ? (await invoke("status", { port: null })).port : Number($("port").value);
    box.innerHTML = "";
    const t = document.createElement("span");
    t.className = "tag pc";
    t.textContent = "Внешний IP";
    const u = document.createElement("span");
    u.className = "addr-url";
    u.textContent = `ws://${ip}:${port}`;
    box.append(t, u);
    box.insertAdjacentHTML("beforeend", '<p class="hint">Чтобы зайти извне, пробросьте порт на роутере. Без белого IP может не работать.</p>');
  } catch (e) {
    box.textContent = String(e);
  }
};

refresh();
setInterval(refresh, 1000);
