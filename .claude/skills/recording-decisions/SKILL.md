---
name: recording-decisions
description: When and how to write an ADR in docs/decisions. Load when choosing between real alternatives (dependency, protocol or storage format, process model, UX convention) or reversing an earlier decision.
---

# Recording decisions

An ADR is due when a choice had real alternatives and a future reader would ask "why this?".

1. Number: highest `NNNN` in `docs/decisions/` plus one.
2. Create `docs/decisions/NNNN-kebab-title.md` from the template.
3. Reversing an earlier ADR: set its status to `Superseded by NNNN` and reference it from the new one.
4. Commit the ADR together with the change it explains.

```markdown
# NNNN. Title stating the decision

Date: YYYY-MM-DD
Status: Accepted

## Context

The forces and constraints that made this a decision.

## Decision

What we do, in one paragraph.

## Alternatives

- Option: why it lost.

## Consequences

What gets easier and what gets harder.
```
