use axum::{
    body::Body,
    http::{StatusCode, header},
    response::Response,
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "embed/webui/"]
struct WebUiAssets;

/// Build a fallback service that serves embedded web UI static files.
/// Falls back to `index.html` for SPA-style client-side routing.
pub fn serve_webui() -> axum::routing::MethodRouter {
    use axum::routing::get_service;

    get_service(axum::handler::Handler::with_state(
        (),
        |_: axum::extract::State<()>, req: axum::http::Request<Body>| async move {
            webui_handler(req).await
        },
    ))
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
