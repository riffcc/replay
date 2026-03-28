"""Core Board abstraction for replay-experiment.

The Board exposes two distinct surfaces:
- raw stream: immutable append-only chronology for human/audit views
- live board: coordinator-shaped mutable visibility/retention surface for agents

Retention is controlled by leases. Supersession and tombstoning only affect the
live board, never the raw stream.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable, Sequence

from .events import (
    BoardRecord,
    CoordRetiredPayload,
    EventKind,
    RecordLeasedPayload,
    RecordReleasedPayload,
    Transition,
    make_record,
    new_id,
    now_ms,
)


@dataclass(frozen=True)
class Lease:
    lease_id: str
    record_id: str
    agent_id: str
    scope: str
    reason: str
    created_at: int
    expires_at: int | None = None
    released_at: int | None = None
    release_reason: str | None = None

    @property
    def active(self) -> bool:
        return self.released_at is None and not self.expired

    @property
    def expired(self) -> bool:
        return self.expires_at is not None and now_ms() >= self.expires_at


class Board:
    """Shared Board with separate raw and live surfaces."""

    def __init__(self) -> None:
        self._raw_records: list[BoardRecord] = []
        self._records_by_id: dict[str, BoardRecord] = {}
        self._visible_ids: set[str] = set()
        self._retired_ids: set[str] = set()
        self._superseded_by: dict[str, str] = {}
        self._tombstoned_by: dict[str, str] = {}
        self._state_latest: dict[str, str] = {}
        self._leases_by_id: dict[str, Lease] = {}
        self._lease_ids_by_record: dict[str, set[str]] = {}

    def append(self, record: BoardRecord) -> BoardRecord:
        if record.id in self._records_by_id:
            raise ValueError(f"duplicate record id: {record.id}")

        self._raw_records.append(record)
        self._records_by_id[record.id] = record

        if record.transition.visible:
            self._visible_ids.add(record.id)

        self._apply_transition(record)
        self.retire_compactable_records()
        return record

    def get(self, record_id: str) -> BoardRecord | None:
        return self._records_by_id.get(record_id)

    def raw_records(self) -> list[BoardRecord]:
        """Immutable append-only chronology."""
        return list(self._raw_records)

    def records(self, *, include_retired: bool = False) -> list[BoardRecord]:
        """Backward-compatible alias for live-board records."""
        return self.live_records(include_retired=include_retired)

    def live_records(self, *, include_retired: bool = False) -> list[BoardRecord]:
        if include_retired:
            return list(self._raw_records)
        return [r for r in self._raw_records if r.id not in self._retired_ids]

    def visible_records(self) -> list[BoardRecord]:
        return [
            r for r in self._raw_records
            if r.id in self._visible_ids and r.id not in self._retired_ids
        ]

    def interleaved_view(
        self,
        *,
        stream_tags: Sequence[str] | None = None,
        topics: Sequence[str] | None = None,
        refs: Sequence[str] | None = None,
        include_hidden_leased: bool = False,
        agent_id: str | None = None,
        source: str = "live",
    ) -> list[BoardRecord]:
        tag_set = set(stream_tags or [])
        topic_set = set(topics or [])
        ref_set = set(refs or [])
        leased_ids = self._leased_record_ids(agent_id) if agent_id else set()

        out: list[BoardRecord] = []
        iterable = self._raw_records if source == "raw" else self._raw_records
        for record in iterable:
            if source != "raw" and record.id in self._retired_ids:
                continue

            if not self._matches_filters(record, tag_set, topic_set, ref_set):
                continue

            if source == "raw":
                out.append(record)
                continue

            visible = record.id in self._visible_ids
            retained_for_agent = include_hidden_leased and record.id in leased_ids
            if visible or retained_for_agent:
                out.append(record)
        return out

    def lease_record(
        self,
        *,
        record_id: str,
        agent_id: str,
        scope: str,
        reason: str,
        expires_at: int | None = None,
        lease_id: str | None = None,
    ) -> tuple[Lease, BoardRecord]:
        if record_id not in self._records_by_id:
            raise KeyError(f"unknown record id: {record_id}")

        lease = Lease(
            lease_id=lease_id or new_id("lease"),
            record_id=record_id,
            agent_id=agent_id,
            scope=scope,
            reason=reason,
            created_at=now_ms(),
            expires_at=expires_at,
        )
        self._leases_by_id[lease.lease_id] = lease
        self._lease_ids_by_record.setdefault(record_id, set()).add(lease.lease_id)

        source = self._records_by_id[record_id]
        event = self.append(
            make_record(
                kind=EventKind.RECORD_LEASED,
                agent_id=agent_id,
                task_id=source.task_id,
                group_id=source.group_id,
                workspace_id=source.workspace_id,
                routing=source.routing,
                payload=RecordLeasedPayload(
                    lease_id=lease.lease_id,
                    record_id=record_id,
                    scope=scope,  # type: ignore[arg-type]
                    reason=reason,
                    expires_at=expires_at,
                ),
                transition=Transition(
                    visible=False,
                    state_key=f"lease:{lease.lease_id}",
                ),
            )
        )
        return lease, event

    def release_lease(
        self,
        *,
        lease_id: str,
        agent_id: str,
        reason: str,
    ) -> tuple[Lease, BoardRecord]:
        lease = self._leases_by_id.get(lease_id)
        if lease is None:
            raise KeyError(f"unknown lease id: {lease_id}")
        if lease.agent_id != agent_id:
            raise ValueError(f"lease {lease_id} is owned by {lease.agent_id}, not {agent_id}")
        if lease.released_at is not None:
            raise ValueError(f"lease {lease_id} already released")

        released = Lease(
            lease_id=lease.lease_id,
            record_id=lease.record_id,
            agent_id=lease.agent_id,
            scope=lease.scope,
            reason=lease.reason,
            created_at=lease.created_at,
            expires_at=lease.expires_at,
            released_at=now_ms(),
            release_reason=reason,
        )
        self._leases_by_id[lease_id] = released

        source = self._records_by_id[lease.record_id]
        event = self.append(
            make_record(
                kind=EventKind.RECORD_RELEASED,
                agent_id=agent_id,
                task_id=source.task_id,
                group_id=source.group_id,
                workspace_id=source.workspace_id,
                routing=source.routing,
                payload=RecordReleasedPayload(
                    lease_id=lease_id,
                    record_id=lease.record_id,
                    reason=reason,  # type: ignore[arg-type]
                ),
                transition=Transition(
                    visible=False,
                    state_key=f"lease:{lease_id}",
                ),
            )
        )
        self.retire_compactable_records()
        return released, event

    def expire_leases(self) -> list[str]:
        expired: list[str] = []
        for lease_id, lease in list(self._leases_by_id.items()):
            if lease.active and lease.expired:
                self._leases_by_id[lease_id] = Lease(
                    lease_id=lease.lease_id,
                    record_id=lease.record_id,
                    agent_id=lease.agent_id,
                    scope=lease.scope,
                    reason=lease.reason,
                    created_at=lease.created_at,
                    expires_at=lease.expires_at,
                    released_at=now_ms(),
                    release_reason="expired",
                )
                expired.append(lease_id)
        if expired:
            self.retire_compactable_records()
        return expired

    def retained_records_for_agent(self, agent_id: str) -> list[BoardRecord]:
        leased = self._leased_record_ids(agent_id)
        return [
            self._records_by_id[rid]
            for rid in leased
            if rid in self._records_by_id and rid not in self._retired_ids
        ]

    def retire_compactable_records(self, coordinator_agent_id: str = "coordinator") -> list[BoardRecord]:
        retired_ids: list[str] = []
        for record in self._raw_records:
            if record.id in self._retired_ids:
                continue
            if record.id in self._visible_ids:
                continue
            if self._has_active_lease(record.id):
                continue
            if record.id in self._superseded_by or record.id in self._tombstoned_by:
                self._retired_ids.add(record.id)
                retired_ids.append(record.id)

        events: list[BoardRecord] = []
        if retired_ids:
            events.append(
                self.append(
                    make_record(
                        kind=EventKind.COORD_RETIRED,
                        agent_id=coordinator_agent_id,
                        payload=CoordRetiredPayload(
                            target_ids=retired_ids,
                            reason="superseded or tombstoned with no active leases",
                        ),
                        transition=Transition(visible=False),
                    )
                )
            )
        return events

    def active_lease_count(self, record_id: str) -> int:
        return sum(1 for lease in self._leases_for_record(record_id) if lease.active)

    def is_visible(self, record_id: str) -> bool:
        return record_id in self._visible_ids and record_id not in self._retired_ids

    def is_retired(self, record_id: str) -> bool:
        return record_id in self._retired_ids

    def _apply_transition(self, record: BoardRecord) -> None:
        transition = record.transition

        if transition.state_key:
            previous = self._state_latest.get(transition.state_key)
            if previous and previous != record.id:
                self._hide_record(previous, by_record_id=record.id, relation="superseded")
            self._state_latest[transition.state_key] = record.id

        for target_id in transition.supersedes:
            self._hide_record(target_id, by_record_id=record.id, relation="superseded")

        for target_id in transition.tombstones:
            self._hide_record(target_id, by_record_id=record.id, relation="tombstoned")

    def _hide_record(self, record_id: str, *, by_record_id: str, relation: str) -> None:
        if record_id not in self._records_by_id:
            return
        self._visible_ids.discard(record_id)
        if relation == "superseded":
            self._superseded_by[record_id] = by_record_id
        else:
            self._tombstoned_by[record_id] = by_record_id

    def _leases_for_record(self, record_id: str) -> Iterable[Lease]:
        for lease_id in self._lease_ids_by_record.get(record_id, set()):
            lease = self._leases_by_id.get(lease_id)
            if lease is not None:
                yield lease

    def _has_active_lease(self, record_id: str) -> bool:
        return any(lease.active for lease in self._leases_for_record(record_id))

    def _leased_record_ids(self, agent_id: str) -> set[str]:
        leased: set[str] = set()
        for lease in self._leases_by_id.values():
            if lease.agent_id == agent_id and lease.active:
                leased.add(lease.record_id)
        return leased

    def _matches_filters(
        self,
        record: BoardRecord,
        tag_set: set[str],
        topic_set: set[str],
        ref_set: set[str],
    ) -> bool:
        if not tag_set and not topic_set and not ref_set:
            return True

        if tag_set and tag_set.intersection(record.routing.stream_tags):
            return True
        if topic_set and topic_set.intersection(record.routing.topics):
            return True
        if ref_set and ref_set.intersection({ref.id for ref in record.routing.refs}):
            return True
        return False


__all__ = ["Board", "Lease"]
