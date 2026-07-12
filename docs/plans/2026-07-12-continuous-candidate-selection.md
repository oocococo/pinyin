# Continuous Candidate Selection Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Preserve and re-candidate unconsumed pinyin after partial candidate selection, and prevent the commit Space from triggering macOS double-space period substitution.

**Architecture:** Continue reconstructing short Rime sessions. Model selection as complete or partial, derive the partial remainder from Rime's post-selection composition, and keep that remainder in `CaptureState`; consume only a physical Space that is acting as a pinyin commit control key.

**Tech Stack:** Rust, rime-api/librime, macOS CoreGraphics/AppKit FFI, shell/Python TextEdit E2E.

---

### Task 1: Model Rime partial-selection outcomes

**Files:**
- Modify: `src/main.rs`

**Step 1: Write failing helper tests**

Add tests for mapping spaced post-selection preedit suffixes back to raw pinyin,
including a suffix with an internal apostrophe and rejection of no-progress
results.

**Step 2: Run the focused tests**

Run: `cargo test candidate_selection -- --nocapture`

Expected: FAIL because partial outcomes and suffix extraction do not exist.

**Step 3: Implement the outcome**

Add:

```rust
enum CandidateSelection {
    Complete { text: String },
    Partial { selected_text: String, remaining_pinyin: String },
}
```

Read commit first after selection. If absent, validate the composition UTF-8
boundary, derive a strict raw suffix, require a nonempty remainder/menu, close
the short session, and return `Partial`.

**Step 4: Verify**

Run: `cargo test candidate_selection -- --nocapture`

Expected: PASS.

### Task 2: Preserve the remainder in CaptureState

**Files:**
- Modify: `src/main.rs`

**Step 1: Write failing state tests**

Cover one partial selection, two partial selections followed by a complete one,
page reset, committed-character accounting, Backspace editing the remainder,
and snapshot rollback after a failed rewrite.

**Step 2: Run focused tests**

Run: `cargo test partial_candidate -- --nocapture`

Expected: FAIL because `take_pending_commit` clears the complete buffer.

**Step 3: Add a candidate-specific action**

Create a pending-candidate action with original text, replacement text, selected
text, remaining pinyin, deletion count, and complete/partial kind. For partial
selection set the buffer to the visible remainder, reset page zero, and record
only selected output after the rewrite succeeds. Keep English and no-candidate
literal commits on their existing complete paths.

**Step 4: Verify**

Run: `cargo test partial_candidate -- --nocapture`

Expected: PASS.

### Task 3: Consume the pinyin-commit Space

**Files:**
- Modify: `src/main.rs`
- Modify: `src/mac.rs`

**Step 1: Write failing routing tests**

Assert that an unmodified physical Space with pending pinyin is non-visible,
that its conversion deletes only visible pinyin, and that the callback result
consumes the event. Assert that Space with no pending pinyin remains ordinary.

**Step 2: Run focused tests**

Run: `cargo test space_commit -- --nocapture`

Expected: FAIL because the current Space is visible and callback returns false.

**Step 3: Implement routing**

Expose the macOS Space key code, detect pending-pinyin commit Space before
mutating capture state, push it with `visible=false`, and return consumed only
after successful action handling. Preserve the existing pass-through recovery
assumption if conversion fails.

**Step 4: Verify**

Run: `cargo test space_commit -- --nocapture`

Expected: PASS.

### Task 4: Real Rime and TextEdit regressions

**Files:**
- Modify: `src/main.rs`
- Modify: `scripts/test-listener-behavior.sh`
- Modify: `README.md`

**Step 1: Extend the ignored librime integration test**

Find a displayed candidate that consumes only a prefix, assert a `Partial`
result and nonempty preview for its remainder, then select a complete candidate
from the remainder.

**Step 2: Extend TextEdit E2E**

Add a deterministic partial-selection scenario and assert host text plus the
next candidate log after each selection. With
`NSAutomaticPeriodSubstitutionEnabled=1`, type `ni<Space><Space>` and assert the
converted text ends in exactly one ordinary space and contains no English
period. Record the current preference for evidence but do not mutate the user's
global setting.

**Step 3: Run full verification**

Run:

```bash
RIME_INCLUDE_DIR=/opt/homebrew/opt/librime/include \
RIME_LIB_DIR=/opt/homebrew/opt/librime/lib cargo test
RIME_INCLUDE_DIR=/opt/homebrew/opt/librime/include \
RIME_LIB_DIR=/opt/homebrew/opt/librime/lib cargo test \
  rime_candidate_selection_and_paging_integration -- --ignored --nocapture
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git diff --check
bash scripts/test-listener-behavior.sh
```

Expected: all automated and TextEdit E2E checks pass.

### Task 5: Review and commit

**Files:**
- Review every changed file above.

**Step 1: Inspect the final diff**

Confirm no global macOS preference mutation remains, all Rime sessions close on
success and error, transaction rollback restores capture state, and no unrelated
files are included.

**Step 2: Commit**

```bash
git add src/main.rs src/mac.rs scripts/test-listener-behavior.sh README.md \
  docs/plans/2026-07-12-continuous-candidate-selection-design.md \
  docs/plans/2026-07-12-continuous-candidate-selection.md
git commit -m "Support continuous candidate selection"
```
