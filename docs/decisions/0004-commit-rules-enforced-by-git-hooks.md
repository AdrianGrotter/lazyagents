# 0004. Commit rules are enforced by git hooks

Date: 2026-10-15
Status: Accepted

## Context

The design called for a Claude Code `PreToolUse` guard that inspects `git commit` commands. Extracting a message from arbitrary shell (heredocs, `-F`, editors) is unreliable, and the guard would only apply to Claude.

## Decision

`.githooks/commit-msg` validates Conventional Commits and `.githooks/pre-commit` runs fmt and clippy on commits touching Rust. The nix dev shell sets `core.hooksPath=.githooks`.

## Alternatives

- Claude `PreToolUse` guard on `git commit`: only sees the command line, misses messages from files and editors.

## Consequences

Humans and every agent get the same gate. Commits outside the dev shell fail with a message telling the user to enter it.
