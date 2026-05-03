use crate::rule::model::{Action, RuleStage};
use crate::rule::engine::compiled::CompiledFilter;

pub fn validate_filter_stage(filter: &CompiledFilter, stage: &RuleStage) -> bool {
    match filter {
        CompiledFilter::All => true,
        CompiledFilter::SrcIp(_) | CompiledFilter::DstPort(_) | CompiledFilter::Protocol(_) | CompiledFilter::TransparentMode(_) => {
            true // L3/L4 info available in all stages
        }
        CompiledFilter::Url(_) | CompiledFilter::Host(_) | CompiledFilter::Path(_) | CompiledFilter::Method(_) | CompiledFilter::RequestHeader { .. } => {
            !matches!(stage, RuleStage::Connect)
        }
        CompiledFilter::ResponseHeader { .. } | CompiledFilter::StatusCode(_) => {
            !matches!(stage, RuleStage::Connect | RuleStage::RequestHeaders | RuleStage::RequestBody)
        }
        CompiledFilter::ResponseBody(_) => {
            matches!(stage, RuleStage::ResponseBody)
        }
        CompiledFilter::WebSocketMessage(_) => {
            matches!(stage, RuleStage::WebSocketMessage)
        }
        CompiledFilter::And(filters) | CompiledFilter::Or(filters) => {
            filters.iter().all(|f| validate_filter_stage(f, stage))
        }
        CompiledFilter::Not(f) => validate_filter_stage(f, stage),
        CompiledFilter::Invalid => false,
    }
}

pub fn validate_action_stage(action: &Action, stage: &RuleStage) -> bool {
    match stage {
        RuleStage::Connect => matches!(action, Action::Drop | Action::Abort | Action::Delay { .. } | Action::Throttle { .. } |
            Action::Tag { .. } | Action::SetVariable { .. } | Action::Inspect | Action::RateLimit { .. } |
            Action::RedirectIp { .. } | Action::SetTtl { .. } | Action::ForwardPort { .. }),
        RuleStage::RequestHeaders => matches!(action, Action::Drop | Action::Abort | Action::Delay { .. } | Action::Throttle { .. } |
            Action::Tag { .. } | Action::SetVariable { .. } | Action::Inspect | Action::RateLimit { .. } |
            Action::MockResponse { .. } | Action::MapLocal { .. } | Action::MapRemote { .. } |
            Action::Redirect { .. } |
            Action::AddRequestHeader { .. } | Action::UpdateRequestHeader { .. } | Action::DeleteRequestHeader { .. } |
            Action::SetRequestMethod { .. } | Action::SetRequestUrl { .. } | 
            Action::SetRequestBody { .. }),
        RuleStage::RequestBody => matches!(action, Action::Drop | Action::Abort | Action::Delay { .. } | Action::Throttle { .. } |
            Action::Tag { .. } | Action::SetVariable { .. } | Action::Inspect | Action::RateLimit { .. } |
            Action::SetRequestBody { .. } | Action::TransformRequestBody { .. } |
            // MockResponse in body stage implies aborting upstream and sending response
            Action::MockResponse { .. }),
        RuleStage::ResponseHeaders => matches!(action, Action::Drop | Action::Abort | Action::Delay { .. } | Action::Throttle { .. } |
            Action::Tag { .. } | Action::SetVariable { .. } | Action::Inspect | Action::RateLimit { .. } |
            Action::AddResponseHeader { .. } | Action::UpdateResponseHeader { .. } | Action::DeleteResponseHeader { .. } |
            Action::SetResponseStatus { .. } | Action::SetResponseBody { .. }),
        RuleStage::ResponseBody => matches!(action, Action::Drop | Action::Abort | Action::Delay { .. } | Action::Throttle { .. } |
            Action::Tag { .. } | Action::SetVariable { .. } | Action::Inspect | Action::RateLimit { .. } |
            Action::SetResponseBody { .. } | Action::TransformResponseBody { .. }),
            RuleStage::WebSocketMessage => matches!(action, Action::Drop | Action::Abort | Action::Delay { .. } | Action::Throttle { .. } |
            Action::Tag { .. } | Action::SetVariable { .. } | Action::Inspect | Action::RateLimit { .. } |
            Action::MockWebSocketMessage { .. } | Action::DropWebSocketMessage),
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_action_stage, validate_filter_stage};
    use crate::rule::engine::compiled::{CompiledFilter, CompiledStringMatcher};
    use crate::rule::model::{Action, BodySource, RuleStage, WebSocketDirection};

    #[test]
    fn test_validate_filter_stage_response_filters() {
        let f = CompiledFilter::ResponseHeader {
            name: "Content-Type".to_string(),
            value: Some(CompiledStringMatcher::Contains("json".to_string())),
        };
        assert!(!validate_filter_stage(&f, &RuleStage::RequestHeaders));
        assert!(!validate_filter_stage(&f, &RuleStage::RequestBody));
        assert!(validate_filter_stage(&f, &RuleStage::ResponseHeaders));
        assert!(validate_filter_stage(&f, &RuleStage::ResponseBody));
    }

    #[test]
    fn test_validate_filter_stage_response_body_only() {
        let f = CompiledFilter::ResponseBody(CompiledStringMatcher::Contains("err".to_string()));
        assert!(!validate_filter_stage(&f, &RuleStage::ResponseHeaders));
        assert!(validate_filter_stage(&f, &RuleStage::ResponseBody));
        assert!(!validate_filter_stage(&f, &RuleStage::WebSocketMessage));
    }

    #[test]
    fn test_validate_filter_stage_websocket_only() {
        let f = CompiledFilter::WebSocketMessage(CompiledStringMatcher::Contains("ping".to_string()));
        assert!(validate_filter_stage(&f, &RuleStage::WebSocketMessage));
        assert!(!validate_filter_stage(&f, &RuleStage::RequestHeaders));
        assert!(!validate_filter_stage(&f, &RuleStage::ResponseBody));
    }

    #[test]
    fn test_validate_filter_stage_composite_requires_all_members_valid() {
        let valid_ws = CompiledFilter::WebSocketMessage(CompiledStringMatcher::Contains("x".to_string()));
        let invalid_in_ws = CompiledFilter::ResponseBody(CompiledStringMatcher::Contains("y".to_string()));
        let and_filter = CompiledFilter::And(vec![valid_ws.clone(), invalid_in_ws.clone()]);
        let or_filter = CompiledFilter::Or(vec![valid_ws.clone(), invalid_in_ws]);
        let not_filter = CompiledFilter::Not(Box::new(valid_ws));

        assert!(
            !validate_filter_stage(&and_filter, &RuleStage::WebSocketMessage),
            "AND should fail when one child invalid for stage"
        );
        assert!(
            !validate_filter_stage(&or_filter, &RuleStage::WebSocketMessage),
            "OR currently enforces all children stage-valid"
        );
        assert!(validate_filter_stage(&not_filter, &RuleStage::WebSocketMessage));
    }

    #[test]
    fn test_validate_action_stage_representative_matrix() {
        let set_body = Action::SetRequestBody {
            body: BodySource::Text("x".to_string()),
        };
        let set_status = Action::SetResponseStatus { status: 418 };
        let mock_ws = Action::MockWebSocketMessage {
            direction: WebSocketDirection::Incoming,
            message: "pong".to_string(),
        };

        assert!(validate_action_stage(&set_body, &RuleStage::RequestHeaders));
        assert!(validate_action_stage(&set_body, &RuleStage::RequestBody));
        assert!(!validate_action_stage(&set_body, &RuleStage::Connect));
        assert!(!validate_action_stage(&set_body, &RuleStage::ResponseHeaders));

        assert!(validate_action_stage(&set_status, &RuleStage::ResponseHeaders));
        assert!(!validate_action_stage(&set_status, &RuleStage::RequestHeaders));

        assert!(validate_action_stage(&mock_ws, &RuleStage::WebSocketMessage));
        assert!(!validate_action_stage(&mock_ws, &RuleStage::RequestHeaders));
    }
}
