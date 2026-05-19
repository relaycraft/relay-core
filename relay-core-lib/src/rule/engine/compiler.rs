use crate::rule::engine::compiled::{CompiledFilter, CompiledRule, CompiledStringMatcher};
use crate::rule::model::{Filter, Rule, StringMatcher};
use glob::Pattern;
use ipnetwork::IpNetwork;
use regex::Regex;
use std::str::FromStr;

pub fn compile_rule(rule: Rule) -> CompiledRule {
    let filter = compile_filter(&rule.filter);
    CompiledRule {
        original: rule,
        filter,
    }
}

pub fn compile_filter(filter: &Filter) -> CompiledFilter {
    match filter {
        Filter::All => CompiledFilter::All,
        Filter::SrcIp(cidr) => IpNetwork::from_str(cidr)
            .map(CompiledFilter::SrcIp)
            .unwrap_or(CompiledFilter::Invalid),
        Filter::DstPort(p) => CompiledFilter::DstPort(*p),
        Filter::Protocol(p) => CompiledFilter::Protocol(p.clone()),
        Filter::TransparentMode(enabled) => CompiledFilter::TransparentMode(*enabled),
        Filter::Url(m) => CompiledFilter::Url(compile_matcher(m)),
        Filter::Host(m) => CompiledFilter::Host(compile_matcher(m)),
        Filter::Path(m) => CompiledFilter::Path(compile_matcher(m)),
        Filter::Method(m) => CompiledFilter::Method(compile_matcher(m)),
        Filter::RequestHeader { name, value } => CompiledFilter::RequestHeader {
            name: name.clone(),
            value: value.as_ref().map(compile_matcher),
        },
        Filter::ResponseHeader { name, value } => CompiledFilter::ResponseHeader {
            name: name.clone(),
            value: value.as_ref().map(compile_matcher),
        },
        Filter::StatusCode(c) => CompiledFilter::StatusCode(*c),
        Filter::ResponseBody(m) => CompiledFilter::ResponseBody(compile_matcher(m)),
        Filter::WebSocketMessage(m) => CompiledFilter::WebSocketMessage(compile_matcher(m)),
        Filter::And(filters) => CompiledFilter::And(filters.iter().map(compile_filter).collect()),
        Filter::Or(filters) => CompiledFilter::Or(filters.iter().map(compile_filter).collect()),
        Filter::Not(f) => CompiledFilter::Not(Box::new(compile_filter(f))),
    }
}

pub fn compile_matcher(matcher: &StringMatcher) -> CompiledStringMatcher {
    match matcher {
        StringMatcher::Exact(s) => CompiledStringMatcher::Exact(s.clone()),
        StringMatcher::Contains(s) => CompiledStringMatcher::Contains(s.clone()),
        StringMatcher::Prefix(s) => CompiledStringMatcher::Prefix(s.clone()),
        StringMatcher::Suffix(s) => CompiledStringMatcher::Suffix(s.clone()),
        StringMatcher::Regex(s) => Regex::new(s)
            .map(CompiledStringMatcher::Regex)
            .unwrap_or(CompiledStringMatcher::Invalid),
        StringMatcher::Glob(s) => Pattern::new(s)
            .map(CompiledStringMatcher::Glob)
            .unwrap_or(CompiledStringMatcher::Invalid),
    }
}
