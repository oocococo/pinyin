#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: portable macOS packaging must run on macOS" >&2
  exit 1
fi

for tool in otool install_name_tool ditto codesign; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: required tool not found: $tool" >&2
    exit 1
  fi
done

bash scripts/package-binary.sh

base_dir="$PWD/dist/rime-poc-macos"
portable_dir="$PWD/dist/rime-poc-macos-portable"
zip_path="$PWD/dist/rime-poc-macos-portable.zip"
lib_dir="$portable_dir/lib"
exe="$portable_dir/rime-poc"

rm -rf "$portable_dir" "$zip_path"
ditto "$base_dir" "$portable_dir"
mkdir -p "$lib_dir"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/rime-poc-portable.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

queue_file="$tmp_dir/queue"
seen_file="$tmp_dir/seen"
map_file="$tmp_dir/dep-map"
: > "$seen_file"
: > "$map_file"
printf '%s\n' "$exe" > "$queue_file"

portable_dep() {
  case "$1" in
    /opt/homebrew/*|/usr/local/*) return 0 ;;
    *) return 1 ;;
  esac
}

list_portable_deps() {
  otool -L "$1" | awk 'NR > 1 { print $1 }' | while IFS= read -r dep; do
    if portable_dep "$dep"; then
      printf '%s\n' "$dep"
    fi
  done
}

while [[ -s "$queue_file" ]]; do
  image="$(sed -n '1p' "$queue_file")"
  sed '1d' "$queue_file" > "$queue_file.next"
  mv "$queue_file.next" "$queue_file"

  while IFS= read -r dep; do
    [[ -n "$dep" ]] || continue

    if [[ ! -f "$dep" ]]; then
      echo "error: dependency not found: $dep" >&2
      exit 1
    fi

    if grep -Fqx "$dep" "$seen_file"; then
      continue
    fi

    base="$(basename "$dep")"
    existing="$(
      awk -F '\t' -v base="$base" '$2 == base { print $1; exit }' "$map_file"
    )"
    if [[ -n "$existing" && "$existing" != "$dep" ]]; then
      echo "error: dependency basename collision:" >&2
      echo "  $existing" >&2
      echo "  $dep" >&2
      exit 1
    fi

    install -m 755 "$dep" "$lib_dir/$base"
    chmod u+w "$lib_dir/$base"

    printf '%s\n' "$dep" >> "$seen_file"
    printf '%s\t%s\n' "$dep" "$base" >> "$map_file"
    printf '%s\n' "$lib_dir/$base" >> "$queue_file"
  done < <(list_portable_deps "$image")
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

while IFS= read -r lib; do
  [[ "$lib" != "$exe" ]] || continue
  install_name_tool -id "@rpath/$(basename "$lib")" "$lib"
  install_name_tool -add_rpath "@loader_path" "$lib" 2>/dev/null || true
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

exec "$PWD/rime-poc" --listen "$@"
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
export RIME_POC_NATIVE_LOG_EVENTS="${RIME_POC_NATIVE_LOG_EVENTS:-1}"

log_dir="${RIME_POC_LOG_DIR:-$PWD/logs}"
mkdir -p "$log_dir"
log_file="${RIME_POC_LOG_FILE:-$log_dir/rime-poc-listener-$(date +%Y%m%d-%H%M%S).log}"

echo "rime-poc debug listener log:"
echo "  $log_file"
echo

"$PWD/rime-poc" --doctor --listen --log-events "$@" 2>&1 | tee -a "$log_file"
RUN_LISTENER_DEBUG
chmod +x "$portable_dir/run-listener-debug.sh"

cat > "$portable_dir/README.txt" <<'README'
rime-poc portable macOS package

This directory bundles:
  - rime-poc
  - librime and Homebrew dylib dependencies under lib/
  - Rime shared/user data under data/
  - rime-poc.toml trigger and conversion config

No Homebrew install is required on the target Mac.

1. Grant macOS permissions to this binary:
   rime-poc-macos-portable/rime-poc

   Required permissions:
   - Privacy & Security > Accessibility
   - Privacy & Security > Input Monitoring

2. Start the listener:
   ./run-listener.sh

   For diagnosis with event logs:
   ./run-listener-debug.sh

3. Type a trigger in any text field:
   ;;woyaoceshizhongwenshurufa,nihaoma!;;

Expected output:
   我要测试中文输入法，你好吗！

Experimental mixed Chinese/English mode:
   ./run-listener.sh --conversion-mode rime-auto

Or edit rime-poc.toml:
   conversion_mode = "rime-auto"

Notes:
  - macOS permissions cannot be pre-granted by this package.
  - If Gatekeeper blocks the binary, a signed/notarized .app/.dmg is the next
    packaging step.
README

"$exe" --body 'woyaoceshi,nihaoma!' >/dev/null
"$exe" --body --conversion-mode rime-auto 'wo ai OpenAI,yong Rust kaifa' >/dev/null

(
  cd "$PWD/dist"
  ditto -c -k --keepParent "$(basename "$portable_dir")" "$(basename "$zip_path")"
)

echo "Portable package:"
echo "  $portable_dir"
echo
echo "Portable zip:"
echo "  $zip_path"
echo
echo "Bundled dylibs:"
awk -F '\t' '{ print "  " $2 }' "$map_file" | sort
