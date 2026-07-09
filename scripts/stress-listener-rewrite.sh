#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "skip: listener stress test requires macOS" >&2
  exit 77
fi

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
  sed -n "s/^[[:space:]]*$key[[:space:]]*=[[:space:]]*\"\\(.*\\)\"[[:space:]]*$/\\1/p" rime-poc.toml | head -n 1
}

export RIME_INCLUDE_DIR="${RIME_INCLUDE_DIR:-$(brew --prefix librime)/include}"
export RIME_LIB_DIR="${RIME_LIB_DIR:-$(brew --prefix librime)/lib}"
export RIME_SHARED_DATA_DIR="${RIME_SHARED_DATA_DIR:-$(default_shared_data_dir)}"
export RIME_SCHEMA="${RIME_SCHEMA:-luna_pinyin_simp}"
export CARGO_HOME="${CARGO_HOME:-/private/tmp/pal-cargo-home-rime-poc}"
export RIME_POC_SKIP_OPEN_SETTINGS=1
export RIME_POC_NATIVE_LOG_EVENTS=1

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

trigger_prefix="${RIME_POC_TEST_TRIGGER_PREFIX:-$(toml_string trigger_prefix)}"
if [[ -z "$trigger_prefix" ]]; then
  echo "error: unable to read trigger_prefix from rime-poc.toml" >&2
  exit 1
fi

cleanup_user_data_dir=""
if [[ -z "${RIME_USER_DATA_DIR:-}" ]]; then
  cleanup_user_data_dir="$(mktemp -d "${TMPDIR:-/tmp}/rime-poc-stress-user.XXXXXX")"
  export RIME_USER_DATA_DIR="$cleanup_user_data_dir"
  if [[ -f "$PWD/data/user/default.custom.yaml" ]]; then
    cp "$PWD/data/user/default.custom.yaml" "$RIME_USER_DATA_DIR/default.custom.yaml"
  fi
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/rime-poc-stress.XXXXXX")"
listener_log="$work_dir/listener.log"
actual_file="$work_dir/actual.txt"
expected_file="$work_dir/expected.txt"
input_file="$work_dir/input.txt"
pid_file="$work_dir/listener.pid"
input_source_helper="$work_dir/select-system-input-source"

cleanup() {
  if [[ -f "$pid_file" ]]; then
    local pid
    pid="$(cat "$pid_file" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  fi
  osascript -e 'tell application "TextEdit" to if (count of documents) > 0 then close front document saving no' >/dev/null 2>&1 || true
  if [[ -n "$cleanup_user_data_dir" ]]; then
    rm -rf "$cleanup_user_data_dir"
  fi
  if [[ "${RIME_POC_KEEP_STRESS_ARTIFACTS:-0}" != "1" ]]; then
    rm -rf "$work_dir"
  else
    echo "kept stress artifacts: $work_dir" >&2
  fi
}
trap cleanup EXIT

cat > "$work_dir/select-system-input-source.mm" <<'MM'
#import <Carbon/Carbon.h>
#import <Foundation/Foundation.h>

static BOOL select_source_by_id(NSString *source_id) {
  NSDictionary *filter = @{ (__bridge NSString *)kTISPropertyInputSourceID: source_id };
  NSArray *sources = CFBridgingRelease(TISCreateInputSourceList((__bridge CFDictionaryRef)filter, false));
  for (id item in sources) {
    TISInputSourceRef source = (__bridge TISInputSourceRef)item;
    if (TISSelectInputSource(source) == noErr) {
      fprintf(stderr, "selected input source: %s\n", source_id.UTF8String);
      return YES;
    }
  }
  return NO;
}

int main(void) {
  @autoreleasepool {
    NSArray<NSString *> *preferred = @[
      @"com.apple.keylayout.ABC",
      @"com.apple.keylayout.US"
    ];
    for (NSString *source_id in preferred) {
      if (select_source_by_id(source_id)) {
        return 0;
      }
    }

    NSDictionary *filter = @{
      (__bridge NSString *)kTISPropertyInputSourceType: (__bridge NSString *)kTISTypeKeyboardLayout,
      (__bridge NSString *)kTISPropertyInputSourceIsASCIICapable: @YES,
    };
    NSArray *sources = CFBridgingRelease(TISCreateInputSourceList((__bridge CFDictionaryRef)filter, false));
    for (id item in sources) {
      TISInputSourceRef source = (__bridge TISInputSourceRef)item;
      CFStringRef source_id = (CFStringRef)TISGetInputSourceProperty(
          source,
          kTISPropertyInputSourceID);
      if (source_id != NULL &&
          CFStringHasPrefix(source_id, CFSTR("com.apple.")) &&
          TISSelectInputSource(source) == noErr) {
        char buffer[256] = {};
        CFStringGetCString(source_id, buffer, sizeof(buffer), kCFStringEncodingUTF8);
        fprintf(stderr, "selected input source: %s\n", buffer);
        return 0;
      }
    }
  }

  fprintf(stderr, "error: unable to select an Apple ASCII keyboard layout\n");
  return 1;
}
MM

clang++ "$work_dir/select-system-input-source.mm" \
  -fobjc-arc \
  -framework Carbon \
  -framework Foundation \
  -o "$input_source_helper"
"$input_source_helper"

groups=(
  jdaskldj asjld ajsld jasld jasd jasd lkasld aslkj dajd akljd
  aldj alsdj alksjd alksjd asdlj jasdkl aslkdj lakjsd jaskldj
  qwe nihao woyaoceshi zhongwenshurufa zaijian ceshi shurufa
)

printf '%s' "$trigger_prefix" > "$input_file"
for group in "${groups[@]}"; do
  printf '%s ' "$group" >> "$input_file"
done

expected=""
for group in "${groups[@]}"; do
  output="$(cargo run --quiet -- --body "$group")"
  actual="$(printf '%s\n' "$output" | sed -n 's/^output:[[:space:]]*//p')"
  expected+="$actual"
done
printf '%s' "$expected" > "$expected_file"

cargo build --quiet

target/debug/rime-poc --listen --log-events --inject-delay-ms "${RIME_POC_STRESS_INJECT_DELAY_MS:-1}" >"$listener_log" 2>&1 &
listener_pid="$!"
echo "$listener_pid" > "$pid_file"

for _ in {1..80}; do
  if grep -q "rime-poc listener started" "$listener_log"; then
    break
  fi
  if ! kill -0 "$listener_pid" 2>/dev/null; then
    echo "listener exited before startup" >&2
    cat "$listener_log" >&2
    exit 1
  fi
  sleep 0.1
done

if ! grep -q "rime-poc listener started" "$listener_log"; then
  echo "listener did not start in time" >&2
  cat "$listener_log" >&2
  exit 1
fi

python3 - "$input_file" "$actual_file" "$expected_file" "$input_source_helper" <<'PY'
import pathlib
import subprocess
import sys
import time
import os

input_text = pathlib.Path(sys.argv[1]).read_text()
actual_path = pathlib.Path(sys.argv[2])
expected_text = pathlib.Path(sys.argv[3]).read_text()
input_source_helper = sys.argv[4]
settle_seconds = float(os.environ.get("RIME_POC_STRESS_SETTLE_SECONDS", "30"))

prepare_script = r'''
  tell application "TextEdit"
    activate
    make new document
    set text of front document to ""
  end tell
  delay 0.2
'''

type_script = r'''
on run argv
  set inputText to item 1 of argv
  tell application "System Events"
    tell process "TextEdit"
      set frontmost to true
    end tell
    keystroke inputText
  end tell
end run
'''

subprocess.run(["osascript", "-e", prepare_script], check=True)
subprocess.run([input_source_helper], check=True)
subprocess.run(["osascript", "-e", prepare_script], check=True)
subprocess.run([input_source_helper], check=True)
subprocess.run(["osascript", "-e", type_script, input_text], check=True)

read_script = r'''
tell application "TextEdit"
  if (count of documents) is 0 then
    return ""
  end if
  return text of front document
end tell
'''
deadline = time.monotonic() + settle_seconds
actual = ""
while True:
    result = subprocess.run(
        ["osascript", "-e", read_script],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    actual = result.stdout.rstrip("\n")
    actual_path.write_text(actual)
    if actual == expected_text or time.monotonic() >= deadline:
        break
    time.sleep(0.25)
PY

actual="$(cat "$actual_file")"
expected="$(cat "$expected_file")"

if [[ "$actual" != "$expected" ]]; then
  echo "stress listener rewrite test failed" >&2
  echo "input:    $(cat "$input_file")" >&2
  echo "expected: $expected" >&2
  echo "actual:   $actual" >&2
  echo "listener log: $listener_log" >&2
  echo "--- listener log tail ---" >&2
  tail -n 220 "$listener_log" >&2 || true
  RIME_POC_KEEP_STRESS_ARTIFACTS=1
  exit 1
fi

echo "stress listener rewrite test passed"
