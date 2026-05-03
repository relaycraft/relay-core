use std::sync::atomic::{AtomicUsize, Ordering};

/// Total number of flows dropped due to backpressure (channel full)
pub static FLOWS_DROPPED_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// Increment the dropped flows counter
pub fn inc_flows_dropped() {
    FLOWS_DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Get the current count of dropped flows
pub fn get_flows_dropped() -> usize {
    FLOWS_DROPPED_TOTAL.load(Ordering::Relaxed)
}
