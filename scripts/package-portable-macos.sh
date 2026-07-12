#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: portable macOS packaging must run on macOS" >&2
  exit 1
fi

for tool in otool install_name_tool ditto codesign realpath; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: required tool not found: $tool" >&2
    exit 1
  fi
done

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
case "$(uname -m)" in
  arm64) architecture="arm64" ;;
  x86_64) architecture="x86_64" ;;
  *)
    echo "error: unsupported macOS architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

package_name="pinyin-v${version}-macos-${architecture}"
portable_dir="$PWD/dist/$package_name"
zip_path="$PWD/dist/$package_name.zip"
lib_dir="$portable_dir/lib"
exe="$portable_dir/pinyin"

bash scripts/package-binary.sh

rm -f "$zip_path"
mkdir -p "$lib_dir"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/pinyin-portable.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

queue_file="$tmp_dir/queue"
seen_file="$tmp_dir/seen"
map_file="$tmp_dir/dep-map"
: > "$seen_file"
: > "$map_file"
printf '%s\n' "$exe" > "$queue_file"

portable_dependency() {
  case "$1" in
    /opt/homebrew/*|/usr/local/*) return 0 ;;
    *) return 1 ;;
  esac
}

list_portable_dependencies() {
  otool -L "$1" | awk 'NR > 1 { print $1 }' | while IFS= read -r dependency; do
    if portable_dependency "$dependency"; then
      printf '%s\n' "$dependency"
    fi
  done
}

while [[ -s "$queue_file" ]]; do
  image="$(sed -n '1p' "$queue_file")"
  sed '1d' "$queue_file" > "$queue_file.next"
  mv "$queue_file.next" "$queue_file"

  while IFS= read -r dependency; do
    [[ -n "$dependency" ]] || continue

    if [[ ! -f "$dependency" ]]; then
      echo "error: dependency not found: $dependency" >&2
      exit 1
    fi

    if grep -Fqx "$dependency" "$seen_file"; then
      continue
    fi

    base="$(basename "$dependency")"
    existing="$(awk -F '\t' -v base="$base" '$2 == base { print $1; exit }' "$map_file")"
    if [[ -n "$existing" && "$existing" != "$dependency" ]]; then
      echo "error: dependency basename collision:" >&2
      echo "  $existing" >&2
      echo "  $dependency" >&2
      exit 1
    fi

    install -m 755 "$dependency" "$lib_dir/$base"
    chmod u+w "$lib_dir/$base"

    printf '%s\n' "$dependency" >> "$seen_file"
    printf '%s\t%s\n' "$dependency" "$base" >> "$map_file"
    printf '%s\n' "$lib_dir/$base" >> "$queue_file"
  done < <(list_portable_dependencies "$image")
done

if [[ ! -s "$map_file" ]]; then
  echo "error: no Homebrew dylib dependencies found to bundle" >&2
  exit 1
fi

if [[ -n "${RIME_LIB_DIR:-}" ]]; then
  install_name_tool -delete_rpath "$RIME_LIB_DIR" "$exe" 2>/dev/null || true
fi
if command -v brew >/dev/null 2>&1; then
  install_name_tool -delete_rpath "$(brew --prefix librime)/lib" "$exe" 2>/dev/null || true
fi
install_name_tool -add_rpath "@executable_path/lib" "$exe" 2>/dev/null || true

targets_file="$tmp_dir/targets"
printf '%s\n' "$exe" > "$targets_file"
find "$lib_dir" -maxdepth 1 -type f -name '*.dylib' -print | sort >> "$targets_file"

while IFS= read -r library; do
  [[ "$library" != "$exe" ]] || continue
  install_name_tool -id "@rpath/$(basename "$library")" "$library"
  install_name_tool -add_rpath "@loader_path" "$library" 2>/dev/null || true
done < "$targets_file"

while IFS= read -r target; do
  while IFS=$'\t' read -r old base; do
    install_name_tool -change "$old" "@rpath/$base" "$target" 2>/dev/null || true
  done < "$map_file"
done < "$targets_file"

while IFS= read -r target; do
  codesign --force --sign - "$target" >/dev/null
done < "$targets_file"

bad_links_file="$tmp_dir/bad-links"
: > "$bad_links_file"
while IFS= read -r target; do
  otool -L "$target" \
    | awk 'NR > 1 { print $1 }' \
    | grep -E '^(/opt/homebrew|/usr/local)/' \
    | sed "s#^#$target -> #" >> "$bad_links_file" || true
done < "$targets_file"

if [[ -s "$bad_links_file" ]]; then
  echo "error: portable package still has absolute Homebrew links:" >&2
  cat "$bad_links_file" >&2
  exit 1
fi

cat > "$portable_dir/run-listener.sh" <<'RUN_LISTENER'
#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

export RIME_SHARED_DATA_DIR="${RIME_SHARED_DATA_DIR:-$PWD/data/shared}"
export RIME_USER_DATA_DIR="${RIME_USER_DATA_DIR:-$PWD/data/user}"
export RIME_SCHEMA="${RIME_SCHEMA:-luna_pinyin_simp}"

exec "$PWD/pinyin" --listen "$@"
RUN_LISTENER
chmod +x "$portable_dir/run-listener.sh"

cat > "$portable_dir/run-listener-debug.sh" <<'RUN_LISTENER_DEBUG'
#!/usr/bin/env bash
set -euo pipefail
set -o pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

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
chmod +x "$portable_dir/run-listener-debug.sh"

bash scripts/collect-third-party-licenses.sh "$portable_dir" "$map_file"

export RIME_SHARED_DATA_DIR="$portable_dir/data/shared"
smoke_user_dir="$tmp_dir/smoke-user"
mkdir -p "$smoke_user_dir"
if [[ -f "$portable_dir/data/user/default.custom.yaml" ]]; then
  cp "$portable_dir/data/user/default.custom.yaml" "$smoke_user_dir/default.custom.yaml"
fi
export RIME_USER_DATA_DIR="$smoke_user_dir"
export RIME_SCHEMA=luna_pinyin_simp
"$exe" --body nihao >/dev/null

(
  cd "$PWD/dist"
  ditto -c -k --keepParent "$package_name" "$package_name.zip"
)

echo "Portable package:"
echo "  $portable_dir"
echo
echo "Portable zip:"
echo "  $zip_path"
