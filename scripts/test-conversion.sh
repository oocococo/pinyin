#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v brew >/dev/null 2>&1; then
  echo "error: Homebrew is required to locate librime" >&2
  exit 1
fi

export RIME_INCLUDE_DIR="${RIME_INCLUDE_DIR:-$(brew --prefix librime)/include}"
export RIME_LIB_DIR="${RIME_LIB_DIR:-$(brew --prefix librime)/lib}"
export RIME_SHARED_DATA_DIR="${RIME_SHARED_DATA_DIR:-$PWD/data/shared}"
export RIME_USER_DATA_DIR="${RIME_USER_DATA_DIR:-$PWD/data/user}"
export RIME_SCHEMA="${RIME_SCHEMA:-luna_pinyin_simp}"
export CARGO_HOME="${CARGO_HOME:-/private/tmp/pal-cargo-home-rime-poc}"

if [[ ! -f "$RIME_INCLUDE_DIR/rime_api.h" ]]; then
  echo "error: rime_api.h not found under RIME_INCLUDE_DIR=$RIME_INCLUDE_DIR" >&2
  echo "hint: brew install librime" >&2
  exit 1
fi

if [[ ! -d "$RIME_SHARED_DATA_DIR" ]]; then
  echo "error: Rime shared data dir does not exist: $RIME_SHARED_DATA_DIR" >&2
  echo "hint: bash scripts/download-rime-data.sh" >&2
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

run_case '“好”，“再见”，“你好吗”，“我要测试”' ';;"hao","zaijian","nihaoma","woyaoceshi";;'
run_case '我要测试中文输入法，你好吗！“好……再见”－加号＋问号～' ';;woyaoceshizhongwenshurufa,nihaoma!"hao...zaijian"-jiahao+wenhao~;;'
run_case '好……再见，你好吗' ';;hao……zaijian,nihaoma;;'
run_case '我爱OpenAI，用Rust开发' --conversion-mode rime-auto ';;wo ai OpenAI,yong Rust kaifa;;'

echo "all conversion smoke tests passed"
