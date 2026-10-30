#!/usr/bin/env bash
# Быстрая проверка сервера 0.1.0: поднимает сервер, гоняет двух клиентов, проверяет историю и вытеснение сессии.
set -e
cd "$(dirname "$0")/.."
PORT=${PORT:-7878}
DATA=$(mktemp -d)
BIN=${CARGO_TARGET_DIR:-target}/debug
cargo build -q -p server
"$BIN/server" --port "$PORT" --data "$DATA" > /tmp/smoke_server.log 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null' EXIT
sleep 1

curl -sf "localhost:$PORT/health" >/dev/null || { echo "FAIL: health"; exit 1; }

# Регистрируем двух пользователей (тестовый клиент при необходимости сам бы зарегистрировался).
curl -sf -X POST "localhost:$PORT/register" -H 'content-type: application/json' \
  -d '{"username":"vasya","display_name":"Вася","password":"123456"}' >/dev/null
curl -sf -X POST "localhost:$PORT/register" -H 'content-type: application/json' \
  -d '{"username":"petya","display_name":"Петя","password":"123456"}' >/dev/null

# Долгоживущий клиент Вася держит соединение — его потом должно вытеснить.
( (sleep 4) | "$BIN/testclient" --username vasya --password 123456 --url "http://127.0.0.1:$PORT" > /tmp/smoke_a.log 2>&1 ) &
VASYA=$!
sleep 1
( (sleep 0.5; printf 'тест от Пети\n'; sleep 1) | "$BIN/testclient" --username petya --password 123456 --url "http://127.0.0.1:$PORT" > /tmp/smoke_b.log 2>&1 )

# Второй вход под тем же аккаунтом вытесняет первую сессию.
( printf '' | timeout 3 "$BIN/testclient" --username vasya --password 123456 --url "http://127.0.0.1:$PORT" > /tmp/smoke_dup.log 2>&1 ) || true
wait $VASYA

TOKEN=$(curl -sf -X POST "localhost:$PORT/login" -H 'content-type: application/json' \
  -d '{"username":"vasya","password":"123456"}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])')
curl -sf "localhost:$PORT/history?token=$TOKEN" | grep -q 'тест от Пети' || { echo "FAIL: history"; exit 1; }
grep -q 'сессия закрыта' /tmp/smoke_a.log || { echo "FAIL: старую сессию не вытеснили"; cat /tmp/smoke_a.log; exit 1; }
echo "OK: health, аккаунты, сообщения, история, вытеснение дубля сессии"
