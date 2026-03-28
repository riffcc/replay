//! Session coordinator — shared runtime state for Replay.
//!
//! Tracks active tasks, lane plans, worktree/group planning, and the
//! shared coordination stream. Provider pressure is tracked separately
//! in provider_pressure.rs.
//!
//! The coordination stream is a single append-only sequence of records
//! that all agents contribute to and read from. It serves as both the
//! chronological timeline and the live coordination state — the same
//! records carry "what happened" and "what's happening now". Records
//! become hidden via supersession and eventually retire when no active
//! leases depend on them.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::beads::Issue;

// ── Task / lane planning (existing) ──

/// A task lane in the dry-run / parallel planner.
#[derive(Debug, Clone)]
pub struct TaskLane {
    pub id: String,
    pub issue_id: String,
    pub title: String,
    pub assigned_model: Option<String>,
    pub active: bool,
}

// ── Coordination stream types ──

pub type RecordId = String;
pub type Timestamp = u64; // ms since epoch

/// What kind of event this record represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecordKind {
    // Task lifecycle
    #[serde(rename = "task.status")]
    TaskStatus,
    #[serde(rename = "task.relation")]
    TaskRelation,

    // Agent activity
    #[serde(rename = "agent.activity")]
    AgentActivity,

    // Edits and writes
    #[serde(rename = "edit.intent")]
    EditIntent,
    #[serde(rename = "section.claim")]
    SectionClaim,
    #[serde(rename = "section.release")]
    SectionRelease,
    #[serde(rename = "write.applied")]
    WriteApplied,
    #[serde(rename = "write.rejected")]
    WriteRejected,

    // Review
    #[serde(rename = "review.requested")]
    ReviewRequested,
    #[serde(rename = "review.result")]
    ReviewResult,
    #[serde(rename = "confidence")]
    Confidence,

    // Coordination
    #[serde(rename = "coord.compact")]
    Compact,
    #[serde(rename = "coord.group")]
    Group,
    #[serde(rename = "coord.reassign")]
    Reassign,
}

/// A record in the coordination stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: RecordId,
    pub ts: Timestamp,
    pub kind: RecordKind,
    pub agent_id: String,
    pub task_id: Option<String>,
    /// Stream tags for topical filtering (e.g. "global", "task:abc", "workspace:ws1").
    #[serde(default)]
    pub tags: Vec<String>,
    /// For compaction: groups records by key. Latest writer wins.
    pub state_key: Option<String>,
    /// Whether this record is visible on the live surface.
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Record IDs this supersedes (hides from live surface).
    #[serde(default)]
    pub supersedes: Vec<RecordId>,
    /// Opaque payload.
    #[serde(default)]
    pub payload: serde_json::Value,
}

fn default_true() -> bool { true }

/// A lease that prevents a record from being retired.
#[derive(Debug, Clone)]
pub struct Lease {
    pub id: String,
    pub record_id: RecordId,
    pub agent_id: String,
    pub expires_at: Option<Timestamp>,
    pub released: bool,
}

impl Lease {
    pub fn is_active(&self, now: Timestamp) -> bool {
        !self.released && !self.expires_at.is_some_and(|exp| now >= exp)
    }
}

// ── Section claims ──

/// An inclusive 1-based line span within a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineSpan {
    pub start: usize,
    pub end: usize,
}

impl LineSpan {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn overlaps(&self, other: &LineSpan) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// A region-scoped lock on a section of a file.
#[derive(Debug, Clone)]
pub struct SectionClaim {
    pub claim_id: String,
    pub agent_id: String,
    pub file_path: String,
    pub spans: Vec<LineSpan>,
    pub purpose: String,
    pub acquired_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub released: bool,
}

impl SectionClaim {
    pub fn is_active(&self, now: Timestamp) -> bool {
        !self.released && !self.expires_at.is_some_and(|exp| now >= exp)
    }
}

/// Check if any span in `a` overlaps any span in `b`.
fn spans_overlap(a: &[LineSpan], b: &[LineSpan]) -> bool {
    a.iter().any(|sa| b.iter().any(|sb| sa.overlaps(sb)))
}

/// Result of attempting to claim a section.
pub struct ClaimResult {
    pub ok: bool,
    pub claim: SectionClaim,
    /// Active claims by other agents that overlap with the requested region.
    pub contenders: Vec<SectionClaim>,
}

// ── The coordination stream ──

/// A record annotated with its stream marker for interleaved views.
pub struct MarkedRecord<'a> {
    pub record: &'a Record,
    /// Which stream this record matched (e.g. "task:abc", "file:src/main.rs", "global").
    pub marker: &'a str,
}

/// Append-only coordination stream with visibility and retention management.
pub struct Stream {
    records: Vec<Record>,
    by_id: HashMap<RecordId, usize>,
    visible: HashSet<RecordId>,
    retired: HashSet<RecordId>,
    superseded_by: HashMap<RecordId, RecordId>,
    /// state_key → latest record ID (latest-writer-wins compaction).
    state_latest: HashMap<String, RecordId>,
    leases: HashMap<String, Lease>,
    leases_by_record: HashMap<RecordId, Vec<String>>,
}

impl Stream {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            by_id: HashMap::new(),
            visible: HashSet::new(),
            retired: HashSet::new(),
            superseded_by: HashMap::new(),
            state_latest: HashMap::new(),
            leases: HashMap::new(),
            leases_by_record: HashMap::new(),
        }
    }

    /// Append a record. Applies visibility transitions and sweeps retirable records.
    pub fn append(&mut self, record: Record) {
        let idx = self.records.len();
        let id = record.id.clone();

        // Visibility
        if record.visible {
            self.visible.insert(id.clone());
        }

        // Explicit supersession
        for target in &record.supersedes {
            if self.by_id.contains_key(target) {
                self.visible.remove(target);
                self.superseded_by.insert(target.clone(), id.clone());
            }
        }

        // state_key compaction: latest writer wins
        if let Some(ref key) = record.state_key {
            if let Some(prev) = self.state_latest.insert(key.clone(), id.clone()) {
                if prev != id {
                    self.visible.remove(&prev);
                    self.superseded_by.insert(prev, id.clone());
                }
            }
        }

        self.by_id.insert(id, idx);
        self.records.push(record);

        self.sweep();
    }

    pub fn get(&self, id: &str) -> Option<&Record> {
        self.by_id.get(id).map(|&i| &self.records[i])
    }

    pub fn len(&self) -> usize { self.records.len() }

    /// All records, unfiltered.
    pub fn raw(&self) -> &[Record] { &self.records }

    /// Records currently visible on the live surface.
    pub fn visible(&self) -> Vec<&Record> {
        self.records.iter().filter(|r| self.visible.contains(&r.id)).collect()
    }

    /// Filtered view: records matching any of the given tags, in chronological order.
    pub fn view(&self, tags: &[&str]) -> Vec<&Record> {
        if tags.is_empty() {
            return self.visible();
        }
        self.records.iter()
            .filter(|r| {
                if !self.visible.contains(&r.id) { return false; }
                r.tags.iter().any(|t| tags.contains(&t.as_str()))
            })
            .collect()
    }

    /// Filtered view including hidden records the agent has leased.
    pub fn view_with_leased(&self, tags: &[&str], agent_id: &str) -> Vec<&Record> {
        let now = now_ms();
        let leased: HashSet<&str> = self.leases.values()
            .filter(|l| l.agent_id == agent_id && l.is_active(now))
            .map(|l| l.record_id.as_str())
            .collect();

        self.records.iter()
            .filter(|r| {
                let vis = self.visible.contains(&r.id) || leased.contains(r.id.as_str());
                if !vis { return false; }
                if tags.is_empty() { return true; }
                r.tags.iter().any(|t| tags.contains(&t.as_str()))
            })
            .collect()
    }

    /// Interleaved view with stream markers. Returns records matching any of the
    /// given tags, each annotated with which tag(s) it matched. Chronological order.
    /// Models receive this so they intuitively see how streams relate.
    pub fn interleaved_view(&self, tags: &[&str], agent_id: Option<&str>) -> Vec<MarkedRecord<'_>> {
        let now = now_ms();
        let leased: HashSet<&str> = agent_id
            .map(|aid| {
                self.leases.values()
                    .filter(|l| l.agent_id == aid && l.is_active(now))
                    .map(|l| l.record_id.as_str())
                    .collect()
            })
            .unwrap_or_default();

        self.records.iter()
            .filter_map(|r| {
                let vis = self.visible.contains(&r.id) || leased.contains(r.id.as_str());
                if !vis { return None; }

                let matched_tags: Vec<&str> = if tags.is_empty() {
                    vec!["global"]
                } else {
                    r.tags.iter()
                        .filter(|t| tags.contains(&t.as_str()))
                        .map(|t| t.as_str())
                        .collect()
                };

                if matched_tags.is_empty() && !tags.is_empty() {
                    return None;
                }

                // Pick the most specific matching tag as the marker
                let marker = matched_tags.iter()
                    .find(|t| **t != "global")
                    .or(matched_tags.first())
                    .copied()
                    .unwrap_or("global");

                Some(MarkedRecord { record: r, marker })
            })
            .collect()
    }

    // ── Leases ──

    pub fn lease(&mut self, record_id: &str, agent_id: &str, expires_at: Option<Timestamp>) -> String {
        let id = new_id("lease");
        let lease = Lease {
            id: id.clone(),
            record_id: record_id.to_string(),
            agent_id: agent_id.to_string(),
            expires_at,
            released: false,
        };
        self.leases.insert(id.clone(), lease);
        self.leases_by_record
            .entry(record_id.to_string())
            .or_default()
            .push(id.clone());
        id
    }

    pub fn release_lease(&mut self, lease_id: &str) {
        if let Some(lease) = self.leases.get_mut(lease_id) {
            lease.released = true;
        }
        self.sweep();
    }

    pub fn expire_leases(&mut self) {
        let now = now_ms();
        for lease in self.leases.values_mut() {
            if !lease.released && lease.expires_at.is_some_and(|exp| now >= exp) {
                lease.released = true;
            }
        }
        self.sweep();
    }

    // ── Private ──

    /// Retire hidden records with no active leases. Returns retired IDs.
    fn sweep(&mut self) -> Vec<RecordId> {
        let now = now_ms();
        let mut to_retire = Vec::new();

        for record in &self.records {
            let id = &record.id;
            if self.retired.contains(id) || self.visible.contains(id) {
                continue;
            }
            if !self.superseded_by.contains_key(id) {
                continue;
            }
            let has_lease = self.leases_by_record
                .get(id)
                .is_some_and(|ids| ids.iter().any(|lid| {
                    self.leases.get(lid).is_some_and(|l| l.is_active(now))
                }));
            if !has_lease {
                to_retire.push(id.clone());
            }
        }

        for id in &to_retire {
            self.retired.insert(id.clone());
        }
        to_retire
    }
}

impl Default for Stream {
    fn default() -> Self { Self::new() }
}

// ── Coordinator state ──

/// Session-wide coordination state.
pub struct CoordinatorState {
    pub tasks: HashMap<String, Issue>,
    pub lanes: HashMap<String, TaskLane>,
    pub active_model: Option<String>,
    pub last_refresh: Instant,
    /// The shared coordination stream.
    pub stream: Stream,
    /// Active section claims, keyed by claim_id.
    claims: HashMap<String, SectionClaim>,
}

impl CoordinatorState {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            lanes: HashMap::new(),
            active_model: None,
            last_refresh: Instant::now(),
            stream: Stream::new(),
            claims: HashMap::new(),
        }
    }

    pub fn set_active_model(&mut self, model_id: impl Into<String>) {
        self.active_model = Some(model_id.into());
        self.last_refresh = Instant::now();
    }

    pub fn initialize_from_tasks_snapshot(&mut self, issues: impl IntoIterator<Item = Issue>) {
        self.tasks.clear();
        for issue in issues {
            self.tasks.insert(issue.id.clone(), issue);
        }
        self.last_refresh = Instant::now();
    }

    pub fn ingest_issue(&mut self, issue: Issue) {
        self.tasks.insert(issue.id.clone(), issue);
        self.last_refresh = Instant::now();
    }

    pub fn upsert_lane(&mut self, lane: TaskLane) {
        self.lanes.insert(lane.id.clone(), lane);
        self.last_refresh = Instant::now();
    }

    pub fn plan_parallel_work(&self, issue_id: &str) -> Vec<TaskLane> {
        self.tasks
            .get(issue_id)
            .map(|issue| {
                vec![TaskLane {
                    id: format!("lane:{issue_id}"),
                    issue_id: issue.id.clone(),
                    title: issue.title.clone(),
                    assigned_model: self.active_model.clone(),
                    active: true,
                }]
            })
            .unwrap_or_default()
    }

    // ── Section claims ──

    /// Active claims for a file, optionally filtered by time.
    pub fn active_claims(&self, path: &str) -> Vec<&SectionClaim> {
        let now = now_ms();
        self.claims.values()
            .filter(|c| c.is_active(now) && c.file_path == path)
            .collect()
    }

    /// Try to claim a section. Returns contenders if blocked.
    /// With `force`, steals overlapping claims from other agents.
    pub fn claim_section(
        &mut self,
        agent_id: &str,
        file_path: &str,
        spans: Vec<LineSpan>,
        purpose: &str,
        force: bool,
    ) -> ClaimResult {
        let now = now_ms();
        let contenders: Vec<SectionClaim> = self.claims.values()
            .filter(|c| {
                c.is_active(now)
                    && c.file_path == file_path
                    && c.agent_id != agent_id
                    && spans_overlap(&c.spans, &spans)
            })
            .cloned()
            .collect();

        if !contenders.is_empty() && !force {
            let claim = SectionClaim {
                claim_id: new_id("claim"),
                agent_id: agent_id.to_string(),
                file_path: file_path.to_string(),
                spans,
                purpose: purpose.to_string(),
                acquired_at: now,
                expires_at: None,
                released: false,
            };
            return ClaimResult { ok: false, claim, contenders };
        }

        // Force: release contenders
        if force {
            for c in &contenders {
                self.release_claim_inner(&c.claim_id);
            }
        }

        let claim_id = new_id("claim");
        let claim = SectionClaim {
            claim_id: claim_id.clone(),
            agent_id: agent_id.to_string(),
            file_path: file_path.to_string(),
            spans: spans.clone(),
            purpose: purpose.to_string(),
            acquired_at: now,
            expires_at: None,
            released: false,
        };
        self.claims.insert(claim_id.clone(), claim.clone());

        // Emit to stream
        self.stream.append(Record {
            id: new_id("evt"),
            ts: now,
            kind: RecordKind::SectionClaim,
            agent_id: agent_id.to_string(),
            task_id: None,
            tags: vec!["global".into(), format!("file:{file_path}")],
            state_key: None,
            visible: true,
            supersedes: Vec::new(),
            payload: serde_json::json!({
                "claim_id": claim_id,
                "file_path": file_path,
                "spans": spans,
                "purpose": purpose,
                "forced": force,
            }),
        });

        ClaimResult { ok: true, claim, contenders }
    }

    /// Release a claim.
    pub fn release_claim(&mut self, claim_id: &str) {
        if let Some(claim) = self.release_claim_inner(claim_id) {
            let now = now_ms();
            self.stream.append(Record {
                id: new_id("evt"),
                ts: now,
                kind: RecordKind::SectionRelease,
                agent_id: claim.agent_id.clone(),
                task_id: None,
                tags: vec!["global".into(), format!("file:{}", claim.file_path)],
                state_key: None,
                visible: false, // releases are bookkeeping, not live-visible
                supersedes: Vec::new(),
                payload: serde_json::json!({
                    "claim_id": claim_id,
                    "file_path": claim.file_path,
                }),
            });
        }
    }

    fn release_claim_inner(&mut self, claim_id: &str) -> Option<SectionClaim> {
        if let Some(claim) = self.claims.get_mut(claim_id) {
            claim.released = true;
            Some(claim.clone())
        } else {
            None
        }
    }

    /// Check if a write to the given spans would conflict with any active claim
    /// owned by a different agent.
    pub fn check_claim_conflict(
        &self,
        agent_id: &str,
        file_path: &str,
        spans: &[LineSpan],
    ) -> Vec<&SectionClaim> {
        let now = now_ms();
        self.claims.values()
            .filter(|c| {
                c.is_active(now)
                    && c.file_path == file_path
                    && c.agent_id != agent_id
                    && spans_overlap(&c.spans, spans)
            })
            .collect()
    }

    /// Expire claims that have passed their expiration time.
    pub fn expire_claims(&mut self) {
        let now = now_ms();
        let expired: Vec<String> = self.claims.values()
            .filter(|c| !c.released && c.expires_at.is_some_and(|exp| now >= exp))
            .map(|c| c.claim_id.clone())
            .collect();
        for id in expired {
            self.release_claim_inner(&id);
        }
    }

    // ── Director: task status transitions with compaction ──

    /// Advance a task's status. Automatically supersedes the previous status record
    /// via state_key compaction.
    pub fn advance_task_status(&mut self, task_id: &str, status: &str, agent_id: &str) {
        let now = now_ms();
        self.stream.append(Record {
            id: new_id("evt"),
            ts: now,
            kind: RecordKind::TaskStatus,
            agent_id: agent_id.to_string(),
            task_id: Some(task_id.to_string()),
            tags: vec!["global".into(), format!("task:{task_id}")],
            state_key: Some(format!("task:{task_id}:status")),
            visible: true,
            supersedes: Vec::new(),
            payload: serde_json::json!({ "status": status }),
        });
    }

    // ── Director: stream compaction ──

    /// Run compaction: retire superseded records, expire leases and claims.
    /// Returns the number of records retired.
    pub fn compact(&mut self) -> usize {
        self.expire_claims();

        // Expire leases first, then sweep — count all retirements
        let now = now_ms();
        for lease in self.stream.leases.values_mut() {
            if !lease.released && lease.expires_at.is_some_and(|exp| now >= exp) {
                lease.released = true;
            }
        }
        self.stream.sweep().len()
    }

    // ── Stream ──

    /// Emit a record to the coordination stream.
    pub fn emit(&mut self, record: Record) {
        self.stream.append(record);
        self.last_refresh = Instant::now();
    }
}

// ── Helpers ──

fn new_id(prefix: &str) -> String {
    format!("{}-{}", prefix, &uuid::Uuid::new_v4().simple().to_string()[..10])
}

fn now_ms() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as u64
}

/// Build a record.
pub fn record(kind: RecordKind, agent_id: &str, payload: serde_json::Value) -> Record {
    Record {
        id: new_id("evt"),
        ts: now_ms(),
        kind,
        agent_id: agent_id.to_string(),
        task_id: None,
        tags: vec!["global".into()],
        state_key: None,
        visible: true,
        supersedes: Vec::new(),
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record(kind: RecordKind) -> Record {
        record(kind, "test-agent", serde_json::json!({}))
    }

    #[test]
    fn append_and_get() {
        let mut s = Stream::new();
        let r = test_record(RecordKind::AgentActivity);
        let id = r.id.clone();
        s.append(r);
        assert_eq!(s.len(), 1);
        assert!(s.get(&id).is_some());
    }

    #[test]
    fn visible_by_default() {
        let mut s = Stream::new();
        let r = test_record(RecordKind::AgentActivity);
        let id = r.id.clone();
        s.append(r);
        assert_eq!(s.visible().len(), 1);
        assert!(s.visible().iter().any(|r| r.id == id));
    }

    #[test]
    fn hidden_record_not_visible() {
        let mut s = Stream::new();
        let mut r = test_record(RecordKind::AgentActivity);
        r.visible = false;
        let id = r.id.clone();
        s.append(r);
        assert!(s.visible().is_empty());
        assert_eq!(s.raw().len(), 1);
        assert!(s.get(&id).is_some());
    }

    #[test]
    fn state_key_supersedes_previous() {
        let mut s = Stream::new();

        let mut r1 = test_record(RecordKind::TaskStatus);
        r1.state_key = Some("task:abc:status".into());
        let id1 = r1.id.clone();
        s.append(r1);

        let mut r2 = test_record(RecordKind::TaskStatus);
        r2.state_key = Some("task:abc:status".into());
        let id2 = r2.id.clone();
        s.append(r2);

        let vis = s.visible();
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].id, id2);
        // r1 is hidden and retired (no leases)
        assert!(!s.visible.contains(&id1));
    }

    #[test]
    fn explicit_supersedes() {
        let mut s = Stream::new();

        let r1 = test_record(RecordKind::EditIntent);
        let id1 = r1.id.clone();
        s.append(r1);

        let mut r2 = test_record(RecordKind::WriteApplied);
        r2.supersedes = vec![id1.clone()];
        s.append(r2);

        assert!(!s.visible.contains(&id1));
    }

    #[test]
    fn lease_prevents_retirement() {
        let mut s = Stream::new();

        let mut r1 = test_record(RecordKind::TaskStatus);
        r1.state_key = Some("task:abc:status".into());
        let id1 = r1.id.clone();
        s.append(r1);

        // Lease r1 before it gets superseded
        let lease_id = s.lease(&id1, "test-agent", None);

        // Supersede r1
        let mut r2 = test_record(RecordKind::TaskStatus);
        r2.state_key = Some("task:abc:status".into());
        s.append(r2);

        // r1 is hidden but NOT retired (lease holds)
        assert!(!s.visible.contains(&id1));
        assert!(!s.retired.contains(&id1));

        // Release lease → r1 gets retired
        s.release_lease(&lease_id);
        assert!(s.retired.contains(&id1));
    }

    #[test]
    fn view_filters_by_tags() {
        let mut s = Stream::new();

        let mut r1 = test_record(RecordKind::EditIntent);
        r1.tags = vec!["task:abc".into()];
        let id1 = r1.id.clone();
        s.append(r1);

        let mut r2 = test_record(RecordKind::EditIntent);
        r2.tags = vec!["task:xyz".into()];
        s.append(r2);

        let view = s.view(&["task:abc"]);
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].id, id1);
    }

    #[test]
    fn view_with_leased_shows_hidden() {
        let mut s = Stream::new();

        let r1 = test_record(RecordKind::EditIntent);
        let id1 = r1.id.clone();
        s.append(r1);

        let _lease_id = s.lease(&id1, "agent-a", None);

        // Supersede r1
        let mut r2 = test_record(RecordKind::WriteApplied);
        r2.supersedes = vec![id1.clone()];
        s.append(r2);

        // Normal view: r1 hidden
        assert!(!s.visible.contains(&id1));

        // Leased view for agent-a: r1 visible
        let view = s.view_with_leased(&[], "agent-a");
        assert!(view.iter().any(|r| r.id == id1));

        // Leased view for agent-b: r1 not visible
        let view = s.view_with_leased(&[], "agent-b");
        assert!(!view.iter().any(|r| r.id == id1));
    }

    // ── Section claim tests ──

    #[test]
    fn non_overlapping_claims_succeed() {
        let mut c = CoordinatorState::new();

        let r1 = c.claim_section("agent-a", "src/main.rs", vec![LineSpan::new(1, 20)], "refactor imports", false);
        assert!(r1.ok);

        let r2 = c.claim_section("agent-b", "src/main.rs", vec![LineSpan::new(50, 80)], "add tests", false);
        assert!(r2.ok);
        assert!(r2.contenders.is_empty());
    }

    #[test]
    fn overlapping_claim_blocked() {
        let mut c = CoordinatorState::new();

        let r1 = c.claim_section("agent-a", "src/main.rs", vec![LineSpan::new(10, 30)], "edit", false);
        assert!(r1.ok);

        let r2 = c.claim_section("agent-b", "src/main.rs", vec![LineSpan::new(25, 40)], "edit", false);
        assert!(!r2.ok);
        assert_eq!(r2.contenders.len(), 1);
        assert_eq!(r2.contenders[0].agent_id, "agent-a");
    }

    #[test]
    fn force_claim_steals() {
        let mut c = CoordinatorState::new();

        let r1 = c.claim_section("agent-a", "src/main.rs", vec![LineSpan::new(10, 30)], "edit", false);
        assert!(r1.ok);
        let stolen_id = r1.claim.claim_id.clone();

        let r2 = c.claim_section("agent-b", "src/main.rs", vec![LineSpan::new(20, 40)], "force edit", true);
        assert!(r2.ok);

        // Original claim is released
        assert!(c.claims[&stolen_id].released);
    }

    #[test]
    fn same_agent_same_region_ok() {
        let mut c = CoordinatorState::new();

        let r1 = c.claim_section("agent-a", "src/main.rs", vec![LineSpan::new(10, 30)], "edit", false);
        assert!(r1.ok);

        // Same agent, overlapping region — not a conflict
        let r2 = c.claim_section("agent-a", "src/main.rs", vec![LineSpan::new(20, 40)], "more edit", false);
        assert!(r2.ok);
    }

    #[test]
    fn different_files_no_conflict() {
        let mut c = CoordinatorState::new();

        let r1 = c.claim_section("agent-a", "src/main.rs", vec![LineSpan::new(1, 100)], "edit", false);
        assert!(r1.ok);

        let r2 = c.claim_section("agent-b", "src/app.rs", vec![LineSpan::new(1, 100)], "edit", false);
        assert!(r2.ok);
        assert!(r2.contenders.is_empty());
    }

    #[test]
    fn released_claim_no_conflict() {
        let mut c = CoordinatorState::new();

        let r1 = c.claim_section("agent-a", "src/main.rs", vec![LineSpan::new(10, 30)], "edit", false);
        assert!(r1.ok);
        c.release_claim(&r1.claim.claim_id);

        // Now agent-b can claim the same region
        let r2 = c.claim_section("agent-b", "src/main.rs", vec![LineSpan::new(10, 30)], "edit", false);
        assert!(r2.ok);
    }

    #[test]
    fn claim_emits_to_stream() {
        let mut c = CoordinatorState::new();

        let initial_len = c.stream.len();
        c.claim_section("agent-a", "src/main.rs", vec![LineSpan::new(1, 10)], "edit", false);
        assert_eq!(c.stream.len(), initial_len + 1);

        let records = c.stream.visible();
        let last = records.last().unwrap();
        assert_eq!(last.kind, RecordKind::SectionClaim);
    }

    #[test]
    fn check_conflict_without_claiming() {
        let mut c = CoordinatorState::new();

        c.claim_section("agent-a", "src/main.rs", vec![LineSpan::new(10, 30)], "edit", false);

        let conflicts = c.check_claim_conflict("agent-b", "src/main.rs", &[LineSpan::new(25, 40)]);
        assert_eq!(conflicts.len(), 1);

        let no_conflict = c.check_claim_conflict("agent-b", "src/main.rs", &[LineSpan::new(50, 60)]);
        assert!(no_conflict.is_empty());
    }

    #[test]
    fn task_status_compaction() {
        let mut c = CoordinatorState::new();

        c.advance_task_status("task-a", "pending", "agent-1");
        c.advance_task_status("task-a", "in_progress", "agent-1");
        c.advance_task_status("task-a", "done", "agent-1");

        // Only the latest status should be visible
        let vis = c.stream.visible();
        let statuses: Vec<&Record> = vis.iter()
            .filter(|r| r.kind == RecordKind::TaskStatus)
            .copied()
            .collect();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].payload["status"], "done");
    }

    #[test]
    fn compact_expires_and_sweeps() {
        let mut c = CoordinatorState::new();

        // Create a record, lease it, supersede it
        let mut r1 = test_record(RecordKind::TaskStatus);
        r1.state_key = Some("task:abc:status".into());
        let id1 = r1.id.clone();
        c.stream.append(r1);

        // Lease expires 50ms from now — holds during append, expires before compact
        let lease_id = c.stream.lease(&id1, "agent-a", Some(now_ms() + 50));

        let mut r2 = test_record(RecordKind::TaskStatus);
        r2.state_key = Some("task:abc:status".into());
        c.stream.append(r2);

        // r1 is hidden but lease holds it
        assert!(!c.stream.retired.contains(&id1));

        // Wait for lease to expire, then compact
        std::thread::sleep(std::time::Duration::from_millis(60));
        let retired = c.compact();
        assert!(retired > 0);
        assert!(c.stream.retired.contains(&id1));
    }

    #[test]
    fn interleaved_view_with_markers() {
        let mut s = Stream::new();

        let mut r1 = test_record(RecordKind::EditIntent);
        r1.tags = vec!["global".into(), "task:abc".into()];
        let id1 = r1.id.clone();
        s.append(r1);

        let mut r2 = test_record(RecordKind::WriteApplied);
        r2.tags = vec!["global".into(), "task:xyz".into()];
        let id2 = r2.id.clone();
        s.append(r2);

        let mut r3 = test_record(RecordKind::SectionClaim);
        r3.tags = vec!["global".into(), "task:abc".into(), "file:src/main.rs".into()];
        let id3 = r3.id.clone();
        s.append(r3);

        // View for task:abc — should see r1 and r3, marked with task:abc
        let view = s.interleaved_view(&["task:abc"], None);
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].record.id, id1);
        assert_eq!(view[0].marker, "task:abc");
        assert_eq!(view[1].record.id, id3);
        // r3 has both task:abc and file:src/main.rs — picks the most specific non-global one
        assert_ne!(view[1].marker, "global");

        // View for both — all 3 records, each marked with their matching tag
        let view = s.interleaved_view(&["task:abc", "task:xyz"], None);
        assert_eq!(view.len(), 3);

        // Global view (no filter) — all records
        let view = s.interleaved_view(&[], None);
        assert_eq!(view.len(), 3);
    }
}
