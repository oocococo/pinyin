# ADR-0001: Rebuild short Rime sessions for candidate interaction

## Status

Accepted

## Context

The listener currently rebuilds a Rime session whenever it renders candidate
preview data. Candidate selection and page navigation now need to preserve a
small amount of interaction state while keeping `CaptureState` cloneable for
rewrite prediction and error rollback. A `rime_api::Session` owns a native
session, is not cloneable, and would make capture snapshots and failure recovery
substantially more complex.

Functional requirements are:

- configurable number keys select an available candidate only while ASCII
  pinyin has a candidate menu;
- configurable previous/next keys navigate pages only while that menu exists;
- a configurable English key commits pending pinyin text unchanged whenever it
  contains at least one ASCII letter;
- candidate and page keys pass through normally outside a pending candidate
  menu, and the English key passes through when no letters are pending;
- Shift never clears an active session, so shifted punctuation remains usable.

The interaction must not reintroduce global key swallowing, host/capture text
desynchronization, or hidden-prefix deletion regressions.

## Decision

Keep only a lightweight candidate page number in `CaptureState`. Build a short
Rime session from the current pending pinyin whenever the panel is refreshed or
a candidate is selected, advance that session to the requested page, and copy
owned candidate strings out before closing it.

Page reconstruction calls librime's direct `change_page` API. User-facing
selection keys map to indexes on the displayed page, and an available slot is
selected through librime's direct `select_candidate_on_current_page` API. This
does not simulate the configured host key or depend on a schema's visible select
labels. The displayed candidate text remains the safe fallback if librime
accepted an available slot but produced no commit. Candidate and English commits
use the existing serialized rewrite operation and committed-output accounting.

Only ASCII letters plus the apostrophe syllable separator are capture-level
pinyin. Literal digits and configured control keys are never retained in the
candidate buffer when no pinyin is pending.

For modified ASCII keys, the native layer precomputes separate key-code
translation tables for Shift, Caps Lock, and Shift+Caps Lock from the current
macOS keyboard layout at listener startup and whenever the input source changes.
The event-tap hot path selects and reads the corresponding cache. This is
necessary because a CGEvent can report raw text `1` even though Shift makes the
host insert `!`; calling TIS/Carbon translation APIs from inside the event-tap
callback can block the tap.

## Consequences

### Positive

- `CaptureState` remains cloneable and rollback stays deterministic.
- Page navigation, selection, and panel rendering all derive from the same raw
  pinyin and page number.
- Rime session failure remains contained by the existing transaction guard.
- Special keys can be changed in TOML without coupling to a schema's visible
  select labels.

### Negative

- Each panel refresh or selection creates a short native Rime session.
- Page reconstruction calls `change_page` repeatedly from page zero.
- Selection needs a safe text fallback if an available candidate does not
  produce a commit.
- Keyboard-layout changes rebuild three small 128-key modified-text caches for
  Shift, Caps Lock, and Shift+Caps Lock.

### Neutral

- Candidate page state resets to zero whenever pending pinyin changes.
- Candidate count cannot exceed the number of configured selection keys.

## Alternatives Considered

**Keep one persistent Rime session in `ListenerRuntime`.**

- Rejected for this iteration because native session ownership cannot participate
  in cloned capture snapshots, making rewrite prediction, rollback, and replay
  recovery much harder.

**Flatten and cache all candidates in Rust.**

- Rejected because it bypasses Rime's page semantics and can become stale as the
  composition changes.

**Treat configured keys as normal separators and post-process output.**

- Rejected because the key would already have reached the host and the first
  candidate conversion would have destroyed the state needed for page/slot
  selection.

## Failure Modes

- If preview/page reconstruction fails, the special key is not consumed and the
  existing capture state is retained.
- If pending letters produce no candidate menu, commit the letters and control
  key together as raw literal text; punctuation mapping must not change the key.
- If a configured selection key has no corresponding candidate on the current
  page, it is not treated as candidate selection.
- If native selection yields no commit, inject the candidate string already
  shown for that slot.
- If a requested page is out of range, clamp to Rime's actual page and keep the
  key consumed so it does not leak into host text.
- If modified-key translation is unavailable, fall back to the text carried by
  the original CGEvent.
- If a configured key conflicts with another interaction key, a trigger, or
  ASCII pinyin, configuration validation rejects startup.

## References

- librime `RimeApi.change_page`
- librime `RimeApi.select_candidate_on_current_page`
- librime `RimeMenu.page_no` and `is_last_page`
