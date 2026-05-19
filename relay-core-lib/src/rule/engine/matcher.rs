use crate::rule::engine::compiled::{CompiledFilter, CompiledStringMatcher};
use relay_core_api::flow::{Flow, Layer};
use std::net::IpAddr;
use std::str::FromStr;

pub fn matches(filter: &CompiledFilter, flow: &Flow) -> bool {
    match filter {
        CompiledFilter::All => true,
        CompiledFilter::SrcIp(net) => {
            if let Ok(ip) = IpAddr::from_str(&flow.network.client_ip) {
                net.contains(ip)
            } else {
                false
            }
        }
        CompiledFilter::DstPort(port) => flow.network.server_port == *port,
        CompiledFilter::Protocol(proto) => {
            let p = format!("{:?}", flow.network.protocol);
            p.eq_ignore_ascii_case(proto)
        }
        CompiledFilter::TransparentMode(enabled) => {
            flow.tags.contains(&"transparent".to_string()) == *enabled
        }
        CompiledFilter::Url(matcher) => match &flow.layer {
            Layer::Http(http) => match_string(matcher, http.request.url.as_str()),
            Layer::WebSocket(ws) => match_string(matcher, ws.handshake_request.url.as_str()),
            _ => false,
        },
        CompiledFilter::Host(matcher) => match &flow.layer {
            Layer::Http(http) => http
                .request
                .url
                .host_str()
                .map(|h| match_string(matcher, h))
                .unwrap_or(false),
            Layer::WebSocket(ws) => ws
                .handshake_request
                .url
                .host_str()
                .map(|h| match_string(matcher, h))
                .unwrap_or(false),
            _ => false,
        },
        CompiledFilter::Path(matcher) => match &flow.layer {
            Layer::Http(http) => match_string(matcher, http.request.url.path()),
            Layer::WebSocket(ws) => match_string(matcher, ws.handshake_request.url.path()),
            _ => false,
        },
        CompiledFilter::Method(matcher) => match &flow.layer {
            Layer::Http(http) => match_string(matcher, &http.request.method),
            Layer::WebSocket(ws) => match_string(matcher, &ws.handshake_request.method),
            _ => false,
        },
        CompiledFilter::RequestHeader { name, value } => match &flow.layer {
            Layer::Http(http) => match_header(&http.request.headers, name, value),
            Layer::WebSocket(ws) => match_header(&ws.handshake_request.headers, name, value),
            _ => false,
        },
        CompiledFilter::ResponseHeader { name, value } => match &flow.layer {
            Layer::Http(http) => {
                if let Some(res) = &http.response {
                    match_header(&res.headers, name, value)
                } else {
                    false
                }
            }
            Layer::WebSocket(ws) => match_header(&ws.handshake_response.headers, name, value),
            _ => false,
        },
        CompiledFilter::StatusCode(code) => match &flow.layer {
            Layer::Http(http) => {
                if let Some(res) = &http.response {
                    res.status == *code
                } else {
                    false
                }
            }
            Layer::WebSocket(ws) => ws.handshake_response.status == *code,
            _ => false,
        },
        CompiledFilter::ResponseBody(matcher) => {
            if let Layer::Http(http) = &flow.layer {
                if let Some(res) = &http.response {
                    if let Some(body) = &res.body {
                        match_string(matcher, &body.content)
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        }
        CompiledFilter::WebSocketMessage(matcher) => {
            if let Layer::WebSocket(ws) = &flow.layer {
                if let Some(msg) = ws.messages.last() {
                    match_string(matcher, &msg.content.content)
                } else {
                    false
                }
            } else {
                false
            }
        }
        CompiledFilter::And(filters) => filters.iter().all(|f| matches(f, flow)),
        CompiledFilter::Or(filters) => filters.iter().any(|f| matches(f, flow)),
        CompiledFilter::Not(filter) => !matches(filter, flow),
        CompiledFilter::Invalid => false,
    }
}

pub fn match_string(matcher: &CompiledStringMatcher, target: &str) -> bool {
    match matcher {
        CompiledStringMatcher::Exact(s) => target == s,
        CompiledStringMatcher::Contains(s) => target.contains(s),
        CompiledStringMatcher::Prefix(s) => target.starts_with(s),
        CompiledStringMatcher::Suffix(s) => target.ends_with(s),
        CompiledStringMatcher::Regex(re) => re.is_match(target),
        CompiledStringMatcher::Glob(pattern) => pattern.matches(target),
        CompiledStringMatcher::Invalid => false,
    }
}

pub fn match_header(
    headers: &[(String, String)],
    name: &str,
    value_matcher: &Option<CompiledStringMatcher>,
) -> bool {
    for (k, v) in headers {
        if k.eq_ignore_ascii_case(name) {
            if let Some(matcher) = value_matcher {
                if match_string(matcher, v) {
                    return true;
                }
            } else {
                return true;
            }
        }
    }
    false
}
