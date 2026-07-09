#!/usr/bin/env bash
set -euo pipefail
set -o pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v brew >/dev/null 2>&1; then
  echo "error: Homebrew is required to locate librime" >&2
  exit 1
fi

default_shared_data_dir() {
  local candidates=(
    "$PWD/data/shared"
    "/Library/Input Methods/Squirrel.app/Contents/SharedSupport"
    "/opt/homebrew/share/rime-data"
    "/usr/local/share/rime-data"
    "/usr/share/rime-data"
  )

  local candidate
  for candidate in "${candidates[@]}"; do
    if [[ -d "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return
    fi
  done

  printf '%s\n' "$PWD/data/shared"
}

export RIME_INCLUDE_DIR="${RIME_INCLUDE_DIR:-$(brew --prefix librime)/include}"
export RIME_LIB_DIR="${RIME_LIB_DIR:-$(brew --prefix librime)/lib}"
export RIME_SHARED_DATA_DIR="${RIME_SHARED_DATA_DIR:-$(default_shared_data_dir)}"
export RIME_USER_DATA_DIR="${RIME_USER_DATA_DIR:-$PWD/data/user}"
export RIME_SCHEMA="${RIME_SCHEMA:-luna_pinyin_simp}"
export CARGO_HOME="${CARGO_HOME:-/private/tmp/pal-cargo-home-rime-poc}"
export RIME_POC_NATIVE_LOG_EVENTS="${RIME_POC_NATIVE_LOG_EVENTS:-1}"

if [[ ! -f "$RIME_INCLUDE_DIR/rime_api.h" ]]; then
  echo "error: rime_api.h not found under RIME_INCLUDE_DIR=$RIME_INCLUDE_DIR" >&2
  echo "hint: brew install librime" >&2
  exit 1
fi

if [[ ! -d "$RIME_SHARED_DATA_DIR" ]]; then
  echo "error: Rime shared data dir does not exist: $RIME_SHARED_DATA_DIR" >&2
  echo "hint: bash scripts/download-rime-data.sh" >&2
  echo "hint: or set RIME_SHARED_DATA_DIR to an existing Rime shared data directory" >&2
  exit 1
fi

log_dir="${RIME_POC_LOG_DIR:-$PWD/logs}"
mkdir -p "$log_dir"
log_file="${RIME_POC_LOG_FILE:-$log_dir/rime-poc-listener-$(date +%Y%m%d-%H%M%S).log}"

echo "rime-poc debug listener log:"
echo "  $log_file"
echo

cargo run -- --doctor --listen --log-events "$@" 2>&1 | tee -a "$log_file"
