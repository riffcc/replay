"""Simulated agent behaviors for replay-experiment."""

from __future__ import annotations

from dataclasses import dataclass, field

from .board import Board
from .events import (
    AgentActivityPayload,
    AgentCommentPayload,
    AgentHandoffPayload,
    ConfidenceReportedPayload,
    EditIntentPayload,
    EventKind,
    FileTarget,
    LineSpan,
    ReviewRequestedPayload,
    ReviewResultPayload,
    Routing,
    SectionClaimPayload,
    SectionReleasePayload,
    ShadowAbandonedPayload,
    ShadowCommittedPayload,
    ShadowCreatedPayload,
    ShadowReadyPayload,
    ShadowUpdatedPayload,
    Transition,
    WriteAppliedPayload,
    WriteRejectedPayload,
    WriteProposedPayload,
    make_record,
    new_id,
    now_ms,
)
from .workspace import Workspace


@dataclass
class Agent:
    agent_id: str
    board: Board
    default_group_id: str | None = None
    default_task_id: str | None = None
    default_workspace_id: str | None = None
    comments: list[str] = field(default_factory=list)

    def _routing(self, *, task_id: str | None = None, group_id: str | None = None, workspace_id: str | None = None, topics: list[str] | None = None) -> Routing:
        tags = ["global"]
        if task_id:
            tags.append(f"task:{task_id}")
        if group_id:
            tags.append(f"group:{group_id}")
        if workspace_id:
            tags.append(f"workspace:{workspace_id}")
        return Routing(stream_tags=tags, topics=topics or [])

    def set_activity(self, state: str, summary: str, *, task_id: str | None = None):
        task_id = task_id or self.default_task_id
        return self.board.append(
            make_record(
                kind=EventKind.AGENT_ACTIVITY,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.default_workspace_id,
                payload=AgentActivityPayload(state=state, summary=summary),  # type: ignore[arg-type]
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.default_workspace_id, topics=["agents"]),
                transition=Transition(
                    state_key=f"agent:{self.agent_id}:activity",
                    visible=True,
                    compaction_mode="aggressive",
                ),
            )
        )

    def comment(self, text: str, *, task_id: str | None = None, mentions: list[str] | None = None):
        task_id = task_id or self.default_task_id
        self.comments.append(text)
        return self.board.append(
            make_record(
                kind=EventKind.AGENT_COMMENT,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.default_workspace_id,
                payload=AgentCommentPayload(text=text, mentions=mentions or []),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.default_workspace_id, topics=["chat"]),
            )
        )

    def handoff(self, summary: str, *, to_agent_id: str | None = None, requested_review: bool = False, task_id: str | None = None):
        task_id = task_id or self.default_task_id
        return self.board.append(
            make_record(
                kind=EventKind.AGENT_HANDOFF,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.default_workspace_id,
                payload=AgentHandoffPayload(
                    to_agent_id=to_agent_id,
                    summary=summary,
                    requested_review=requested_review,
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.default_workspace_id, topics=["handoff"]),
            )
        )

    def lease_record(self, record_id: str, *, scope: str = "task", reason: str = "needed for context") -> str:
        lease, _ = self.board.lease_record(
            record_id=record_id,
            agent_id=self.agent_id,
            scope=scope,
            reason=reason,
        )
        return lease.lease_id

    def release_record(self, lease_id: str, *, reason: str = "completed") -> None:
        self.board.release_lease(lease_id=lease_id, agent_id=self.agent_id, reason=reason)

    def request_review(self, *, targets: list[FileTarget], summary: str, kind: str = "code", requested_from: str | None = None, task_id: str | None = None) -> str:
        task_id = task_id or self.default_task_id
        review_id = new_id("review")
        self.board.append(
            make_record(
                kind=EventKind.REVIEW_REQUESTED,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.default_workspace_id,
                payload=ReviewRequestedPayload(
                    review_id=review_id,
                    kind=kind,  # type: ignore[arg-type]
                    targets=targets,
                    summary=summary,
                    requested_from=requested_from,
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.default_workspace_id, topics=["review"]),
            )
        )
        return review_id

    def submit_review(self, *, review_id: str, verdict: str, confidence: float, findings: list[str], task_id: str | None = None):
        task_id = task_id or self.default_task_id
        self.board.append(
            make_record(
                kind=EventKind.REVIEW_RESULT,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.default_workspace_id,
                payload=ReviewResultPayload(
                    review_id=review_id,
                    verdict=verdict,  # type: ignore[arg-type]
                    confidence=confidence,
                    findings=findings,
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.default_workspace_id, topics=["review"]),
            )
        )
        self.board.append(
            make_record(
                kind=EventKind.CONFIDENCE_REPORTED,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.default_workspace_id,
                payload=ConfidenceReportedPayload(subject=review_id, confidence=confidence),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.default_workspace_id, topics=["review", "confidence"]),
            )
        )


@dataclass
class EditingAgent(Agent):
    workspace: Workspace | None = None

    def edit_intent(self, *, target: FileTarget, purpose: str, task_id: str | None = None, mode: str = "write", atomic: bool = False, lint_gate: bool = False):
        task_id = task_id or self.default_task_id
        return self.board.append(
            make_record(
                kind=EventKind.EDIT_INTENT,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=target.workspace_id,
                payload=EditIntentPayload(target=target, purpose=purpose, mode=mode, atomic=atomic, lint_gate=lint_gate),  # type: ignore[arg-type]
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=target.workspace_id, topics=["edits"]),
            )
        )

    def smart_read(self, *, path: str, span: LineSpan, purpose: str, task_id: str | None = None) -> dict[str, object]:
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        target = self.workspace.target_for(path, span)
        intent = self.edit_intent(target=target, purpose=purpose, task_id=task_id, mode="read")
        lease_id = self.lease_record(intent.id, scope="operation", reason=f"reading context for {purpose}")
        relevant = self.board.interleaved_view(
            stream_tags=[f"workspace:{self.workspace.workspace_id}", f"task:{task_id}"],
            topics=["edits", "writes", "claims", "review", "shadow"],
            include_hidden_leased=True,
            agent_id=self.agent_id,
        )
        file_state = self.workspace.get_file(path)
        lines = file_state.read_span(span)
        return {
            "intent_id": intent.id,
            "lease_id": lease_id,
            "target": target,
            "version": file_state.version,
            "lines": lines,
            "relevant_records": relevant,
        }

    def smart_write(
        self,
        *,
        path: str,
        span: LineSpan,
        new_lines: list[str],
        purpose: str,
        task_id: str | None = None,
        claim_id: str | None = None,
        expected_version: int | None = None,
        lint_gate: bool = False,
        atomic: bool = False,
        auto_claim: bool = False,
    ) -> dict[str, object]:
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        read_snapshot = self.smart_read(path=path, span=span, purpose=purpose, task_id=task_id)
        target = read_snapshot["target"]
        assert isinstance(target, FileTarget)
        self.edit_intent(target=target, purpose=purpose, task_id=task_id, mode="write", atomic=atomic, lint_gate=lint_gate)

        effective_claim_id = claim_id
        transient_claim = False
        if effective_claim_id is None and auto_claim:
            ok, new_claim_id, _ = self.claim_section(
                path=path,
                spans=[span],
                purpose=purpose,
                task_id=task_id,
            )
            if not ok:
                self.release_record(str(read_snapshot["lease_id"]), reason="abandoned")
                return {
                    "ok": False,
                    "reason": "claim_conflict",
                    "claim_id": None,
                    "before_version": read_snapshot["version"],
                    "after_version": None,
                    "intent_id": read_snapshot["intent_id"],
                }
            effective_claim_id = new_claim_id
            transient_claim = True

        ok = self.write(
            path=path,
            span=span,
            new_lines=new_lines,
            purpose=purpose,
            claim_id=effective_claim_id,
            expected_version=(read_snapshot["version"] if expected_version is None else expected_version),
            lint_gate=lint_gate,
            atomic=atomic,
            task_id=task_id,
        )
        after_version = self.workspace.get_file(path).version if ok else None
        self.release_record(str(read_snapshot["lease_id"]), reason=("completed" if ok else "abandoned"))
        if transient_claim and effective_claim_id is not None:
            self.release_section(claim_id=effective_claim_id, path=path, spans=[span], reason=("completed" if ok else "abandoned"), task_id=task_id)
        return {
            "ok": ok,
            "reason": "ok" if ok else "write_rejected",
            "claim_id": effective_claim_id,
            "before_version": read_snapshot["version"],
            "after_version": after_version,
            "intent_id": read_snapshot["intent_id"],
            "read_lease_id": read_snapshot["lease_id"],
        }

    def spawn_subagent(self, *, suffix: str, task_id: str | None = None) -> "EditingAgent":
        return EditingAgent(
            agent_id=f"{self.agent_id}.{suffix}",
            board=self.board,
            default_group_id=self.default_group_id,
            default_task_id=task_id or self.default_task_id,
            default_workspace_id=self.default_workspace_id,
            workspace=self.workspace,
        )

    def create_shadow(self, *, path: str, spans: list[LineSpan], purpose: str, task_id: str | None = None) -> str:
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        shadow_id = new_id("shadow")
        shadow = self.workspace.create_shadow(
            shadow_id=shadow_id,
            owner_agent_id=self.agent_id,
            path=path,
            spans=spans,
            purpose=purpose,
        )
        self.board.append(
            make_record(
                kind=EventKind.SHADOW_CREATED,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.workspace.workspace_id,
                payload=ShadowCreatedPayload(
                    shadow_id=shadow.shadow_id,
                    target=self.workspace.target_for(path, *spans),
                    purpose=purpose,
                    base_version=shadow.base_version,
                    contributors=sorted(shadow.contributors),
                    status=shadow.status,  # type: ignore[arg-type]
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["shadow", "edits"]),
                transition=Transition(state_key=f"shadow:{shadow.shadow_id}", visible=True, compaction_mode="conservative"),
            )
        )
        return shadow_id

    def update_shadow(self, *, shadow_id: str, span: LineSpan, new_lines: list[str], summary: str, task_id: str | None = None, status: str | None = None):
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        shadow = self.workspace.update_shadow(
            shadow_id=shadow_id,
            span=span,
            new_lines=new_lines,
            contributor=self.agent_id,
            status=status,
        )
        self.board.append(
            make_record(
                kind=EventKind.SHADOW_UPDATED,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.workspace.workspace_id,
                payload=ShadowUpdatedPayload(
                    shadow_id=shadow.shadow_id,
                    target=self.workspace.target_for(shadow.file_path, *shadow.spans),
                    summary=summary,
                    contributors=sorted(shadow.contributors),
                    status=shadow.status,  # type: ignore[arg-type]
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["shadow", "edits"]),
                transition=Transition(state_key=f"shadow:{shadow.shadow_id}", visible=True, compaction_mode="conservative"),
            )
        )

    def mark_shadow_ready(self, *, shadow_id: str, summary: str, task_id: str | None = None):
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        shadow = self.workspace.get_shadow(shadow_id)
        shadow.status = "ready"
        self.board.append(
            make_record(
                kind=EventKind.SHADOW_READY,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.workspace.workspace_id,
                payload=ShadowReadyPayload(
                    shadow_id=shadow.shadow_id,
                    target=self.workspace.target_for(shadow.file_path, *shadow.spans),
                    summary=summary,
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["shadow"]),
            )
        )

    def commit_shadow(self, *, shadow_id: str, summary: str, task_id: str | None = None, lint_gate: bool = False) -> bool:
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        shadow = self.workspace.get_shadow(shadow_id)
        ok, reason, before, after = self.workspace.commit_shadow(
            shadow_id=shadow_id,
            agent_id=self.agent_id,
            lint_gate=lint_gate,
        )
        if ok:
            self.board.append(
                make_record(
                    kind=EventKind.SHADOW_COMMITTED,
                    agent_id=self.agent_id,
                    task_id=task_id,
                    group_id=self.default_group_id,
                    workspace_id=self.workspace.workspace_id,
                    payload=ShadowCommittedPayload(
                        shadow_id=shadow.shadow_id,
                        target=self.workspace.target_for(shadow.file_path, *shadow.spans),
                        before_version=int(before or 0),
                        after_version=int(after or 0),
                        summary=summary,
                    ),
                    routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["shadow", "writes"]),
                )
            )
            return True

        self.board.append(
            make_record(
                kind=EventKind.WRITE_REJECTED,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.workspace.workspace_id,
                payload=WriteRejectedPayload(
                    target=self.workspace.target_for(shadow.file_path, *shadow.spans),
                    claim_id=None,
                    reason=reason,  # type: ignore[arg-type]
                    detail="shadow commit rejected",
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["shadow", "writes"]),
            )
        )
        return False

    def abandon_shadow(self, *, shadow_id: str, reason: str, task_id: str | None = None):
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        shadow = self.workspace.abandon_shadow(shadow_id)
        self.board.append(
            make_record(
                kind=EventKind.SHADOW_ABANDONED,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.workspace.workspace_id,
                payload=ShadowAbandonedPayload(
                    shadow_id=shadow.shadow_id,
                    target=self.workspace.target_for(shadow.file_path, *shadow.spans),
                    reason=reason,
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["shadow"]),
            )
        )

    def claim_section(
        self,
        *,
        path: str,
        spans: list[LineSpan],
        purpose: str,
        task_id: str | None = None,
        lease_ms: int | None = 30_000,
        force: bool = False,
    ) -> tuple[bool, str, list[str]]:
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        claim_id = new_id("claim")
        acquired_at = now_ms()
        expires_at = acquired_at + lease_ms if lease_ms is not None else None
        ok, claim, contenders = self.workspace.claim_section(
            claim_id=claim_id,
            agent_id=self.agent_id,
            path=path,
            spans=spans,
            purpose=purpose,
            acquired_at=acquired_at,
            expires_at=expires_at,
            force=force,
        )
        target = self.workspace.target_for(path, *spans)
        self.board.append(
            make_record(
                kind=EventKind.SECTION_CLAIM,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.workspace.workspace_id,
                payload=SectionClaimPayload(
                    claim_id=claim.claim_id,
                    target=target,
                    purpose=purpose,
                    lease_owner=self.agent_id,
                    lease_expires_at=expires_at,
                    status="stolen" if force and contenders else ("contended" if not ok else "held"),
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["claims"]),
                transition=Transition(
                    state_key=f"claim:{claim.claim_id}",
                    visible=True,
                    compaction_mode="conservative",
                ),
            )
        )
        return ok, claim.claim_id, [c.claim_id for c in contenders]

    def release_section(self, *, claim_id: str, path: str, spans: list[LineSpan], reason: str = "completed", task_id: str | None = None):
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        self.workspace.release_claim(claim_id)
        self.board.append(
            make_record(
                kind=EventKind.SECTION_RELEASE,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.workspace.workspace_id,
                payload=SectionReleasePayload(
                    claim_id=claim_id,
                    target=self.workspace.target_for(path, *spans),
                    reason=reason,  # type: ignore[arg-type]
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["claims"]),
                transition=Transition(
                    visible=False,
                ),
            )
        )

    def write(
        self,
        *,
        path: str,
        span: LineSpan,
        new_lines: list[str],
        purpose: str,
        claim_id: str | None = None,
        expected_version: int | None = None,
        lint_gate: bool = False,
        atomic: bool = False,
        task_id: str | None = None,
    ) -> bool:
        if self.workspace is None:
            raise RuntimeError("editing agent has no workspace")
        task_id = task_id or self.default_task_id
        target = self.workspace.target_for(path, span)
        self.board.append(
            make_record(
                kind=EventKind.WRITE_PROPOSED,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.workspace.workspace_id,
                payload=WriteProposedPayload(target=target, claim_id=claim_id, summary=purpose),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["writes"]),
            )
        )
        ok, reason, before, after = self.workspace.write_lines(
            agent_id=self.agent_id,
            path=path,
            span=span,
            new_lines=new_lines,
            claim_id=claim_id,
            expected_version=expected_version,
            now_ms=now_ms(),
        )
        post_target = self.workspace.target_for(path, span)
        if lint_gate and any("FAIL" in line for line in new_lines):
            ok = False
            reason = "lint_failed"
        if ok:
            self.board.append(
                make_record(
                    kind=EventKind.WRITE_APPLIED,
                    agent_id=self.agent_id,
                    task_id=task_id,
                    group_id=self.default_group_id,
                    workspace_id=self.workspace.workspace_id,
                    payload=WriteAppliedPayload(
                        target=post_target,
                        claim_id=claim_id,
                        before_version=before,
                        after_version=after,
                        summary=purpose,
                        lint_ok=(True if lint_gate else None),
                    ),
                    routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["writes"]),
                )
            )
            return True

        self.board.append(
            make_record(
                kind=EventKind.WRITE_REJECTED,
                agent_id=self.agent_id,
                task_id=task_id,
                group_id=self.default_group_id,
                workspace_id=self.workspace.workspace_id,
                payload=WriteRejectedPayload(
                    target=post_target,
                    claim_id=claim_id,
                    reason=reason,  # type: ignore[arg-type]
                    detail=("atomic write rejected" if atomic else None),
                ),
                routing=self._routing(task_id=task_id, group_id=self.default_group_id, workspace_id=self.workspace.workspace_id, topics=["writes"]),
            )
        )
        return False


__all__ = ["Agent", "EditingAgent"]
