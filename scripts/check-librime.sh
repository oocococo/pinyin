#!/usr/bin/env bash
set -u

if ! command -v brew >/dev/null 2>&1; then
  echo "brew: not found"
  exit 1
fi

if ! brew list --versions librime >/dev/null 2>&1; then
  echo "librime: not installed"
  echo "install: brew install librime"
  exit 1
fi

prefix="$(brew --prefix librime)"
include_dir="${RIME_INCLUDE_DIR:-$prefix/include}"
lib_dir="${RIME_LIB_DIR:-$prefix/lib}"

echo "librime prefix: $prefix"
echo "include dir:    $include_dir"
echo "lib dir:        $lib_dir"

missing=0

if [ ! -f "$include_dir/rime_api.h" ]; then
  echo "missing: $include_dir/rime_api.h"
  missing=1
else
  echo "found:   $include_dir/rime_api.h"
fi

if [ ! -f "$lib_dir/librime.dylib" ] && [ ! -f "$lib_dir/librime.a" ]; then
  echo "missing: $lib_dir/librime.dylib or $lib_dir/librime.a"
  missing=1
else
  echo "found:   librime library"
fi

if [ "$missing" -ne 0 ]; then
  echo "try: brew reinstall librime"
  exit 1
fi

cat <<EOF

export RIME_INCLUDE_DIR="$include_dir"
export RIME_LIB_DIR="$lib_dir"
EOF
