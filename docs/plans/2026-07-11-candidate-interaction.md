# Candidate Interaction Implementation Plan

**Goal:** Add non-disruptive Shift behavior, configurable candidate selection and paging, and configurable English raw commit while keeping non-pinyin keys out of the candidate buffer.

**Architecture:** Keep pending pinyin and candidate page in cloneable `CaptureState`; reconstruct short Rime sessions for page previews and selection. Use librime's direct page/current-page-selection APIs instead of simulated host keys. Route configured interaction keys before generic capture, consume candidate/page keys only for an existing candidate menu (and selection only for an available slot), and commit through the existing serialized rewrite path.

**Tech Stack:** Rust, rime-api/librime, macOS CoreGraphics/AppKit FFI, TOML, shell/Python TextEdit E2E.

---

### Task 1: Configuration and validation

**Files:**
- Modify: `src/main.rs`
- Modify: `src/mac/native.mm`
- Modify: `rime-poc.toml`
- Modify: `README.md`

**Step 1: Add failing configuration tests**

Cover defaults, unique numeric selection keys, single-character page/English
keys, candidate-count capacity, and conflicts between interaction keys, trigger
characters, and ASCII pinyin.

**Step 2: Add config fields**

Add `candidate_select_keys`, `candidate_page_next_key`,
`candidate_page_previous_key`, and `english_commit_key` to file/runtime config,
doctor output, sample TOML, and documentation. Defaults are `1234567890`, `=`,
`-`, and backtick respectively.

**Step 3: Run focused config tests**

Run: `cargo test config`

Expected: PASS.

### Task 2: Pinyin-only capture and Shift behavior

**Files:**
- Modify: `src/main.rs`

**Step 1: Add failing capture tests**

Verify digits and configured special characters pass through without entering an
empty candidate buffer; shifted `!` converts pending pinyin as punctuation; only
ASCII letters/apostrophe form pending pinyin; single Shift does not clear state.

**Step 2: Refactor active character routing**

Separate idle trigger detection from active input. Keep non-pinyin literals out
of the active buffer when no pinyin is pending, while retaining suffix detection
and punctuation-triggered conversion when pinyin is pending.

**Step 3: Remove Shift session clearing**

Let Shift keydown/release be no-ops; shifted text continues through the ordinary
text route. Precompute Shift, Caps Lock, and Shift+Caps Lock translations from
the current keyboard layout so the event-tap hot path only reads cached text.

### Task 3: Candidate pages and selection

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/main.rs`
- Modify: `src/mac/native.mm`

**Step 1: Add Rime page preview tests**

Build a preview for a known pinyin input, advance pages with librime's
`change_page` API, select an available slot with
`select_candidate_on_current_page`, and assert page metadata and owned
candidates remain valid after session close.

**Step 2: Store/reset candidate page state**

Reset page zero whenever pending pinyin changes, is restored, committed, or
cleared. Clamp navigation at first/last page.

**Step 3: Route configured page and selection keys**

When pending pinyin has a candidate menu, consume page keys and refresh the
panel. Map a configured selection key to an available displayed slot, use the
direct current-page selection API to resolve the Rime commit (falling back to
displayed text), rewrite pending raw text, record committed output, and remain
active. Outside a menu, or when no candidate exists for a selection slot, keep
the configured character's normal literal behavior.

**Step 4: Render configurable labels**

Format labels in Rust and stop the native panel from hard-coding `1.`, `2.`, ...
so configured select keys are visible.

### Task 4: English raw commit

**Files:**
- Modify: `src/main.rs`

**Step 1: Add failing state tests**

When pending pinyin contains an ASCII letter, the configured English key must
commit the complete raw pending text (including apostrophe separators), consume
itself, clear the pending buffer, reset the page, and stay active. With no
pending letters it must pass through and remain outside capture state; this
behavior does not require Rime to have produced a candidate menu.

**Step 2: Reuse direct pending commit action**

Use the same rewrite/committed-count path as candidate selection. Identity
English output must not create a no-op one-step restore record.

### Task 5: Verification and integration

**Files:**
- Modify: `scripts/test-listener-behavior.sh`
- Modify: `scripts/stress-listener-rewrite.sh` only if the new routing requires it.

**Step 1: Unit and static checks**

Run `cargo fmt --check`, full `cargo test`, `cargo clippy --all-targets -- -D warnings`,
`bash -n` on changed scripts, and `git diff --check`.

**Step 2: TextEdit E2E**

Verify Shift+1 punctuation, numeric candidate selection, next/previous paging,
literal special keys with no pending pinyin, English raw commit, and all prior
deletion/rewrite scenarios.

**Step 3: Stress and integrate**

Run the rapid rewrite stress test, review the diff, commit separately from the
recovery checkpoint, then merge into a clean, current `main` and push.
