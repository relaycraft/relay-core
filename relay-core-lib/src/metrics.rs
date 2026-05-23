use std::sync::atomic::{AtomicUsize, Ordering};

/// Total number of flows dropped due to backpressure (channel full)
pub static FLOWS_DROPPED_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of bodies degraded (budget exceeded, rules skipped)
pub static PROXY_BODY_DEGRADED_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// O4: Total number of HTTP requests processed through the streaming pipeline
pub static PROXY_HTTP_REQUEST_TOTAL: AtomicUsize = AtomicUsize::new(0);

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
