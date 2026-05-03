use crate::rule::model::BodyTransform;
use relay_core_api::flow::BodyData;
use serde_json::Value;
use regex::Regex;

pub fn apply_transform(body: &mut BodyData, transform: &BodyTransform) {
    // Only transform if encoding is utf-8
    if body.encoding != "utf-8" {
        return;
    }

    match transform {
        BodyTransform::RegexReplace { pattern, replacement } => {
            if let Ok(re) = Regex::new(pattern) {
                body.content = re.replace_all(&body.content, replacement).to_string();
            }
        }
        BodyTransform::JsonPathSet { path, value } => {
            if let Ok(mut v) = serde_json::from_str::<Value>(&body.content) {
                let ptr = jsonpath_to_pointer(path);
                // Try to parse value as JSON, else treat as string
                let new_val = if let Ok(parsed) = serde_json::from_str::<Value>(value) {
                    parsed
                } else {
                    Value::String(value.clone())
                };

                if let Some(target) = v.pointer_mut(&ptr) {
                     *target = new_val;
                 } else {
                     // Try to insert if parent exists
                     if let Some(last_slash) = ptr.rfind('/') {
                         let (parent_ptr, last_segment) = ptr.split_at(last_slash);
                         let last_segment = &last_segment[1..];
                         
                         let parent = if parent_ptr.is_empty() {
                             Some(&mut v)
                         } else {
                             v.pointer_mut(parent_ptr)
                         };
                         
                         if let Some(Value::Object(map)) = parent {
                             map.insert(last_segment.to_string(), new_val);
                         }
                     }
                 }
                
                if let Ok(json_str) = serde_json::to_string(&v) {
                    body.content = json_str;
                }
            }
        }
        BodyTransform::JsonPathDelete { path } => {
            if let Ok(mut v) = serde_json::from_str::<Value>(&body.content) {
                let ptr = jsonpath_to_pointer(path);
                
                // To delete, we need parent pointer and last key/index
                // Handle root deletion? Not supported usually.
                if let Some(last_slash) = ptr.rfind('/') {
                    let (parent_ptr, last_segment) = ptr.split_at(last_slash);
                    let last_segment = &last_segment[1..]; // Skip '/'
                    
                    // parent_ptr might be empty if ptr is "/foo"
                    let parent = if parent_ptr.is_empty() {
                        Some(&mut v)
                    } else {
                        v.pointer_mut(parent_ptr)
                    };

                    if let Some(parent) = parent {
                        if let Value::Object(map) = parent {
                            map.remove(last_segment);
                        } else if let Value::Array(arr) = parent
                            && let Ok(idx) = last_segment.parse::<usize>()
                                && idx < arr.len() {
                                    arr.remove(idx);
                                }
                    }
                }

                if let Ok(json_str) = serde_json::to_string(&v) {
                    body.content = json_str;
                }
            }
        }
    }
}

fn escape_json_pointer(s: &str) -> String {
    s.replace("~", "~0").replace("/", "~1")
}

fn jsonpath_to_pointer(path: &str) -> String {
    if path.is_empty() || path == "$" {
        return "".to_string();
    }
    
    let mut pointer = String::new();
    let path = path.trim_start_matches('$');
    
    let mut chars = path.chars().peekable();
    
    // Handle first segment if it doesn't start with separator
    if let Some(&c) = chars.peek()
        && c != '.' && c != '[' {
            pointer.push('/');
        }

    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !pointer.ends_with('/') {
                    pointer.push('/');
                }
                let mut content = String::new();
                while let Some(&next) = chars.peek() {
                    if next == '.' || next == '[' {
                        break;
                    }
                    if let Some(c) = chars.next() {
                        content.push(c);
                    }
                }
                pointer.push_str(&escape_json_pointer(&content));
            },
            '[' => {
                if !pointer.ends_with('/') {
                    pointer.push('/');
                }
                let mut content = String::new();
                while let Some(&next) = chars.peek() {
                    if next == ']' {
                        chars.next(); // consume ']'
                        break;
                    }
                    if let Some(c) = chars.next() {
                        content.push(c);
                    }
                }
                let content = content.trim_matches('\'').trim_matches('"');
                pointer.push_str(&escape_json_pointer(content));
            }
            c => {
                // Start of a segment without dot (only possible at very beginning)
                let mut content = String::new();
                content.push(c);
                while let Some(&next) = chars.peek() {
                    if next == '.' || next == '[' {
                        break;
                    }
                    if let Some(c) = chars.next() {
                        content.push(c);
                    }
                }
                pointer.push_str(&escape_json_pointer(&content));
            }
        }
    }
    pointer
}
