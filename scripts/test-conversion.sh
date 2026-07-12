#!/usr/bin/env bash
set -euo pipefail

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

toml_string() {
  local key="$1"
  sed -n "s/^[[:space:]]*$key[[:space:]]*=[[:space:]]*\"\\(.*\\)\"[[:space:]]*$/\\1/p" pinyin.toml | head -n 1
}

export RIME_INCLUDE_DIR="${RIME_INCLUDE_DIR:-$(brew --prefix librime)/include}"
export RIME_LIB_DIR="${RIME_LIB_DIR:-$(brew --prefix librime)/lib}"
export RIME_SHARED_DATA_DIR="${RIME_SHARED_DATA_DIR:-$(default_shared_data_dir)}"
export RIME_SCHEMA="${RIME_SCHEMA:-luna_pinyin_simp}"
export CARGO_HOME="${CARGO_HOME:-/private/tmp/pal-cargo-home-pinyin}"
trigger_prefix="${PINYIN_TEST_TRIGGER_PREFIX:-$(toml_string trigger_prefix)}"
trigger_suffix="${PINYIN_TEST_TRIGGER_SUFFIX:-$(toml_string trigger_suffix)}"

cleanup_user_data_dir=""
if [[ -z "${RIME_USER_DATA_DIR:-}" ]]; then
  cleanup_user_data_dir="$(mktemp -d "${TMPDIR:-/tmp}/pinyin-test-user.XXXXXX")"
  export RIME_USER_DATA_DIR="$cleanup_user_data_dir"
  if [[ -f "$PWD/data/user/default.custom.yaml" ]]; then
    cp "$PWD/data/user/default.custom.yaml" "$RIME_USER_DATA_DIR/default.custom.yaml"
  fi
  trap 'rm -rf "$cleanup_user_data_dir"' EXIT
else
  export RIME_USER_DATA_DIR
fi

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

if [[ -z "$trigger_prefix" || -z "$trigger_suffix" ]]; then
  echo "error: unable to read trigger_prefix/trigger_suffix from pinyin.toml" >&2
  exit 1
fi

if [[ $# -gt 0 ]]; then
  cargo run -- "$@"
  exit 0
fi

cargo fmt --check
cargo test

run_case() {
  local expected="$1"
  shift
  local output
  local actual

  output="$(cargo run --quiet -- "$@")"
  actual="$(printf '%s\n' "$output" | sed -n 's/^output:[[:space:]]*//p')"

  if [[ "$actual" != "$expected" ]]; then
    echo "case failed" >&2
    echo "args:     $*" >&2
    echo "expected: $expected" >&2
    echo "actual:   $actual" >&2
    echo "$output" >&2
    exit 1
  fi

  printf 'ok: %s => %s\n' "$*" "$actual"
}

triggered_text() {
  printf '%s%s%s\n' "$trigger_prefix" "$1" "$trigger_suffix"
}

run_case '“好”，“再见”，“你好吗”，“我要测试”' "$(triggered_text '"hao","zaijian","nihaoma","woyaoceshi"')"
run_case '我要测试中文输入法，你好吗！“好……再见”－加号＋问号～' "$(triggered_text 'woyaoceshizhongwenshurufa,nihaoma!"hao...zaijian"-jiahao+wenhao~')"
run_case '好……再见，你好吗' "$(triggered_text 'hao……zaijian,nihaoma')"
run_case '我爱OpenAI，用Rust开发' --conversion-mode rime-auto "$(triggered_text 'wo ai OpenAI,yong Rust kaifa')"

echo "all conversion smoke tests passed"
