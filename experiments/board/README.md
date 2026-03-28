# replay-experiment / board

This workspace is the home for the Python simulation of Replay's shared Board model.

## Goal

Model a multi-agent coordination system where:
- one shared Board acts as both timeline and live coordination surface
- agents coordinate through meaningful Board records
- the coordinator compacts stale state and shapes the live surface
- superseded records can disappear from live views immediately
- leased records remain retained until nobody still depends on them
- same-workspace, same-file parallelism is possible through section claims
- review and confidence exchange are first-class behaviors
- SmartRead/SmartWrite-style read/write coordination is simulated
- task agents can fan out sub-edits to subagents in the same workspace
- shadow writes allow speculative collaborative drafting before atomic commit

## Initial module layout

- `events.py` — shared record envelope and event taxonomy
- `board.py` — Board append/read/view behavior and lease-aware retention
- `coordinator.py` — compaction, grouping, and coordination policy
- `workspace.py` — simulated files, versions, claims, shadow buffers, and edit application
- `agent.py` — simulated agents and behaviors
- `sim.py` — runnable canonical scenario
- `DESIGN.md` — focused design doc, invariants, and failure semantics

## Schema direction

The v0 schema is intentionally opinionated:
- one shared JSONL-like Board, not per-agent streams
- rich routing metadata for interleaved global + topical views
- structural edit regions, not only raw line ranges
- explicit transition metadata for supersession, tombstoning, and live visibility
- record leases that defer retirement without implying authority
- no heartbeats; only meaningful activity belongs on the Board
- compaction policy should be configurable, not hard-coded

## Core invariant

A record may leave the live Board as soon as it is superseded, but it may not be retired while any active lease depends on it.

## Raw stream vs live board

The simulation now treats these as distinct surfaces:

- **Raw stream**
  - immutable
  - append-only
  - chronological
  - the human/audit-facing event stream

- **Live board**
  - mutable
  - shaped by coordinator policy
  - hides superseded/tombstoned records
  - retains hidden records while leases are active

Compaction affects the **live board**, not the **raw stream**.

## Implemented so far

- fused Board record schema and event taxonomy
- append-only shared Board with:
  - immutable raw chronology
  - visible live surface
  - interleaved filtered views
  - supersession/tombstoning
  - lease-aware retirement from the live board
- lightweight coordinator for:
  - task creation/status changes
  - task relations
  - workspace/group assignment
  - aggressive task-surface compaction
  - claim reassignment records
- simulated workspace with:
  - line-oriented files
  - versioned writes
  - section claims
  - overlap detection
  - basic force-steal behavior
  - shadow write buffers
  - atomic shadow commit
- simulated agents with:
  - activity, comments, handoffs
  - record leasing/release
  - review requests/results/confidence
  - edit intent, section claiming, and writes
  - SmartRead/SmartWrite-style read-then-write coordination
  - optional auto-claim for transient edits
  - subagent spawning within the same workspace
  - shadow create/update/ready/commit/abandon flow
- canonical scenario in `sim.py` covering:
  - same-file non-overlapping edits
  - overlapping write rejection
  - lease-aware compaction
  - review flow
  - subagent fan-out and cheap parallel sub-editing
  - collaborative shadow drafting before atomic commit

## Current limitations

Still intentionally simplified:
- no structural anchor relocation yet
- SmartRead/SmartWrite reconciliation is still lightweight and line/version based
- no automatic scheduler or director policy yet
- no richer dependency inference yet
- no persistence layer beyond in-memory Board state
- no multi-turn subagent negotiation yet
- no shadow drift/rebase handling yet
- pretty stream rendering exists only as a minimal scaffold, not a full TUI

## Seeing the simulation

Run:

```bash
python3 -m experiments.board.sim --view both
```

Other views:

```bash
python3 -m experiments.board.sim --view raw
python3 -m experiments.board.sim --view live
```

- `--view raw` shows the immutable append-only stream
- `--view live` shows the coordinator-curated live board + retention state
- `--view both` shows both surfaces

## Stream rendering direction

The raw stream renderer now aims toward a format like:

```text
[09:10.100][vbp][alpha]📝 write(experiments/board/events.py)
```

Current implementation is a simple scaffold with:
- timestamp
- task/bead-ish token
- agent lane token
- icon by event kind
- short event summary

Future improvements:
- real rainbow temporal coloring
- better bead/thread naming
- richer tool/action summaries
- TUI-friendly live tailing
