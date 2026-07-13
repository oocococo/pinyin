#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <package-dir> <dependency-map>" >&2
  exit 2
fi

package_dir="$1"
dependency_map="$2"
licenses_dir="$package_dir/LICENSES"
manifest="$package_dir/BUILD-MANIFEST.txt"

for tool in brew cargo find jq realpath; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: required tool not found: $tool" >&2
    exit 1
  fi
done

if [[ ! -f "$dependency_map" ]]; then
  echo "error: dependency map not found: $dependency_map" >&2
  exit 1
fi

rm -rf "$licenses_dir"
mkdir -p "$licenses_dir/homebrew" "$licenses_dir/rime-data" "$licenses_dir/rust"

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
architecture="$(uname -m)"

cat > "$manifest" <<EOF
pinyin build manifest
version: $version
architecture: $architecture
built_at_utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)

Homebrew runtime formulas:
EOF

formula_file="$(mktemp "${TMPDIR:-/tmp}/pinyin-formulas.XXXXXX")"
trap 'rm -f "$formula_file"' EXIT

while IFS=$'\t' read -r dependency _base; do
  [[ -n "$dependency" ]] || continue
  resolved="$(realpath "$dependency")"
  case "$resolved" in
    */Cellar/*)
      formula_and_rest="${resolved#*/Cellar/}"
      printf '%s\n' "${formula_and_rest%%/*}" >> "$formula_file"
      ;;
    *)
      echo "error: cannot map Homebrew dependency to a formula: $dependency" >&2
      exit 1
      ;;
  esac
done < "$dependency_map"

sort -u "$formula_file" -o "$formula_file"

while IFS= read -r formula; do
  [[ -n "$formula" ]] || continue
  prefix="$(brew --prefix "$formula")"
  info="$(brew info --json=v2 "$formula")"
  formula_version="$(printf '%s' "$info" | jq -r '.formulae[0].installed[0].version // .formulae[0].versions.stable')"
  formula_license="$(printf '%s' "$info" | jq -r '.formulae[0].license // "unknown"')"
  formula_homepage="$(printf '%s' "$info" | jq -r '.formulae[0].homepage // "unknown"')"
  destination="$licenses_dir/homebrew/$formula"
  mkdir -p "$destination"

  found=0
  while IFS= read -r license_file; do
    [[ -n "$license_file" ]] || continue
    cp "$license_file" "$destination/$(basename "$license_file")"
    found=1
  done < <(
    find -L "$prefix" -maxdepth 4 -type f \
      \( -iname 'LICENSE*' -o -iname 'COPYING*' -o -iname 'NOTICE*' \) \
      2>/dev/null | sort
  )

  if [[ "$found" -eq 0 && "$formula" == "boost" ]]; then
    cp licenses/BSL-1.0.txt "$destination/LICENSE.txt"
    found=1
  fi

  if [[ "$found" -eq 0 ]]; then
    echo "error: no license text found for Homebrew formula: $formula" >&2
    exit 1
  fi

  printf '  - %s %s | %s | %s\n' \
    "$formula" "$formula_version" "$formula_license" "$formula_homepage" >> "$manifest"
done < "$formula_file"

cat >> "$manifest" <<'EOF'

Rime data repositories:
EOF

for repository in rime-prelude rime-essay rime-luna-pinyin rime-pinyin-simp; do
  source_dir="data/packages/$repository"
  license_file="$source_dir/LICENSE"
  if [[ ! -f "$license_file" || ! -d "$source_dir/.git" ]]; then
    echo "error: missing source revision or license for $repository" >&2
    exit 1
  fi
  revision="$(git -C "$source_dir" rev-parse HEAD)"
  mkdir -p "$licenses_dir/rime-data/$repository"
  cp "$license_file" "$licenses_dir/rime-data/$repository/LICENSE"
  printf '  - %s | revision %s | https://github.com/rime/%s\n' \
    "$repository" "$revision" "$repository" >> "$manifest"
done

cat >> "$manifest" <<'EOF'

Rust crates:
EOF

cargo metadata --format-version 1 --locked \
  | jq -r '.packages[] | select(.name != "pinyin") | [.name, .version, (.license // "unknown"), .manifest_path] | @tsv' \
  | while IFS=$'\t' read -r crate crate_version crate_license manifest_path; do
      source_dir="${manifest_path%/Cargo.toml}"
      destination="$licenses_dir/rust/${crate}-${crate_version}"
      mkdir -p "$destination"
      found=0
      while IFS= read -r license_file; do
        [[ -n "$license_file" ]] || continue
        cp "$license_file" "$destination/$(basename "$license_file")"
        found=1
      done < <(
        find "$source_dir" -maxdepth 2 -type f \
          \( -iname 'LICENSE*' -o -iname 'COPYING*' -o -iname 'NOTICE*' \) \
          2>/dev/null | sort
      )
      if [[ "$found" -eq 0 ]]; then
        echo "error: no license text found for Rust crate: $crate $crate_version" >&2
        exit 1
      fi
      printf '  - %s %s | %s\n' "$crate" "$crate_version" "$crate_license" >> "$manifest"
    done

echo "Collected third-party licenses under $licenses_dir"
