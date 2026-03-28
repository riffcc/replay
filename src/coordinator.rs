//! Session coordinator — shared runtime state for Replay.
//!
//! This module tracks active tasks, lane plans, and model/provider pressure so
//! the TUI and agents can make parallelism and routing decisions from one place.

use std::collections::HashMap;
use std::time::Instant;

use crate::beads::Issue;

/// How provider pressure should be displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatesDisplayMode {
    Off,
    Soft,
    On,
}

impl Default for RatesDisplayMode {
    fn default() -> Self {
        Self::Soft
    }
}

impl RatesDisplayMode {
    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "soft" => Some(Self::Soft),
            "on" => Some(Self::On),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Soft => "soft",
            Self::On => "on",
        }
    }
}

/// Confidence in a provider budget estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetConfidence {
    /// No direct measurement yet.
    Unknown,
    /// Estimated from observed usage / throttling.
    Estimated,
    /// Backed by a direct provider response header or explicit quota signal.
    Measured,
}

impl Default for BudgetConfidence {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Which broad tier a model belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTier {
    Strong,
    Standard,
    Weak,
    Free,
}

impl Default for ModelTier {
    fn default() -> Self {
        Self::Standard
    }
}

/// Live estimate of a provider/model budget.
#[derive(Debug, Clone)]
pub struct ProviderBudget {
    pub provider: String,
    pub model_id: String,
    /// Remaining budget estimate, in provider-specific units.
    pub remaining_estimate: Option<f64>,
    /// Used budget estimate, in provider-specific units.
    pub used_estimate: Option<f64>,
    /// Estimated reset time for rolling windows / quotas.
    pub reset_at: Option<Instant>,
    /// Estimated spend in USD, where applicable.
    pub spend_estimate_usd: Option<f64>,
    /// Recent throttles or hard quota failures.
    pub throttle_count: u32,
    pub last_seen: Instant,
    pub confidence: BudgetConfidence,
    pub tier: ModelTier,
}

impl ProviderBudget {
    pub fn new(provider: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model_id: model_id.into(),
            remaining_estimate: None,
            used_estimate: None,
            reset_at: None,
            spend_estimate_usd: None,
            throttle_count: 0,
            last_seen: Instant::now(),
            confidence: BudgetConfidence::Unknown,
            tier: ModelTier::Standard,
        }
    }

    pub fn fullness_pct(&self) -> Option<u8> {
        let used = self.used_estimate?;
        let remaining = self.remaining_estimate?;
        let total = used + remaining;
        if total <= 0.0 {
            return None;
        }
        Some(((used / total) * 100.0).round().clamp(0.0, 100.0) as u8)
    }

    pub fn headroom_pct(&self) -> Option<u8> {
        self.fullness_pct().map(|p| 100u8.saturating_sub(p))
    }
}

/// A task lane in the dry-run / parallel planner.
#[derive(Debug, Clone)]
pub struct TaskLane {
    pub id: String,
    pub issue_id: String,
    pub title: String,
    pub assigned_model: Option<String>,
    pub active: bool,
}

/// Session-wide coordination state.
#[derive(Debug)]
pub struct CoordinatorState {
    pub tasks: HashMap<String, Issue>,
    pub lanes: HashMap<String, TaskLane>,
    pub providers: HashMap<String, ProviderBudget>,
    pub active_model: Option<String>,
    pub last_refresh: Instant,
}

impl CoordinatorState {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            lanes: HashMap::new(),
            providers: HashMap::new(),
            active_model: None,
            last_refresh: Instant::now(),
        }
    }

    /// Remember the active model and ensure a budget entry exists for it.
    pub fn set_active_model(&mut self, model_id: impl Into<String>, provider: impl Into<String>) {
        let model_id = model_id.into();
        let provider = provider.into();
        self.active_model = Some(model_id.clone());
        self.providers
            .entry(model_id.clone())
            .or_insert_with(|| ProviderBudget::new(provider.clone(), model_id.clone()))
            .provider = provider;
    }

    pub fn ingest_issue(&mut self, issue: Issue) {
        self.tasks.insert(issue.id.clone(), issue);
    }

    pub fn upsert_lane(&mut self, lane: TaskLane) {
        self.lanes.insert(lane.id.clone(), lane);
    }

    pub fn set_budget(
        &mut self,
        model_id: &str,
        provider: impl Into<String>,
        remaining_estimate: Option<f64>,
        used_estimate: Option<f64>,
        reset_at: Option<Instant>,
        spend_estimate_usd: Option<f64>,
        confidence: BudgetConfidence,
        tier: ModelTier,
    ) {
        let provider = provider.into();
        let entry = self.providers
            .entry(model_id.to_string())
            .or_insert_with(|| ProviderBudget::new(provider.clone(), model_id));
        entry.provider = provider;
        entry.remaining_estimate = remaining_estimate;
        entry.used_estimate = used_estimate;
        entry.reset_at = reset_at;
        entry.spend_estimate_usd = spend_estimate_usd;
        entry.confidence = confidence;
        entry.tier = tier;
        entry.last_seen = Instant::now();
    }

    pub fn record_usage(
        &mut self,
        model_id: &str,
        provider: impl Into<String>,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        let provider = provider.into();
        let entry = self.providers
            .entry(model_id.to_string())
            .or_insert_with(|| ProviderBudget::new(provider.clone(), model_id));
        entry.provider = provider;
        let used = entry.used_estimate.unwrap_or(0.0) + (input_tokens + output_tokens) as f64;
        entry.used_estimate = Some(used);
        if let Some(remaining) = entry.remaining_estimate {
            entry.remaining_estimate = Some((remaining - (input_tokens + output_tokens) as f64).max(0.0));
        }
        entry.confidence = match entry.confidence {
            BudgetConfidence::Unknown => BudgetConfidence::Estimated,
            other => other,
        };
        entry.last_seen = Instant::now();
    }

    pub fn record_throttle(&mut self, model_id: &str) {
        if let Some(entry) = self.providers.get_mut(model_id) {
            entry.throttle_count = entry.throttle_count.saturating_add(1);
            entry.last_seen = Instant::now();
            entry.confidence = BudgetConfidence::Estimated;
        }
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

    pub fn budget_summary(&self, model_id: &str) -> Option<String> {
        let budget = self.providers.get(model_id)?;
        let pct = budget.fullness_pct()?;
        let headroom = budget.headroom_pct().unwrap_or(0);
        let reset = budget.reset_at.map(|_| " reset?".to_string()).unwrap_or_default();
        Some(format!("{pct}% used · {headroom}% headroom{reset}"))
    }
}
