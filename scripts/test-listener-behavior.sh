#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "skip: listener behavior test requires macOS" >&2
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

export RIME_INCLUDE_DIR="${RIME_INCLUDE_DIR:-$(brew --prefix librime)/include}"
export RIME_LIB_DIR="${RIME_LIB_DIR:-$(brew --prefix librime)/lib}"
export RIME_SHARED_DATA_DIR="${RIME_SHARED_DATA_DIR:-$(default_shared_data_dir)}"
export RIME_SCHEMA="${RIME_SCHEMA:-luna_pinyin_simp}"
export CARGO_HOME="${CARGO_HOME:-/private/tmp/pal-cargo-home-rime-poc}"
export RIME_POC_SKIP_OPEN_SETTINGS=1
export RIME_POC_NATIVE_LOG_EVENTS=1

if [[ ! -f "$RIME_INCLUDE_DIR/rime_api.h" ]]; then
  echo "error: rime_api.h not found under RIME_INCLUDE_DIR=$RIME_INCLUDE_DIR" >&2
  exit 1
fi

if [[ ! -d "$RIME_SHARED_DATA_DIR" ]]; then
  echo "error: Rime shared data dir does not exist: $RIME_SHARED_DATA_DIR" >&2
  echo "hint: bash scripts/download-rime-data.sh" >&2
  exit 1
fi

trigger_prefix="${RIME_POC_TEST_TRIGGER_PREFIX:-;;}"
if [[ -z "$trigger_prefix" ]]; then
  echo "error: RIME_POC_TEST_TRIGGER_PREFIX must not be empty" >&2
  exit 1
fi

if [[ "${RIME_POC_ALLOW_EXISTING_LISTENER:-0}" != "1" ]]; then
  existing_listeners="$(pgrep -fl "[r]ime-poc --listen" || true)"
  if [[ -n "$existing_listeners" ]]; then
    echo "error: an existing rime-poc listener is already running" >&2
    echo "$existing_listeners" >&2
    echo "hint: stop it first, or set RIME_POC_ALLOW_EXISTING_LISTENER=1 if isolation is not required" >&2
    exit 1
  fi
fi

cleanup_user_data_dir=""
if [[ -z "${RIME_USER_DATA_DIR:-}" ]]; then
  cleanup_user_data_dir="$(mktemp -d "${TMPDIR:-/tmp}/rime-poc-behavior-user.XXXXXX")"
  export RIME_USER_DATA_DIR="$cleanup_user_data_dir"
  if [[ -f "$PWD/data/user/default.custom.yaml" ]]; then
    cp "$PWD/data/user/default.custom.yaml" "$RIME_USER_DATA_DIR/default.custom.yaml"
  fi
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/rime-poc-behavior.XXXXXX")"
listener_log="$work_dir/listener.log"
pid_file="$work_dir/listener.pid"
input_source_helper="$work_dir/select-system-input-source"
listener_config="$work_dir/rime-poc.toml"

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
  if [[ "${RIME_POC_KEEP_BEHAVIOR_ARTIFACTS:-0}" != "1" ]]; then
    rm -rf "$work_dir"
  else
    echo "kept behavior artifacts: $work_dir" >&2
  fi
}
trap cleanup EXIT

python3 - "$PWD/rime-poc.toml" "$listener_config" "$trigger_prefix" <<'PY'
import json
import re
import sys

source_path, destination_path, trigger = sys.argv[1:]
with open(source_path, encoding="utf-8") as source:
    config = source.read()

encoded_trigger = json.dumps(trigger, ensure_ascii=False)
config = re.sub(
    r'(?m)^trigger_prefix\s*=.*$',
    f"trigger_prefix = {encoded_trigger}",
    config,
    count=1,
)
config = re.sub(
    r'(?m)^trigger_suffix\s*=.*$',
    f"trigger_suffix = {encoded_trigger}",
    config,
    count=1,
)
with open(destination_path, "w", encoding="utf-8") as destination:
    destination.write(config)
PY
export RIME_POC_CONFIG="$listener_config"

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
    if (select_source_by_id(@"com.apple.keylayout.ABC") ||
        select_source_by_id(@"com.apple.keylayout.US")) {
      return 0;
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

expected_punctuation="$(cargo run --quiet -- --body 'woyaoceshi,' | sed -n 's/^output:[[:space:]]*//p')"
expected_backspace="$(cargo run --quiet -- --body 'le' | sed -n 's/^output:[[:space:]]*//p')"
expected_first_segment="$(cargo run --quiet -- --body 'woyao' | sed -n 's/^output:[[:space:]]*//p')"
expected_second_segment="$(cargo run --quiet -- --body 'ceshi' | sed -n 's/^output:[[:space:]]*//p')"

cargo build --quiet

target/debug/rime-poc --listen --log-events --inject-delay-ms "${RIME_POC_BEHAVIOR_INJECT_DELAY_MS:-1}" >"$listener_log" 2>&1 &
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

python3 - \
  "$trigger_prefix" \
  "$expected_punctuation" \
  "$expected_backspace" \
  "$expected_first_segment" \
  "$expected_second_segment" \
  "$input_source_helper" <<'PY'
import os
import subprocess
import sys
import time

trigger_prefix = sys.argv[1]
expected_punctuation = sys.argv[2]
expected_backspace = sys.argv[3]
expected_first_segment = sys.argv[4]
expected_second_segment = sys.argv[5]
input_source_helper = sys.argv[6]
settle_seconds = float(os.environ.get("RIME_POC_BEHAVIOR_SETTLE_SECONDS", "10"))
exit_backspaces = max(1, len(trigger_prefix))
sentinel = "1234"

prepare_script = r'''
tell application "TextEdit"
  activate
  make new document
  set text of front document to ""
end tell
delay 0.2
'''

clear_script = r'''
tell application "TextEdit"
  set text of front document to ""
end tell
'''

read_script = r'''
tell application "TextEdit"
  if (count of documents) is 0 then
    return ""
  end if
  return text of front document
end tell
'''

def run_osascript(script, *args):
    return subprocess.run(
        ["osascript", "-e", script, *args],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.rstrip("\n")

def select_abc():
    subprocess.run([input_source_helper], check=True)

def read_text():
    return run_osascript(read_script)

def wait_for(expected):
    deadline = time.monotonic() + settle_seconds
    actual = ""
    while True:
        actual = read_text()
        if actual == expected or time.monotonic() >= deadline:
            return actual
        time.sleep(0.25)

def type_text(value):
    script = r'''
on run argv
  tell application "System Events"
    tell process "TextEdit"
      set frontmost to true
    end tell
    keystroke (item 1 of argv)
  end tell
end run
'''
    run_osascript(script, value)

def press_backspace(count):
    script = r'''
on run argv
  set keyCount to (item 1 of argv) as integer
  tell application "System Events"
    tell process "TextEdit"
      set frontmost to true
    end tell
    repeat keyCount times
      key code 51
    end repeat
  end tell
end run
'''
    run_osascript(script, str(count))

def press_escape():
    script = r'''
tell application "System Events"
  tell process "TextEdit"
    set frontmost to true
  end tell
  key code 53
end tell
'''
    run_osascript(script)

def start_session_after_sentinel():
    run_osascript(clear_script)
    select_abc()
    type_text(sentinel + trigger_prefix)
    time.sleep(0.5)

run_osascript(prepare_script)
select_abc()
run_osascript(clear_script)
select_abc()

type_text(trigger_prefix + "woyaoceshi,")
actual = wait_for(expected_punctuation)
if actual != expected_punctuation:
    raise SystemExit(
        "punctuation separator case failed\n"
        f"expected: {expected_punctuation}\n"
        f"actual:   {actual}"
    )

press_escape()
run_osascript(clear_script)
select_abc()
type_text(trigger_prefix)
time.sleep(0.5)
type_text("bug")
press_backspace(3)
type_text("le ")
actual = wait_for(expected_backspace)
if actual != expected_backspace:
    raise SystemExit(
        "backspace empty-buffer case failed\n"
        f"expected: {expected_backspace}\n"
        f"actual:   {actual}"
    )

run_osascript(clear_script)
select_abc()
type_text(trigger_prefix)
time.sleep(0.5)
type_text("bug")
press_backspace(3 + exit_backspaces)
type_text("le ")
actual = wait_for("le ")
if actual not in {"le ", "Le "}:
    raise SystemExit(
        "backspace on empty-buffer exit case failed\n"
        "expected: le  or Le \n"
        f"actual:   {actual}"
    )

start_session_after_sentinel()
type_text("woyao ")
actual = wait_for(sentinel + expected_first_segment)
if actual != sentinel + expected_first_segment:
    raise SystemExit(
        "first committed segment setup failed\n"
        f"expected: {sentinel + expected_first_segment}\n"
        f"actual:   {actual}"
    )

type_text("ceshi ")
actual = wait_for(sentinel + expected_first_segment + expected_second_segment)
if actual != sentinel + expected_first_segment + expected_second_segment:
    raise SystemExit(
        "second committed segment setup failed\n"
        f"expected: {sentinel + expected_first_segment + expected_second_segment}\n"
        f"actual:   {actual}"
    )

press_backspace(1)
actual = wait_for(sentinel + expected_first_segment + "ceshi")
if actual != sentinel + expected_first_segment + "ceshi":
    raise SystemExit(
        "latest conversion restore failed\n"
        f"expected: {sentinel + expected_first_segment + 'ceshi'}\n"
        f"actual:   {actual}"
    )

press_backspace(len("ceshi") + len(expected_first_segment))
actual = wait_for(sentinel)
if actual != sentinel:
    raise SystemExit(
        "deleting all visible session text failed\n"
        f"expected: {sentinel}\n"
        f"actual:   {actual}"
    )

type_text("le ")
actual = wait_for(sentinel + expected_backspace)
if actual != sentinel + expected_backspace:
    raise SystemExit(
        "session exited before hidden prefix deletion\n"
        f"expected: {sentinel + expected_backspace}\n"
        f"actual:   {actual}"
    )

press_escape()
start_session_after_sentinel()
type_text("woyao ")
actual = wait_for(sentinel + expected_first_segment)
if actual != sentinel + expected_first_segment:
    raise SystemExit("hidden-prefix consumption setup failed")

press_backspace(1)
actual = wait_for(sentinel + "woyao")
if actual != sentinel + "woyao":
    raise SystemExit("hidden-prefix conversion restore failed")
press_backspace(len("woyao"))
actual = wait_for(sentinel)
if actual != sentinel:
    raise SystemExit("hidden-prefix raw deletion failed")
press_backspace(exit_backspaces)
actual = wait_for(sentinel)
if actual != sentinel:
    raise SystemExit(
        "hidden-prefix Backspaces deleted text before the trigger\n"
        f"expected: {sentinel}\n"
        f"actual:   {actual}"
    )

type_text("le ")
actual = wait_for(sentinel + "le ")
if actual not in {sentinel + "le ", sentinel + "Le "}:
    raise SystemExit(
        "session did not exit after hidden prefix deletion\n"
        f"expected: {sentinel + 'le '} or {sentinel + 'Le '}\n"
        f"actual:   {actual}"
    )

start_session_after_sentinel()
type_text("vke ")
actual = wait_for(sentinel + "vke")
if actual != sentinel + "vke":
    raise SystemExit(
        "invalid pinyin fallback did not preserve raw text\n"
        f"expected: {sentinel + 'vke'}\n"
        f"actual:   {actual}"
    )
time.sleep(0.5)
type_text("abc")
actual = wait_for(sentinel + "vkeabc")
if actual != sentinel + "vkeabc":
    raise SystemExit(
        "keys after invalid pinyin were swallowed\n"
        f"expected: {sentinel + 'vkeabc'}\n"
        f"actual:   {actual}"
    )
PY

echo "listener behavior test passed"
