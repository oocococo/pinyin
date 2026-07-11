# Listener Session Recovery Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Keep an active listener session alive until all committed output and the hidden trigger prefix have been deleted, and release the keyboard rewrite transaction after an invalid-pinyin conversion error.

**Architecture:** Extend `CaptureState` with explicit committed-output character accounting instead of inferring session ownership from the pending raw buffer. Treat conversion as a recoverable transaction: preserve typed buffer/visibility state, use raw identity output for no-candidate input, reconstruct host text on other pre-commit failures, and abort only transactions that have not queued native rewrite work.

**Tech Stack:** Rust, librime, macOS CoreGraphics/AppKit FFI, Rust unit tests.

---

### Task 1: Reproduce the session-boundary regression

**Files:**
- Modify: `src/main.rs`

**Step 1: Write the failing test**

Add a state-machine test that opens a session, records two successful incremental conversions, restores/deletes the latest conversion, deletes the earlier committed output, and asserts that the session remains active for two additional Backspaces representing the hidden two-character prefix.

**Step 2: Run the focused test and verify it fails**

Run: `RIME_INCLUDE_DIR=/opt/homebrew/opt/librime/include RIME_LIB_DIR=/opt/homebrew/opt/librime/lib cargo test deleting_multiple_committed_segments_preserves_hidden_prefix_budget`

Expected: FAIL because the current `marker_chars_visible` budget is consumed while deleting earlier committed Chinese output.

### Task 2: Track committed output explicitly

**Files:**
- Modify: `src/main.rs`

**Step 1: Add session-owned committed character state**

Add `committed_output_chars: usize` to `CaptureState`, initialize/reset it with the session, increment it after each successful incremental conversion, and subtract the latest inserted segment when a conversion is restored to raw pinyin.

**Step 2: Make Backspace consume state in document order**

When the raw buffer is empty, decrement committed output first. Only after it reaches zero may Backspace consume the hidden-prefix budget and exit the session.

**Step 3: Run the focused test**

Expected: PASS, including explicit assertions that the final committed Chinese character leaves the session active with the full hidden-prefix budget.

### Task 3: Recover from invalid-pinyin conversion failures

**Files:**
- Modify: `src/main.rs`
- Modify: `src/mac.rs`

**Step 1: Write recovery-state tests**

Verify that a failed `ConversionAction` can restore its exact typed text and per-character visibility to an active `CaptureState` instead of leaving an empty buffer.

**Step 2: Expose a guarded native transaction abort hook**

Add a native abort entry point that refuses to cancel a running or queued rewrite operation, and bind it in `src/mac.rs` for the Rust transaction guard.

**Step 3: Handle conversion errors before any rewrite is committed**

Treat a no-candidate Rime result as an identity/raw conversion. For other failures, restore the failed action to `CaptureState`, reconstruct the raw host text when buffered replay made it invisible, and abort any pre-opened uncommitted rewrite transaction.

**Step 4: Verify the concrete input**

Run both segmented and `rime-auto` CLI modes against `vke` and confirm each returns `output: vke`, then verify a synthetic pre-commit error cannot leave a rewrite transaction active by guard test and macOS build.

### Task 4: Full verification and integration

**Files:**
- Modify only if verification reveals a regression.

**Step 1: Format and run unit tests**

Run `cargo fmt --check` and the full `cargo test` suite with the Homebrew librime include/library paths.

**Step 2: Run repository checks**

Run `git diff --check` and the listener behavior script. The macOS E2E must cover multi-segment deletion, consumed hidden-prefix Backspaces with sentinel text, invalid-pinyin raw fallback, and keys typed after fallback.

**Step 3: Review and commit**

Inspect the final diff for state-reset completeness and transaction completion on every pre-commit error path, then commit the implementation on the isolated branch and integrate only after confirming the main worktree is clean and current.
