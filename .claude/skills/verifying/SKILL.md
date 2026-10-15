---
name: verifying
description: The done gate for lazyagents. Load before claiming a step, task or milestone is complete, and before committing.
---

# Verifying

1. Run `just check`. Read the whole output: test counts, warnings, hook and harness results. It is green only when the command exits 0.
2. For anything visible in a terminal (the TUI), run the scenario through the tmux harness (skill: running-dev) and quote the captured screen lines that prove it.
3. For a bug fix, show the regression test failing before the fix and passing after.
4. Report what you ran and what it printed. Red is reported as red, with the failing output; every claim of done carries its evidence.
