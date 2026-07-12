#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ $# -lt 3 || $# -gt 4 ]]; then
  echo "usage: $0 <archive.zip> <version> <arm64|x86_64> [--skip-run]" >&2
  exit 2
fi

archive="$1"
version="$2"
architecture="$3"
run_binary=1

if [[ "${4:-}" == "--skip-run" ]]; then
  run_binary=0
elif [[ -n "${4:-}" ]]; then
  echo "error: unknown option: $4" >&2
  exit 2
fi

case "$architecture" in
  arm64|x86_64) ;;
  *)
    echo "error: unsupported architecture: $architecture" >&2
    exit 2
    ;;
esac

if [[ ! -f "$archive" ]]; then
  echo "error: release archive not found: $archive" >&2
  exit 1
fi

for tool in codesign ditto file otool; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: required tool not found: $tool" >&2
    exit 1
  fi
done

package_name="pinyin-v${version}-macos-${architecture}"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/pinyin-release-check.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

ditto -x -k "$archive" "$tmp_dir"
package_dir="$tmp_dir/$package_name"
exe="$package_dir/pinyin"

for required in \
  "$exe" \
  "$package_dir/pinyin.toml" \
  "$package_dir/run-listener.sh" \
  "$package_dir/run-listener-debug.sh" \
  "$package_dir/README.txt" \
  "$package_dir/LICENSE" \
  "$package_dir/THIRD_PARTY_NOTICES.md" \
  "$package_dir/BUILD-MANIFEST.txt" \
  "$package_dir/data/shared" \
  "$package_dir/data/shared/opencc/t2s.json" \
  "$package_dir/data/shared/opencc/TSPhrases.ocd2" \
  "$package_dir/data/user" \
  "$package_dir/lib" \
  "$package_dir/LICENSES/homebrew" \
  "$package_dir/LICENSES/rime-data" \
  "$package_dir/LICENSES/rust"; do
  if [[ ! -e "$required" ]]; then
    echo "error: required package entry not found: ${required#"$package_dir/"}" >&2
    exit 1
  fi
done

if [[ ! -x "$exe" || ! -x "$package_dir/run-listener.sh" || ! -x "$package_dir/run-listener-debug.sh" ]]; then
  echo "error: package executables do not have execute permission" >&2
  exit 1
fi

file_output="$(file "$exe")"
if [[ "$file_output" != *"$architecture"* ]]; then
  echo "error: binary architecture mismatch" >&2
  echo "expected: $architecture" >&2
  echo "actual:   $file_output" >&2
  exit 1
fi

bad_links="$tmp_dir/bad-links"
: > "$bad_links"
while IFS= read -r target; do
  otool -L "$target" \
    | awk 'NR > 1 { print $1 }' \
    | grep -E '^(/opt/homebrew|/usr/local)/' \
    | sed "s#^#$target -> #" >> "$bad_links" || true
done < <(find "$package_dir" -type f \( -name pinyin -o -name '*.dylib' \) | sort)

if [[ -s "$bad_links" ]]; then
  echo "error: archive contains absolute Homebrew links:" >&2
  cat "$bad_links" >&2
  exit 1
fi

grep -Fqx "trigger_prefix = \"''\"" "$package_dir/pinyin.toml"
grep -Fqx "trigger_suffix = \"''\"" "$package_dir/pinyin.toml"
grep -Fq "version: $version" "$package_dir/BUILD-MANIFEST.txt"
grep -Fq "architecture: $architecture" "$package_dir/BUILD-MANIFEST.txt"
grep -Fq "''nihao<空格>shijie<空格>''" "$package_dir/README.txt"

for license_group in homebrew rime-data rust; do
  if ! find "$package_dir/LICENSES/$license_group" -type f -print -quit | grep -q .; then
    echo "error: empty license group: $license_group" >&2
    exit 1
  fi
done

codesign --verify --deep --strict "$exe"

if [[ "$run_binary" -eq 1 ]]; then
  case "$(uname -m):$architecture" in
    arm64:arm64|x86_64:x86_64) ;;
    *)
      echo "error: cannot execute $architecture binary on $(uname -m); use --skip-run" >&2
      exit 1
      ;;
  esac

  export RIME_SHARED_DATA_DIR="$package_dir/data/shared"
  export RIME_USER_DATA_DIR="$package_dir/data/user"
  export RIME_SCHEMA=luna_pinyin_simp
  output="$("$exe" --doctor --body nihao)"
  actual="$(printf '%s\n' "$output" | sed -n 's/^output:[[:space:]]*//p')"
  if [[ "$actual" != "你好" ]]; then
    echo "error: packaged conversion failed" >&2
    echo "expected: 你好" >&2
    echo "actual:   $actual" >&2
    echo "$output" >&2
    exit 1
  fi
fi

echo "release package verified: $archive"
