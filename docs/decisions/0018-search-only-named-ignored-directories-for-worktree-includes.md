# 0018. Search only the ignored directories `.worktreeinclude` names

Date: 2026-10-17
Status: Accepted

## Context

A new worktree gets the project's gitignored files that `.worktreeinclude` names (gitignore syntax). The daemon found them with two `git ls-files --others --ignored` passes, one with the include patterns and one with `--exclude-standard`, and copied their intersection. Both passes walk every ignored directory: on a Rust project with a large `target/` they took 3.9 s, for a `.worktreeinclude` that names one `.env`. An unanchored pattern such as `*.env` can in principle match deep inside `target/` or `node_modules/`, so listing exactly what gitignore semantics allow means walking those trees.

Claude Code, whose `.worktreeinclude` this is, lists ignored entries with `--directory`, so a directory ignored as a whole appears once as `dir/` and is not walked, and searches such a directory only when an include pattern names a path in it.

## Decision

`git ls-files --others --ignored --exclude-standard --directory` lists the ignored files outside ignored directories and each ignored directory once. A listed directory is kept only when an include line, read up to its first wildcard, names it, a path in it or a directory holding it; a line without a `/` before its end, or starting with `**/`, is tried at every depth, and a line starting with a wildcard reaches into no such directory. The two original passes then run limited to the kept files and directories (as `:(literal)` pathspecs, in batches of 500), and their intersection is copied.

## Alternatives

- Keep the two whole-tree passes: exact for every pattern, but the cost grows with the largest ignored tree, which is what the user never wants copied.
- Turn each include line into a pathspec (`:(glob)**/<line>`): unanchored lines, the common case, still walk everything.
- Match the patterns in Rust with the `ignore` crate: a new dependency of seven crates, and git would still have to list the candidates.

## Consequences

Creating a worktree costs about what `git status` costs, whatever lies in ignored directories. A wildcard pattern no longer finds files inside an ignored directory it does not name: `*.env` copies `.env` and `pkg/a/.env` but not `target/debug/x.env`, as in Claude Code. Naming the directory (`target/debug/*.env`) brings such files back.
