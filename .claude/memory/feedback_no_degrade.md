---
name: never degrade functionality
description: NEVER disable, cap, limit, or degrade any functionality without explicit guidance. Fix the actual bug instead.
type: feedback
---

NEVER disable, cap, limit, or degrade the functionality of any part of the system without explicit guidance from Wings.

**Why:** Wings values the full power of every feature. Capping file sizes, disabling tree-sitter for large files, gating features behind modes — these are all degradations disguised as fixes. The correct response to a bug is to fix the bug, not to work around it by reducing what the tool can do.

**How to apply:** When a component causes a problem (memory leak, crash, etc), investigate and fix the root cause. If you can't fix it immediately, file a bead and document what you know. Do NOT add size caps, feature gates, or "skip if too big" guards unless Wings explicitly asks for them.
