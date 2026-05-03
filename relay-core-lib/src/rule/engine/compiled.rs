use ipnetwork::IpNetwork;
use regex::Regex;
use glob::Pattern;
use crate::rule::Rule;

#[derive(Debug)]
pub struct CompiledRule {
    pub original: Rule,
    pub filter: CompiledFilter,
}

#[derive(Debug, Clone)]
pub enum CompiledFilter {
    All,
    SrcIp(IpNetwork),
    DstPort(u16),
    Protocol(String),
    TransparentMode(bool),
    Url(CompiledStringMatcher),
    Host(CompiledStringMatcher),
    Path(CompiledStringMatcher),
    Method(CompiledStringMatcher),
    RequestHeader { name: String, value: Option<CompiledStringMatcher> },
    ResponseHeader { name: String, value: Option<CompiledStringMatcher> },
    StatusCode(u16),
    ResponseBody(CompiledStringMatcher),
    WebSocketMessage(CompiledStringMatcher),
    And(Vec<CompiledFilter>),
    Or(Vec<CompiledFilter>),
    Not(Box<CompiledFilter>),
    Invalid, // Fallback for compilation errors
}

#[derive(Debug, Clone)]
pub enum CompiledStringMatcher {
    Exact(String),
    Contains(String),
    Prefix(String),
    Suffix(String),
    Regex(Regex),
    Glob(Pattern),
    Invalid,
}
