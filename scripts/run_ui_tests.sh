#!/usr/bin/env bash
# Поднимает сервер и статику клиента, прогоняет браузерный тест.
set -e
cd "$(dirname "$0")/.."
DATA=$(mktemp -d)
BIN=${CARGO_TARGET_DIR:-target}/debug
cargo build -q -p server
"$BIN/server" --port 7878 --data "$DATA" > /tmp/ui_server.log 2>&1 &
SRV=$!
(cd client/ui && python3 -m http.server 8000 --bind 127.0.0.1 > /tmp/ui_static.log 2>&1) &
STATIC=$!
trap 'kill $SRV $STATIC 2>/dev/null' EXIT
sleep 2
python3 scripts/ui_test.py
