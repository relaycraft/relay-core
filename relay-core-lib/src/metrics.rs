use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Total number of flows dropped due to backpressure (channel full)
pub static FLOWS_DROPPED_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// Total bytes sent to upstream servers across all connections
pub static PROXY_BYTES_SENT_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Total bytes received from upstream servers across all connections
pub static PROXY_BYTES_RECV_TOTAL: AtomicU64 = AtomicU64::new(0);

/// O4: Total number of bodies degraded (budget exceeded, rules skipped)
pub static PROXY_BODY_DEGRADED_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of HTTP requests processed through the streaming pipeline
pub static PROXY_HTTP_REQUEST_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of sandbox rejections
pub static PROXY_SANDBOX_REJECT_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of invalid method rejections (strict_http_semantics)
pub static PROXY_INVALID_METHOD_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of invalid status code rejections (strict_http_semantics)
pub static PROXY_INVALID_STATUS_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of idempotent request retries
pub static PROXY_RETRY_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of bodies processed in tap (streaming) mode
pub static PROXY_STREAM_MODE_TAP_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of bodies degraded from tap to pass-through
pub static PROXY_STREAM_MODE_DEGRADE_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// Increment the dropped flows counter
pub fn inc_flows_dropped() {
    FLOWS_DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Get the current count of dropped flows
pub fn get_flows_dropped() -> usize {
    FLOWS_DROPPED_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the body degraded counter
pub fn inc_proxy_body_degraded() {
    PROXY_BODY_DEGRADED_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of degraded bodies
pub fn get_proxy_body_degraded() -> usize {
    PROXY_BODY_DEGRADED_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the HTTP request counter
pub fn inc_proxy_http_request() {
    PROXY_HTTP_REQUEST_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of HTTP requests
pub fn get_proxy_http_request() -> usize {
    PROXY_HTTP_REQUEST_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the sandbox reject counter
pub fn inc_proxy_sandbox_reject() {
    PROXY_SANDBOX_REJECT_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of sandbox rejections
pub fn get_proxy_sandbox_reject() -> usize {
    PROXY_SANDBOX_REJECT_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the invalid method counter
pub fn inc_proxy_invalid_method() {
    PROXY_INVALID_METHOD_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of invalid method rejections
pub fn get_proxy_invalid_method() -> usize {
    PROXY_INVALID_METHOD_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the invalid status counter
pub fn inc_proxy_invalid_status() {
    PROXY_INVALID_STATUS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of invalid status rejections
pub fn get_proxy_invalid_status() -> usize {
    PROXY_INVALID_STATUS_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the retry counter
pub fn inc_proxy_retry() {
    PROXY_RETRY_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of retries
pub fn get_proxy_retry() -> usize {
    PROXY_RETRY_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the stream mode tap counter
pub fn inc_proxy_stream_mode_tap() {
    PROXY_STREAM_MODE_TAP_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of tap-mode streams
pub fn get_proxy_stream_mode_tap() -> usize {
    PROXY_STREAM_MODE_TAP_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the stream mode degrade counter
pub fn inc_proxy_stream_mode_degrade() {
    PROXY_STREAM_MODE_DEGRADE_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of degraded streams
pub fn get_proxy_stream_mode_degrade() -> usize {
    PROXY_STREAM_MODE_DEGRADE_TOTAL.load(Ordering::Relaxed)
}

/// Add bytes sent (client→proxy→upstream direction)
pub fn add_bytes_sent(n: u64) {
    PROXY_BYTES_SENT_TOTAL.fetch_add(n, Ordering::Relaxed);
}

/// Get total bytes sent
pub fn get_bytes_sent() -> u64 {
    PROXY_BYTES_SENT_TOTAL.load(Ordering::Relaxed)
}

/// Add bytes received (upstream→proxy→client direction)
pub fn add_bytes_recv(n: u64) {
    PROXY_BYTES_RECV_TOTAL.fetch_add(n, Ordering::Relaxed);
}

/// Get total bytes received
pub fn get_bytes_recv() -> u64 {
    PROXY_BYTES_RECV_TOTAL.load(Ordering::Relaxed)
}

/// Per-connection byte counter for deriving per-connection stats.
/// Snapshots global counters at construction; delta at disconnect.
#[derive(Debug, Clone)]
pub struct ConnectionMeter {
    bytes_sent_at_start: u64,
    bytes_recv_at_start: u64,
}

impl ConnectionMeter {
    pub fn new() -> Self {
        Self {
            bytes_sent_at_start: get_bytes_sent(),
            bytes_recv_at_start: get_bytes_recv(),
        }
    }

    pub fn snapshot_bytes_sent(&self) -> u64 {
        get_bytes_sent().saturating_sub(self.bytes_sent_at_start)
    }

    pub fn snapshot_bytes_recv(&self) -> u64 {
        get_bytes_recv().saturating_sub(self.bytes_recv_at_start)
    }
}

impl Default for ConnectionMeter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bytes_sent_counting() {
        let before = get_bytes_sent();
        add_bytes_sent(100);
        assert!(get_bytes_sent() >= before + 100);
    }

    #[test]
    fn test_bytes_recv_counting() {
        let before = get_bytes_recv();
        add_bytes_recv(200);
        assert!(get_bytes_recv() >= before + 200);
    }

    #[test]
    fn test_connection_meter_delta() {
        let before_sent = get_bytes_sent();
        let before_recv = get_bytes_recv();
        let meter = ConnectionMeter::new();
        add_bytes_sent(300);
        add_bytes_recv(400);
        assert!(meter.snapshot_bytes_sent() >= 300);
        assert!(meter.snapshot_bytes_recv() >= 400);
        // reset approximate range for other tests
        let _ = (before_sent, before_recv);
    }

    #[test]
    fn test_connection_meter_independent_snapshots() {
        let m1 = ConnectionMeter::new();
        let base_sent = m1.bytes_sent_at_start;
        // Add some bytes to advance the global counter
        add_bytes_sent(100);
        let m2 = ConnectionMeter::new();
        add_bytes_sent(200);
        assert!(m1.snapshot_bytes_sent() >= 300);
        assert!(m2.snapshot_bytes_sent() >= 200);
        // m2 should see fewer bytes than m1 (m2 was created after m1)
        assert!(m2.snapshot_bytes_sent() <= m1.snapshot_bytes_sent());
        // Restore approximate cleanliness
        let _ = base_sent;
    }
}
