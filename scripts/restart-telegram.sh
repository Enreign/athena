#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WRAPPER="$ROOT_DIR/scripts/athena-with-vaultwarden.sh"
ATHENA_BIN="${ATHENA_BIN:-$ROOT_DIR/target/debug/athena}"
LOG_FILE="${ATHENA_TELEGRAM_LOG:-$ROOT_DIR/athena_telegram.log}"
LOCAL_CA="${LOCAL_CA_CERT:-$HOME/.vaultwarden-data/ssl/local-ca.crt}"
BW_ENV_FILE="${ATHENA_BW_ENV_FILE:-$ROOT_DIR/.env.vaultwarden.local}"
DEFAULT_KEYCHAIN_SERVICE="athena-vaultwarden"
DEFAULT_KEYCHAIN_CLIENT_ID_ACCOUNT="BW_CLIENTID"
DEFAULT_KEYCHAIN_CLIENT_SECRET_ACCOUNT="BW_CLIENTSECRET"
DEFAULT_KEYCHAIN_MASTER_PASSWORD_ACCOUNT="BW_MASTER_PASSWORD"
KEYCHAIN_SERVICE="${ATHENA_BW_KEYCHAIN_SERVICE:-$DEFAULT_KEYCHAIN_SERVICE}"
KEYCHAIN_CLIENT_ID_ACCOUNT="${ATHENA_BW_KEYCHAIN_CLIENT_ID_ACCOUNT:-$DEFAULT_KEYCHAIN_CLIENT_ID_ACCOUNT}"
KEYCHAIN_CLIENT_SECRET_ACCOUNT="${ATHENA_BW_KEYCHAIN_CLIENT_SECRET_ACCOUNT:-$DEFAULT_KEYCHAIN_CLIENT_SECRET_ACCOUNT}"
KEYCHAIN_MASTER_PASSWORD_ACCOUNT="${ATHENA_BW_KEYCHAIN_MASTER_PASSWORD_ACCOUNT:-$DEFAULT_KEYCHAIN_MASTER_PASSWORD_ACCOUNT}"
KEYCHAIN_PATH="${ATHENA_BW_KEYCHAIN_PATH:-$HOME/Library/Keychains/login.keychain-db}"

usage() {
  cat <<'EOF'
Usage: scripts/restart-telegram.sh

Restarts Athena Telegram bot with secrets loaded from Vaultwarden.

Environment overrides:
  ATHENA_BIN           Path to Athena binary (default: ./target/debug/athena)
  ATHENA_TELEGRAM_LOG  Log file path (default: ./athena_telegram.log)
  LOCAL_CA_CERT        Local Vaultwarden CA cert path
  ATHENA_BW_KEYCHAIN_SERVICE                  Keychain service (default: athena-vaultwarden)
  ATHENA_BW_KEYCHAIN_CLIENT_ID_ACCOUNT        Account for BW_CLIENTID
  ATHENA_BW_KEYCHAIN_CLIENT_SECRET_ACCOUNT    Account for BW_CLIENTSECRET
  ATHENA_BW_KEYCHAIN_MASTER_PASSWORD_ACCOUNT  Account for BW_MASTER_PASSWORD
  ATHENA_BW_KEYCHAIN_PATH                     Keychain path (default: ~/Library/Keychains/login.keychain-db)
  ATHENA_BW_ENV_FILE                          Env file path (default: ./.env.vaultwarden.local)
EOF
}

require_interactive_bw() {
  if [[ ! -t 0 || ! -t 1 ]]; then
    cat >&2 <<'EOF'
Bitwarden is locked and this shell is non-interactive.
If Keychain-backed automation is not configured, unlock first in your terminal, then re-run:
  export NODE_EXTRA_CA_CERTS="$HOME/.vaultwarden-data/ssl/local-ca.crt"
  export BW_SESSION="$(bw unlock --raw)"
EOF
    exit 1
  fi
}

load_secret_from_keychain() {
  local env_name="$1"
  local account="$2"
  local default_account="$3"

  if [[ -n "${!env_name:-}" ]]; then
    return 0
  fi
  if ! command -v security >/dev/null 2>&1; then
    return 1
  fi
  local value svc acc
  local -a keychain_args=()
  if [[ -f "$KEYCHAIN_PATH" ]]; then
    keychain_args=(-k "$KEYCHAIN_PATH")
  fi

  # Try configured service/account first, then defaults as fallback.
  for svc in "$KEYCHAIN_SERVICE" "$DEFAULT_KEYCHAIN_SERVICE"; do
    for acc in "$account" "$default_account"; do
      value="$(
        security find-generic-password "${keychain_args[@]}" -w -s "$svc" -a "$acc" 2>/dev/null || true
      )"
      if [[ -n "$value" ]]; then
        export "$env_name=$value"
        return 0
      fi
    done
  done
  return 1
}

load_bw_credentials() {
  load_secret_from_keychain BW_CLIENTID "$KEYCHAIN_CLIENT_ID_ACCOUNT" "$DEFAULT_KEYCHAIN_CLIENT_ID_ACCOUNT" || true
  load_secret_from_keychain BW_CLIENTSECRET "$KEYCHAIN_CLIENT_SECRET_ACCOUNT" "$DEFAULT_KEYCHAIN_CLIENT_SECRET_ACCOUNT" || true
  load_secret_from_keychain BW_MASTER_PASSWORD "$KEYCHAIN_MASTER_PASSWORD_ACCOUNT" "$DEFAULT_KEYCHAIN_MASTER_PASSWORD_ACCOUNT" || true

  if [[ -z "${BW_MASTER_PASSWORD:-}" ]]; then
    echo "Warning: BW master password not found. Set BW_MASTER_PASSWORD in '$BW_ENV_FILE' or Keychain (service='$KEYCHAIN_SERVICE', account='$KEYCHAIN_MASTER_PASSWORD_ACCOUNT', path='$KEYCHAIN_PATH')." >&2
  fi
}

load_env_file() {
  if [[ ! -f "$BW_ENV_FILE" ]]; then
    return 0
  fi
  # shellcheck disable=SC1090
  set -a
  source "$BW_ENV_FILE"
  set +a
}

try_bw_login_noninteractive() {
  if [[ -z "${BW_CLIENTID:-}" || -z "${BW_CLIENTSECRET:-}" ]]; then
    return 1
  fi
  echo "Bitwarden unauthenticated; logging in with API key..."
  bw login --apikey >/dev/null 2>&1
}

try_bw_unlock_noninteractive() {
  if [[ -z "${BW_MASTER_PASSWORD:-}" ]]; then
    return 1
  fi
  echo "Bitwarden locked; unlocking with master password from env/Keychain..."
  local pw_file
  pw_file="$(mktemp)"
  chmod 600 "$pw_file"
  printf '%s' "$BW_MASTER_PASSWORD" >"$pw_file"
  local session
  session="$(bw unlock --passwordfile "$pw_file" --raw 2>/dev/null || true)"
  rm -f "$pw_file"
  if [[ -z "$session" ]]; then
    return 1
  fi
  export BW_SESSION="$session"
  return 0
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if ! command -v bw >/dev/null 2>&1; then
  echo "Bitwarden CLI (bw) is required." >&2
  exit 1
fi

if [[ ! -x "$WRAPPER" ]]; then
  echo "Missing wrapper script: $WRAPPER" >&2
  exit 1
fi

if [[ -z "${NODE_EXTRA_CA_CERTS:-}" && -f "$LOCAL_CA" ]]; then
  export NODE_EXTRA_CA_CERTS="$LOCAL_CA"
fi

load_env_file
load_bw_credentials

status_json="$(bw status 2>/dev/null || true)"
if [[ -z "$status_json" ]]; then
  echo "Unable to read Bitwarden status. Check Vaultwarden URL and TLS CA." >&2
  exit 1
fi

bw_state="$(printf '%s' "$status_json" | sed -n 's/.*"status":"\([^"]*\)".*/\1/p')"
case "${bw_state:-unknown}" in
  unlocked)
    if [[ -z "${BW_SESSION:-}" ]]; then
      try_bw_unlock_noninteractive || {
        require_interactive_bw
        echo "Bitwarden is unlocked but BW_SESSION is not set; unlocking interactively..."
        export BW_SESSION="$(bw unlock --raw)"
      }
    fi
    ;;
  locked)
    if ! try_bw_unlock_noninteractive; then
      require_interactive_bw
      echo "Bitwarden is locked; unlocking interactively..."
      export BW_SESSION="$(bw unlock --raw)"
    fi
    ;;
  unauthenticated)
    if ! try_bw_login_noninteractive; then
      require_interactive_bw
      echo "Bitwarden is not logged in; running bw login interactively..."
      bw login
    fi
    if ! try_bw_unlock_noninteractive; then
      require_interactive_bw
      echo "Unlocking Bitwarden interactively..."
      export BW_SESSION="$(bw unlock --raw)"
    fi
    ;;
  *)
    echo "Unexpected Bitwarden state: ${bw_state:-unknown}" >&2
    exit 1
    ;;
esac

if [[ -z "${BW_SESSION:-}" ]]; then
  echo "BW_SESSION is not set. Configure Keychain credentials or unlock manually." >&2
  exit 1
fi

# Keep local cache fresh after unlock/login.
bw sync >/dev/null 2>&1 || true

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
nohup "$WRAPPER" "$ATHENA_BIN" telegram >"$LOG_FILE" 2>&1 &
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
