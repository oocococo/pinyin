#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
data_dir="${RIME_POC_DATA_DIR:-$root_dir/data}"
package_dir="$data_dir/packages"
shared_dir="$data_dir/shared"
user_dir="$data_dir/user"

mkdir -p "$package_dir" "$shared_dir" "$user_dir"

clone_or_update() {
  local name="$1"
  local url="$2"
  local dir="$package_dir/$name"

  if [ -d "$dir/.git" ]; then
    echo "updating $name"
    git -C "$dir" pull --ff-only
  else
    echo "cloning $name"
    git clone --depth 1 "$url" "$dir"
  fi
}

copy_package_files() {
  local dir="$1"

  find "$dir" -maxdepth 1 -type f \
    \( -name '*.yaml' -o -name '*.txt' -o -name '*.json' -o -name '*.ocd' \) \
    -exec cp {} "$shared_dir"/ \;

  if [ -d "$dir/opencc" ]; then
    mkdir -p "$shared_dir/opencc"
    cp -R "$dir/opencc"/. "$shared_dir/opencc"/
  fi
}

clone_or_update rime-prelude https://github.com/rime/rime-prelude.git
clone_or_update rime-essay https://github.com/rime/rime-essay.git
clone_or_update rime-luna-pinyin https://github.com/rime/rime-luna-pinyin.git
clone_or_update rime-pinyin-simp https://github.com/rime/rime-pinyin-simp.git

copy_package_files "$package_dir/rime-prelude"
copy_package_files "$package_dir/rime-essay"
copy_package_files "$package_dir/rime-luna-pinyin"
copy_package_files "$package_dir/rime-pinyin-simp"

cat > "$user_dir/default.custom.yaml" <<'YAML'
patch:
  schema_list:
    - schema: luna_pinyin_simp
    - schema: pinyin_simp
YAML

cat <<EOF

Rime data downloaded.
shared: $shared_dir
user:   $user_dir

export RIME_SHARED_DATA_DIR="$shared_dir"
export RIME_USER_DATA_DIR="$user_dir"
export RIME_SCHEMA=luna_pinyin_simp
EOF
