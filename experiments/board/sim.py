"""Runnable simulation entrypoint for replay-experiment.

This module renders two distinct surfaces:
- raw stream: immutable append-only chronology
- live board: coordinator-curated current surface

It supports two scenarios:
- canonical: the original tiny deterministic demo
- swarm: a more realistic 30-agent run across 3 groups of 10 with noisy
  coordination, contention, retries, reviews, and shadow collaboration
"""

from __future__ import annotations

import argparse
import random
import re
import sys
import time
from dataclasses import dataclass
from datetime import datetime
from typing import Callable

from .agent import Agent, EditingAgent
from .board import Board, Lease
from .coordinator import Coordinator
from .events import EventKind, FileTarget, LineSpan
from .workspace import Workspace


ICON_BY_KIND = {
    EventKind.TASK_CREATED: "📌",
    EventKind.TASK_STATUS: "📋",
    EventKind.TASK_RELATION: "🔗",
    EventKind.AGENT_ACTIVITY: "🤖",
    EventKind.AGENT_COMMENT: "💬",
    EventKind.AGENT_HANDOFF: "🤝",
    EventKind.EDIT_INTENT: "📝",
    EventKind.SECTION_CLAIM: "🔒",
    EventKind.SECTION_RELEASE: "🔓",
    EventKind.WRITE_PROPOSED: "✏️ ",
    EventKind.WRITE_APPLIED: "✅",
    EventKind.WRITE_REJECTED: "⛔",
    EventKind.SHADOW_CREATED: "🌒",
    EventKind.SHADOW_UPDATED: "🫥",
    EventKind.SHADOW_READY: "🌕",
    EventKind.SHADOW_COMMITTED: "🚀",
    EventKind.SHADOW_ABANDONED: "🗑️",
    EventKind.REVIEW_REQUESTED: "👀",
    EventKind.REVIEW_RESULT: "🧪",
    EventKind.CONFIDENCE_REPORTED: "📏",
    EventKind.RECORD_LEASED: "📎",
    EventKind.RECORD_RELEASED: "🪄",
    EventKind.COORD_GROUPING: "🧭",
    EventKind.COORD_COMPACT: "🧹",
    EventKind.COORD_REASSIGN: "🔁",
    EventKind.COORD_RETIRED: "🪦",
}

RAINBOW = [31, 33, 32, 36, 34, 35, 91, 95, 94, 96]
RESET = "\x1b[0m"


@dataclass
class DemoContext:
    board: Board
    coordinator: Coordinator
    workspace: Workspace
    agent_a: EditingAgent
    agent_b: EditingAgent
    agent_c: EditingAgent
    reviewer: Agent
    claim_a: str | None = None
    claim_b: str | None = None
    planning_lease: Lease | None = None
    pending_record_id: str | None = None
    shadow_id: str | None = None
    review_id: str | None = None
    smart_a: dict[str, object] | None = None
    overlapping_ok: bool | None = None
    committed_shadow: bool | None = None


@dataclass
class SwarmContext:
    board: Board
    coordinator: Coordinator
    workspaces: dict[str, Workspace]
    agents: dict[str, EditingAgent]
    reviewers: dict[str, Agent]
    groups: list[str]
    shadow_ids: dict[str, str]
    claims: dict[tuple[str, str], str]
    review_ids: list[str]


@dataclass(frozen=True)
class TimedAction:
    at: float
    fn: Callable[[], None]
    label: str


@dataclass(frozen=True)
class AgentSpec:
    bead: str
    lane: str
    group_id: str
    task_id: str
    agent_id: str
    role: str
    focus_path: str
    primary_span: LineSpan
    secondary_span: LineSpan | None = None


@dataclass(frozen=True)
class SimulationResult:
    board: Board
    workspaces: dict[str, Workspace]
    primary_workspace_id: str
    scenario: str


def create_demo_context() -> DemoContext:
    board = Board()
    coordinator = Coordinator(board)
    workspace = Workspace("ws-main")
    workspace.add_file(
        "src/example.py",
        """def alpha():
    return 1


def beta():
    return 2


def gamma():
    return 3
""",
    )

    agent_a = EditingAgent(
        agent_id="agent-a",
        board=board,
        default_group_id="group-main",
        default_task_id="task-a",
        default_workspace_id=workspace.workspace_id,
        workspace=workspace,
    )
    agent_b = EditingAgent(
        agent_id="agent-b",
        board=board,
        default_group_id="group-main",
        default_task_id="task-b",
        default_workspace_id=workspace.workspace_id,
        workspace=workspace,
    )
    agent_c = agent_a.spawn_subagent(suffix="sub1", task_id="task-c")
    reviewer = Agent(
        agent_id="agent-review",
        board=board,
        default_group_id="group-main",
        default_task_id="task-a",
        default_workspace_id=workspace.workspace_id,
    )

    return DemoContext(
        board=board,
        coordinator=coordinator,
        workspace=workspace,
        agent_a=agent_a,
        agent_b=agent_b,
        agent_c=agent_c,
        reviewer=reviewer,
    )


def _fmt_ts(ts: int) -> str:
    dt = datetime.fromtimestamp(ts / 1000)
    return dt.strftime("%H:%M.%S") + f".{int(ts % 1000):03d}"


def _lane(agent_id: str) -> str:
    if "." in agent_id:
        return agent_id.split(".", 1)[1]
    return agent_id.removeprefix("agent-")


def _bead(record) -> str:
    if record.task_id:
        tail = record.task_id.removeprefix("task-")
        return tail.split("-", 1)[0]
    if record.group_id:
        return record.group_id.removeprefix("group-")
    return "-"


def _summary(record) -> str:
    payload = record.payload
    kind = str(record.kind)
    if kind == EventKind.EDIT_INTENT and hasattr(payload, "target"):
        return f"edit.intent({payload.target.path})"
    if kind == EventKind.WRITE_PROPOSED and hasattr(payload, "target"):
        return f"write.proposed({payload.target.path})"
    if kind == EventKind.WRITE_APPLIED and hasattr(payload, "target"):
        return f"write.applied({payload.target.path})"
    if kind == EventKind.WRITE_REJECTED and hasattr(payload, "target"):
        return f"write.rejected({payload.target.path})"
    if kind == EventKind.SECTION_CLAIM and hasattr(payload, "target"):
        return f"section.claim({payload.target.path})"
    if kind == EventKind.SECTION_RELEASE and hasattr(payload, "target"):
        return f"section.release({payload.target.path})"
    if kind == EventKind.SHADOW_CREATED and hasattr(payload, "target"):
        return f"shadow.created({payload.target.path})"
    if kind == EventKind.SHADOW_UPDATED and hasattr(payload, "target"):
        return f"shadow.updated({payload.target.path})"
    if kind == EventKind.SHADOW_COMMITTED and hasattr(payload, "target"):
        return f"shadow.committed({payload.target.path})"
    if kind == EventKind.REVIEW_REQUESTED:
        return f"review.requested({getattr(payload, 'review_id', '-')})"
    if kind == EventKind.REVIEW_RESULT:
        return f"review.result({getattr(payload, 'review_id', '-')})"
    if kind == EventKind.TASK_STATUS:
        return f"task.status({getattr(payload, 'state', '-')})"
    if kind == EventKind.TASK_CREATED:
        return f"task.created({getattr(payload, 'title', '-')})"
    if kind == EventKind.AGENT_ACTIVITY:
        return f"activity({getattr(payload, 'summary', '-')})"
    if kind == EventKind.AGENT_HANDOFF:
        return f"handoff({getattr(payload, 'summary', '-')})"
    if kind == EventKind.RECORD_LEASED:
        return f"record.leased({getattr(payload, 'record_id', '-')})"
    if kind == EventKind.RECORD_RELEASED:
        return f"record.released({getattr(payload, 'record_id', '-')})"
    return kind


def _colorize_timestamp(text: str) -> str:
    sec = int(time.time())
    color = RAINBOW[sec % len(RAINBOW)]
    return f"\x1b[{color}m{text}{RESET}"


def render_record(record) -> str:
    icon = ICON_BY_KIND.get(record.kind, "•")
    ts = _colorize_timestamp(f"[{_fmt_ts(record.ts)}]")
    return f"{ts}[{_bead(record)}][{_lane(record.agent_id)}]{icon} {_summary(record)}"


def render_raw_stream(board: Board) -> str:
    return "\n".join(render_record(record) for record in board.raw_records())


def render_live_board(board: Board) -> str:
    lines = ["== live board =="]
    for record in board.visible_records():
        lines.append(
            f"{record.kind:<22} agent={record.agent_id:<14} task={record.task_id or '-':<12} id={record.id}"
        )
    return "\n".join(lines)


def render_live_retention(board: Board) -> str:
    lines = ["== live retention state =="]
    for record in board.live_records(include_retired=True):
        retired = "retired" if board.is_retired(record.id) else "active"
        visible = "visible" if board.is_visible(record.id) else "hidden"
        lines.append(f"{record.kind:<22} {record.id} {visible} {retired}")
    return "\n".join(lines)


def print_summary(result: SimulationResult, view: str = "both") -> None:
    board = result.board
    workspace = result.workspaces[result.primary_workspace_id]
    if view in {"raw", "both"}:
        print("== raw stream ==")
        print(render_raw_stream(board))
    if view in {"live", "both"}:
        if view == "both":
            print()
        print(render_live_board(board))
        print()
        print(render_live_retention(board))
    print("\n== final file ==")
    file_state = workspace.get_file(next(iter(workspace.files.keys())))
    print(file_state.text())
    print(f"\nversion={file_state.version}")


def parse_duration(value: str) -> float:
    match = re.fullmatch(r"\s*(\d+(?:\.\d+)?)\s*([sm]?)\s*", value)
    if not match:
        raise argparse.ArgumentTypeError(f"invalid duration: {value!r} (expected like 60s or 2m)")
    amount = float(match.group(1))
    unit = match.group(2) or "s"
    if unit == "s":
        return amount
    if unit == "m":
        return amount * 60.0
    raise argparse.ArgumentTypeError(f"unsupported duration unit in {value!r}")


def scenario_actions(ctx: DemoContext) -> list[TimedAction]:
    actions: list[TimedAction] = []

    def add(at: float, label: str, fn: Callable[[], None]) -> None:
        actions.append(TimedAction(at=at, label=label, fn=fn))

    add(0.00, "create task a", lambda: ctx.coordinator.create_task(task_id="task-a", title="Refactor alpha", summary="Split work in one shared workspace"))
    add(0.02, "create task b", lambda: ctx.coordinator.create_task(task_id="task-b", title="Refactor beta", summary="Related task in same workspace"))
    add(0.04, "create task c", lambda: ctx.coordinator.create_task(task_id="task-c", title="Adjust gamma", summary="Cheap sub-edit in same workspace"))
    add(0.08, "relate a->b", lambda: ctx.coordinator.relate_tasks(task_id="task-a", other_task_id="task-b", relation="related_to", note="shared file"))
    add(0.10, "relate a->c", lambda: ctx.coordinator.relate_tasks(task_id="task-a", other_task_id="task-c", relation="parent_of", note="fan-out subedit"))
    add(0.13, "assign shared workspace", lambda: ctx.coordinator.assign_workspace(group_id="group-main", workspace_id=ctx.workspace.workspace_id, task_ids=["task-a", "task-b", "task-c"], reason="related tasks can share one simulated worktree"))

    def pending_seed() -> None:
        pending = ctx.coordinator.set_task_status(task_id="task-a", state="pending", reason="seeded")
        ctx.pending_record_id = pending.id
    add(0.17, "seed pending", pending_seed)

    def lease_pending() -> None:
        assert ctx.pending_record_id is not None
        lease, _ = ctx.board.lease_record(record_id=ctx.pending_record_id, agent_id="agent-a", scope="task", reason="planning from pending state")
        ctx.planning_lease = lease
    add(0.20, "lease pending", lease_pending)

    add(0.23, "start task a", lambda: ctx.coordinator.set_task_status(task_id="task-a", state="in_progress", reason="claimed by agent-a"))
    add(0.25, "start task b", lambda: ctx.coordinator.set_task_status(task_id="task-b", state="in_progress", reason="claimed by agent-b"))
    add(0.27, "start task c", lambda: ctx.coordinator.set_task_status(task_id="task-c", state="in_progress", reason="spawned subtask"))
    add(0.30, "activity a", lambda: ctx.agent_a.set_activity("working", "editing alpha function"))
    add(0.32, "activity b", lambda: ctx.agent_b.set_activity("working", "editing beta function"))
    add(0.34, "activity c", lambda: ctx.agent_c.set_activity("working", "editing gamma via subagent fan-out"))

    def claim_a() -> None:
        ok_a, claim_id, _ = ctx.agent_a.claim_section(path="src/example.py", spans=[LineSpan(1, 2)], purpose="rewrite alpha body")
        assert ok_a
        ctx.claim_a = claim_id
    add(0.38, "claim alpha", claim_a)

    def claim_b() -> None:
        ok_b, claim_id, contenders = ctx.agent_b.claim_section(path="src/example.py", spans=[LineSpan(5, 6)], purpose="rewrite beta body")
        assert ok_b and not contenders
        ctx.claim_b = claim_id
    add(0.40, "claim beta", claim_b)

    def alpha_write() -> None:
        assert ctx.claim_a is not None
        ctx.smart_a = ctx.agent_a.smart_write(path="src/example.py", span=LineSpan(1, 2), new_lines=["def alpha():", "    return 10"], purpose="update alpha return", claim_id=ctx.claim_a, atomic=True, lint_gate=True)
        assert ctx.smart_a["ok"] is True
    add(0.46, "alpha smart write", alpha_write)

    def overlap_reject() -> None:
        assert ctx.claim_b is not None
        ctx.overlapping_ok = ctx.agent_b.write(path="src/example.py", span=LineSpan(1, 2), new_lines=["def alpha():", "    return 99"], purpose="illegal overlapping edit", claim_id=ctx.claim_b, expected_version=0, atomic=True)
        assert not ctx.overlapping_ok
    add(0.52, "beta conflicting write", overlap_reject)

    def create_shadow() -> None:
        ctx.shadow_id = ctx.agent_b.create_shadow(path="src/example.py", spans=[LineSpan(5, 6), LineSpan(9, 10)], purpose="draft beta+gamma as a team before commit")
    add(0.58, "create shadow", create_shadow)

    def shadow_beta() -> None:
        assert ctx.shadow_id is not None
        ctx.agent_b.update_shadow(shadow_id=ctx.shadow_id, span=LineSpan(5, 6), new_lines=["def beta():", "    return 20"], summary="draft beta update in shadow")
    add(0.64, "draft beta in shadow", shadow_beta)

    def shadow_gamma() -> None:
        assert ctx.shadow_id is not None
        ctx.agent_c.update_shadow(shadow_id=ctx.shadow_id, span=LineSpan(9, 10), new_lines=["def gamma():", "    return 30"], summary="subagent contributes gamma change to shared shadow")
    add(0.70, "draft gamma in shadow", shadow_gamma)

    def ready_shadow() -> None:
        assert ctx.shadow_id is not None
        ctx.agent_b.mark_shadow_ready(shadow_id=ctx.shadow_id, summary="shadow draft is coherent and ready")
    add(0.76, "mark shadow ready", ready_shadow)

    def commit_shadow() -> None:
        assert ctx.shadow_id is not None
        ctx.committed_shadow = ctx.agent_b.commit_shadow(shadow_id=ctx.shadow_id, summary="commit beta+gamma from shared shadow", lint_gate=True)
        assert ctx.committed_shadow is True
    add(0.82, "commit shadow", commit_shadow)

    def request_review() -> None:
        targets = [ctx.workspace.target_for("src/example.py", LineSpan(1, 2), LineSpan(5, 6), LineSpan(9, 10))]
        ctx.review_id = ctx.agent_a.request_review(targets=targets, summary="check alpha/beta/gamma edits", requested_from="agent-review")
    add(0.88, "request review", request_review)

    def submit_review() -> None:
        assert ctx.review_id is not None
        ctx.reviewer.submit_review(review_id=ctx.review_id, verdict="approve", confidence=0.84, findings=["alpha, beta, and gamma edits are isolated and coherent"])
    add(0.92, "submit review", submit_review)

    add(0.94, "handoff", lambda: ctx.agent_a.handoff("ready for merge after approval", to_agent_id="agent-review", requested_review=True))

    def release_pending() -> None:
        assert ctx.planning_lease is not None
        ctx.agent_a.release_record(ctx.planning_lease.lease_id, reason="completed")
    add(0.96, "release planning lease", release_pending)

    def release_a() -> None:
        assert ctx.claim_a is not None
        ctx.agent_a.release_section(claim_id=ctx.claim_a, path="src/example.py", spans=[LineSpan(1, 2)])
    add(0.97, "release alpha claim", release_a)

    def release_b() -> None:
        assert ctx.claim_b is not None
        ctx.agent_b.release_section(claim_id=ctx.claim_b, path="src/example.py", spans=[LineSpan(5, 6)])
    add(0.98, "release beta claim", release_b)

    add(0.985, "task a review", lambda: ctx.coordinator.set_task_status(task_id="task-a", state="review", reason="changes landed"))
    add(0.990, "task a done", lambda: ctx.coordinator.set_task_status(task_id="task-a", state="done", reason="review approved", confidence=0.84))
    add(0.995, "task b done", lambda: ctx.coordinator.set_task_status(task_id="task-b", state="done", reason="coordinated write landed", confidence=0.8))
    add(1.000, "task c done", lambda: ctx.coordinator.set_task_status(task_id="task-c", state="done", reason="subagent shadow edit landed", confidence=0.72))
    return actions


def create_swarm_context(agent_count: int = 30) -> SwarmContext:
    board = Board()
    coordinator = Coordinator(board)
    groups = ["g1", "g2", "g3"]
    workspaces: dict[str, Workspace] = {}
    agents: dict[str, EditingAgent] = {}
    reviewers: dict[str, Agent] = {}
    shadow_ids: dict[str, str] = {}
    claims: dict[tuple[str, str], str] = {}
    review_ids: list[str] = []

    per_group = max(1, agent_count // len(groups))
    function_names = [
        "alpha", "beta", "gamma", "delta", "epsilon",
        "zeta", "eta", "theta", "iota", "kappa",
        "lambda", "mu",
    ]

    for gi, group in enumerate(groups, start=1):
        ws = Workspace(f"ws-{group}")
        paths = []
        lines: list[str] = []
        for fi, name in enumerate(function_names, start=1):
            lines.extend([f"def {group}_{name}():", f"    return {fi}", "", ""])
        path = f"src/{group}_module.py"
        ws.add_file(path, "\n".join(lines).rstrip() + "\n")
        workspaces[group] = ws
        paths.append(path)

        task_ids = [f"task-{group}-core", f"task-{group}-shadow", f"task-{group}-review"]
        for task_id in task_ids:
            coordinator.create_task(task_id=task_id, title=task_id, summary=f"swarm task for {group}")
        coordinator.relate_tasks(task_id=task_ids[0], other_task_id=task_ids[1], relation="related_to", note="shared hot file")
        coordinator.relate_tasks(task_id=task_ids[0], other_task_id=task_ids[2], relation="related_to", note="review traffic")
        coordinator.assign_workspace(group_id=f"group-{group}", workspace_id=ws.workspace_id, task_ids=task_ids, reason="maximum-chaos shared workspace")
        for task_id in task_ids:
            coordinator.set_task_status(task_id=task_id, state="in_progress", reason="swarm run active")

        for lane_idx in range(per_group):
            lane = f"{lane_idx:02d}"
            role = (
                "reviewer" if lane_idx in {8, 9} else
                "shadow" if lane_idx in {6, 7} else
                "writer"
            )
            task_id = task_ids[2] if role == "reviewer" else (task_ids[1] if role == "shadow" else task_ids[0])
            agent_id = f"agent-{group}.{lane}"
            primary_index = lane_idx % 10
            primary_start = 1 + (primary_index * 4)
            secondary_start = 1 + (((primary_index + 1) % 10) * 4)
            spec = AgentSpec(
                bead=group,
                lane=lane,
                group_id=f"group-{group}",
                task_id=task_id,
                agent_id=agent_id,
                role=role,
                focus_path=path,
                primary_span=LineSpan(primary_start, primary_start + 1),
                secondary_span=LineSpan(secondary_start, secondary_start + 1),
            )
            agent = EditingAgent(
                agent_id=spec.agent_id,
                board=board,
                default_group_id=spec.group_id,
                default_task_id=spec.task_id,
                default_workspace_id=ws.workspace_id,
                workspace=ws,
            )
            agents[agent_id] = agent
            if role == "reviewer":
                reviewers[agent_id] = Agent(
                    agent_id=agent_id,
                    board=board,
                    default_group_id=spec.group_id,
                    default_task_id=spec.task_id,
                    default_workspace_id=ws.workspace_id,
                )
            agent.comment(f"joined {group} swarm as {role}", task_id=spec.task_id)

    return SwarmContext(
        board=board,
        coordinator=coordinator,
        workspaces=workspaces,
        agents=agents,
        reviewers=reviewers,
        groups=groups,
        shadow_ids=shadow_ids,
        claims=claims,
        review_ids=review_ids,
    )


def swarm_actions(ctx: SwarmContext, *, seed: int = 7) -> list[TimedAction]:
    rng = random.Random(seed)
    actions: list[TimedAction] = []

    specs: list[AgentSpec] = []
    for agent_id, agent in ctx.agents.items():
        group = agent.default_group_id.removeprefix("group-") if agent.default_group_id else "g1"
        lane = agent_id.split(".", 1)[1]
        lane_idx = int(lane)
        role = "reviewer" if agent_id in ctx.reviewers else ("shadow" if lane_idx in {6, 7} else "writer")
        primary_index = lane_idx % 10
        primary_start = 1 + (primary_index * 4)
        secondary_start = 1 + (((primary_index + 1) % 10) * 4)
        specs.append(AgentSpec(
            bead=group,
            lane=lane,
            group_id=f"group-{group}",
            task_id=agent.default_task_id or f"task-{group}-core",
            agent_id=agent_id,
            role=role,
            focus_path=f"src/{group}_module.py",
            primary_span=LineSpan(primary_start, primary_start + 1),
            secondary_span=LineSpan(secondary_start, secondary_start + 1),
        ))

    def add(at: float, label: str, fn: Callable[[], None]) -> None:
        actions.append(TimedAction(at=max(0.0, min(1.0, at)), label=label, fn=fn))

    # Phase 1: activity burst
    for spec in specs:
        agent = ctx.agents[spec.agent_id]
        add(0.03 + rng.random() * 0.10, f"activity {spec.agent_id}", lambda a=agent, s=spec: a.set_activity("working", f"{s.role} on {s.focus_path}"))

    # Phase 2: lots of claims, including intentional collisions
    for spec in specs:
        agent = ctx.agents[spec.agent_id]
        target_span = spec.primary_span if spec.role != "reviewer" else spec.secondary_span or spec.primary_span

        def claim_fn(a=agent, s=spec, span=target_span):
            ok, claim_id, contenders = a.claim_section(path=s.focus_path, spans=[span], purpose=f"{s.role} editing {span.start}-{span.end}")
            if ok:
                ctx.claims[(s.agent_id, "primary")] = claim_id
            elif contenders and rng.random() < 0.35:
                ok2, claim_id2, _ = a.claim_section(path=s.focus_path, spans=[span], purpose=f"forced takeover {span.start}-{span.end}", force=True)
                if ok2:
                    ctx.claims[(s.agent_id, "primary")] = claim_id2
                    ctx.coordinator.reassign_claim(claim_id=claim_id2, from_agent_id=None, to_agent_id=s.agent_id, task_id=s.task_id, group_id=s.group_id, workspace_id=a.default_workspace_id, reason="chaos steal after contention")
        add(0.12 + rng.random() * 0.12, f"claim {spec.agent_id}", claim_fn)

    # Phase 3: writers try direct writes, many conflicting or stale
    for spec in specs:
        if spec.role == "reviewer":
            continue
        agent = ctx.agents[spec.agent_id]
        return_value = rng.randint(10, 999)
        expected_version = 0 if rng.random() < 0.45 else None
        use_primary = rng.random() < 0.7
        span = spec.primary_span if use_primary else (spec.secondary_span or spec.primary_span)

        def write_fn(a=agent, s=spec, value=return_value, ev=expected_version, chosen_span=span):
            claim_id = ctx.claims.get((s.agent_id, "primary"))
            if s.role == "shadow":
                a.comment(f"holding direct write; preferring shadow draft on {chosen_span.start}-{chosen_span.end}")
                return
            a.smart_write(
                path=s.focus_path,
                span=chosen_span,
                new_lines=[f"def {s.bead}_fn_{s.lane}():", f"    return {value}"],
                purpose=f"writer pass {s.lane}",
                claim_id=claim_id,
                expected_version=ev,
                atomic=True,
                lint_gate=rng.random() < 0.2,
                auto_claim=claim_id is None,
            )
        add(0.26 + rng.random() * 0.16, f"write {spec.agent_id}", write_fn)

    # Phase 4: shared shadow work per group
    for group in ctx.groups:
        shadowers = [s for s in specs if s.bead == group and s.role == "shadow"]
        if not shadowers:
            continue
        owner = ctx.agents[shadowers[0].agent_id]
        spans = [shadowers[0].primary_span]
        if len(shadowers) > 1:
            spans.append(shadowers[1].primary_span)

        def create_shadow(group_name=group, a=owner, target_spans=tuple(spans)):
            shadow_id = a.create_shadow(path=f"src/{group_name}_module.py", spans=list(target_spans), purpose=f"{group_name} collaborative shadow")
            ctx.shadow_ids[group_name] = shadow_id
        add(0.46 + rng.random() * 0.04, f"shadow create {group}", create_shadow)

        for idx, shadower in enumerate(shadowers):
            agent = ctx.agents[shadower.agent_id]
            val = 700 + idx + rng.randint(0, 90)
            span = shadower.primary_span

            def update_shadow(a=agent, s=shadower, value=val, chosen_span=span):
                shadow_id = ctx.shadow_ids.get(s.bead)
                if shadow_id is None:
                    return
                a.update_shadow(
                    shadow_id=shadow_id,
                    span=chosen_span,
                    new_lines=[f"def {s.bead}_shadow_{s.lane}():", f"    return {value}"],
                    summary=f"shadow draft by {s.agent_id}",
                )
            add(0.54 + rng.random() * 0.08, f"shadow update {shadower.agent_id}", update_shadow)

        def ready_shadow(group_name=group, a=owner):
            shadow_id = ctx.shadow_ids.get(group_name)
            if shadow_id is None:
                return
            a.mark_shadow_ready(shadow_id=shadow_id, summary=f"{group_name} shadow ready after chaos")
        add(0.66 + rng.random() * 0.05, f"shadow ready {group}", ready_shadow)

        def commit_shadow(group_name=group, a=owner):
            shadow_id = ctx.shadow_ids.get(group_name)
            if shadow_id is None:
                return
            ok = a.commit_shadow(shadow_id=shadow_id, summary=f"{group_name} atomic shadow landing", lint_gate=False)
            if not ok:
                a.comment(f"shadow commit drifted in {group_name}; needs rebase")
        add(0.73 + rng.random() * 0.06, f"shadow commit {group}", commit_shadow)

    # Phase 5: reviews
    for group in ctx.groups:
        reviewers = [aid for aid in ctx.reviewers if f"-{group}." in aid]
        requesters = [s for s in specs if s.bead == group and s.role == "writer"][:2]
        if not reviewers or not requesters:
            continue
        requester = ctx.agents[requesters[0].agent_id]
        reviewer = ctx.reviewers[reviewers[0]]

        def request_review(group_name=group, req=requester, rev_id=reviewer.agent_id):
            rid = req.request_review(
                targets=[ctx.workspaces[group_name].target_for(f"src/{group_name}_module.py", LineSpan(1, 10))],
                summary=f"review hot edits in {group_name}",
                requested_from=rev_id,
            )
            ctx.review_ids.append(rid)
        add(0.79 + rng.random() * 0.05, f"review request {group}", request_review)

        def submit_review(group_name=group, rev=reviewer):
            if not ctx.review_ids:
                return
            rid = ctx.review_ids.pop(0)
            verdict = "approve" if rng.random() < 0.7 else "needs_changes"
            rev.submit_review(
                review_id=rid,
                verdict=verdict,
                confidence=round(rng.uniform(0.51, 0.93), 2),
                findings=[f"{group_name} stream checked under contention"],
            )
        add(0.86 + rng.random() * 0.05, f"review submit {group}", submit_review)

    # Phase 6: handoffs, releases, done states
    for spec in specs:
        agent = ctx.agents[spec.agent_id]
        add(0.90 + rng.random() * 0.04, f"handoff {spec.agent_id}", lambda a=agent, s=spec: a.handoff(f"{s.role} lane {s.lane} ready for next arbitration", to_agent_id=None, requested_review=s.role != "reviewer"))

        def release_claim(a=agent, s=spec):
            claim_id = ctx.claims.get((s.agent_id, "primary"))
            if claim_id:
                span = s.primary_span
                a.release_section(claim_id=claim_id, path=s.focus_path, spans=[span], reason="completed")
        add(0.94 + rng.random() * 0.03, f"release {spec.agent_id}", release_claim)

    for group in ctx.groups:
        add(0.975 + rng.random() * 0.01, f"task done {group} core", lambda g=group: ctx.coordinator.set_task_status(task_id=f"task-{g}-core", state="done", reason="swarm writers settled", confidence=0.66))
        add(0.985 + rng.random() * 0.01, f"task done {group} shadow", lambda g=group: ctx.coordinator.set_task_status(task_id=f"task-{g}-shadow", state="done", reason="shadow landed or stabilized", confidence=0.74))
        add(0.995 + rng.random() * 0.004, f"task done {group} review", lambda g=group: ctx.coordinator.set_task_status(task_id=f"task-{g}-review", state="done", reason="review traffic drained", confidence=0.71))

    return sorted(actions, key=lambda a: a.at)


def run_actions(actions: list[TimedAction], duration_seconds: float | None, *, emit_raw: bool, board: Board) -> None:
    raw_index = 0
    start = time.monotonic()
    for action in actions:
        if duration_seconds is not None and duration_seconds > 0:
            target_elapsed = duration_seconds * action.at
            remaining = target_elapsed - (time.monotonic() - start)
            if remaining > 0:
                time.sleep(remaining)
        action.fn()
        if emit_raw:
            records = board.raw_records()
            new_records = records[raw_index:]
            for record in new_records:
                print(render_record(record), flush=True)
            raw_index = len(records)


def run_demo_realtime(duration_seconds: float | None = None, *, emit_raw: bool = False) -> SimulationResult:
    ctx = create_demo_context()
    run_actions(scenario_actions(ctx), duration_seconds, emit_raw=emit_raw, board=ctx.board)
    return SimulationResult(board=ctx.board, workspaces={ctx.workspace.workspace_id: ctx.workspace}, primary_workspace_id=ctx.workspace.workspace_id, scenario="canonical")


def run_swarm_realtime(duration_seconds: float | None = None, *, emit_raw: bool = False, agent_count: int = 30, seed: int = 7) -> SimulationResult:
    ctx = create_swarm_context(agent_count=agent_count)
    run_actions(swarm_actions(ctx, seed=seed), duration_seconds, emit_raw=emit_raw, board=ctx.board)
    primary = f"ws-{ctx.groups[0]}"
    return SimulationResult(board=ctx.board, workspaces=ctx.workspaces, primary_workspace_id=primary, scenario="swarm")


def build_demo() -> SimulationResult:
    return run_demo_realtime(duration_seconds=None, emit_raw=False)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--view", choices=["raw", "live", "both"], default="both")
    parser.add_argument("--time", type=parse_duration, default=None, help="run the simulation over a real duration like 60s or 2m")
    parser.add_argument("--scenario", choices=["canonical", "swarm"], default="swarm")
    parser.add_argument("--agents", type=int, default=30)
    parser.add_argument("--seed", type=int, default=7)
    args = parser.parse_args()

    if args.scenario == "swarm":
        result = run_swarm_realtime(duration_seconds=args.time, emit_raw=args.view in {"raw", "both"}, agent_count=args.agents, seed=args.seed)
    else:
        result = run_demo_realtime(duration_seconds=args.time, emit_raw=args.view in {"raw", "both"})

    if args.time is not None and args.view in {"raw", "both"}:
        if args.view == "both":
            print()
            print(render_live_board(result.board))
            print()
            print(render_live_retention(result.board))
            print("\n== final file ==")
            file_state = result.workspaces[result.primary_workspace_id].get_file(next(iter(result.workspaces[result.primary_workspace_id].files.keys())))
            print(file_state.text())
            print(f"\nversion={file_state.version}")
        return

    if args.scenario == "canonical":
        board = result.board
        write_applied = [r for r in board.live_records(include_retired=False) if r.kind == EventKind.WRITE_APPLIED]
        write_rejected = [r for r in board.live_records(include_retired=False) if r.kind == EventKind.WRITE_REJECTED]
        shadow_created = [r for r in board.raw_records() if r.kind == EventKind.SHADOW_CREATED]
        shadow_updated = [r for r in board.raw_records() if r.kind == EventKind.SHADOW_UPDATED]
        shadow_committed = [r for r in board.live_records(include_retired=False) if r.kind == EventKind.SHADOW_COMMITTED]
        review_results = [r for r in board.live_records(include_retired=False) if r.kind == EventKind.REVIEW_RESULT]
        assert len(write_applied) == 1
        assert len(write_rejected) == 1
        assert len(shadow_created) == 1
        assert len(shadow_updated) >= 2
        assert len(shadow_committed) == 1
        assert len(review_results) == 1

    if args.time is not None and args.view == "live":
        print("--time drives real-time execution; showing resulting live view immediately.\n", file=sys.stderr)

    print_summary(result, view=args.view)


if __name__ == "__main__":
    main()
