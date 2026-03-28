# replay-experiment / board design

## Purpose

This experiment models a shared multi-agent coordination surface for Replay.

The core claim is that one shared Board can act as both:
- a chronological timeline of meaningful actions
- and the live coordination state agents use to work together

The experiment intentionally rejects separate per-agent logs, heartbeat spam, and coarse file locking.

## Core model

### One shared Board

All agents and the coordinator contribute records to one shared Board.

The Board is:
- chronological
- shared
- filtered into topical/interleaved views
- compacted aggressively when records become obsolete
- retention-aware through record leases

### Fused timeline and live state

The Board is not split into:
- an append-only history log
- plus a separate state store

Instead, the same record surface carries both:
- what happened
- and what still matters now

This is why transitions, supersession, tombstoning, visibility, and retention are first-class.

## Board invariants

### Visibility vs retention

A record has two separate concerns:
- whether it should appear on the live Board surface
- whether it must still be retained because an agent depends on it

Core invariant:

> A record may leave the live Board as soon as it is superseded, but it may not be retired while any active lease depends on it.

### Leases do not imply authority

Record leases only defer retirement.
They do not grant write permission, claim ownership, merge rights, or priority.

### Meaningful records only

The Board should contain:
- task progression
- agent activity
- edit intent
- section claims/releases
- write proposals/results
- review requests/results
- confidence reports
- coordinator shaping actions

The Board should not contain heartbeat noise.

## Coordinator responsibilities

The coordinator is a shaping policy layer, not a second hidden truth store.

In v0 it is responsible for:
- creating tasks
- assigning related tasks into a shared group/workspace
- emitting task relations
- advancing task status
- compacting obsolete task state
- recording reassignment events

In later versions it may also:
- infer dependency structure
- derive groupings automatically
- detect stale section claims
- rebalance work
- promote or demote records across views

## Section claims

Section claims replace whole-file locks.

A section claim is:
- scoped to a file and one or more line spans
- temporarily owned by an agent
- compatible with other claims in the same file if spans do not overlap
- released explicitly or superseded operationally

This enables:
- same-file parallel work
- same-workspace coordination
- smaller edits to be farmed out independently

### Claim conflict rule

Two claims conflict only if:
- they target the same file
- and their spans overlap
- and the incumbent claim is still active

Distinct spans in the same file are allowed simultaneously.

## SmartRead / SmartWrite semantics

The experiment includes a simplified SmartRead/SmartWrite protocol.

### SmartRead

A SmartRead operation:
- emits an `edit.intent` in read mode
- leases that record for the operation
- reads the relevant file span and current file version
- consults an interleaved Board slice for relevant writes, claims, edits, and reviews

### SmartWrite

A SmartWrite operation:
- performs a SmartRead first
- optionally acquires a transient section claim
- emits an `edit.intent` in write mode
- writes against an expected file version
- optionally enforces a simple lint gate
- releases the read lease after completion or failure
- releases transient claims automatically

In v0, reconciliation is line/version based rather than structural.

## Review and confidence

Review is part of the protocol.

Agents can:
- request review
- return verdicts
- attach findings
- report confidence

The point is to make cross-checking part of the shared operational surface rather than an implicit private thought.

## Subagent fan-out

A task agent can spawn subagents inside the same workspace.

This models:
- farming out smaller sub-edits
- mixing stronger and weaker agents
- same-workspace cooperation without whole-file serialization

The canonical scenario includes a parent agent spawning a subagent to perform a cheap gamma edit while peer agents handle alpha and beta.

## Current failure semantics

### Write rejection

A write may be rejected due to:
- claim conflict
- stale target/claim
- stale file version
- lint failure
- policy denial

### Claim contention

A claim request may fail if another active overlapping claim exists.

### Record retirement

A hidden superseded or tombstoned record may be retired only when it has no active leases.

## Known limitations

This experiment intentionally does not yet model:
- structural anchor relocation across line drift
- true AST-aware region reconciliation
- automatic dead-agent detection policy
- automatic subagent scheduling
- real git worktrees or filesystem integration
- richer merge/rebase semantics
- long-lived persistence or replay from disk

## Canonical scenario coverage

The current simulation demonstrates:
- related tasks sharing one workspace
- two agents editing different sections of the same file
- an overlapping write rejection
- SmartRead/SmartWrite-style read/write coordination
- a subagent performing a third edit in the same file
- review and confidence exchange
- lease-aware compaction and retirement
