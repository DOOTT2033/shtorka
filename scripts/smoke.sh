#!/usr/bin/env bash
# Быстрая проверка сервера: поднимает сервер, гоняет двух клиентов, проверяет историю.
set -e
cd "$(dirname "$0")/.."
PORT=${PORT:-7878}
BIN=target/debug
cargo build -q -p server
$BIN/server --port "$PORT" > /tmp/smoke_server.log 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null' EXIT
sleep 1

curl -sf "localhost:$PORT/health" >/dev/null || { echo "FAIL: health"; exit 1; }

# Долгоживущий клиент Вася держит соединение, чтобы проверить дубль имени.
( (sleep 4) | $BIN/testclient --name Вася --url "ws://127.0.0.1:$PORT" > /tmp/smoke_a.log 2>&1 ) &
VASYA=$!
sleep 1
( (sleep 0.5; printf 'тест от Пети\n'; sleep 1) | $BIN/testclient --name Петя --url "ws://127.0.0.1:$PORT" > /tmp/smoke_b.log 2>&1 )
( printf '' | timeout 3 $BIN/testclient --name Вася --url "ws://127.0.0.1:$PORT" > /tmp/smoke_dup.log 2>&1 ) || true
wait $VASYA

curl -sf "localhost:$PORT/history" | grep -q 'тест от Пети' || { echo "FAIL: history"; exit 1; }
grep -q 'Имя уже занято' /tmp/smoke_dup.log || { echo "FAIL: duplicate name not rejected"; cat /tmp/smoke_dup.log; exit 1; }
echo "OK: health, сообщения, история, отказ при дубле имени"
