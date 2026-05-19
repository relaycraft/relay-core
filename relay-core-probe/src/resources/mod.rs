use crate::server::ProbeContext;
use crate::tools::ToolError;
use relay_core_api::modification::FlowQuery;
use rmcp::model::{AnnotateAble, RawResource, Resource, ResourceContents};
use std::sync::Arc;

/// 返回静态资源列表（非模板化的固定 URI）
pub fn static_resource_list() -> Vec<Resource> {
    vec![
        RawResource::new("flows://", "Recent Flows")
            .with_description("List of recent HTTP/WebSocket flows (latest 50)")
            .with_mime_type("text/markdown")
            .no_annotation(),
        RawResource::new("rules://", "Active Rules")
            .with_description("Currently active interception/modification rules")
            .with_mime_type("application/json")
            .no_annotation(),
        RawResource::new("proxy://status", "Proxy Status")
            .with_description("Proxy health metrics and running status")
            .with_mime_type("application/json")
            .no_annotation(),
        RawResource::new("audit://recent", "Recent Audit Events")
            .with_description("Most recent adapter/runtime audit events")
            .with_mime_type("application/json")
            .no_annotation(),
        RawResource::new("ca://install", "CA Certificate Install Guide")
            .with_description("Platform-specific one-liner commands to trust the RelayCore CA cert for HTTPS interception")
            .with_mime_type("text/markdown")
            .no_annotation(),
    ]
}

/// 根据 URI 路由到对应资源的读取逻辑
pub async fn read_resource(
    ctx: &Arc<ProbeContext>,
    uri: &str,
) -> Result<Vec<ResourceContents>, ToolError> {
    if uri == "flows://" {
        flows_list(ctx).await
    } else if let Some(id) = uri.strip_prefix("flows://") {
        flow_detail(ctx, id).await
    } else if uri == "rules://" {
        rules_list(ctx).await
    } else if uri == "proxy://status" {
        proxy_status(ctx).await
    } else if uri == "audit://recent" {
        recent_audit(ctx).await
    } else if uri == "ca://install" {
        ca_install_guide(ctx).await
    } else {
        Err(ToolError::not_found(format!("Unknown resource URI: {uri}")))
    }
}

async fn flows_list(ctx: &Arc<ProbeContext>) -> Result<Vec<ResourceContents>, ToolError> {
    let summaries = ctx
        .flows
        .search_flows(FlowQuery {
            limit: Some(50),
            ..Default::default()
        })
        .await;

    let mut md = String::from("# Recent Flows\n\n");
    if summaries.is_empty() {
        md.push_str("_No flows captured yet._\n");
    } else {
        md.push_str("| ID (prefix) | Method | URL | Status | Duration | Tags |\n");
        md.push_str("|-------------|--------|-----|--------|----------|------|\n");
        for s in &summaries {
            let id_short = &s.id[..8.min(s.id.len())];
            let status = s
                .status
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string());
            let dur = s
                .duration_ms
                .map(|d| format!("{}ms", d))
                .unwrap_or_else(|| "-".to_string());
            let tags = s.tags.join(", ");
            let url = if s.url.len() > 60 {
                format!("{}…", &s.url[..60])
            } else {
                s.url.clone()
            };
            md.push_str(&format!(
                "| {}… | {} | {} | {} | {} | {} |\n",
                id_short, s.method, url, status, dur, tags
            ));
        }
        md.push_str(&format!("\n_Total: {} flows_\n", summaries.len()));
    }

    Ok(vec![ResourceContents::text(md, "flows://")])
}

async fn flow_detail(ctx: &Arc<ProbeContext>, id: &str) -> Result<Vec<ResourceContents>, ToolError> {
    match ctx.flows.get_flow(id).await {
        Some(flow) => {
            let json = serde_json::to_string_pretty(&flow).map_err(|e| ToolError::internal(e.to_string()))?;
            Ok(vec![ResourceContents::text(
                json,
                format!("flows://{}", id),
            )])
        }
        None => Err(ToolError::not_found(format!("Flow not found: {id}"))),
    }
}

async fn rules_list(ctx: &Arc<ProbeContext>) -> Result<Vec<ResourceContents>, ToolError> {
    let rules = ctx.rules.get_rules().await;
    let json = serde_json::to_string_pretty(&rules).map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(vec![ResourceContents::text(json, "rules://")])
}

async fn proxy_status(ctx: &Arc<ProbeContext>) -> Result<Vec<ResourceContents>, ToolError> {
    let json = serde_json::to_string_pretty(&ctx.status.status_report().await)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(vec![ResourceContents::text(json, "proxy://status")])
}

async fn recent_audit(ctx: &Arc<ProbeContext>) -> Result<Vec<ResourceContents>, ToolError> {
    let json =
        serde_json::to_string_pretty(&ctx.audit.audit_snapshot(50)).map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(vec![ResourceContents::text(json, "audit://recent")])
}

async fn ca_install_guide(_ctx: &Arc<ProbeContext>) -> Result<Vec<ResourceContents>, ToolError> {
    let guide = r#"# Install RelayCore CA Certificate

To intercept HTTPS traffic, your system must trust the RelayCore CA.
Copy and paste the command for your OS into a terminal.

## macOS (one command)
```bash
sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain ca_cert.pem
```
Then configure proxy: System Settings → Network → Proxies → Web Proxy (HTTP) + Secure Web Proxy (HTTPS) → 127.0.0.1:8080

## Linux (Debian/Ubuntu)
```bash
sudo cp ca_cert.pem /usr/local/share/ca-certificates/relay-core.crt && sudo update-ca-certificates
```

## Linux (Fedora/RHEL)
```bash
sudo cp ca_cert.pem /etc/pki/ca-trust/source/anchors/ && sudo update-ca-trust
```

## Windows
1. Press `Win + R`, type `certmgr.msc`, press Enter
2. Right-click "Trusted Root Certification Authorities" → All Tasks → Import
3. Select `ca_cert.pem` (change file filter to "All Files")
4. Complete the wizard with default settings

## Where is ca_cert.pem?
Generated by `relay-core-cli ca init`. Default location is the current directory.
If you're using relay-core-probe as a library, pass the CA cert path to CoreState.

## Verify
After installation, visit https://example.com — the flow should appear in search_flows.
"#;
    Ok(vec![ResourceContents::text(
        guide.to_string(),
        "ca://install",
    )])
}

#[cfg(test)]
mod tests {
    use super::read_resource;
    use crate::server::ProbeContext;
    use relay_core_runtime::CoreState;
    use std::sync::Arc;

    #[tokio::test]
    async fn proxy_status_resource_uses_shared_status_report_shape() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(ProbeContext::new(state));
        let contents = read_resource(&ctx, "proxy://status")
            .await
            .expect("resource should load");

        let text = match &contents[0] {
            rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
            other => panic!("unexpected resource contents: {:?}", other),
        };

        let json: serde_json::Value =
            serde_json::from_str(&text).expect("proxy status should be valid json");
        assert_eq!(json["status"]["phase"], "created");
        assert_eq!(json["status"]["running"], false);
        assert!(json["metrics"].is_object());
        assert!(json.get("lifecycle").is_none());
    }

    #[tokio::test]
    async fn recent_audit_resource_uses_shared_audit_snapshot_shape() {
        let state = Arc::new(CoreState::new(None).await);
        let ctx = Arc::new(ProbeContext::new(state));
        let contents = read_resource(&ctx, "audit://recent")
            .await
            .expect("resource should load");

        let text = match &contents[0] {
            rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
            other => panic!("unexpected resource contents: {:?}", other),
        };

        let json: serde_json::Value =
            serde_json::from_str(&text).expect("audit resource should be valid json");
        assert!(json["events"].is_array());
    }
}
