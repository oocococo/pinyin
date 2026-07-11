# rime-poc

Minimal PoC for calling `librime` from Rust, converting captured pinyin into
the first Rime candidate, and running a macOS listener that deletes the trigger
text and injects the converted result.

This is intentionally separate from the espanso workspace. It only validates
the first usable flow before moving anything into a real product repository.

## Prerequisites

Install `librime` and Rime schema data first. On macOS, the fastest dev setup is:

```sh
brew install librime
```

Check that it is actually installed before exporting paths. `brew --prefix
librime` can print the theoretical prefix even when the formula is not
installed.

```sh
brew list --versions librime
test -f "$(brew --prefix librime)/include/rime_api.h"
test -f "$(brew --prefix librime)/lib/librime.dylib"
```

The Rust FFI crate looks in `/usr/include` and `/usr/lib` by default. For a
Homebrew install, point it at the formula paths when building:

```sh
export RIME_INCLUDE_DIR="$(brew --prefix librime)/include"
export RIME_LIB_DIR="$(brew --prefix librime)/lib"
```

You also need Rime data such as `prelude`, `essay`, and either `luna-pinyin` or
`pinyin-simp`. Point the CLI at those directories with:

```sh
export RIME_SHARED_DATA_DIR=/path/to/rime-data
export RIME_USER_DATA_DIR="$HOME/Library/Rime"
export RIME_SCHEMA=luna_pinyin_simp
```

When using Squirrel's installed data, the shared directory is commonly:

```sh
/Library/Input Methods/Squirrel.app/Contents/SharedSupport
```

## Trigger config

The first-version matcher reads trigger strings from `rime-poc.toml` in the
current directory, or from `--config <FILE>` / `RIME_POC_CONFIG`:

```toml
trigger_prefix = ";;"
trigger_suffix = ";;"
candidate_layout = "horizontal"
candidate_count = 5
candidate_select_keys = "1234567890"
candidate_page_next_key = "="
candidate_page_previous_key = "-"
english_commit_key = "`"
```

Trigger strings cannot contain punctuation that is reserved for separating
pinyin runs inside the body: comma, period, question mark, exclamation mark, minus, plus,
ellipsis, tilde, or double quote, including common Chinese/full-width variants.
The listener candidate UI supports `candidate_layout = "horizontal"` or
`"vertical"`. It shows up to `candidate_count` candidates and labels them with
`candidate_select_keys`; the count cannot exceed the number of configured
selection keys. The defaults are a horizontal list with 5 candidates, numeric
selection keys `1234567890`, `=` for the next page, `-` for the previous page,
and backtick for committing pending input as raw English.

## Usage

```sh
cargo run -- ";;woyaoceshizhongwenshurufa,nihaoma?\"hao...zaijian\";;"
```

Useful flags:

```sh
cargo run -- --doctor
cargo run -- --body "woyaoceshizhongwenshurufa,nihaoma"
cargo run -- --config ./rime-poc.toml --schema pinyin_simp --shared-data-dir /path/to/rime-data --user-data-dir "$HOME/Library/Rime" ";;woyaoceshi;;"
```

Run the dependency check without compiling Rust:

```sh
bash scripts/check-librime.sh
```

Run the full local conversion smoke test:

```sh
bash scripts/test-conversion.sh
```

Or pass one trigger text to test a single case:

```sh
bash scripts/test-conversion.sh ';;"hao","zaijian";;'
```

Start the macOS listener:

```sh
bash scripts/run-listener.sh
```

The listener uses Cocoa global keyboard monitoring and CGEvent injection. macOS
must grant Accessibility permission to the terminal app or compiled binary. If
permission is missing, the CLI opens the Accessibility settings page, prints the
exact next step, and waits for you to press Enter after granting permission. When
it is running, type a configured trigger such as:

```text
;;woyaoceshizhongwenshurufa,nihaoma!;;
```

The listener deletes the full trigger text with Backspace and injects:

```text
我要测试中文输入法，你好吗！
```

The listener also supports incremental conversion after the prefix is typed.
While the session is active, Space or any non-pinyin separator that is not part
of the configured trigger converts the preceding pinyin immediately, so you do
not have to wait for the closing trigger. Space acts like a commit key and is
not reinserted; punctuation separators are kept and mapped where a mapping
exists. The candidate panel remains the active-state indicator after committed
output. The closing trigger only removes pending raw text and the suffix; it
does not rewrite earlier converted text. Pressing Backspace immediately after
an incremental conversion restores the original pinyin text for editing.
The listener separately tracks pending raw text, committed output that still
belongs to the active session, and the hidden opening prefix. Deleting all
visible session output therefore keeps the session active until one additional
Backspace per opening-prefix character has been received. Those final virtual
Backspaces are consumed by the event tap, so they do not delete text that was
present before the trigger.

If a pinyin run has no Rime candidate, the listener commits that run unchanged
instead of treating it as a fatal conversion error. For example, `vke<Space>`
leaves `vke` as raw text and the listener remains usable. Rewrite transactions
also have an abort guard: any error before a native rewrite is committed
restores the logical input and releases/replays buffered keyboard events.

The configured trigger is reserved for state changes: the same configured pair
enters and exits the active session. Trigger characters such as
`;` are not treated as incremental separators, avoiding ambiguity between a
single trigger character and the full trigger. Each opening trigger starts an
isolated session; text before that trigger is not part of the active capture
buffer. When the opening trigger is detected, the listener deletes those trigger
characters and shows a non-activating macOS candidate panel as the active-state
indicator. As pinyin is typed, the panel shows the current Rime preedit and the
configured number of candidates. Only ASCII letters and apostrophes enter the
pending pinyin buffer. While that pending pinyin produces a candidate menu, a
configured selection key chooses its corresponding available candidate and the
configured page keys (`=` / `-` by default) move to the next / previous page. A
selection key is not consumed when its page has no corresponding candidate.
Candidate selection and paging keys type normally when there is no pending
candidate menu and never enter an empty pinyin buffer. If pending letters have
no Rime menu (for example invalid pinyin), both those letters and the control
key are committed literally, so `vke-` remains ASCII `vke-`.

The configured English commit key (backtick by default) commits the pending
pinyin text unchanged whenever it contains an ASCII letter, and consumes only
the delimiter. With no pending letters it types normally. Shift itself never
clears the session: an unmodified Shift is a no-op, and shifted text such as
`Shift+1` continues through the normal `!` punctuation path.

Candidate previews, page changes, and selections use reconstructed short Rime
sessions. Page navigation calls librime's page API and candidate selection calls
the current-page selection API; configured host keys are not sent to Rime as a
schema-dependent shortcut.

The panel is positioned from the macOS Accessibility caret bounds when
available, including browser text-marker ranges, then falls back to focused
text-field bounds and finally the mouse anchor. Mouse context changes,
active application changes, input source
changes, and Command-Tab/Command-Backtick window switch shortcuts clear the
active session and hide the panel. The listener also records the macOS input
source fingerprint when a session opens and verifies it on later key events; if
the input route changes before conversion, it abandons the internal session
without deleting or injecting text. Only macOS system input sources with
`source=com.apple...` are allowed to open or continue a session; third-party
input methods are ignored even if they are in an English/direct-input mode.
The listener requires a suppressing CGEvent tap; it stops instead of falling
back to a passive global monitor if that tap cannot be created. During the short
rewrite window where the listener deletes raw pinyin and
injects converted text, the macOS event tap temporarily buffers ordinary typed
characters and replays them into the active session after the rewrite finishes.
For separator-triggered conversion, this rewrite transaction starts before the
separator is handed to the capture state and Rime conversion path. Each
delete-and-inject rewrite is queued as one native operation; the next rewrite
does not start until the previous operation has posted its backspaces, posted its
replacement text, settled briefly, and replayed buffered input. If replayed
buffered input triggers another rewrite, replay pauses and keeps the remaining
events queued for the next transaction, so repeated fast conversions stay
serialized. Outside that rewrite transaction, key events are observed and passed
through.
Command-C,
Control-C, and common Command editing shortcuts abort the session; Control-W
keeps the session active and removes the previous raw pinyin word from the
buffer. When the closing trigger exits the session, only the current visible
marker and pending text are removed or converted.

For example:

```text
;;woyaoceshi<Space>
```

leaves the visible text as:

```text
我要测试
```

Continuing from that state, typing:

```text
zhongwenshurufa<Space>
```

leaves:

```text
我要测试中文输入法
```

Typing the closing trigger then removes only the suffix and hides the panel:

```text
我要测试中文输入法
```

Useful listener flags:

```sh
bash scripts/run-listener.sh --max-buffer-chars 4096 --inject-delay-ms 1
bash scripts/run-listener.sh --candidate-layout vertical --candidate-count 8
bash scripts/run-listener.sh --log-events
```

For diagnosis, run the debug wrapper. It prints doctor output, turns on native
and Rust event logs, and writes the same output to `logs/`:

```sh
bash scripts/run-listener-debug.sh
```

Package a standalone release binary directory:

```sh
bash scripts/package-binary.sh
```

The package is written to:

```text
dist/rime-poc-macos
```

Grant Accessibility permission to this executable, not just the terminal:

```text
dist/rime-poc-macos/rime-poc
```

Then run:

```sh
dist/rime-poc-macos/run-listener.sh
```

For diagnosis with the packaged binary:

```sh
dist/rime-poc-macos/run-listener-debug.sh
```

The debug log is written under:

```text
dist/rime-poc-macos/logs
```

Package a portable zip that bundles `librime`, its Homebrew dylib
dependencies, and the Rime data files:

```sh
bash scripts/package-portable-macos.sh
```

The portable directory and zip are written to:

```text
dist/rime-poc-macos-portable
dist/rime-poc-macos-portable.zip
```

On the target Mac, unzip it, grant Accessibility and Input Monitoring
permissions to `rime-poc-macos-portable/rime-poc`, then run:

```sh
./run-listener.sh
```

The portable zip does not require Homebrew on the target Mac. macOS permissions
still need to be granted per machine.

Download minimal Rime data for this PoC:

```sh
bash scripts/download-rime-data.sh
export RIME_SHARED_DATA_DIR="$PWD/data/shared"
export RIME_USER_DATA_DIR="$PWD/data/user"
export RIME_SCHEMA=luna_pinyin_simp
```

The CLI/listener strips the configured prefix/suffix. In the default
`segmented` mode, it splits the body into pinyin runs and separators, sends each
pinyin run to Rime independently, then rejoins the converted text. Half-width
separators are converted where there is a Chinese punctuation equivalent:

```text
,  -> ，
.  -> 。
?  -> ？
!  -> ！
-  -> －
+  -> ＋
... -> ……
~  -> ～
"  -> “ / ”
```

For an experimental mixed Chinese/English trial, switch to `rime-auto`. This
feeds the whole body to one Rime session and lets the active schema decide when
to commit text and how to handle English-looking input:

```sh
cargo run -- --conversion-mode rime-auto ';;wo ai OpenAI,yong Rust kaifa;;'
```

The same mode can be set in `rime-poc.toml`:

```toml
conversion_mode = "rime-auto"
```

`rime-auto` is intentionally experimental. It is useful for measuring Rime's
native mixed-input behavior, while `segmented` remains the safer default for
predictable auto-replacement.

The CLI prints the body, final output, tokenization, and each Rime segment's
preedit/first candidate. It exits with an error if `librime`, schema data, or
the trigger config are invalid.
