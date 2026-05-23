use std::sync::atomic::{AtomicUsize, Ordering};

/// Total number of flows dropped due to backpressure (channel full)
pub static FLOWS_DROPPED_TOTAL: AtomicUsize = AtomicUsize::new(0);

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
