use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

#[async_trait]
pub trait RuleStateStore: Send + Sync + std::fmt::Debug {
    /// Increment a counter for the given key.
    /// Returns the new value.
    /// The window parameter suggests a time window for rate limiting,
    /// but in this simple interface it might just set the TTL for the key if it's new.
    async fn increment_counter(&self, key: &str, window: Duration) -> u64;

    async fn get_variable(&self, key: &str) -> Option<String>;

    async fn set_variable(&self, key: &str, value: String, ttl: Option<Duration>);
}

#[derive(Clone, Debug)]
pub struct InMemoryRuleStateStore {
    variables: Arc<Mutex<HashMap<String, VariableState>>>,
    counters: Arc<Mutex<HashMap<String, CounterState>>>,
}

#[derive(Clone, Debug)]
struct CounterState {
    count: u64,
    expires_at: Instant,
}

#[derive(Clone, Debug)]
struct VariableState {
    value: String,
    expires_at: Option<Instant>,
}

impl Default for InMemoryRuleStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRuleStateStore {
    pub fn new() -> Self {
        Self {
            variables: Arc::new(Mutex::new(HashMap::new())),
            counters: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl RuleStateStore for InMemoryRuleStateStore {
    async fn increment_counter(&self, key: &str, window: Duration) -> u64 {
        let now = Instant::now();
        let mut counters = self.counters.lock().await;
        let window = if window.is_zero() {
            Duration::from_millis(1)
        } else {
            window
        };

        let entry = counters
            .entry(key.to_string())
            .or_insert_with(|| CounterState {
                count: 0,
                expires_at: now + window,
            });

        if now >= entry.expires_at {
            entry.count = 0;
            entry.expires_at = now + window;
        }

        entry.count = entry.count.saturating_add(1);
        entry.count
    }

    async fn get_variable(&self, key: &str) -> Option<String> {
        let mut variables = self.variables.lock().await;
        if let Some(v) = variables.get(key) {
            if let Some(exp) = v.expires_at
                && Instant::now() >= exp
            {
                variables.remove(key);
                return None;
            }
            return Some(v.value.clone());
        }
        None
    }

    async fn set_variable(&self, key: &str, value: String, ttl: Option<Duration>) {
        let expires_at = ttl.and_then(|d| {
            if d.is_zero() {
                None
            } else {
                Some(Instant::now() + d)
            }
        });
        let mut variables = self.variables.lock().await;
        variables.insert(key.to_string(), VariableState { value, expires_at });
    }
}

#[cfg(test)]
mod tests {
    use super::{InMemoryRuleStateStore, RuleStateStore};
    use std::time::Duration;

    #[tokio::test]
    async fn test_increment_counter_respects_window() {
        let store = InMemoryRuleStateStore::new();
        let window = Duration::from_millis(30);
        let key = "rate:k1";

        let c1 = store.increment_counter(key, window).await;
        let c2 = store.increment_counter(key, window).await;
        assert_eq!(c1, 1);
        assert_eq!(c2, 2);

        tokio::time::sleep(Duration::from_millis(40)).await;
        let c3 = store.increment_counter(key, window).await;
        assert_eq!(c3, 1, "counter should reset after window expires");
    }

    #[tokio::test]
    async fn test_increment_counter_isolated_by_key() {
        let store = InMemoryRuleStateStore::new();
        let window = Duration::from_millis(100);

        let a1 = store.increment_counter("a", window).await;
        let b1 = store.increment_counter("b", window).await;
        let a2 = store.increment_counter("a", window).await;

        assert_eq!(a1, 1);
        assert_eq!(b1, 1);
        assert_eq!(a2, 2);
    }

    #[tokio::test]
    async fn test_variable_ttl_expires() {
        let store = InMemoryRuleStateStore::new();
        store
            .set_variable("k1", "v1".to_string(), Some(Duration::from_millis(30)))
            .await;
        assert_eq!(store.get_variable("k1").await.as_deref(), Some("v1"));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(store.get_variable("k1").await, None);
    }

    #[tokio::test]
    async fn test_variable_without_ttl_persists() {
        let store = InMemoryRuleStateStore::new();
        store.set_variable("k2", "v2".to_string(), None).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(store.get_variable("k2").await.as_deref(), Some("v2"));
    }

    #[tokio::test]
    async fn test_variable_zero_ttl_treated_as_no_expiry() {
        let store = InMemoryRuleStateStore::new();
        store
            .set_variable("k3", "v3".to_string(), Some(Duration::ZERO))
            .await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(store.get_variable("k3").await.as_deref(), Some("v3"));
    }

    #[tokio::test]
    async fn test_variable_overwrite_resets_expiry_policy() {
        let store = InMemoryRuleStateStore::new();
        store
            .set_variable("k4", "short".to_string(), Some(Duration::from_millis(20)))
            .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        store.set_variable("k4", "stable".to_string(), None).await;

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(store.get_variable("k4").await.as_deref(), Some("stable"));
    }
}
