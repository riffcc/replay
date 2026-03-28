"""Coordinator policy for replay-experiment.

The coordinator is intentionally lightweight in v0:
- it groups related tasks into workspaces
- shapes the live Board surface via supersession/tombstoning-aware policy
- can compact obsolete task states aggressively
- can reassign claims when forced
"""

from __future__ import annotations

from dataclasses import dataclass, field

from .board import Board
from .events import (
    CoordCompactPayload,
    CoordGroupingPayload,
    CoordReassignPayload,
    EventKind,
    Routing,
    TaskRelationPayload,
    TaskState,
    TaskStatusPayload,
    Transition,
    make_record,
)


@dataclass
class Coordinator:
    board: Board
    coordinator_agent_id: str = "coordinator"
    task_to_workspace: dict[str, str] = field(default_factory=dict)
    task_to_group: dict[str, str] = field(default_factory=dict)
    relations: dict[str, list[tuple[str, str]]] = field(default_factory=dict)

    def create_task(self, *, task_id: str, title: str, summary: str) -> None:
        self.board.append(
            make_record(
                kind=EventKind.TASK_CREATED,
                agent_id=self.coordinator_agent_id,
                task_id=task_id,
                payload={"title": title, "summary": summary},
                routing=Routing(stream_tags=["global", f"task:{task_id}"], topics=["tasks"]),
            )
        )

    def set_task_status(
        self,
        *,
        task_id: str,
        state: TaskState,
        reason: str | None = None,
        confidence: float | None = None,
    ):
        return self.board.append(
            make_record(
                kind=EventKind.TASK_STATUS,
                agent_id=self.coordinator_agent_id,
                task_id=task_id,
                payload=TaskStatusPayload(state=state, reason=reason, confidence=confidence),
                routing=Routing(stream_tags=["global", f"task:{task_id}"], topics=["tasks"]),
                transition=Transition(
                    state_key=f"task:{task_id}:status",
                    visible=True,
                    compaction_mode="aggressive",
                ),
            )
        )

    def relate_tasks(self, *, task_id: str, other_task_id: str, relation: str, note: str | None = None):
        self.relations.setdefault(task_id, []).append((relation, other_task_id))
        return self.board.append(
            make_record(
                kind=EventKind.TASK_RELATION,
                agent_id=self.coordinator_agent_id,
                task_id=task_id,
                payload=TaskRelationPayload(
                    relation=relation,  # type: ignore[arg-type]
                    other_task_id=other_task_id,
                    direct=True,
                    note=note,
                ),
                routing=Routing(
                    stream_tags=["global", f"task:{task_id}", f"task:{other_task_id}"],
                    topics=["tasks", "relations"],
                ),
                transition=Transition(
                    state_key=f"relation:{task_id}:{relation}:{other_task_id}",
                    visible=True,
                    compaction_mode="conservative",
                ),
            )
        )

    def assign_workspace(
        self,
        *,
        group_id: str,
        workspace_id: str,
        task_ids: list[str],
        reason: str,
    ):
        for task_id in task_ids:
            self.task_to_workspace[task_id] = workspace_id
            self.task_to_group[task_id] = group_id
        return self.board.append(
            make_record(
                kind=EventKind.COORD_GROUPING,
                agent_id=self.coordinator_agent_id,
                group_id=group_id,
                workspace_id=workspace_id,
                payload=CoordGroupingPayload(
                    group_id=group_id,
                    workspace_id=workspace_id,
                    task_ids=task_ids,
                    reason=reason,
                ),
                routing=Routing(
                    stream_tags=["global", f"group:{group_id}", f"workspace:{workspace_id}"],
                    topics=["groups", "workspaces"],
                ),
                transition=Transition(
                    state_key=f"group:{group_id}:workspace",
                    visible=True,
                    compaction_mode="conservative",
                ),
            )
        )

    def compact_task_surface(self, *, task_id: str, reason: str = "status advanced"):
        visible = [r for r in self.board.visible_records() if r.task_id == task_id]
        obsolete = [r.id for r in visible if r.kind == EventKind.TASK_STATUS]
        if len(obsolete) <= 1:
            return None
        return self.board.append(
            make_record(
                kind=EventKind.COORD_COMPACT,
                agent_id=self.coordinator_agent_id,
                task_id=task_id,
                payload=CoordCompactPayload(
                    policy="aggressive",
                    target_ids=obsolete[:-1],
                    reason=reason,
                ),
                routing=Routing(stream_tags=["global", f"task:{task_id}"], topics=["tasks", "compaction"]),
                transition=Transition(
                    tombstones=obsolete[:-1],
                    visible=False,
                    compaction_mode="aggressive",
                ),
            )
        )

    def reassign_claim(
        self,
        *,
        claim_id: str,
        from_agent_id: str | None,
        to_agent_id: str | None,
        task_id: str | None,
        group_id: str | None,
        workspace_id: str | None,
        reason: str,
    ):
        return self.board.append(
            make_record(
                kind=EventKind.COORD_REASSIGN,
                agent_id=self.coordinator_agent_id,
                task_id=task_id,
                group_id=group_id,
                workspace_id=workspace_id,
                payload=CoordReassignPayload(
                    subject_kind="claim",
                    subject_id=claim_id,
                    from_agent_id=from_agent_id,
                    to_agent_id=to_agent_id,
                    reason=reason,
                ),
                routing=Routing(
                    stream_tags=["global"] + ([f"task:{task_id}"] if task_id else []),
                    topics=["claims", "reassign"],
                ),
            )
        )


__all__ = ["Coordinator"]
