//! HAR 1.2 fidelity tests (roadmap §13-6, §24.7).
//!
//! HAR distinguishes "this phase took no time" (`0`) from "this phase was not measured" (`-1`). The
//! exporter used `unwrap_or(0)` for `wait`/`receive` and a fixed `-1` for `connect`/`ssl`/`dns`, so
//! it claimed to have measured phases it never touched and reported real absences as zero durations —
//! exactly the "用固定 0 冒充实际 timing" failure the roadmap's Definition of Done forbids.

use relay_core_api::flow::{
    BodyData, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
    TransportProtocol,
};
use relay_core_api::har::flow_to_har_entry;
use serde_json::json;
use std::collections::HashMap;
use url::Url;
use uuid::Uuid;

fn flow_with_timing(timing: ResponseTiming) -> Flow {
    Flow {
        id: Uuid::new_v4(),
        start_time: chrono::Utc::now(),
        end_time: None,
        close_reason: None,
        network: NetworkInfo {
            client_ip: "127.0.0.1".to_string(),
            client_port: 12345,
            server_ip: "93.184.216.34".to_string(),
            server_port: 80,
            protocol: TransportProtocol::TCP,
            tls: false,
            tls_version: None,
            sni: None,
        },
        layer: Layer::Http(HttpLayer {
            request: HttpRequest {
                method: "GET".to_string(),
                url: Url::parse("http://example.com/a").expect("url"),
                version: "HTTP/1.1".to_string(),
                headers: vec![("host".to_string(), "example.com".to_string())],
                cookies: vec![],
                query: vec![],
                body: None,
            },
            response: Some(HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: vec![("content-type".to_string(), "text/plain".to_string())],
                cookies: vec![],
                body: Some(BodyData {
                    encoding: "utf-8".to_string(),
                    content: "hello".to_string(),
                    size: 5,
                }),
                timing,
            }),
            error: None,
        }),
        tags: vec![],
        meta: HashMap::new(),
        resilience_trace: None,
        rule_variables: HashMap::new(),
        matched_rules: vec![],
    }
}

fn timing_with(
    ttfb: Option<u64>,
    ttlb: Option<u64>,
    connect: Option<u64>,
    ssl: Option<u64>,
) -> ResponseTiming {
    ResponseTiming {
        time_to_first_byte: ttfb,
        time_to_last_byte: ttlb,
        connect_time_ms: connect,
        ssl_time_ms: ssl,
    }
}

/// Phases that were measured must report their measurement, including a genuine zero.
#[test]
fn measured_phases_are_reported_as_measured() {
    let flow = flow_with_timing(timing_with(Some(12), Some(30), Some(3), Some(8)));
    let har = flow_to_har_entry(&flow);
    let timings = &har["timings"];

    assert_eq!(timings["wait"], json!(12), "wait is time to first byte");
    assert_eq!(timings["receive"], json!(18), "receive is the remainder");
    assert_eq!(
        timings["connect"],
        json!(3),
        "a measured connect must be reported"
    );
    assert_eq!(
        timings["ssl"],
        json!(8),
        "a measured TLS handshake must be reported"
    );
}

/// A phase nobody measured must be `-1`, not `0`: zero means "instantaneous", which is a claim the
/// exporter cannot support.
#[test]
fn unmeasured_phases_are_minus_one_not_zero() {
    let flow = flow_with_timing(timing_with(None, None, None, None));
    let har = flow_to_har_entry(&flow);
    let timings = &har["timings"];

    for phase in ["wait", "receive", "connect", "ssl", "dns", "blocked"] {
        assert_eq!(
            timings[phase],
            json!(-1),
            "{phase} was not measured, so HAR requires -1 rather than a fabricated measurement"
        );
    }
}

/// The total must not claim a duration when nothing was measured.
#[test]
fn total_time_reflects_only_measured_values() {
    let unmeasured = flow_to_har_entry(&flow_with_timing(timing_with(None, None, None, None)));
    assert_eq!(
        unmeasured["time"],
        json!(-1),
        "an unmeasured exchange must not report a zero total"
    );

    let measured = flow_to_har_entry(&flow_with_timing(timing_with(
        Some(5),
        Some(20),
        None,
        None,
    )));
    assert_eq!(measured["time"], json!(20));
}

/// A measured zero is still a measurement, so `wait` reports `0` when the first byte was immediate
/// while an unmeasured phase stays `-1`.
#[test]
fn a_measured_zero_is_distinguishable_from_an_absence() {
    let flow = flow_with_timing(timing_with(Some(0), Some(0), None, None));
    let timings = &flow_to_har_entry(&flow)["timings"];

    assert_eq!(
        timings["wait"],
        json!(0),
        "an immediate first byte is a real zero"
    );
    assert_eq!(
        timings["connect"],
        json!(-1),
        "connect was never measured and must stay unknown"
    );
}

/// A non-HTTP flow has no HAR timing to report, and must not pretend otherwise.
#[test]
fn a_flow_without_http_reports_no_fabricated_timings() {
    let mut flow = flow_with_timing(timing_with(Some(1), Some(2), None, None));
    flow.layer = Layer::Unknown;

    let har = flow_to_har_entry(&flow);
    assert_eq!(har["timings"], json!({}));
    assert_eq!(har["time"], json!(-1));
}
