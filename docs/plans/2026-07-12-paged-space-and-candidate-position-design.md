# Paged Space Selection and Candidate Position Design

## Goal

Make Space choose the first candidate on the page currently displayed, while
preserving any pinyin that a partial Rime selection did not consume. Keep the
candidate panel below the insertion caret instead of flipping it above the
caret when vertical screen space is limited.

## Candidate-selection behavior

The current listener handles configured number keys and page keys through the
candidate interaction path, but Space is handled later as a generic conversion
separator. Generic conversion reconstructs a fresh Rime session on page zero,
so paging is lost and the first candidate from page zero is committed as a
whole conversion.

The listener should classify an unmodified physical Space with pending pinyin
as `Select(0)` before resolving the current candidate preview. It can then reuse
the existing page-aware `select_candidate` and `commit_candidate_selection`
flow. This preserves the displayed page, and it also preserves a partial
selection's remaining pinyin because that flow already distinguishes complete
and partial Rime outcomes. Space remains consumed as a control key after a
successful candidate rewrite. If candidate preview or selection fails, the
existing generic Space conversion remains the fallback.

Alternatives considered were changing the generic conversion function to
accept a page number, or teaching `AppConfig::candidate_interaction_key` that
Space is a configurable selection key. Passing a page into generic conversion
would duplicate the partial-selection logic, while putting Space into the
configuration classifier would conflate a built-in input convention with the
user-configurable digit mapping. A small event-level classifier is the narrowest
change.

## Candidate-panel placement

Accessibility APIs already provide the caret rectangle, converted from the
top-left AX coordinate system to AppKit's bottom-left coordinate system. The
normal frame calculation places the panel below that anchor, but an explicit
fallback moves it above the caret whenever the panel would cross the screen's
lower visible edge. A subsequent clamp can also move the panel upward across
the caret.

For caret and focused-input anchors, vertical placement should remain the exact
below-anchor position and must not flip or clamp upward. Horizontal clamping is
retained. The mouse fallback keeps its existing visible-screen clamping because
it is not a reliable caret anchor and must remain discoverable. This follows the
requested invariant for real text inputs without changing fallback behavior in
applications that expose no accessibility text range.

## Verification

Add a pure classifier test that proves physical Space maps to the current page's
first selection while configured keys retain their meanings. Extend the real
librime integration test to select the first candidate from page two and assert
that it matches the displayed candidate; if Rime returns a partial result, also
assert that the remainder is nonempty and previewable. Exercise the same action
in TextEdit by paging and pressing Space, then compare the committed text with
the listener's page-one/index-zero log. Compile the Objective-C++ native layer
on macOS, then run the Rust suite, ignored librime integration, Clippy,
formatting, and diff checks.
