use axum::{
    body::Body,
    http::{StatusCode, header},
    response::Response,
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "embed/webui/"]
struct WebUiAssets;

/// Fallback handler for embedded web UI static files (SPA: unknown paths → index.html).
pub fn serve_webui() -> axum::routing::MethodRouter {
    use axum::routing::any;

    any(webui_handler)
}

async fn webui_handler(req: axum::http::Request<Body>) -> Response {
    let path = req.uri().path();
    let path = path.trim_start_matches('/');

    let file_path = if path.is_empty() { "index.html" } else { path };

    match WebUiAssets::get(file_path) {
        Some(file) => {
            let content_type = mime_guess_from_path(file_path);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(file.data))
                .unwrap()
        }
        None => {
            // SPA fallback: serve index.html for any non-file path
            if let Some(index_file) = WebUiAssets::get("index.html") {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .body(Body::from(index_file.data))
                    .unwrap()
            } else {
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from("Web UI not found. Build the webui first."))
                    .unwrap()
            }
        }
    }
}

fn mime_guess_from_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn embedded_index_is_non_empty() {
        let file = WebUiAssets::get("index.html").expect("index.html must be embedded");
        assert!(file.data.len() > 100, "embedded index.html too small: {}", file.data.len());
    }

    #[tokio::test]
    async fn webui_handler_serves_index_at_root() {
        let req = axum::http::Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();
        let resp = webui_handler(req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        assert!(body.len() > 100, "response body empty");
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("html"), "expected html, got: {}", &text[..text.len().min(80)]);
    }
}
