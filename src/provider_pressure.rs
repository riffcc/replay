//! Runtime provider pressure tracking.
//!
//! This module is intentionally separate from task/beads coordination.
//! It tracks short-window provider pressure using a token-bucket estimate
//! plus reactive 429 / retry-after backoff signals.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Conservative short-window pacing profile for a provider route.
#[derive(Debug, Clone, Copy)]
pub struct BucketProfile {
    pub capacity: f64,
    pub refill_per_sec: f64,
}

impl BucketProfile {
    pub const fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            capacity,
            refill_per_sec,
        }
    }

    /// Generic conservative default derived from the Palace token bucket.
    /// Roughly: 60 req/min with a small burst, biased slightly under the edge.
    pub fn for_provider(provider: &str) -> Self {
        match provider.trim().to_ascii_lowercase().as_str() {
            "openai" => Self::new(5.0, 0.9),
            "z.ai" | "zai" => Self::new(5.0, 0.9),
            "minimax" => Self::new(5.0, 0.9),
            _ => Self::new(5.0, 0.9),
        }
    }
}

/// Simple token bucket used to estimate short-window pressure.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(profile: BucketProfile) -> Self {
        Self {
            capacity: profile.capacity,
            refill_per_sec: profile.refill_per_sec,
            tokens: profile.capacity,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
            self.last_refill = now;
        }
    }

    pub fn consume(&mut self, amount: f64, now: Instant) {
        self.refill(now);
        self.tokens = (self.tokens - amount).max(0.0);
    }

    pub fn headroom_pct(&mut self, now: Instant) -> u8 {
        self.refill(now);
        if self.capacity <= 0.0 {
            return 0;
        }
        ((self.tokens / self.capacity) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8
    }

    pub fn fullness_pct(&mut self, now: Instant) -> u8 {
        100u8.saturating_sub(self.headroom_pct(now))
    }

    pub fn seconds_until(&mut self, needed: f64, now: Instant) -> Option<u64> {
        self.refill(now);
        if self.tokens >= needed {
            return Some(0);
        }
        if self.refill_per_sec <= 0.0 {
            return None;
        }
        let deficit = (needed - self.tokens).max(0.0);
        Some((deficit / self.refill_per_sec).ceil() as u64)
    }
}

/// Snapshot used by the UI / router.
#[derive(Debug, Clone)]
pub struct ProviderPressureSnapshot {
    pub provider: String,
    pub model_id: String,
    pub fullness_pct: u8,
    pub headroom_pct: u8,
    pub in_flight_runs: u32,
    pub throttle_count: u32,
    pub backing_off: bool,
    pub retry_after_secs: Option<u64>,
    pub total_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct ProviderRoutePressure {
    pub provider: String,
    pub model_id: String,
    bucket: TokenBucket,
    pub in_flight_runs: u32,
    pub throttle_count: u32,
    pub total_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub backoff_until: Option<Instant>,
    pub last_retry_after: Option<Duration>,
    pub last_seen: Instant,
}

impl ProviderRoutePressure {
    pub fn new(provider: impl Into<String>, model_id: impl Into<String>) -> Self {
        let provider = provider.into();
        let model_id = model_id.into();
        Self {
            bucket: TokenBucket::new(BucketProfile::for_provider(&provider)),
            provider,
            model_id,
            in_flight_runs: 0,
            throttle_count: 0,
            total_requests: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            backoff_until: None,
            last_retry_after: None,
            last_seen: Instant::now(),
        }
    }

    pub fn note_run_start(&mut self) {
        self.in_flight_runs = self.in_flight_runs.saturating_add(1);
        self.last_seen = Instant::now();
    }

    pub fn note_run_finish(&mut self) {
        self.in_flight_runs = self.in_flight_runs.saturating_sub(1);
        self.last_seen = Instant::now();
    }

    /// Record one completed API request. We consume one request token here
    /// because Replay currently sees Usage per API response but not request-start.
    pub fn record_response(&mut self, input_tokens: u64, output_tokens: u64) {
        let now = Instant::now();
        self.bucket.consume(1.0, now);
        self.total_requests = self.total_requests.saturating_add(1);
        self.total_input_tokens = self.total_input_tokens.saturating_add(input_tokens);
        self.total_output_tokens = self.total_output_tokens.saturating_add(output_tokens);
        self.last_seen = now;
    }

    pub fn record_rate_limited(&mut self, retry_after_secs: Option<u64>) {
        let now = Instant::now();
        self.throttle_count = self.throttle_count.saturating_add(1);
        self.last_retry_after = retry_after_secs.map(Duration::from_secs);
        self.backoff_until = retry_after_secs.map(|secs| now + Duration::from_secs(secs));
        self.last_seen = now;
    }

    pub fn backing_off(&self, now: Instant) -> bool {
        self.backoff_until.is_some_and(|until| until > now)
    }

    pub fn retry_after_secs(&self, now: Instant) -> Option<u64> {
        self.backoff_until
            .and_then(|until| until.checked_duration_since(now))
            .map(|d| d.as_secs())
    }

    pub fn snapshot(&mut self) -> ProviderPressureSnapshot {
        let now = Instant::now();
        let backing_off = self.backing_off(now);
        let retry_after_secs = self.retry_after_secs(now);
        let fullness_pct = if backing_off {
            100
        } else {
            self.bucket.fullness_pct(now)
        };
        let headroom_pct = if backing_off {
            0
        } else {
            self.bucket.headroom_pct(now)
        };

        ProviderPressureSnapshot {
            provider: self.provider.clone(),
            model_id: self.model_id.clone(),
            fullness_pct,
            headroom_pct,
            in_flight_runs: self.in_flight_runs,
            throttle_count: self.throttle_count,
            backing_off,
            retry_after_secs,
            total_requests: self.total_requests,
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
        }
    }
}

/// Session-wide provider pressure tracker.
#[derive(Debug, Default)]
pub struct ProviderPressureTracker {
    routes: HashMap<String, ProviderRoutePressure>,
    active_model_id: Option<String>,
}

impl ProviderPressureTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_active_route(&mut self, provider: impl Into<String>, model_id: impl Into<String>) {
        let provider = provider.into();
        let model_id = model_id.into();
        self.active_model_id = Some(model_id.clone());
        let p = provider.clone();
        self.routes
            .entry(model_id.clone())
            .or_insert_with(|| ProviderRoutePressure::new(p, model_id.clone()))
            .provider = provider;
    }

    pub fn note_run_start(&mut self, provider: impl Into<String>, model_id: impl Into<String>) {
        let provider = provider.into();
        let model_id = model_id.into();
        self.set_active_route(provider.clone(), model_id.clone());
        if let Some(route) = self.routes.get_mut(&model_id) {
            route.provider = provider;
            route.note_run_start();
        }
    }

    pub fn note_run_finish(&mut self, provider: impl Into<String>, model_id: impl Into<String>) {
        let provider = provider.into();
        let model_id = model_id.into();
        let route = self
            .routes
            .entry(model_id.clone())
            .or_insert_with(|| ProviderRoutePressure::new(provider.clone(), model_id.clone()));
        route.provider = provider;
        route.note_run_finish();
    }

    pub fn record_response(
        &mut self,
        provider: impl Into<String>,
        model_id: impl Into<String>,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        let provider = provider.into();
        let model_id = model_id.into();
        let route = self
            .routes
            .entry(model_id.clone())
            .or_insert_with(|| ProviderRoutePressure::new(provider.clone(), model_id.clone()));
        route.provider = provider;
        route.record_response(input_tokens, output_tokens);
    }

    pub fn record_rate_limited(
        &mut self,
        provider: impl Into<String>,
        model_id: impl Into<String>,
        retry_after_secs: Option<u64>,
    ) {
        let provider = provider.into();
        let model_id = model_id.into();
        let route = self
            .routes
            .entry(model_id.clone())
            .or_insert_with(|| ProviderRoutePressure::new(provider.clone(), model_id.clone()));
        route.provider = provider;
        route.record_rate_limited(retry_after_secs);
    }

    pub fn active_snapshot(&mut self) -> Option<ProviderPressureSnapshot> {
        let model_id = self.active_model_id.clone()?;
        self.snapshot(&model_id)
    }

    pub fn snapshot(&mut self, model_id: &str) -> Option<ProviderPressureSnapshot> {
        self.routes.get_mut(model_id).map(|route| route.snapshot())
    }

    pub fn all_snapshots(&mut self) -> Vec<ProviderPressureSnapshot> {
        self.routes
            .values_mut()
            .map(|route| route.snapshot())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_refills_over_time() {
        let mut bucket = TokenBucket::new(BucketProfile::new(5.0, 1.0));
        let now = Instant::now();
        bucket.consume(5.0, now);
        assert_eq!(bucket.headroom_pct(now), 0);

        let later = now + Duration::from_secs(2);
        assert_eq!(bucket.seconds_until(1.0, later), Some(0));
        assert!(bucket.headroom_pct(later) >= 40);
    }

    #[test]
    fn rate_limit_forces_backoff_snapshot() {
        let mut tracker = ProviderPressureTracker::new();
        tracker.record_rate_limited("OpenAI", "gpt-5.4", Some(30));
        let snap = tracker.snapshot("gpt-5.4").expect("snapshot");
        assert!(snap.backing_off);
        assert_eq!(snap.fullness_pct, 100);
        assert_eq!(snap.headroom_pct, 0);
        assert_eq!(snap.retry_after_secs, Some(30));
    }
}
