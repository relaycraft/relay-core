use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Per-upstream circuit breaker state.
#[derive(Debug, Clone, Default)]
struct HostState {
    failure_count: u32,
    last_failure: Option<Instant>,
    circuit_open_until: Option<Instant>,
}

/// Simple circuit breaker that tracks per-host failure counts.
/// After `failure_threshold` consecutive failures, the circuit
/// opens for `backoff_duration`. During the open state, requests
/// are rejected immediately. After the backoff expires, one trial
/// request is allowed (half-open); if it succeeds, the circuit closes.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    hosts: Arc<RwLock<HashMap<String, HostState>>>,
    failure_threshold: u32,
    backoff_duration: Duration,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, backoff_duration: Duration) -> Self {
        Self {
            hosts: Arc::new(RwLock::new(HashMap::new())),
            failure_threshold,
            backoff_duration,
        }
    }

    /// Check whether a request to `host` is allowed through the circuit.
    /// Returns `true` if the request should proceed.
    pub async fn allow_request(&self, host: &str) -> bool {
        let hosts = self.hosts.read().await;
        if let Some(state) = hosts.get(host)
            && let Some(open_until) = state.circuit_open_until
            && Instant::now() < open_until
        {
            return false; // Circuit is open
        }
        true
    }

    /// Record a successful request to `host`. Resets the circuit.
    pub async fn record_success(&self, host: &str) {
        let mut hosts = self.hosts.write().await;
        hosts.remove(host);
    }

    /// Record a failed request to `host`. Increments failure count
    /// and may open the circuit if the threshold is reached.
    pub async fn record_failure(&self, host: &str) {
        let mut hosts = self.hosts.write().await;
        let state = hosts.entry(host.to_string()).or_default();
        let now = Instant::now();

        // Reset if failures are old (> 2 * backoff)
        if let Some(last) = state.last_failure
            && now.duration_since(last) > self.backoff_duration * 2
        {
            state.failure_count = 0;
        }

        state.failure_count += 1;
        state.last_failure = Some(now);

        if state.failure_count >= self.failure_threshold {
            state.circuit_open_until = Some(now + self.backoff_duration);
            tracing::warn!(
                "Circuit breaker OPEN for host={} ({} failures in window), backoff {}s",
                host,
                state.failure_count,
                self.backoff_duration.as_secs()
            );
        }
    }

    /// Returns whether the circuit is currently open for `host`.
    pub async fn is_open(&self, host: &str) -> bool {
        !self.allow_request(host).await
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(3, Duration::from_secs(30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_circuit_breaker_opens_after_failures() {
        let cb = CircuitBreaker::new(2, Duration::from_millis(100));
        let host = "api.example.com:443";

        assert!(cb.allow_request(host).await);

        cb.record_failure(host).await;
        assert!(cb.allow_request(host).await);

        cb.record_failure(host).await;
        // Circuit should be open now (2 failures = threshold)
        assert!(!cb.allow_request(host).await);

        // Wait for backoff to expire
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(cb.allow_request(host).await); // Half-open
    }

    #[tokio::test]
    async fn test_circuit_breaker_resets_on_success() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(30));
        let host = "healthy.example.com:443";

        cb.record_failure(host).await;
        cb.record_failure(host).await;
        cb.record_success(host).await;

        // Should be back to 0 failures
        assert!(cb.allow_request(host).await);
        assert!(!cb.is_open(host).await);
    }
}
