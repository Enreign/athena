#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
ATHENA_BIN="${ATHENA_BIN:-$ROOT_DIR/target/debug/athena}"
LOG_FILE="${ATHENA_TELEGRAM_LOG:-$ROOT_DIR/athena_telegram.log}"

usage() {
  cat <<'EOF'
Usage: scripts/restart-telegram.sh

Restarts Athena Telegram bot.

Environment overrides:
  ATHENA_BIN           Path to Athena binary (default: ./target/debug/athena)
  ATHENA_TELEGRAM_LOG  Log file path (default: ./athena_telegram.log)
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if ! "$ATHENA_BIN" telegram --help >/dev/null 2>&1; then
  echo "Building Athena with Telegram feature..."
  (cd "$ROOT_DIR" && cargo build --features telegram >/dev/null)
fi

existing_pids="$(pgrep -f "target/debug/athena telegram|cargo run --features telegram -- telegram" || true)"
if [[ -n "$existing_pids" ]]; then
  echo "Stopping existing Telegram bot process(es): $existing_pids"
  while IFS= read -r pid; do
    [[ -n "$pid" ]] && kill "$pid" || true
  done <<<"$existing_pids"
  sleep 1
  remaining="$(pgrep -f "target/debug/athena telegram|cargo run --features telegram -- telegram" || true)"
  if [[ -n "$remaining" ]]; then
    while IFS= read -r pid; do
      [[ -n "$pid" ]] && kill -9 "$pid" || true
    done <<<"$remaining"
  fi
fi

echo "Starting Telegram bot..."
nohup "$ATHENA_BIN" telegram >"$LOG_FILE" 2>&1 &
new_pid=$!

sleep 2
if ps -p "$new_pid" >/dev/null 2>&1; then
  echo "Telegram bot restarted successfully (pid=$new_pid)"
  echo "Log file: $LOG_FILE"
  exit 0
fi

echo "Telegram bot exited immediately. Recent logs:" >&2
tail -n 60 "$LOG_FILE" >&2 || true
exit 1
