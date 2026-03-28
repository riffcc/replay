"""Simulated workspace and file version model for replay-experiment.

This module stays intentionally simple:
- files are line-oriented
- versions increment on successful writes
- section claims are region-scoped leases over simulated files
- overlapping claims conflict, but distinct sections in the same file are allowed
- writes can be guarded by claim ownership and base-version checks
- shadow writes allow speculative multi-agent drafting before atomic commit
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Iterable

from .events import FileTarget, LineSpan, StructuralRegion


@dataclass(frozen=True)
class RegionLock:
    claim_id: str
    agent_id: str
    file_path: str
    spans: tuple[LineSpan, ...]
    purpose: str
    acquired_at: int
    expires_at: int | None = None
    released: bool = False

    def is_active(self, now_ms: int) -> bool:
        if self.released:
            return False
        return self.expires_at is None or now_ms < self.expires_at


@dataclass
class FileState:
    path: str
    lines: list[str]
    version: int = 0

    @classmethod
    def from_text(cls, path: str, text: str) -> "FileState":
        return cls(path=path, lines=text.splitlines())

    def text(self) -> str:
        return "\n".join(self.lines)

    def line_count(self) -> int:
        return len(self.lines)

    def read_span(self, span: LineSpan) -> list[str]:
        start = max(1, span.start)
        end = max(start, span.end)
        return self.lines[start - 1 : end]

    def replace_span(self, span: LineSpan, new_lines: list[str]) -> tuple[int, int]:
        """Replace an inclusive 1-based line span and bump file version.

        Returns `(old_version, new_version)`.
        """
        start = max(1, span.start)
        end = max(start, span.end)
        before = self.version
        self.lines[start - 1 : end] = new_lines
        self.version += 1
        return before, self.version


@dataclass
class ShadowWrite:
    shadow_id: str
    workspace_id: str
    file_path: str
    base_version: int
    spans: tuple[LineSpan, ...]
    purpose: str
    owner_agent_id: str
    contributors: set[str] = field(default_factory=set)
    draft_lines_by_span: dict[int, list[str]] = field(default_factory=dict)
    status: str = "draft"
    committed: bool = False
    abandoned: bool = False

    def add_contributor(self, agent_id: str) -> None:
        self.contributors.add(agent_id)

    def update_span(self, span: LineSpan, new_lines: list[str], *, contributor: str) -> None:
        self.add_contributor(contributor)
        self.draft_lines_by_span[span.start] = list(new_lines)

    def span_lines(self, span: LineSpan) -> list[str] | None:
        return self.draft_lines_by_span.get(span.start)

    def materialized_lines(self, file_state: FileState) -> list[str]:
        lines = list(file_state.lines)
        for span in sorted(self.spans, key=lambda s: s.start, reverse=True):
            replacement = self.draft_lines_by_span.get(span.start)
            if replacement is None:
                continue
            start = max(1, span.start)
            end = max(start, span.end)
            lines[start - 1 : end] = replacement
        return lines


@dataclass
class Workspace:
    workspace_id: str
    files: dict[str, FileState] = field(default_factory=dict)
    claims: dict[str, RegionLock] = field(default_factory=dict)
    shadows: dict[str, ShadowWrite] = field(default_factory=dict)

    def add_file(self, path: str, text: str) -> FileState:
        file_state = FileState.from_text(path, text)
        self.files[path] = file_state
        return file_state

    def get_file(self, path: str) -> FileState:
        try:
            return self.files[path]
        except KeyError as exc:
            raise KeyError(f"unknown file in workspace {self.workspace_id}: {path}") from exc

    def target_for(self, path: str, *spans: LineSpan) -> FileTarget:
        file_state = self.get_file(path)
        return FileTarget(
            workspace_id=self.workspace_id,
            path=path,
            version=file_state.version,
            regions=[StructuralRegion(kind="line_span", span=span) for span in spans],
        )

    def active_claims(self, *, path: str | None = None, now_ms: int | None = None) -> list[RegionLock]:
        current = now_ms if now_ms is not None else 0
        out: list[RegionLock] = []
        for claim in self.claims.values():
            active = claim.is_active(current) if now_ms is not None else not claim.released
            if not active:
                continue
            if path is None or claim.file_path == path:
                out.append(claim)
        return out

    def claim_section(
        self,
        *,
        claim_id: str,
        agent_id: str,
        path: str,
        spans: Iterable[LineSpan],
        purpose: str,
        acquired_at: int,
        expires_at: int | None = None,
        force: bool = False,
    ) -> tuple[bool, RegionLock, list[RegionLock]]:
        spans_tuple = tuple(spans)
        contenders = [
            claim
            for claim in self.claims.values()
            if not claim.released
            and claim.file_path == path
            and claim.agent_id != agent_id
            and regions_overlap(claim.spans, spans_tuple)
            and (claim.expires_at is None or acquired_at < claim.expires_at)
        ]
        if contenders and not force:
            attempted = RegionLock(
                claim_id=claim_id,
                agent_id=agent_id,
                file_path=path,
                spans=spans_tuple,
                purpose=purpose,
                acquired_at=acquired_at,
                expires_at=expires_at,
                released=False,
            )
            return False, attempted, contenders

        if force:
            for contender in contenders:
                self.release_claim(contender.claim_id)

        claim = RegionLock(
            claim_id=claim_id,
            agent_id=agent_id,
            file_path=path,
            spans=spans_tuple,
            purpose=purpose,
            acquired_at=acquired_at,
            expires_at=expires_at,
            released=False,
        )
        self.claims[claim_id] = claim
        return True, claim, contenders

    def release_claim(self, claim_id: str) -> RegionLock:
        claim = self.claims[claim_id]
        released = RegionLock(
            claim_id=claim.claim_id,
            agent_id=claim.agent_id,
            file_path=claim.file_path,
            spans=claim.spans,
            purpose=claim.purpose,
            acquired_at=claim.acquired_at,
            expires_at=claim.expires_at,
            released=True,
        )
        self.claims[claim_id] = released
        return released

    def create_shadow(
        self,
        *,
        shadow_id: str,
        owner_agent_id: str,
        path: str,
        spans: Iterable[LineSpan],
        purpose: str,
    ) -> ShadowWrite:
        file_state = self.get_file(path)
        shadow = ShadowWrite(
            shadow_id=shadow_id,
            workspace_id=self.workspace_id,
            file_path=path,
            base_version=file_state.version,
            spans=tuple(spans),
            purpose=purpose,
            owner_agent_id=owner_agent_id,
            contributors={owner_agent_id},
        )
        self.shadows[shadow_id] = shadow
        return shadow

    def get_shadow(self, shadow_id: str) -> ShadowWrite:
        try:
            return self.shadows[shadow_id]
        except KeyError as exc:
            raise KeyError(f"unknown shadow in workspace {self.workspace_id}: {shadow_id}") from exc

    def update_shadow(
        self,
        *,
        shadow_id: str,
        span: LineSpan,
        new_lines: list[str],
        contributor: str,
        status: str | None = None,
    ) -> ShadowWrite:
        shadow = self.get_shadow(shadow_id)
        shadow.update_span(span, new_lines, contributor=contributor)
        if status is not None:
            shadow.status = status
        return shadow

    def shadow_preview(self, shadow_id: str) -> list[str]:
        shadow = self.get_shadow(shadow_id)
        file_state = self.get_file(shadow.file_path)
        return shadow.materialized_lines(file_state)

    def commit_shadow(
        self,
        *,
        shadow_id: str,
        agent_id: str,
        lint_gate: bool = False,
    ) -> tuple[bool, str, int | None, int | None]:
        shadow = self.get_shadow(shadow_id)
        if shadow.abandoned:
            return False, "abandoned", None, None
        if shadow.committed:
            return False, "stale_target", None, None
        if agent_id not in shadow.contributors:
            return False, "policy_denied", None, None

        file_state = self.get_file(shadow.file_path)
        if file_state.version != shadow.base_version:
            return False, "stale_version", None, None

        for span in sorted(shadow.spans, key=lambda s: s.start, reverse=True):
            replacement = shadow.span_lines(span)
            if replacement is None:
                continue
            if lint_gate and any("FAIL" in line for line in replacement):
                return False, "lint_failed", None, None

        before = file_state.version
        for span in sorted(shadow.spans, key=lambda s: s.start, reverse=True):
            replacement = shadow.span_lines(span)
            if replacement is None:
                continue
            file_state.replace_span(span, replacement)
        after = file_state.version
        shadow.committed = True
        shadow.status = "committed"
        return True, "ok", before, after

    def abandon_shadow(self, shadow_id: str) -> ShadowWrite:
        shadow = self.get_shadow(shadow_id)
        shadow.abandoned = True
        shadow.status = "abandoned"
        return shadow

    def write_lines(
        self,
        *,
        agent_id: str,
        path: str,
        span: LineSpan,
        new_lines: list[str],
        claim_id: str | None = None,
        expected_version: int | None = None,
        now_ms: int | None = None,
    ) -> tuple[bool, str, int | None, int | None]:
        file_state = self.get_file(path)
        if expected_version is not None and file_state.version != expected_version:
            return False, "stale_version", None, None

        active_conflicts = [
            claim
            for claim in self.active_claims(path=path, now_ms=now_ms)
            if claim.agent_id != agent_id and regions_overlap(claim.spans, (span,))
        ]
        if active_conflicts:
            return False, "claim_conflict", None, None

        if claim_id is not None:
            claim = self.claims.get(claim_id)
            if claim is None or claim.released:
                return False, "stale_target", None, None
            if claim.agent_id != agent_id:
                return False, "policy_denied", None, None
            if claim.file_path != path or not regions_overlap(claim.spans, (span,)):
                return False, "policy_denied", None, None

        before, after = file_state.replace_span(span, new_lines)
        return True, "ok", before, after


def regions_overlap(left: Iterable[LineSpan], right: Iterable[LineSpan]) -> bool:
    for a in left:
        for b in right:
            if spans_overlap(a, b):
                return True
    return False



def spans_overlap(a: LineSpan, b: LineSpan) -> bool:
    return not (a.end < b.start or b.end < a.start)


__all__ = [
    "FileState",
    "RegionLock",
    "ShadowWrite",
    "Workspace",
    "regions_overlap",
    "spans_overlap",
]
