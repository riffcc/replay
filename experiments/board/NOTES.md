# Notes

This directory replaces the old single-file `experiments/1-board.py` sketch with a proper workspace.

The design intent is:
- one shared Board, not per-agent streams
- meaningful activity records, not heartbeat spam
- fused timeline + live state surface
- topical/interleaved views over the same underlying Board
- section claims instead of coarse file locks
- simulated first, then eventual real workspace/worktree integration
- supersession controls visibility while leases control retention
- SmartRead/SmartWrite should consult the Board before acting and reconcile after writes
- task agents should be able to fan out smaller sub-edits to subagents in the same workspace
- review and confidence exchange are part of the protocol, not an afterthought
