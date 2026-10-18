# 0022. Filter the help popup behind `/`, so its keys can scroll it

Date: 2026-10-18
Status: Accepted

## Context

The help popup took every typed character as filter text and closed on any other key. At
60x20 it was cut off with no sign of more and its Agent pane section could not be reached:
`↓` did nothing and `pagedown` closed it. Scrolling it with the keys every other scrolling
view uses (`j`/`k`, `g`/`G`) collides with typing those letters into the filter.

## Decision

The help popup scrolls with the shared motion keys (`j`/`k`, arrows, `ctrl-d`/`ctrl-u`,
page keys, `g`/`G`), shows `↑ N more above` / `↓ N more below` when it does not fit, and
wraps long descriptions. Filtering starts with `/`, as the Agents filter does: the filter
is typed in the popup's bottom row, `enter` keeps it and `esc` clears it. Outside the
filter, `esc`, `q`, `enter` and the help key close the popup; other keys do nothing,
instead of closing it.

## Alternatives

- Keep typing-to-filter and scroll only with the arrow and page keys: `j`/`k`/`g`/`G`
  would mean something different here than in every other scrolling view.
- Typing filters, and `j`/`k` scroll only while the filter is empty: the same key would
  scroll or type depending on state the user cannot see.

## Consequences

Filtering costs one extra key (`/`). Any unbound key no longer closes the popup, so a
stray key does not lose the user's place.
