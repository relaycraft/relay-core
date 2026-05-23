use std::sync::atomic::{AtomicUsize, Ordering};

/// Total number of flows dropped due to backpressure (channel full)
pub static FLOWS_DROPPED_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of bodies degraded (budget exceeded, rules skipped)
pub static PROXY_BODY_DEGRADED_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of requests processed in streaming mode
pub static PROXY_STREAM_MODE_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of invalid HTTP methods rejected
pub static PROXY_INVALID_METHOD_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of invalid HTTP status codes seen
pub static PROXY_INVALID_STATUS_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of proxy retries
pub static PROXY_RETRY_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of sandbox rejections
pub static PROXY_SANDBOX_REJECT_TOTAL: AtomicUsize = AtomicUsize::new(0);

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

/// O4: Increment the stream mode counter
pub fn inc_proxy_stream_mode() {
    PROXY_STREAM_MODE_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of stream-mode requests
pub fn get_proxy_stream_mode() -> usize {
    PROXY_STREAM_MODE_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the invalid method counter
pub fn inc_proxy_invalid_method() {
    PROXY_INVALID_METHOD_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of invalid methods
pub fn get_proxy_invalid_method() -> usize {
    PROXY_INVALID_METHOD_TOTAL.load(Ordering::Relaxed)
}

/// O4: Increment the invalid status counter
pub fn inc_proxy_invalid_status() {
    PROXY_INVALID_STATUS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of invalid statuses
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

/// O4: Increment the sandbox reject counter
pub fn inc_proxy_sandbox_reject() {
    PROXY_SANDBOX_REJECT_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// O4: Get the current count of sandbox rejections
pub fn get_proxy_sandbox_reject() -> usize {
    PROXY_SANDBOX_REJECT_TOTAL.load(Ordering::Relaxed)
}
