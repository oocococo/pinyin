# Paged Space Selection and Candidate Position Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make Space select the displayed page's first candidate, preserve partial-selection pinyin, and keep the candidate panel below the caret.

**Architecture:** Classify an unmodified physical Space as a first-candidate interaction before the existing page-aware selection flow. Reuse the existing complete/partial candidate commit path, and change only caret/focused-input vertical positioning in the native panel frame calculation.

**Tech Stack:** Rust, rime-api/librime, Objective-C++ AppKit and Accessibility APIs.

---

### Task 1: Classify Space as the current page's first selection

**Files:**
- Modify: `src/main.rs`
- Modify: `scripts/test-listener-behavior.sh`
- Test: `src/main.rs`

**Step 1: Write the failing regression test**

Add a test for a candidate-interaction classifier that maps a physical Space
with pending pinyin to `Select(0)`, while leaving configured selection and page
keys unchanged.

**Step 2: Run the focused test**

Run: `cargo test space_selects_first_candidate_on_current_page -- --nocapture`

Expected: FAIL because the event-level classifier does not exist.

**Step 3: Implement the minimal classifier and routing**

Pass the key code into candidate interaction handling. Resolve physical Space
to `Select(0)` and otherwise delegate to `AppConfig::candidate_interaction_key`.
Keep the existing preview page and call `select_candidate` with that page.

**Step 4: Verify the focused test**

Run: `cargo test space_selects_first_candidate_on_current_page -- --nocapture`

Expected: PASS.

### Task 2: Cover page-aware complete and partial selection

**Files:**
- Modify: `src/main.rs`

**Step 1: Extend the ignored librime regression**

Select index zero on page one and compare the result with page one's displayed
first candidate. Accept complete or partial Rime outcomes, but require a partial
outcome to retain a nonempty, previewable remainder.

**Step 2: Run the librime regression**

Run: `cargo test rime_candidate_selection_and_paging_integration -- --ignored --nocapture`

Expected: PASS and page-one selection matches the displayed candidate.

**Step 3: Exercise the physical Space event in TextEdit**

Change the existing page-one E2E selection from digit `1` to Space and assert
the commit log still reports page one, index zero.

Run: `bash scripts/test-listener-behavior.sh`

Expected: PASS and the page-one committed text differs from page zero.

### Task 3: Keep the panel below text anchors

**Files:**
- Modify: `src/mac/native.mm`

**Step 1: Remove caret/focused-input vertical flipping**

Keep the initially computed below-anchor `y` for caret and focused-element
anchors. Apply visible-screen vertical fallback and clamping only to the mouse
anchor.

**Step 2: Compile the native layer**

Run: `cargo test space_selects_first_candidate_on_current_page --no-run`

Expected: PASS, including Objective-C++ compilation.

### Task 4: Full verification and review

**Files:**
- Review: `src/main.rs`
- Review: `src/mac/native.mm`
- Review: `docs/plans/2026-07-12-paged-space-and-candidate-position-design.md`
- Review: `docs/plans/2026-07-12-paged-space-and-candidate-position.md`

**Step 1: Run automated checks**

Run:

```bash
cargo test
cargo test rime_candidate_selection_and_paging_integration -- --ignored --nocapture
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git diff --check
bash scripts/test-listener-behavior.sh
```

Expected: all checks pass.

**Step 2: Review and commit**

Confirm Space fallback remains safe on Rime errors, partial selection still
resets the new remainder to page zero, and no caret/focused anchor can be moved
above its below-caret origin by screen clamping. Commit the focused change.
