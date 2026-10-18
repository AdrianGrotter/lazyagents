---
name: committing
description: "Conventional Commits rules for this repo: types, scopes, subject format, commit granularity. Load before every git commit."
---

# Committing

1. Follow the `verifying` skill and commit only on green. One green run on the combined tree covers several commits made from it.
2. Stage one logical change: a feature with its tests and docs, a fix with its regression test. Unrelated changes become separate commits.
3. Write the message:

   ```
   <type>(<scope>): <subject>

   <body: why the change was made, wrapped at 72>
   ```

   - type: `feat` `fix` `docs` `refactor` `test` `chore` `build` `ci` `perf`
   - scope (optional): `protocol` `daemon` `tui` `cli` `client` `docs` `nix` `repo`, naming the crate or area touched; leave it out when the change spans several.
   - subject: imperative mood, lower case, no trailing period; the whole first line is at most 72 characters. Count it before committing; the commit-msg hook rejects 73+.
   - breaking change to the protocol or the on-disk state format: `!` after the scope and a `BREAKING CHANGE:` footer.
4. Commit with a heredoc so the body keeps its line breaks:

   ```bash
   git commit -F - <<'EOF'
   feat(daemon): persist agents atomically

   Write state.toml through a temp file and rename so a crash mid-write
   never leaves a truncated file.
   EOF
   ```

5. Done when `git log -1` shows the commit. When a git hook rejects it, fix what it reports and commit again; every commit passes through the hooks.

Examples: `feat(tui): add attention sorting`, `fix(daemon): clamp pane size to 2x2`, `test(protocol): round-trip every request`, `docs: document scroll mode`, `build(nix): add cargo-insta`.
