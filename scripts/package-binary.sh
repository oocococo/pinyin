#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: macOS packaging must run on macOS" >&2
  exit 1
fi

for tool in brew cargo ditto install; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: required tool not found: $tool" >&2
    exit 1
  fi
done

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
if [[ -z "$version" ]]; then
  echo "error: unable to read package version from Cargo.toml" >&2
  exit 1
fi

case "$(uname -m)" in
  arm64) architecture="arm64" ;;
  x86_64) architecture="x86_64" ;;
  *)
    echo "error: unsupported macOS architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

export RIME_INCLUDE_DIR="${RIME_INCLUDE_DIR:-$(brew --prefix librime)/include}"
export RIME_LIB_DIR="${RIME_LIB_DIR:-$(brew --prefix librime)/lib}"
export CARGO_HOME="${CARGO_HOME:-/private/tmp/pal-cargo-home-pinyin}"

for required in \
  "$RIME_INCLUDE_DIR/rime_api.h" \
  pinyin.toml \
  LICENSE \
  THIRD_PARTY_NOTICES.md \
  RELEASE_NOTES.md; do
  if [[ ! -f "$required" ]]; then
    echo "error: required release file not found: $required" >&2
    exit 1
  fi
done

if [[ ! -d data/shared ]]; then
  echo "error: data/shared does not exist" >&2
  echo "hint: bash scripts/download-rime-data.sh" >&2
  exit 1
fi

package_name="pinyin-v${version}-macos-${architecture}"
dist_dir="$PWD/dist/$package_name"

cargo build --release --locked

rm -rf "$dist_dir"
mkdir -p "$dist_dir/data/shared" "$dist_dir/data/user"

install -m 755 target/release/pinyin "$dist_dir/pinyin"
install -m 644 pinyin.toml "$dist_dir/pinyin.toml"
install -m 644 LICENSE "$dist_dir/LICENSE"
install -m 644 THIRD_PARTY_NOTICES.md "$dist_dir/THIRD_PARTY_NOTICES.md"
install -m 644 RELEASE_NOTES.md "$dist_dir/RELEASE_NOTES.md"

ditto data/shared "$dist_dir/data/shared"
if [[ -f data/user/default.custom.yaml ]]; then
  install -m 644 data/user/default.custom.yaml "$dist_dir/data/user/default.custom.yaml"
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

exec "$PWD/pinyin" --listen "$@"
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
export PINYIN_NATIVE_LOG_EVENTS="${PINYIN_NATIVE_LOG_EVENTS:-1}"

log_dir="${PINYIN_LOG_DIR:-$PWD/logs}"
mkdir -p "$log_dir"
log_file="${PINYIN_LOG_FILE:-$log_dir/pinyin-listener-$(date +%Y%m%d-%H%M%S).log}"

echo "pinyin 调试日志："
echo "  $log_file"
echo

"$PWD/pinyin" --doctor --listen --log-events "$@" 2>&1 | tee -a "$log_file"
RUN_LISTENER_DEBUG
chmod +x "$dist_dir/run-listener-debug.sh"

cat > "$dist_dir/README.txt" <<'README'
pinyin macOS 便携包

pinyin 让你保持 macOS 系统英文输入源，只在需要时通过前缀临时输入中文。

1. 在“系统设置 → 隐私与安全性”中，为本目录里的 pinyin 开启：
   - 辅助功能
   - 输入监控

2. 在终端运行：
   ./run-listener.sh

3. 保持系统 ABC 输入源，在普通文本框中输入：
   ''nihao<空格>shijie<空格>''

   结果：
   你好世界

默认进入与退出字符串均为两个英文单引号：''
配置文件：pinyin.toml

诊断命令：
   ./run-listener-debug.sh

注意：
- 必须手动授予 macOS 权限。
- 当前构建尚未完成 Apple Developer ID 签名与公证。
- 调试日志可能包含按键和输入内容，公开前请删除敏感信息。
README

echo "Binary package:"
echo "  $dist_dir"
