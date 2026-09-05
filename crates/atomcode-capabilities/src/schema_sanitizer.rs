//! Universal tool parameter schema sanitizer.
//!
//! Cleans JSON schemas from external MCP servers, code generators, and LLM tool definitions:
//! - Strips leaked HTTP protocol headers mistakenly exposed as tool arguments
//! - Fixes generator pseudo-object glitches (e.g. Brave Search `safesearch`, `freshness`, `units`)
//! - Heuristically resolves bare objects (extracting enums from descriptions or falling back to `anyOf`)
//! - Strips JS runtime artifacts like `Number.MAX_SAFE_INTEGER`

/// Leaked HTTP protocol headers that external code generators mistakenly expose to LLM tools.
pub const LEAKED_HTTP_HEADERS: &[&str] = &[
    "accept",
    "cache-control",
    "user-agent",
    "api-version",
    "authorization",
    "content-type",
    "x-api-key",
    "cookie",
    "referer",
];

/// Extract single- or double-quoted enum tokens from a description string.
/// For example, "Safe search setting ('off', 'strict')" -> ["off", "strict"]
/// or "Filters: 'pd', 'pw', 'pm', 'py'" -> ["pd", "pw", "pm", "py"].
pub fn extract_quoted_enums(desc: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut in_quote = false;
    let mut quote_char = ' ';
    let mut cur = String::new();
    for c in desc.chars() {
        if !in_quote && (c == '\'' || c == '"') {
            in_quote = true;
            quote_char = c;
            cur.clear();
        } else if in_quote && c == quote_char {
            in_quote = false;
            let trimmed = cur.trim();
            if !trimmed.is_empty()
                && trimmed.len() <= 32
                && !trimmed.contains(' ')
                && !tokens.iter().any(|t| t == trimmed)
            {
                tokens.push(trimmed.to_string());
            }
            cur.clear();
        } else if in_quote {
            cur.push(c);
        }
    }
    tokens
}

/// Check if a JSON schema node is a bare/pseudo-object without concrete properties.
pub fn is_bare_object(val: &serde_json::Map<String, serde_json::Value>) -> bool {
    let type_is_obj = match val.get("type") {
        Some(serde_json::Value::String(s)) => s == "object",
        Some(serde_json::Value::Array(arr)) => arr.iter().any(|v| v == "object"),
        _ => false,
    };
    if !type_is_obj {
        return false;
    }
    let has_non_empty_props = val
        .get("properties")
        .and_then(|p| p.as_object())
        .map_or(false, |m| !m.is_empty());
    let has_additional = val
        .get("additionalProperties")
        .map_or(false, |v| v != &serde_json::Value::Bool(false));
    !has_non_empty_props && !has_additional
}

/// Sanitize external MCP JSON schemas to remove JS runtime artifacts, protocol leaks,
/// and known generator glitches that confuse LLMs.
pub fn sanitize_mcp_schema(mut schema: serde_json::Value) -> serde_json::Value {
    fn clean_node(v: &mut serde_json::Value, is_root: bool, inside_union: bool) {
        match v {
            serde_json::Value::Object(map) => {
                // 1. Strip JS Number.MAX_SAFE_INTEGER noise (9007199254740991)
                const JS_SAFE_INT_THRESHOLD: f64 = 9_000_000_000_000_000.0;
                if let Some(max) = map.get("maximum").and_then(|m| m.as_f64()) {
                    if max >= JS_SAFE_INT_THRESHOLD {
                        map.remove("maximum");
                    }
                }
                if let Some(min) = map.get("minimum").and_then(|m| m.as_f64()) {
                    if min <= -JS_SAFE_INT_THRESHOLD {
                        map.remove("minimum");
                    }
                }

                // 2. Fix known generator type glitches (e.g. Brave Search pseudo-objects)
                // and heuristic bare-object fallbacks
                if let Some(props) = map.get_mut("properties").and_then(|p| p.as_object_mut()) {
                    // Filter HTTP protocol leaks that code generators mistakenly expose to LLM
                    props.retain(|k, _| {
                        let lower = k.to_ascii_lowercase();
                        !LEAKED_HTTP_HEADERS.contains(&lower.as_str())
                    });

                    // Sanitize each property
                    for (prop_name, prop_val) in props.iter_mut() {
                        if let Some(p_map) = prop_val.as_object_mut() {
                            let name_lower = prop_name.to_ascii_lowercase();
                            let desc = p_map
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_string();

                            if name_lower == "safesearch" {
                                p_map.insert("type".to_string(), serde_json::json!("string"));
                                p_map.remove("properties");
                                if desc.contains("'strict'")
                                    && !desc.contains("'moderate'")
                                    && !desc.to_ascii_lowercase().contains("moderate")
                                {
                                    p_map.insert(
                                        "enum".to_string(),
                                        serde_json::json!(["off", "strict"]),
                                    );
                                } else {
                                    p_map.insert(
                                        "enum".to_string(),
                                        serde_json::json!(["off", "moderate", "strict"]),
                                    );
                                }
                                if desc.is_empty() {
                                    p_map.insert(
                                        "description".to_string(),
                                        serde_json::json!("Safe search setting ('off', 'moderate', 'strict')."),
                                    );
                                }
                            } else if name_lower == "freshness" {
                                p_map.insert("type".to_string(), serde_json::json!("string"));
                                p_map.remove("properties");
                                p_map.insert(
                                    "enum".to_string(),
                                    serde_json::json!(["pd", "pw", "pm", "py"]),
                                );
                                if desc.is_empty() {
                                    p_map.insert(
                                        "description".to_string(),
                                        serde_json::json!("Filters search results by discovery time: 'pd' (day), 'pw' (week), 'pm' (month), 'py' (year), or custom date range (YYYY-MM-DDtoYYYY-MM-DD)."),
                                    );
                                }
                            } else if name_lower == "units" {
                                p_map.insert("type".to_string(), serde_json::json!("string"));
                                p_map.remove("properties");
                                p_map.insert(
                                    "enum".to_string(),
                                    serde_json::json!(["metric", "imperial"]),
                                );
                                if desc.is_empty() {
                                    p_map.insert(
                                        "description".to_string(),
                                        serde_json::json!("Unit system: 'metric' or 'imperial'."),
                                    );
                                }
                            } else if name_lower == "goggles" {
                                p_map.insert("type".to_string(), serde_json::json!("string"));
                                p_map.remove("properties");
                            } else if is_bare_object(p_map) {
                                // Heuristic: try to extract quoted enums from description
                                let enums = extract_quoted_enums(&desc);
                                if enums.len() >= 2 {
                                    p_map.insert("type".to_string(), serde_json::json!("string"));
                                    p_map.insert("enum".to_string(), serde_json::json!(enums));
                                    p_map.remove("properties");
                                } else {
                                    // Fallback: relax to anyOf [string, object] to avoid Constrained Decoding trapping LLM into {}
                                    p_map.remove("type");
                                    p_map.remove("properties");
                                    p_map.insert(
                                        "anyOf".to_string(),
                                        serde_json::json!([
                                            { "type": "string" },
                                            { "type": "object" }
                                        ]),
                                    );
                                }
                            }
                        }
                    }

                    // Clean required array: remove leaked headers AND properties that don't exist
                    let prop_keys: std::collections::HashSet<String> = props.keys().cloned().collect();
                    if let Some(reqs) = map.get_mut("required").and_then(|r| r.as_array_mut()) {
                        reqs.retain(|item| {
                            if let Some(s) = item.as_str() {
                                let lower = s.to_ascii_lowercase();
                                !LEAKED_HTTP_HEADERS.contains(&lower.as_str()) && prop_keys.contains(s)
                            } else {
                                true
                            }
                        });
                    }
                } else if !is_root && !inside_union && is_bare_object(map) {
                    let desc = map
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    let enums = extract_quoted_enums(&desc);
                    if enums.len() >= 2 {
                        map.insert("type".to_string(), serde_json::json!("string"));
                        map.insert("enum".to_string(), serde_json::json!(enums));
                        map.remove("properties");
                    } else {
                        map.remove("type");
                        map.remove("properties");
                        map.insert(
                            "anyOf".to_string(),
                            serde_json::json!([
                                { "type": "string" },
                                { "type": "object" }
                            ]),
                        );
                    }
                    return;
                }

                // Recurse into child schemas
                if let Some(props) = map.get_mut("properties").and_then(|p| p.as_object_mut()) {
                    for child in props.values_mut() {
                        clean_node(child, false, false);
                    }
                }
                if let Some(items) = map.get_mut("items") {
                    clean_node(items, false, false);
                }
                if let Some(any_of) = map.get_mut("anyOf").and_then(|a| a.as_array_mut()) {
                    for child in any_of {
                        clean_node(child, false, true);
                    }
                }
                if let Some(one_of) = map.get_mut("oneOf").and_then(|o| o.as_array_mut()) {
                    for child in one_of {
                        clean_node(child, false, true);
                    }
                }
                if let Some(all_of) = map.get_mut("allOf").and_then(|a| a.as_array_mut()) {
                    for child in all_of {
                        clean_node(child, false, true);
                    }
                }
                if let Some(defs) = map.get_mut("$defs").and_then(|d| d.as_object_mut()) {
                    for child in defs.values_mut() {
                        clean_node(child, false, false);
                    }
                }
                if let Some(defs) = map.get_mut("definitions").and_then(|d| d.as_object_mut()) {
                    for child in defs.values_mut() {
                        clean_node(child, false, false);
                    }
                }
            }
            serde_json::Value::Array(arr) => {
                for child in arr {
                    clean_node(child, false, inside_union);
                }
            }
            _ => {}
        }
    }

    clean_node(&mut schema, true, false);
    schema
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sanitize_mcp_schema() {
        let raw = json!({
            "type": "object",
            "properties": {
                "count": {
                    "type": "integer",
                    "maximum": 9007199254740991i64,
                    "minimum": -9007199254740991i64
                },
                "safesearch": {
                    "type": "object",
                    "description": "Safe search setting ('off', 'moderate', 'strict')"
                },
                "freshness": {
                    "type": "object"
                },
                "units": {
                    "type": "object"
                },
                "accept": {
                    "type": "string"
                }
            },
            "required": ["count", "accept"]
        });

        let cleaned = sanitize_mcp_schema(raw);
        let props = cleaned.get("properties").unwrap().as_object().unwrap();

        let count = props.get("count").unwrap().as_object().unwrap();
        assert!(!count.contains_key("maximum"));
        assert!(!count.contains_key("minimum"));

        let safesearch = props.get("safesearch").unwrap().as_object().unwrap();
        assert_eq!(safesearch.get("type").unwrap(), "string");
        assert_eq!(
            safesearch.get("enum").unwrap(),
            &json!(["off", "moderate", "strict"])
        );

        let freshness = props.get("freshness").unwrap().as_object().unwrap();
        assert_eq!(freshness.get("type").unwrap(), "string");
        assert_eq!(
            freshness.get("enum").unwrap(),
            &json!(["pd", "pw", "pm", "py"])
        );

        let units = props.get("units").unwrap().as_object().unwrap();
        assert_eq!(units.get("type").unwrap(), "string");
        assert_eq!(units.get("enum").unwrap(), &json!(["metric", "imperial"]));

        assert!(!props.contains_key("accept"));

        let required = cleaned.get("required").unwrap().as_array().unwrap();
        assert_eq!(required, &vec![json!("count")]);
    }

    #[test]
    fn test_sanitize_brave_image_and_llm_context_and_heuristics() {
        let raw = json!({
            "type": "object",
            "properties": {
                "safesearch": {
                    "type": "object",
                    "description": "Filters search results for adult content ('off', 'strict')"
                },
                "goggles": {
                    "type": "object",
                    "description": "Goggles URL"
                },
                "category": {
                    "type": "object",
                    "description": "Category filter ('books', 'electronics', 'clothing')"
                },
                "custom_filter": {
                    "type": "object",
                    "description": "Arbitrary filter payload"
                }
            }
        });

        let cleaned = sanitize_mcp_schema(raw);
        let props = cleaned.get("properties").unwrap().as_object().unwrap();

        assert_eq!(props["safesearch"]["type"], "string");
        assert_eq!(props["safesearch"]["enum"], json!(["off", "strict"]));

        assert_eq!(props["goggles"]["type"], "string");

        assert_eq!(props["category"]["type"], "string");
        assert_eq!(
            props["category"]["enum"],
            json!(["books", "electronics", "clothing"])
        );

        assert_eq!(
            props["custom_filter"]["anyOf"],
            json!([{"type": "string"}, {"type": "object"}])
        );
        assert!(!props["custom_filter"]
            .as_object()
            .unwrap()
            .contains_key("type"));
    }
}
