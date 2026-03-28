"""Shared Board record schema and event taxonomy for replay-experiment.

Design constraints for v0:
- one shared Board, not per-agent streams
- a fused event/state surface, not separate history and live-state stores
- rich routing metadata so global and topical views can be interleaved naturally
- no heartbeat spam; only meaningful activity records belong on the Board
- structural regions for coordinated edits, with room for later relocation logic
- compaction and supersession are first-class and policy-configurable
- leases defer retirement of superseded records without implying authority
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from enum import StrEnum
import json
import time
from typing import Any, Literal, TypeAlias
import uuid


EventKindLiteral: TypeAlias = Literal[
    "task.created",
    "task.status",
    "task.relation",
    "agent.activity",
    "agent.comment",
    "agent.handoff",
    "edit.intent",
    "section.claim",
    "section.release",
    "write.proposed",
    "write.applied",
    "write.rejected",
    "shadow.created",
    "shadow.updated",
    "shadow.review_requested",
    "shadow.review_result",
    "shadow.rebased",
    "shadow.ready",
    "shadow.committed",
    "shadow.abandoned",
    "review.requested",
    "review.result",
    "confidence.reported",
    "record.leased",
    "record.released",
    "coord.compact",
    "coord.grouping",
    "coord.reassign",
    "coord.retired",
]


class EventKind(StrEnum):
    TASK_CREATED = "task.created"
    TASK_STATUS = "task.status"
    TASK_RELATION = "task.relation"

    AGENT_ACTIVITY = "agent.activity"
    AGENT_COMMENT = "agent.comment"
    AGENT_HANDOFF = "agent.handoff"

    EDIT_INTENT = "edit.intent"
    SECTION_CLAIM = "section.claim"
    SECTION_RELEASE = "section.release"
    WRITE_PROPOSED = "write.proposed"
    WRITE_APPLIED = "write.applied"
    WRITE_REJECTED = "write.rejected"

    SHADOW_CREATED = "shadow.created"
    SHADOW_UPDATED = "shadow.updated"
    SHADOW_REVIEW_REQUESTED = "shadow.review_requested"
    SHADOW_REVIEW_RESULT = "shadow.review_result"
    SHADOW_REBASED = "shadow.rebased"
    SHADOW_READY = "shadow.ready"
    SHADOW_COMMITTED = "shadow.committed"
    SHADOW_ABANDONED = "shadow.abandoned"

    REVIEW_REQUESTED = "review.requested"
    REVIEW_RESULT = "review.result"
    CONFIDENCE_REPORTED = "confidence.reported"

    RECORD_LEASED = "record.leased"
    RECORD_RELEASED = "record.released"

    COORD_COMPACT = "coord.compact"
    COORD_GROUPING = "coord.grouping"
    COORD_REASSIGN = "coord.reassign"
    COORD_RETIRED = "coord.retired"


TaskState: TypeAlias = Literal[
    "pending",
    "ready",
    "in_progress",
    "blocked",
    "stuck",
    "review",
    "done",
    "abandoned",
]

TaskRelation: TypeAlias = Literal[
    "related_to",
    "depends_on",
    "blocks",
    "blocked_by",
    "duplicates",
    "parent_of",
    "child_of",
]

AgentActivityState: TypeAlias = Literal[
    "idle",
    "planning",
    "working",
    "reviewing",
    "waiting",
    "stuck",
    "done",
]

WriteFailureReason: TypeAlias = Literal[
    "claim_conflict",
    "stale_target",
    "stale_version",
    "lint_failed",
    "review_blocked",
    "policy_denied",
    "abandoned",
    "unknown",
]

ReviewVerdict: TypeAlias = Literal[
    "approve",
    "needs_changes",
    "blocked",
    "uncertain",
]

CompactionMode: TypeAlias = Literal[
    "aggressive",
    "conservative",
]

RefKind: TypeAlias = Literal[
    "task",
    "group",
    "workspace",
    "agent",
    "claim",
    "event",
    "file",
    "review",
    "topic",
    "lease",
    "shadow",
]


@dataclass(frozen=True)
class Ref:
    kind: RefKind
    id: str


@dataclass(frozen=True)
class Routing:
    """Routing metadata for interleaved global + topical Board views."""

    stream_tags: list[str] = field(default_factory=lambda: ["global"])
    topics: list[str] = field(default_factory=list)
    refs: list[Ref] = field(default_factory=list)


@dataclass(frozen=True)
class LineSpan:
    start: int
    end: int


@dataclass(frozen=True)
class Anchor:
    """Lightweight structural attachment point."""

    label: str
    text_hint: str | None = None
    occurrence: int = 1


@dataclass(frozen=True)
class StructuralRegion:
    """A region an agent intends to inspect or modify."""

    kind: Literal[
        "line_span",
        "function",
        "class",
        "block",
        "symbol",
        "section",
        "file",
    ]
    name: str | None = None
    span: LineSpan | None = None
    start_anchor: Anchor | None = None
    end_anchor: Anchor | None = None
    text_hint: str | None = None


@dataclass(frozen=True)
class FileTarget:
    workspace_id: str
    path: str
    version: int | None = None
    regions: list[StructuralRegion] = field(default_factory=list)


@dataclass(frozen=True)
class Transition:
    """State-surface metadata showing how meaning mutates on the Board."""

    supersedes: list[str] = field(default_factory=list)
    tombstones: list[str] = field(default_factory=list)
    state_key: str | None = None
    visible: bool = True
    compaction_mode: CompactionMode | None = None


@dataclass(frozen=True)
class TaskCreatedPayload:
    title: str
    summary: str


@dataclass(frozen=True)
class TaskStatusPayload:
    state: TaskState
    reason: str | None = None
    confidence: float | None = None


@dataclass(frozen=True)
class TaskRelationPayload:
    relation: TaskRelation
    other_task_id: str
    direct: bool = True
    note: str | None = None


@dataclass(frozen=True)
class AgentActivityPayload:
    state: AgentActivityState
    summary: str
    confidence: float | None = None


@dataclass(frozen=True)
class AgentCommentPayload:
    text: str
    mentions: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class AgentHandoffPayload:
    to_agent_id: str | None
    summary: str
    requested_review: bool = False


@dataclass(frozen=True)
class EditIntentPayload:
    target: FileTarget
    purpose: str
    mode: Literal["read", "write", "rewrite", "review"] = "write"
    atomic: bool = False
    lint_gate: bool = False


@dataclass(frozen=True)
class SectionClaimPayload:
    claim_id: str
    target: FileTarget
    purpose: str
    lease_owner: str
    lease_expires_at: int | None = None
    status: Literal["held", "contended", "stolen"] = "held"


@dataclass(frozen=True)
class SectionReleasePayload:
    claim_id: str
    target: FileTarget
    reason: Literal["completed", "abandoned", "expired", "stolen"]


@dataclass(frozen=True)
class WriteProposedPayload:
    target: FileTarget
    claim_id: str | None = None
    summary: str = ""


@dataclass(frozen=True)
class WriteAppliedPayload:
    target: FileTarget
    claim_id: str | None = None
    before_version: int | None = None
    after_version: int | None = None
    summary: str = ""
    lint_ok: bool | None = None


@dataclass(frozen=True)
class WriteRejectedPayload:
    target: FileTarget
    claim_id: str | None = None
    reason: WriteFailureReason = "unknown"
    detail: str | None = None


@dataclass(frozen=True)
class ShadowCreatedPayload:
    shadow_id: str
    target: FileTarget
    purpose: str
    base_version: int
    contributors: list[str] = field(default_factory=list)
    status: Literal["draft", "review", "ready"] = "draft"


@dataclass(frozen=True)
class ShadowUpdatedPayload:
    shadow_id: str
    target: FileTarget
    summary: str
    contributors: list[str] = field(default_factory=list)
    status: Literal["draft", "review", "ready"] = "draft"


@dataclass(frozen=True)
class ShadowReviewRequestedPayload:
    shadow_id: str
    summary: str
    requested_from: str | None = None


@dataclass(frozen=True)
class ShadowReviewResultPayload:
    shadow_id: str
    verdict: ReviewVerdict
    confidence: float
    findings: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class ShadowRebasedPayload:
    shadow_id: str
    target: FileTarget
    from_version: int
    to_version: int
    summary: str = ""


@dataclass(frozen=True)
class ShadowReadyPayload:
    shadow_id: str
    target: FileTarget
    summary: str = ""


@dataclass(frozen=True)
class ShadowCommittedPayload:
    shadow_id: str
    target: FileTarget
    before_version: int
    after_version: int
    summary: str = ""


@dataclass(frozen=True)
class ShadowAbandonedPayload:
    shadow_id: str
    target: FileTarget
    reason: str


@dataclass(frozen=True)
class ReviewRequestedPayload:
    review_id: str
    kind: Literal["code", "design", "correctness", "confidence"]
    targets: list[FileTarget] = field(default_factory=list)
    summary: str = ""
    requested_from: str | None = None


@dataclass(frozen=True)
class ReviewResultPayload:
    review_id: str
    verdict: ReviewVerdict
    confidence: float
    findings: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class ConfidenceReportedPayload:
    subject: str
    confidence: float
    note: str | None = None


@dataclass(frozen=True)
class RecordLeasedPayload:
    lease_id: str
    record_id: str
    scope: Literal["turn", "operation", "task"]
    reason: str
    expires_at: int | None = None


@dataclass(frozen=True)
class RecordReleasedPayload:
    lease_id: str
    record_id: str
    reason: Literal["completed", "abandoned", "expired", "superseded_context"]


@dataclass(frozen=True)
class CoordCompactPayload:
    policy: CompactionMode
    target_ids: list[str] = field(default_factory=list)
    reason: str = ""


@dataclass(frozen=True)
class CoordGroupingPayload:
    group_id: str
    workspace_id: str
    task_ids: list[str] = field(default_factory=list)
    reason: str = ""


@dataclass(frozen=True)
class CoordReassignPayload:
    subject_kind: Literal["claim", "task", "review", "shadow"]
    subject_id: str
    from_agent_id: str | None
    to_agent_id: str | None
    reason: str


@dataclass(frozen=True)
class CoordRetiredPayload:
    target_ids: list[str] = field(default_factory=list)
    reason: str = ""


Payload: TypeAlias = (
    TaskCreatedPayload
    | TaskStatusPayload
    | TaskRelationPayload
    | AgentActivityPayload
    | AgentCommentPayload
    | AgentHandoffPayload
    | EditIntentPayload
    | SectionClaimPayload
    | SectionReleasePayload
    | WriteProposedPayload
    | WriteAppliedPayload
    | WriteRejectedPayload
    | ShadowCreatedPayload
    | ShadowUpdatedPayload
    | ShadowReviewRequestedPayload
    | ShadowReviewResultPayload
    | ShadowRebasedPayload
    | ShadowReadyPayload
    | ShadowCommittedPayload
    | ShadowAbandonedPayload
    | ReviewRequestedPayload
    | ReviewResultPayload
    | ConfidenceReportedPayload
    | RecordLeasedPayload
    | RecordReleasedPayload
    | CoordCompactPayload
    | CoordGroupingPayload
    | CoordReassignPayload
    | CoordRetiredPayload
)


@dataclass(frozen=True)
class BoardRecord:
    """One record on the shared Board."""

    id: str
    ts: int
    kind: str
    agent_id: str
    task_id: str | None = None
    group_id: str | None = None
    workspace_id: str | None = None
    routing: Routing = field(default_factory=Routing)
    transition: Transition = field(default_factory=Transition)
    payload: Payload | dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "ts": self.ts,
            "kind": self.kind,
            "agent_id": self.agent_id,
            "task_id": self.task_id,
            "group_id": self.group_id,
            "workspace_id": self.workspace_id,
            "routing": _dc(self.routing),
            "transition": _dc(self.transition),
            "payload": _dc(self.payload),
        }

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), sort_keys=True)


def _dc(value: Any) -> Any:
    if hasattr(value, "__dataclass_fields__"):
        return asdict(value)
    return value


def new_id(prefix: str = "evt") -> str:
    return f"{prefix}-{uuid.uuid4().hex[:10]}"


def now_ms() -> int:
    return int(time.time() * 1000)


def make_record(
    *,
    kind: EventKind | EventKindLiteral,
    agent_id: str,
    payload: Payload | dict[str, Any],
    task_id: str | None = None,
    group_id: str | None = None,
    workspace_id: str | None = None,
    routing: Routing | None = None,
    transition: Transition | None = None,
    record_id: str | None = None,
    ts: int | None = None,
) -> BoardRecord:
    return BoardRecord(
        id=record_id or new_id(),
        ts=ts if ts is not None else now_ms(),
        kind=str(kind),
        agent_id=agent_id,
        task_id=task_id,
        group_id=group_id,
        workspace_id=workspace_id,
        routing=routing or Routing(),
        transition=transition or Transition(),
        payload=payload,
    )


__all__ = [
    "AgentActivityPayload",
    "AgentActivityState",
    "AgentCommentPayload",
    "AgentHandoffPayload",
    "Anchor",
    "BoardRecord",
    "CompactionMode",
    "ConfidenceReportedPayload",
    "CoordCompactPayload",
    "CoordGroupingPayload",
    "CoordReassignPayload",
    "CoordRetiredPayload",
    "EditIntentPayload",
    "EventKind",
    "EventKindLiteral",
    "FileTarget",
    "LineSpan",
    "Payload",
    "RecordLeasedPayload",
    "RecordReleasedPayload",
    "Ref",
    "RefKind",
    "ReviewRequestedPayload",
    "ReviewResultPayload",
    "ReviewVerdict",
    "Routing",
    "SectionClaimPayload",
    "SectionReleasePayload",
    "ShadowAbandonedPayload",
    "ShadowCommittedPayload",
    "ShadowCreatedPayload",
    "ShadowReadyPayload",
    "ShadowRebasedPayload",
    "ShadowReviewRequestedPayload",
    "ShadowReviewResultPayload",
    "ShadowUpdatedPayload",
    "StructuralRegion",
    "TaskCreatedPayload",
    "TaskRelation",
    "TaskRelationPayload",
    "TaskState",
    "TaskStatusPayload",
    "Transition",
    "WriteAppliedPayload",
    "WriteFailureReason",
    "WriteProposedPayload",
    "WriteRejectedPayload",
    "make_record",
    "new_id",
    "now_ms",
]
