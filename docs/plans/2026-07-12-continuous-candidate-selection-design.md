# Continuous Candidate Selection Design

## Goal

Allow a candidate that consumes only a prefix of the pending pinyin to commit
that prefix while keeping the unconsumed pinyin visible and immediately showing
its candidates. Also prevent the pinyin-commit Space from arming macOS's
double-space period substitution.

## Observed behavior

With `luna_pinyin_simp`, selecting `我是` from `woshizhongguoren` succeeds but
does not produce a Rime commit. Rime instead reports a composition such as
`我是zhong guo ren`; `composition.sel_start` is the UTF-8 boundary between the
selected text and the remaining preedit, and the menu already contains
candidates for the remainder. A final selection produces a normal commit.

The current implementation treats the absence of a commit as permission to use
the displayed candidate as the complete result. It then clears the entire
capture buffer, which discards `zhongguoren`.

The double-space period is not produced by this repository or the Rime schema.
The host has `NSAutomaticPeriodSubstitutionEnabled` enabled. The physical Space
used to commit pinyin currently reaches AppKit before the rewrite deletes it, so
AppKit still remembers it as the first Space of a double-space gesture.

## Approaches considered

### Rebuild from the remaining pinyin (chosen)

Keep the existing short-lived Rime session. Candidate selection returns either
a complete commit or a partial selection containing the displayed selected text
and the remaining raw pinyin. The listener rewrites the pending raw text to
`selected_text + remaining_pinyin`, records only the selected text as committed,
and keeps the remainder in `CaptureState` on page zero.

This is the smallest change, preserves clone-based transaction rollback, and
matches the requested behavior that the next list be recomputed from the
remaining pinyin. It intentionally does not retain language-model context from
the selected prefix.

### Keep a live Rime session

A session could preserve Rime's exact composition and language-model context
across selections. It would make `CaptureState` non-cloneable or require a
second rollback protocol for native session state, and every edit, replay, and
transaction failure would need session synchronization. That scope is not
needed for the requested behavior.

### Replay selection history into short sessions

Storing every selected page/index and replaying it could preserve context while
keeping sessions short-lived. Candidate ordering can change with learned data,
so page/index is not a durable selection identity; the added complexity also
does not improve the stated remainder-based behavior.

## Data flow

`select_candidate` returns:

```rust
enum CandidateSelection {
    Complete { text: String },
    Partial {
        selected_text: String,
        remaining_pinyin: String,
    },
}
```

After a successful Rime selection, a commit means `Complete`. Without a commit,
the code reads the post-selection composition, validates `sel_start` as a UTF-8
boundary, normalizes the suffix of the preedit, and maps that suffix back to the
original raw-pinyin suffix so internal apostrophes are retained. The result must
be nonempty and strictly shorter than the original input.

For a partial result, the host rewrite deletes the current pending raw text and
inserts selected text plus the remaining raw pinyin. The capture buffer becomes
the visible remainder, the candidate page resets to zero, and only the selected
text increases committed-output accounting. `last_conversion` is cleared, so
Backspace edits the remaining pinyin first instead of unexpectedly undoing a
previous selection.

Complete selection, English commit, invalid-pinyin literal fallback, and final
conversion retain their existing paths.

## Space handling

When an active capture has pending pinyin and receives an unmodified physical
Space, the listener treats that Space as an invisible control key. It is added
to conversion parsing as non-visible, so the rewrite deletes only the pinyin,
and the event callback consumes its keydown and paired keyup. A following Space
is therefore the first Space AppKit sees and remains an ordinary space.

Spaces typed with no pending pinyin continue through the existing literal path.
The implementation must not change the user's global macOS defaults.

## Failure handling and tests

An invalid partial boundary, empty remainder, no forward progress, or missing
post-selection menu is an error and falls back to the existing ordinary-key
behavior. A failed host rewrite restores the exact capture snapshot, including
visibility and page. Candidate-panel refresh remains best-effort after a
successful rewrite.

Regression coverage includes state-level partial/complete transitions, repeated
selection, apostrophe suffix mapping, real librime partial selection, rollback,
Backspace on a remainder, consumed commit Space, and TextEdit E2E with macOS
automatic-period substitution enabled.
