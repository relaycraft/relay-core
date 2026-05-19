use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 流量搜索条件，所有字段可选，多条件为 AND 关系。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowQuery {
    /// 主机名过滤（子串匹配）
    pub host: Option<String>,
    /// 路径过滤（子串匹配）
    pub path_contains: Option<String>,
    /// HTTP 方法过滤（大写，如 "GET"、"POST"）
    pub method: Option<String>,
    /// 状态码下限（含）
    pub status_min: Option<u16>,
    /// 状态码上限（含）
    pub status_max: Option<u16>,
    /// 仅返回含错误的流量
    pub has_error: Option<bool>,
    /// 仅返回 WebSocket 流量
    pub is_websocket: Option<bool>,
    /// 返回条数上限，默认 50
    pub limit: Option<usize>,
    /// 结果偏移量，默认 0
    pub offset: Option<usize>,
}

/// Flow 的轻量摘要，用于列表展示和 AI 快速分析。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowSummary {
    pub id: String,
    pub method: String,
    pub url: String,
    pub host: String,
    pub path: String,
    pub status: Option<u16>,
    pub duration_ms: Option<u64>,
    pub tags: Vec<String>,
    pub start_time_ms: i64,
    pub has_error: bool,
    pub is_websocket: bool,
}

/// 对截获中的 Flow 的修改意图。
/// 由各适配层（Tauri、MCP 等）在 resolve_intercept 时传入，
/// 描述用户希望如何改变请求/响应/WebSocket 消息。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowModification {
    // 请求字段
    pub method: Option<String>,
    pub url: Option<String>,
    pub request_headers: Option<HashMap<String, String>>,
    pub request_body: Option<String>,

    // 响应字段
    pub status_code: Option<u16>,
    pub response_headers: Option<HashMap<String, String>>,
    pub response_body: Option<String>,

    // WebSocket 字段
    pub message_content: Option<String>,
}

impl FlowModification {
    pub fn is_empty(&self) -> bool {
        self.method.is_none()
            && self.url.is_none()
            && self.request_headers.is_none()
            && self.request_body.is_none()
            && self.status_code.is_none()
            && self.response_headers.is_none()
            && self.response_body.is_none()
            && self.message_content.is_none()
    }

    pub fn into_option(self) -> Option<Self> {
        if self.is_empty() { None } else { Some(self) }
    }

    pub fn from_json_value(value: &Value) -> Self {
        Self {
            method: value
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_string),
            url: value.get("url").and_then(Value::as_str).map(str::to_string),
            request_headers: string_map_from_json(value.get("request_headers")),
            request_body: value
                .get("request_body")
                .and_then(Value::as_str)
                .map(str::to_string),
            status_code: value
                .get("status_code")
                .and_then(Value::as_u64)
                .map(|code| code as u16),
            response_headers: string_map_from_json(value.get("response_headers")),
            response_body: value
                .get("response_body")
                .and_then(Value::as_str)
                .map(str::to_string),
            message_content: value
                .get("message_content")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

fn string_map_from_json(value: Option<&Value>) -> Option<HashMap<String, String>> {
    value?.as_object().map(|entries| {
        entries
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::FlowModification;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn flow_modification_into_option_returns_none_when_empty() {
        assert!(FlowModification::default().into_option().is_none());
    }

    #[test]
    fn flow_modification_into_option_preserves_non_empty_payload() {
        let modification = FlowModification {
            request_body: Some("patched".to_string()),
            ..Default::default()
        };

        assert_eq!(
            modification.clone().into_option().unwrap().request_body,
            modification.request_body
        );
    }

    #[test]
    fn flow_modification_from_json_value_reads_supported_fields() {
        let modification = FlowModification::from_json_value(&json!({
            "method": "PATCH",
            "url": "http://example.com/new",
            "request_headers": {
                "X-Test": "1",
                "X-Ignore": 2
            },
            "response_headers": {
                "Content-Type": "application/json"
            },
            "request_body": "body",
            "status_code": 202,
            "response_body": "ok",
            "message_content": "ws"
        }));

        assert_eq!(modification.method.as_deref(), Some("PATCH"));
        assert_eq!(modification.url.as_deref(), Some("http://example.com/new"));
        assert_eq!(
            modification.request_headers,
            Some(HashMap::from([("X-Test".to_string(), "1".to_string())]))
        );
        assert_eq!(
            modification.response_headers,
            Some(HashMap::from([(
                "Content-Type".to_string(),
                "application/json".to_string()
            )]))
        );
        assert_eq!(modification.request_body.as_deref(), Some("body"));
        assert_eq!(modification.status_code, Some(202));
        assert_eq!(modification.response_body.as_deref(), Some("ok"));
        assert_eq!(modification.message_content.as_deref(), Some("ws"));
    }
}
