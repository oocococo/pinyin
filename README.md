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
```

Trigger strings cannot contain punctuation that is reserved for separating
pinyin runs inside the body: comma, period, question mark, exclamation mark, minus, plus,
ellipsis, tilde, or double quote, including common Chinese/full-width variants.

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

Useful listener flags:

```sh
bash scripts/run-listener.sh --max-buffer-chars 4096 --inject-delay-ms 1
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
