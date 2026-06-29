#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v brew >/dev/null 2>&1; then
  echo "error: Homebrew is required to locate librime" >&2
  exit 1
fi

export RIME_INCLUDE_DIR="${RIME_INCLUDE_DIR:-$(brew --prefix librime)/include}"
export RIME_LIB_DIR="${RIME_LIB_DIR:-$(brew --prefix librime)/lib}"
export CARGO_HOME="${CARGO_HOME:-/private/tmp/pal-cargo-home-rime-poc}"

if [[ ! -f "$RIME_INCLUDE_DIR/rime_api.h" ]]; then
  echo "error: rime_api.h not found under RIME_INCLUDE_DIR=$RIME_INCLUDE_DIR" >&2
  echo "hint: brew install librime" >&2
  exit 1
fi

if [[ ! -d data/shared ]]; then
  echo "error: data/shared does not exist" >&2
  echo "hint: bash scripts/download-rime-data.sh" >&2
  exit 1
fi

cargo build --release

dist_dir="$PWD/dist/rime-poc-macos"
rm -rf "$dist_dir"
mkdir -p "$dist_dir/data/shared" "$dist_dir/data/user"

install -m 755 target/release/rime-poc "$dist_dir/rime-poc"
install -m 644 rime-poc.toml "$dist_dir/rime-poc.toml"

ditto data/shared "$dist_dir/data/shared"
if [[ -f data/user/default.custom.yaml ]]; then
  install -m 644 data/user/default.custom.yaml "$dist_dir/data/user/default.custom.yaml"
fi

if command -v install_name_tool >/dev/null 2>&1; then
  install_name_tool -add_rpath "$RIME_LIB_DIR" "$dist_dir/rime-poc" 2>/dev/null || true
fi

cat > "$dist_dir/run-listener.sh" <<'RUN_LISTENER'
#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

if command -v brew >/dev/null 2>&1; then
  export DYLD_FALLBACK_LIBRARY_PATH="$(brew --prefix librime)/lib:${DYLD_FALLBACK_LIBRARY_PATH:-}"
fi

export RIME_SHARED_DATA_DIR="${RIME_SHARED_DATA_DIR:-$PWD/data/shared}"
export RIME_USER_DATA_DIR="${RIME_USER_DATA_DIR:-$PWD/data/user}"
export RIME_SCHEMA="${RIME_SCHEMA:-luna_pinyin_simp}"

exec "$PWD/rime-poc" --listen "$@"
RUN_LISTENER
chmod +x "$dist_dir/run-listener.sh"

cat > "$dist_dir/run-listener-debug.sh" <<'RUN_LISTENER_DEBUG'
#!/usr/bin/env bash
set -euo pipefail
set -o pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

if command -v brew >/dev/null 2>&1; then
  export DYLD_FALLBACK_LIBRARY_PATH="$(brew --prefix librime)/lib:${DYLD_FALLBACK_LIBRARY_PATH:-}"
fi

export RIME_SHARED_DATA_DIR="${RIME_SHARED_DATA_DIR:-$PWD/data/shared}"
export RIME_USER_DATA_DIR="${RIME_USER_DATA_DIR:-$PWD/data/user}"
export RIME_SCHEMA="${RIME_SCHEMA:-luna_pinyin_simp}"
export RIME_POC_NATIVE_LOG_EVENTS="${RIME_POC_NATIVE_LOG_EVENTS:-1}"

log_dir="${RIME_POC_LOG_DIR:-$PWD/logs}"
mkdir -p "$log_dir"
log_file="${RIME_POC_LOG_FILE:-$log_dir/rime-poc-listener-$(date +%Y%m%d-%H%M%S).log}"

echo "rime-poc debug listener log:"
echo "  $log_file"
echo

"$PWD/rime-poc" --doctor --listen --log-events "$@" 2>&1 | tee -a "$log_file"
RUN_LISTENER_DEBUG
chmod +x "$dist_dir/run-listener-debug.sh"

cat > "$dist_dir/README.txt" <<'README'
rime-poc macOS binary package

1. Grant Accessibility permission to this binary:
   dist/rime-poc-macos/rime-poc

2. Start the listener:
   ./run-listener.sh

   For diagnosis with event logs:
   ./run-listener-debug.sh

3. Type a trigger in any text field:
   ;;woyaoceshizhongwenshurufa,nihaoma!;;

Expected output:
   我要测试中文输入法，你好吗！
README

echo "Packaged binary:"
echo "  $dist_dir/rime-poc"
echo
echo "Grant Accessibility permission to that binary, then run:"
echo "  $dist_dir/run-listener.sh"
echo
echo "For diagnosis with event logs, run:"
echo "  $dist_dir/run-listener-debug.sh"
