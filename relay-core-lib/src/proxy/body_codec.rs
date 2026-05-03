use data_encoding::BASE64;

/// Helper to process body bytes into BodyData content and encoding
pub fn process_body(bytes: &[u8], headers: &[(String, String)]) -> (String, String) {
    // 1. Check content-type header
    let content_type = headers.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.to_lowercase())
        .unwrap_or_default();

    // Heuristic 1: If explicit charset is present, treat as text
    if content_type.contains("charset=utf-8") || content_type.contains("charset=us-ascii") || content_type.contains("text/") || content_type.contains("application/json") || content_type.contains("application/xml") || content_type.contains("application/javascript") {
        // Try UTF-8 decode first to be safe, if fails, fallback to base64
        match std::str::from_utf8(bytes) {
            Ok(s) => return ("utf-8".to_string(), s.to_string()),
            Err(_) => return ("base64".to_string(), BASE64.encode(bytes)),
        }
    }

    // Heuristic 2: Known binary types
    if content_type.starts_with("image/") || 
       content_type.starts_with("audio/") || 
       content_type.starts_with("video/") ||
       content_type.contains("application/octet-stream") ||
       content_type.contains("application/pdf") ||
       content_type.contains("application/zip") {
        return ("base64".to_string(), BASE64.encode(bytes));
    }

    // Heuristic 3: Try UTF-8 decode as fallback
    match std::str::from_utf8(bytes) {
        Ok(s) => ("utf-8".to_string(), s.to_string()),
        Err(_) => ("base64".to_string(), BASE64.encode(bytes)),
    }
}
