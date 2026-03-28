#!/usr/bin/env python3
"""
The Board — A Multi-Agent Coordination System

One JSONL stream. Multiple agents. The timeline log and live state
mutate together in the same table. The fusion IS the point.

The Coordinator acts like Left 4 Dead's Director:
- Prunes impossible/irrelevant transitions
- Manages section locks
- Detects dead agents
- Organizes task dependencies
- Creates worktrees for independent tasks
- Allows shared worktrees for dependent tasks

Agents broadcast file modifications to the stream. Other agents see
what's being modified in realtime and coordinate accordingly.

Section Locks replace file locks — you can lock individual sections
of a file, allowing multiple agents to edit the same file simultaneously.
"""

import json
import time
import uuid
import random
import threading
from dataclasses import dataclass, field, asdict
from enum import Enum
from typing import Optional
from collections import defaultdict


# ─────────────────────────────────────────────
# Core Types
# ─────────────────────────────────────────────

class EventType(str, Enum):
    # Bead/task lifecycle
    BEAD_CREATED = "bead.created"
    BEAD_CLAIMED = "bead.claimed"
    BEAD_PROGRESS = "bead.progress"
    BEAD_DONE = "bead.done"
    BEAD_STUCK = "bead.stuck"
    BEAD_REVIEW_REQUEST = "bead.review_request"
    BEAD_REVIEW_RESULT = "bead.review_result"

    # File operations
    FILE_EDIT_START = "file.edit.start"
    FILE_EDIT_COMMIT = "file.edit.commit"
    FILE_EDIT_ABORT = "file.edit.abort"

    # Section locks
    SECTION_LOCK_ACQUIRE = "section.lock.acquire"
    SECTION_LOCK_RELEASE = "section.lock.release"
    SECTION_LOCK_NUDGE = "section.lock.nudge"
    SECTION_LOCK_FORCE = "section.lock.force"

    # Agent lifecycle
    AGENT_SPAWN = "agent.spawn"
    AGENT_HEARTBEAT = "agent.heartbeat"
    AGENT_DEAD = "agent.dead"
    AGENT_MESSAGE = "agent.message"

    # Coordinator
    COORD_PRUNE = "coord.prune"
    COORD_DEPENDENCY = "coord.dependency"
    COORD_WORKTREE = "coord.worktree"

    # Cross-agent communication
    WORK_REQUEST = "work.request"
    WORK_HANDOFF = "work.handoff"
    CONFIDENCE_REPORT = "confidence.report"


class BeadStatus(str, Enum):
    PENDING = "pending"
    IN_PROGRESS = "in_progress"
    STUCK = "stuck"
    IN_REVIEW = "in_review"
    DONE = "done"
    CANCELLED = "cancelled"


@dataclass
class LineRange:
    start: int
    end: int

    def overlaps(self, other: 'LineRange') -> bool:
        return self.start <= other.end and other.start <= self.end

    def __repr__(self):
        return f"L{self.start}-{self.end}"


@dataclass
class SectionLock:
    file_path: str
    lines: LineRange
    owner_agent: str
    purpose: str
    acquired_at: float
    lock_id: str = field(default_factory=lambda: str(uuid.uuid4())[:8])


@dataclass
class BoardEvent:
    """A single event in The Board's unified JSONL stream."""
    event_id: str
    timestamp: float
    event_type: EventType
    agent_id: str
    bead_id: Optional[str] = None
    stream: str = "main"  # main, or a topical stream name

    # Payload fields (sparse — only relevant ones populated)
    file_path: Optional[str] = None
    lines: Optional[list] = None  # List of {start, end} dicts
    purpose: Optional[str] = None
    status: Optional[str] = None
    message: Optional[str] = None
    confidence: Optional[float] = None
    data: Optional[dict] = None

    def to_jsonl(self) -> str:
        d = {k: v for k, v in asdict(self).items() if v is not None}
        return json.dumps(d, default=str)

    @classmethod
    def from_jsonl(cls, line: str) -> 'BoardEvent':
        d = json.loads(line)
        d['event_type'] = EventType(d['event_type'])
        return cls(**d)


# ─────────────────────────────────────────────
# The Board — The One Stream
# ─────────────────────────────────────────────

class TheBoard:
    """
    ONE stream. All agents contribute to it and read from it in realtime.
    The timeline log and live state mutate together in the same table.
    The fusion IS the point.
    """

    def __init__(self):
        self._events: list[BoardEvent] = []
        self._lock = threading.Lock()
        self._subscribers: list[callable] = []
        self._seq = 0

        # Live state views (derived from the stream, not separate)
        self._active_locks: dict[str, SectionLock] = {}  # lock_id -> SectionLock
        self._agent_heartbeats: dict[str, float] = {}  # agent_id -> last_heartbeat
        self._bead_states: dict[str, BeadStatus] = {}  # bead_id -> current status

    def emit(self, event: BoardEvent) -> BoardEvent:
        """Emit an event into the ONE stream."""
        with self._lock:
            self._seq += 1
            event.event_id = f"evt-{self._seq:06d}"
            event.timestamp = time.time()
            self._events.append(event)

            # Update live state from the event (fusion!)
            self._update_live_state(event)

            # Notify subscribers
            for sub in self._subscribers:
                try:
                    sub(event)
                except Exception:
                    pass

        return event

    def _update_live_state(self, event: BoardEvent):
        """Live state mutates from the stream. Same table. Fused."""
        if event.event_type == EventType.AGENT_HEARTBEAT:
            self._agent_heartbeats[event.agent_id] = event.timestamp

        elif event.event_type == EventType.AGENT_SPAWN:
            self._agent_heartbeats[event.agent_id] = event.timestamp

        elif event.event_type in (EventType.BEAD_CREATED, EventType.BEAD_CLAIMED,
                                   EventType.BEAD_PROGRESS, EventType.BEAD_DONE,
                                   EventType.BEAD_STUCK):
            if event.bead_id and event.status:
                self._bead_states[event.bead_id] = BeadStatus(event.status)

        elif event.event_type == EventType.SECTION_LOCK_ACQUIRE:
            if event.data and 'lock_id' in event.data:
                lock = SectionLock(
                    file_path=event.file_path,
                    lines=LineRange(**event.data['lines']),
                    owner_agent=event.agent_id,
                    purpose=event.purpose or "",
                    acquired_at=event.timestamp,
                    lock_id=event.data['lock_id']
                )
                self._active_locks[lock.lock_id] = lock

        elif event.event_type == EventType.SECTION_LOCK_RELEASE:
            if event.data and 'lock_id' in event.data:
                self._active_locks.pop(event.data['lock_id'], None)

        elif event.event_type == EventType.SECTION_LOCK_FORCE:
            if event.data and 'lock_id' in event.data:
                # Force-take: remove old, add new
                old_lock = self._active_locks.pop(event.data['lock_id'], None)
                if old_lock:
                    new_lock_id = str(uuid.uuid4())[:8]
                    self._active_locks[new_lock_id] = SectionLock(
                        file_path=old_lock.file_path,
                        lines=old_lock.lines,
                        owner_agent=event.agent_id,
                        purpose=event.purpose or old_lock.purpose,
                        acquired_at=event.timestamp,
                        lock_id=new_lock_id
                    )

    def subscribe(self, callback: callable):
        """Subscribe to realtime events."""
        self._subscribers.append(callback)

    def read_stream(self, stream: str = None, since: float = 0,
                    limit: int = None) -> list[BoardEvent]:
        """Read events, optionally filtered by stream and time."""
        with self._lock:
            events = self._events
            if since > 0:
                events = [e for e in events if e.timestamp > since]
            if stream:
                events = [e for e in events if e.stream in (stream, "main")]
            if limit:
                events = events[-limit:]
            return list(events)

    def read_interleaved(self, streams: list[str], since: float = 0,
                         limit: int = None) -> list[BoardEvent]:
        """
        Read multiple streams interleaved chronologically.
        Each event gets a tiny marker so the reader knows which stream
        it came from. The interleaving IS the point — the model
        intuitively sees how the streams relate.
        """
        with self._lock:
            events = [e for e in self._events
                      if e.stream in streams or e.stream == "main"]
            if since > 0:
                events = [e for e in events if e.timestamp > since]
            events.sort(key=lambda e: e.timestamp)
            if limit:
                events = events[-limit:]
            return events

    def get_locks_for_file(self, file_path: str) -> list[SectionLock]:
        """Get all active section locks for a file."""
        with self._lock:
            return [l for l in self._active_locks.values()
                    if l.file_path == file_path]

    def check_section_conflict(self, file_path: str,
                                lines: LineRange,
                                requesting_agent: str) -> Optional[SectionLock]:
        """Check if a section conflicts with existing locks."""
        for lock in self.get_locks_for_file(file_path):
            if lock.owner_agent != requesting_agent and lock.lines.overlaps(lines):
                return lock
        return None

    def is_agent_alive(self, agent_id: str, timeout: float = 5.0) -> bool:
        """Check if an agent is alive based on heartbeat."""
        last = self._agent_heartbeats.get(agent_id, 0)
        return (time.time() - last) < timeout

    def get_bead_status(self, bead_id: str) -> Optional[BeadStatus]:
        """Get current bead status from live state."""
        return self._bead_states.get(bead_id)


# ─────────────────────────────────────────────
# The Coordinator (The Director)
# ─────────────────────────────────────────────

class Coordinator:
    """
    The Director from Left 4 Dead. Doesn't micromanage — observes
    the state of play and makes structural decisions.

    - Prunes impossible transitions
    - Cleans up completed work
    - Detects dead agents
    - Manages dependencies
    - Creates worktrees
    """

    def __init__(self, board: TheBoard):
        self.board = board
        self.board.subscribe(self._on_event)
        self._bead_history: dict[str, list[BoardEvent]] = defaultdict(list)

    def _on_event(self, event: BoardEvent):
        """React to events in realtime."""
        if event.bead_id:
            self._bead_history[event.bead_id].append(event)

        # Prune: when bead moves to IN_PROGRESS, drop PENDING events
        if (event.event_type == EventType.BEAD_CLAIMED and
                event.status == BeadStatus.IN_PROGRESS.value):
            self._prune_obsolete(event.bead_id, BeadStatus.PENDING)

        # Prune: when bead is DONE, clean up all prior events
        if (event.event_type == EventType.BEAD_DONE and
                event.status == BeadStatus.DONE.value):
            self._prune_obsolete(event.bead_id, None)  # prune all prior

        # Detect dead agents and release their locks
        self._check_dead_agents()

    def _prune_obsolete(self, bead_id: str, target_status: Optional[BeadStatus]):
        """Prune events relating to an obsolete state."""
        pruned_count = 0
        with self.board._lock:
            if target_status is None:
                # Prune ALL prior events for this bead (it's done)
                before = len(self.board._events)
                # Keep the DONE event, prune everything else for this bead
                self.board._events = [
                    e for e in self.board._events
                    if e.bead_id != bead_id or e.event_type == EventType.BEAD_DONE
                ]
                pruned_count = before - len(self.board._events)
            else:
                # Prune events for this bead with the target status
                before = len(self.board._events)
                self.board._events = [
                    e for e in self.board._events
                    if not (e.bead_id == bead_id and
                            e.status == target_status.value)
                ]
                pruned_count = before - len(self.board._events)

        if pruned_count > 0:
            self.board.emit(BoardEvent(
                event_id="",
                timestamp=0,
                event_type=EventType.COORD_PRUNE,
                agent_id="coordinator",
                bead_id=bead_id,
                message=f"Pruned {pruned_count} obsolete events"
                        f" (was: {target_status.value if target_status else 'all prior'})",
            ))

    def _check_dead_agents(self):
        """Detect dead agents and release their locks."""
        dead_agents = []
        for agent_id, last_hb in self.board._agent_heartbeats.items():
            if not self.board.is_agent_alive(agent_id, timeout=5.0):
                dead_agents.append(agent_id)

        for agent_id in dead_agents:
            # Release all locks held by dead agent
            locks_to_release = [
                l for l in self.board._active_locks.values()
                if l.owner_agent == agent_id
            ]
            for lock in locks_to_release:
                self.board.emit(BoardEvent(
                    event_id="",
                    timestamp=0,
                    event_type=EventType.SECTION_LOCK_RELEASE,
                    agent_id="coordinator",
                    file_path=lock.file_path,
                    data={'lock_id': lock.lock_id},
                    message=f"Released lock from dead agent {agent_id}",
                ))


# ─────────────────────────────────────────────
# Task Agent
# ─────────────────────────────────────────────

class TaskAgent:
    """
    A simulated task agent that reads from and writes to The Board.
    Coordinates with other agents via the ONE stream.
    """

    def __init__(self, agent_id: str, board: TheBoard,
                 model: str = "gpt-5.4", cost_tier: str = "high"):
        self.agent_id = agent_id
        self.board = board
        self.model = model
        self.cost_tier = cost_tier
        self._alive = True

        # Announce ourselves
        self.board.emit(BoardEvent(
            event_id="",
            timestamp=0,
            event_type=EventType.AGENT_SPAWN,
            agent_id=self.agent_id,
            message=f"Agent spawned with model={model}",
            data={"model": model, "cost_tier": cost_tier}
        ))

    def heartbeat(self):
        """Send heartbeat to prove we're alive."""
        if self._alive:
            self.board.emit(BoardEvent(
                event_id="",
                timestamp=0,
                event_type=EventType.AGENT_HEARTBEAT,
                agent_id=self.agent_id,
            ))

    def claim_bead(self, bead_id: str):
        """Claim a bead/task and start working on it."""
        self.board.emit(BoardEvent(
            event_id="",
            timestamp=0,
            event_type=EventType.BEAD_CLAIMED,
            agent_id=self.agent_id,
            bead_id=bead_id,
            status=BeadStatus.IN_PROGRESS.value,
            message=f"Claimed by {self.agent_id}",
        ))

    def start_edit(self, bead_id: str, file_path: str,
                   line_ranges: list[LineRange], purpose: str) -> Optional[str]:
        """
        Start editing a file. Checks for conflicts first.
        Returns lock_id if successful, None if conflicted.
        """
        # Check for conflicts on ALL ranges
        for lr in line_ranges:
            conflict = self.board.check_section_conflict(
                file_path, lr, self.agent_id
            )
            if conflict:
                # Broadcast that we wanted to edit but there's a conflict
                self.board.emit(BoardEvent(
                    event_id="",
                    timestamp=0,
                    event_type=EventType.AGENT_MESSAGE,
                    agent_id=self.agent_id,
                    bead_id=bead_id,
                    file_path=file_path,
                    message=f"Conflict with {conflict.owner_agent} on "
                            f"{conflict.lines} — waiting or rerouting",
                ))
                return None

        # Acquire section locks for all ranges
        lock_id = str(uuid.uuid4())[:8]
        for lr in line_ranges:
            self.board.emit(BoardEvent(
                event_id="",
                timestamp=0,
                event_type=EventType.SECTION_LOCK_ACQUIRE,
                agent_id=self.agent_id,
                bead_id=bead_id,
                file_path=file_path,
                purpose=purpose,
                data={
                    'lock_id': f"{lock_id}-{lr.start}",
                    'lines': {'start': lr.start, 'end': lr.end},
                },
            ))

        # Broadcast edit start
        self.board.emit(BoardEvent(
            event_id="",
            timestamp=0,
            event_type=EventType.FILE_EDIT_START,
            agent_id=self.agent_id,
            bead_id=bead_id,
            file_path=file_path,
            lines=[{'start': lr.start, 'end': lr.end} for lr in line_ranges],
            purpose=purpose,
        ))

        return lock_id

    def commit_edit(self, bead_id: str, file_path: str, lock_id: str,
                    line_ranges: list[LineRange], lint_passed: bool = True):
        """Commit an edit. Releases section locks."""
        if not lint_passed:
            # Atomic lint-before-commit: reject the write
            self.board.emit(BoardEvent(
                event_id="",
                timestamp=0,
                event_type=EventType.FILE_EDIT_ABORT,
                agent_id=self.agent_id,
                bead_id=bead_id,
                file_path=file_path,
                message="Lint failed — write rejected (atomic lint-before-commit)",
            ))
        else:
            # Commit succeeded
            self.board.emit(BoardEvent(
                event_id="",
                timestamp=0,
                event_type=EventType.FILE_EDIT_COMMIT,
                agent_id=self.agent_id,
                bead_id=bead_id,
                file_path=file_path,
                lines=[{'start': lr.start, 'end': lr.end} for lr in line_ranges],
                purpose="Edit committed",
            ))

        # Release all locks for this edit
        for lr in line_ranges:
            lid = f"{lock_id}-{lr.start}"
            self.board.emit(BoardEvent(
                event_id="",
                timestamp=0,
                event_type=EventType.SECTION_LOCK_RELEASE,
                agent_id=self.agent_id,
                bead_id=bead_id,
                file_path=file_path,
                data={'lock_id': lid},
            ))

    def request_review(self, bead_id: str, target_agent: str):
        """Ask another agent to review our work."""
        self.board.emit(BoardEvent(
            event_id="",
            timestamp=0,
            event_type=EventType.BEAD_REVIEW_REQUEST,
            agent_id=self.agent_id,
            bead_id=bead_id,
            message=f"Requesting review from {target_agent}",
            data={"reviewer": target_agent},
        ))

    def submit_review(self, bead_id: str, confidence: float, notes: str):
        """Submit a review of another agent's work."""
        self.board.emit(BoardEvent(
            event_id="",
            timestamp=0,
            event_type=EventType.BEAD_REVIEW_RESULT,
            agent_id=self.agent_id,
            bead_id=bead_id,
            confidence=confidence,
            message=notes,
        ))

    def mark_done(self, bead_id: str):
        """Mark a bead as complete."""
        self.board.emit(BoardEvent(
            event_id="",
            timestamp=0,
            event_type=EventType.BEAD_DONE,
            agent_id=self.agent_id,
            bead_id=bead_id,
            status=BeadStatus.DONE.value,
        ))

    def send_message(self, target_agent: str, message: str,
                     bead_id: Optional[str] = None):
        """Send a message to another agent via the stream."""
        self.board.emit(BoardEvent(
            event_id="",
            timestamp=0,
            event_type=EventType.AGENT_MESSAGE,
            agent_id=self.agent_id,
            bead_id=bead_id,
            message=f"@{target_agent}: {message}",
            data={"to": target_agent},
        ))

    def report_confidence(self, bead_id: str, confidence: float, notes: str):
        """Report confidence level on current work."""
        self.board.emit(BoardEvent(
            event_id="",
            timestamp=0,
            event_type=EventType.CONFIDENCE_REPORT,
            agent_id=self.agent_id,
            bead_id=bead_id,
            confidence=confidence,
            message=notes,
        ))

    def die(self):
        """Simulate agent death."""
        self._alive = False
        self.board.emit(BoardEvent(
            event_id="",
            timestamp=0,
            event_type=EventType.AGENT_DEAD,
            agent_id=self.agent_id,
            message="Agent died",
        ))


# ─────────────────────────────────────────────
# Simulation
# ─────────────────────────────────────────────

def print_event(event: BoardEvent):
    """Pretty-print a board event."""
    stream_marker = f"[{event.stream}]" if event.stream != "main" else "[●]"
    agent = event.agent_id[:12].ljust(12)
    etype = event.event_type.value.ljust(24)
    bead = f"bead:{event.bead_id}" if event.bead_id else ""
    fpath = f"file:{event.file_path}" if event.file_path else ""
    msg = event.message or ""
    conf = f"confidence:{event.confidence:.0%}" if event.confidence else ""
    lines_str = ""
    if event.lines:
        lines_str = f"lines:{event.lines}"

    parts = [p for p in [stream_marker, agent, etype, bead, fpath,
                          lines_str, conf, msg] if p]
    print(f"  {'  '.join(parts)}")


def run_simulation():
    """
    Simulate a multi-agent coding session with The Board.

    Scenario: Three agents working on related tasks in the same worktree.
    - Agent Alpha: refactoring auth module (expensive model)
    - Agent Beta: adding tests for auth (cheap model)
    - Agent Gamma: updating API docs (cheap model)

    Alpha and Beta need to edit the same file (auth.rs).
    Gamma works on a different file but needs to review Alpha's work.
    """

    print("=" * 70)
    print("  THE BOARD — Multi-Agent Coordination Simulation")
    print("  One stream. Multiple agents. The fusion IS the point.")
    print("=" * 70)
    print()

    # Create The Board
    board = TheBoard()

    # Subscribe to all events for visibility
    board.subscribe(print_event)

    # Start the Coordinator (The Director)
    coordinator = Coordinator(board)

    print("─── Phase 1: Spawn agents ───")
    print()

    alpha = TaskAgent("alpha-gpt54", board, model="gpt-5.4", cost_tier="high")
    beta = TaskAgent("beta-minimax", board, model="minimax-m2.7", cost_tier="low")
    gamma = TaskAgent("gamma-glm51", board, model="glm-5.1", cost_tier="medium")

    # Heartbeats
    for agent in [alpha, beta, gamma]:
        agent.heartbeat()

    print()
    print("─── Phase 2: Create beads (tasks) ───")
    print()

    # Create tasks
    bead_refactor = "bead-refactor-auth"
    bead_tests = "bead-auth-tests"
    bead_docs = "bead-api-docs"

    board.emit(BoardEvent(
        event_id="", timestamp=0,
        event_type=EventType.BEAD_CREATED,
        agent_id="coordinator",
        bead_id=bead_refactor,
        status=BeadStatus.PENDING.value,
        message="Refactor auth module — extract session handling",
    ))

    board.emit(BoardEvent(
        event_id="", timestamp=0,
        event_type=EventType.BEAD_CREATED,
        agent_id="coordinator",
        bead_id=bead_tests,
        status=BeadStatus.PENDING.value,
        message="Add unit tests for auth module",
    ))

    board.emit(BoardEvent(
        event_id="", timestamp=0,
        event_type=EventType.BEAD_CREATED,
        agent_id="coordinator",
        bead_id=bead_docs,
        status=BeadStatus.PENDING.value,
        message="Update API docs for auth endpoints",
    ))

    # Coordinator detects dependency
    board.emit(BoardEvent(
        event_id="", timestamp=0,
        event_type=EventType.COORD_DEPENDENCY,
        agent_id="coordinator",
        bead_id=bead_tests,
        message=f"bead-auth-tests depends on bead-refactor-auth (same file)",
        data={"depends_on": bead_refactor, "type": "same_worktree"},
    ))

    print()
    print("─── Phase 3: Agents claim tasks ───")
    print()

    alpha.claim_bead(bead_refactor)
    beta.claim_bead(bead_tests)
    gamma.claim_bead(bead_docs)

    print()
    print("─── Phase 4: Concurrent editing with section locks ───")
    print()

    # Alpha starts editing auth.rs lines 45-80 (session handling)
    lock_alpha = alpha.start_edit(
        bead_refactor, "src/auth.rs",
        [LineRange(45, 80)],
        "Extract session handling into SessionManager struct"
    )
    print(f"  >>> Alpha got lock: {lock_alpha}")

    # Beta tries to edit auth.rs lines 60-75 (OVERLAP!)
    lock_beta_overlap = beta.start_edit(
        bead_tests, "src/auth.rs",
        [LineRange(60, 75)],
        "Add test hooks to session functions"
    )
    print(f"  >>> Beta overlap attempt: {lock_beta_overlap}")

    # Beta edits a DIFFERENT section of auth.rs (lines 120-150) — no conflict
    lock_beta = beta.start_edit(
        bead_tests, "src/auth.rs",
        [LineRange(120, 150)],
        "Add test module at bottom of file"
    )
    print(f"  >>> Beta non-overlapping lock: {lock_beta}")

    # Gamma edits a completely different file — no conflict possible
    lock_gamma = gamma.start_edit(
        bead_docs, "docs/api.md",
        [LineRange(1, 50)],
        "Rewrite auth endpoint documentation"
    )
    print(f"  >>> Gamma got lock: {lock_gamma}")

    print()
    print("─── Phase 5: Edits land, lint-before-commit ───")
    print()

    # Alpha commits successfully (lint passes)
    alpha.commit_edit(bead_refactor, "src/auth.rs", lock_alpha,
                      [LineRange(45, 80)], lint_passed=True)

    # Beta commits test module (lint passes)
    beta.commit_edit(bead_tests, "src/auth.rs", lock_beta,
                     [LineRange(120, 150)], lint_passed=True)

    # Beta NOW tries the previously-conflicted section (Alpha released)
    lock_beta_retry = beta.start_edit(
        bead_tests, "src/auth.rs",
        [LineRange(60, 75)],
        "Add test hooks (retry — Alpha finished)"
    )
    print(f"  >>> Beta retry after Alpha released: {lock_beta_retry}")

    # Beta's edit fails lint!
    beta.commit_edit(bead_tests, "src/auth.rs", lock_beta_retry,
                     [LineRange(60, 75)], lint_passed=False)

    # Gamma commits docs
    gamma.commit_edit(bead_docs, "docs/api.md", lock_gamma,
                      [LineRange(1, 50)], lint_passed=True)

    print()
    print("─── Phase 6: Cross-review (swap papers!) ───")
    print()

    # Alpha asks Gamma to review the refactor
    alpha.request_review(bead_refactor, "gamma-glm51")
    alpha.report_confidence(bead_refactor, 0.85,
                            "Confident in the extraction but unsure about error paths")

    # Gamma reviews Alpha's work
    gamma.submit_review(bead_refactor, confidence=0.92,
                        notes="Clean extraction. Error paths look correct. "
                              "Suggest adding logging to SessionManager::new()")

    # Beta asks Alpha to review the tests
    beta.request_review(bead_tests, "alpha-gpt54")

    # Alpha reviews Beta's work (more expensive model reviewing cheaper model's output)
    alpha.submit_review(bead_tests, confidence=0.78,
                        notes="Tests cover happy path but miss the session-expired "
                              "edge case. Add test_session_expiry().")

    print()
    print("─── Phase 7: Tasks complete, coordinator prunes ───")
    print()

    alpha.mark_done(bead_refactor)
    gamma.mark_done(bead_docs)

    # Beta is still working (needs to fix lint failure and add the test Alpha asked for)
    beta.report_confidence(bead_tests, 0.65,
                           "Need to fix lint failure and add test_session_expiry()")

    print()
    print("─── Phase 8: Agent death and lock recovery ───")
    print()

    # Beta dies while holding no locks (already released)
    # But let's simulate it acquiring a lock first
    lock_beta_final = beta.start_edit(
        bead_tests, "src/auth.rs",
        [LineRange(60, 75)],
        "Final attempt at test hooks"
    )
    print(f"  >>> Beta got lock: {lock_beta_final}")

    # Beta dies!
    beta.die()

    # Coordinator detects death and releases locks
    # (In real system this would be on a heartbeat timeout)
    # Force the check by setting heartbeat age
    board._agent_heartbeats["beta-minimax"] = 0  # ancient heartbeat
    coordinator._check_dead_agents()

    print()
    print("─── Phase 9: Stream state ───")
    print()

    # Show final stream state
    events = board.read_stream()
    print(f"  Total events in stream: {len(events)}")
    print(f"  Active locks: {len(board._active_locks)}")
    print(f"  Bead states:")
    for bead_id, status in board._bead_states.items():
        print(f"    {bead_id}: {status.value}")
    print(f"  Agent liveness:")
    for agent_id in ["alpha-gpt54", "beta-minimax", "gamma-glm51"]:
        alive = board.is_agent_alive(agent_id, timeout=5.0)
        print(f"    {agent_id}: {'alive' if alive else 'DEAD'}")

    print()
    print("─── Phase 10: Interleaved stream view ───")
    print()

    # Show what an agent would see when reading its topical stream
    # interleaved with main
    print("  An agent reading [main + bead-refactor-auth] would see:")
    print("  (chronologically sorted, with stream markers)")
    print()

    refactor_events = [e for e in board.read_stream()
                       if e.bead_id == bead_refactor or
                       e.event_type in (EventType.AGENT_SPAWN,
                                        EventType.COORD_DEPENDENCY)]
    for evt in refactor_events[:15]:
        print_event(evt)

    print()
    print("=" * 70)
    print("  Simulation complete.")
    print()
    print("  Key properties demonstrated:")
    print("  • ONE stream — all agents read and write to the same table")
    print("  • Section locks — two agents edit the same file, different sections")
    print("  • Conflict detection — overlapping edits caught before they happen")
    print("  • Lint-before-commit — atomic write validation")
    print("  • Cross-review — agents swap papers and report confidence")
    print("  • Dead agent recovery — coordinator releases orphaned locks")
    print("  • Coordinator pruning — obsolete events removed on state transitions")
    print("  • Timeline + live state = same table (the fusion IS the point)")
    print("=" * 70)


if __name__ == "__main__":
    run_simulation()
