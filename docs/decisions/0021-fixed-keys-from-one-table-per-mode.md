# 0021. Fixed keys come from one table per mode

Date: 2026-10-18
Status: Accepted

## Context

The diff view, scroll mode and the help popup have keys that are not configurable. Each
mode's keys were written twice: once as match arms in its key handler and once as a
literal hint string for the status bar. The two drifted (scroll mode's hint and handler
disagreed about the arrow keys, and the diff hint was one long string that a narrow
terminal cut mid-word), and nothing tied a new key to its hint.

## Decision

`crates/tui/src/fixed_keys.rs` holds one table per mode of `(key, action)` pairs, plus the
motions (`j`/`k`, arrows, `ctrl-d`/`ctrl-u`, page keys, `g`/`G`) the scrolling modes
share. A mode's key handler looks the key up in its table and matches on the action, and
its hints name the keys found in the same table, so a key cannot be handled without being
hinted or hinted without being handled. The configurable keys (sidebar and pane) already
work this way through the keymap. Popups whose hints are a fixed sentence (confirm, menus,
forms) keep their literal hints; they were not touched.

## Alternatives

- Keep the literal strings and add a test that compares them with the handlers: catches
  drift only for the keys the test thinks of, and leaves the hint impossible to shorten
  hint by hint when it does not fit.
- Make these keys configurable through the keymap: more config surface for keys nobody
  has asked to change.

## Consequences

Adding a fixed key means adding a table row and, if it deserves a hint, a hint row. The
status bar can drop whole hints from the end when they do not fit, since each hint is a
separate string. Scroll mode gained the arrow keys, which every other scrolling view had.
A table key that types no character (arrows, page keys, `esc`) matches with any modifiers
held, as the match arms did for the page keys; letters match exactly.
