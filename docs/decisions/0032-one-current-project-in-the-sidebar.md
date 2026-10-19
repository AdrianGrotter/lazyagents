# 0032. One current project in the sidebar

Date: 2026-10-19
Status: Accepted

## Context

The Projects box listed every project, and its highlight did two jobs: it chose which
project's agents the Agents box listed, and it followed the selected agent. In the
all-agents view, moving through agents moved the Projects highlight to each agent's project,
and `n` put the new agent in whatever project was highlighted at that moment. The two boxes
were tied together in ways that were hard to predict, and the list took up to six rows of
the sidebar for something changed rarely.

## Decision

The Projects box shows one line: the current project, or `All projects` in the all-agents
view, as lazygit's status box shows the current repository. `enter` on it, or a click,
opens a switcher listing `All projects`, every project and `+ add project…`; removing a
project happens there too. Only the user changes the current project: switching, adding a
project, `A`, or `g`/a new agent landing in another project while one project is listed.
Selecting an agent in the all-agents view leaves it alone. `n` always opens a list of
projects first, starting at the current project (in the all-agents view, at the project last
picked for a new agent), then the new agent form.

## Alternatives

- Keep the list and only stop the all-agents view from moving the highlight: the box keeps
  its rows, and `n` still depends on a highlight that is easy to forget about.
- A project field inside the new agent form: the Worktree field depends on whether the
  project is a git repository, so the project has to be known before the form is shown.

## Consequences

The sidebar gains rows for agents, and what `n` does no longer depends on the highlight.
Switching project takes `1 enter` and a pick instead of `j`/`k`, and per-project
needs-input counts move from the sidebar into the switcher; the window title and `g` still
show waiting agents elsewhere.
