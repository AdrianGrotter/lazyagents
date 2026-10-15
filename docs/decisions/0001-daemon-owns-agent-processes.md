# 0001. A daemon owns the agent processes

Date: 2026-10-15
Status: Accepted

## Context

Agents must keep running when the UI closes, and several UI instances may attach at once. Three models were considered.

## Decision

A `lazyagents daemon` process owns every agent's PTY and terminal state and persists agent metadata. The TUI is a client over a Unix socket and is started on demand; it auto-spawns the daemon.

## Alternatives

- tmux-backed (one tmux session per agent, TUI drives tmux): least code, but the UI is bounded by what tmux exposes and focus means leaving the UI.
- PTYs inside the TUI process: simplest embedding, but agents die with the UI.

## Consequences

Closing the UI is free and reattaching is instant. A daemon crash kills running agents; metadata survives and Claude agents resume their session on restart. A versioned protocol is needed between client and daemon.
