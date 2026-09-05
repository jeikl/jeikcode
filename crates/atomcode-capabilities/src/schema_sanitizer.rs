//! Universal tool parameter schema and description sanitizer synthesized from grokbuild & opencode.
//!
//! Cleans JSON schemas and descriptions from external MCP servers, code generators, and LLM tool definitions:
//! - Normalizes and collapses multi-line descriptions into clean single-line strings (`sanitize_description`)
//! - Truncates overlong descriptions to bounded budget (`truncate_description`, default 2048 chars)
//! - Ensures JSON schema root conforms to `{"type": "object", "properties": {}}` (`ensure_object_schema`)
//! - Inlines local `$ref` (#/$defs/... and #/definitions/...) and drops root definition containers (OpenCode pattern)
//! - Flattens safe `allOf` compositors into parent properties (OpenCode / Gemini compatibility)
//! - Strips `"type": "null"` from `anyOf` / `oneOf` unions and unpacks single-element unions (OpenCode pattern)
//! - Removes `additionalProperties: true` that triggers strict schema rejection in some LLM providers
//! - Strips leaked HTTP protocol headers mistakenly exposed as tool arguments (`LEAKED_HTTP_HEADERS`)
//! - Strips JS runtime artifacts like `Number.MAX_SAFE_INTEGER`
//! - Preserves parameter types (e.g. array items) to maintain protobuf compatibility across all providers (Gemini, Claude, OpenAI).

use std::collections::{HashMap, HashSet};

/// Maximum length for MCP tool/server descriptions. Matches grokbuild's `MAX_MCP_DESCRIPTION_LENGTH`.
pub const MAX_MCP_DESCRIPTION_LENGTH: usize = 2048;

pub const TRUNCATION_SUFFIX: &str = "\u{2026} [truncated]";

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

/// Sanitize description by collapsing newlines, carriage returns, and consecutive whitespace into single spaces.
/// Matches grokbuild `sanitize_description` implementation.
pub fn sanitize_description(s: &str) -> String {
    s.split(['\n', '\r'])
        .flat_map(|line| line.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncate a description string to `MAX_MCP_DESCRIPTION_LENGTH` characters, appending suffix if truncated.
/// Operates on char boundaries to avoid splitting multi-byte characters.
/// Matches grokbuild `truncate_description` implementation.
pub fn truncate_description(s: &str) -> String {
    if s.len() <= MAX_MCP_DESCRIPTION_LENGTH || s.chars().count() <= MAX_MCP_DESCRIPTION_LENGTH {
        return s.to_owned();
    }
    let suffix_chars = TRUNCATION_SUFFIX.chars().count();
    let budget = MAX_MCP_DESCRIPTION_LENGTH.saturating_sub(suffix_chars);
    let truncated: String = s.chars().take(budget).collect();
    format!("{truncated}{TRUNCATION_SUFFIX}")
}

/// Ensure the schema has "type": "object".
/// Matches grokbuild `servers.rs:4350-4361` handling for servers (like VSCode or minimal tools)
/// that emit empty schemas or schemas without a `type` field.
pub fn ensure_object_schema(mut schema: serde_json::Value) -> serde_json::Value {
    if !schema.is_object() {
        return serde_json::json!({
            "type": "object"
        });
    }
    if let Some(obj) = schema.as_object_mut() {
        obj.entry("type")
            .or_insert_with(|| serde_json::json!("object"));
    }
    schema
}

/// Extract single- or double-quoted enum tokens from a description string.
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

/// Resolve and inline local references (`$ref: "#/$defs/..."` or `"#/definitions/..."`)
/// Matches OpenCode's `inlineLocalReferences`.
fn inline_local_references(
    node: &mut serde_json::Value,
    defs: &HashMap<String, serde_json::Value>,
    seen: &mut HashSet<String>,
) {
    match node {
        serde_json::Value::Object(map) => {
            if let Some(ref_val) = map.get("$ref").and_then(|r| r.as_str()) {
                let name = ref_val
                    .strip_prefix("#/$defs/")
                    .or_else(|| ref_val.strip_prefix("#/definitions/"));
                if let Some(name) = name {
                    if !seen.contains(name) {
                        if let Some(target) = defs.get(name).cloned() {
                            seen.insert(name.to_string());
                            let mut resolved = target;
                            inline_local_references(&mut resolved, defs, seen);
                            seen.remove(name);

                            map.remove("$ref");
                            if let serde_json::Value::Object(target_map) = resolved {
                                for (k, v) in target_map {
                                    map.entry(k).or_insert(v);
                                }
                            }
                        }
                    }
                }
            }

            for child in map.values_mut() {
                inline_local_references(child, defs, seen);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr {
                inline_local_references(child, defs, seen);
            }
        }
        _ => {}
    }
}

/// Flatten `allOf` arrays into the parent object if allOf contains objects with properties.
/// Matches OpenCode's `canFlattenAllOf`.
fn flatten_all_of(map: &mut serde_json::Map<String, serde_json::Value>) {
    let can_flatten = if let Some(all_of) = map.get("allOf").and_then(|a| a.as_array()) {
        !all_of.is_empty() && all_of.iter().all(|item| item.is_object())
    } else {
        false
    };

    if can_flatten {
        if let Some(serde_json::Value::Array(all_of)) = map.remove("allOf") {
            for item in all_of {
                if let serde_json::Value::Object(item_obj) = item {
                    for (k, v) in item_obj {
                        if k == "properties" {
                            if let serde_json::Value::Object(props) = v {
                                let parent_props = map
                                    .entry("properties")
                                    .or_insert_with(|| serde_json::json!({}))
                                    .as_object_mut();
                                if let Some(parent_props) = parent_props {
                                    for (pk, pv) in props {
                                        parent_props.entry(pk).or_insert(pv);
                                    }
                                }
                            }
                        } else if k == "required" {
                            if let serde_json::Value::Array(reqs) = v {
                                let parent_reqs = map
                                    .entry("required")
                                    .or_insert_with(|| serde_json::json!([]))
                                    .as_array_mut();
                                if let Some(parent_reqs) = parent_reqs {
                                    for req in reqs {
                                        if !parent_reqs.contains(&req) {
                                            parent_reqs.push(req);
                                        }
                                    }
                                }
                            }
                        } else {
                            map.entry(k).or_insert(v);
                        }
                    }
                }
            }
        }
    }
}

/// Strip `{"type": "null"}` from `anyOf` / `oneOf` unions and unpack single-element unions.
/// Matches OpenCode's `stripNull` handling.
fn simplify_union_key(map: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    if let Some(union_arr) = map.get_mut(key).and_then(|u| u.as_array_mut()) {
        // Strip out {"type": "null"}
        union_arr.retain(|item| {
            if let Some(obj) = item.as_object() {
                if obj.get("type").and_then(|t| t.as_str()) == Some("null") && obj.len() == 1 {
                    return false;
                }
            }
            true
        });

        // If union has only 1 element left, unpack it into the parent map
        if union_arr.len() == 1 {
            if let Some(single) = union_arr.pop() {
                map.remove(key);
                if let serde_json::Value::Object(single_obj) = single {
                    for (k, v) in single_obj {
                        map.entry(k).or_insert(v);
                    }
                }
            }
        }
    }
}

/// Sanitize external MCP JSON schemas:
/// - Ensures root is a valid object schema (`ensure_object_schema`)
/// - Inlines local `$ref` definitions and drops definition blocks (OpenCode pattern)
/// - Flattens `allOf` arrays into parent properties (OpenCode pattern)
/// - Simplifies `anyOf` / `oneOf` unions by stripping `type: "null"` and unpacking single elements
/// - Removes `additionalProperties: true`
/// - Strips leaked HTTP protocol headers (`LEAKED_HTTP_HEADERS`) from properties and required arrays
/// - Strips JS runtime artifacts (`Number.MAX_SAFE_INTEGER`)
/// - Leaves tool parameter types intact so provider protobuf validators (Gemini, Claude, OpenAI) don't fail.
pub fn sanitize_mcp_schema(schema: serde_json::Value) -> serde_json::Value {
    let mut schema = ensure_object_schema(schema);

    // 1. Collect root-level definitions for reference inlining
    let mut defs_map = HashMap::new();
    if let Some(root_obj) = schema.as_object() {
        if let Some(defs) = root_obj.get("$defs").and_then(|d| d.as_object()) {
            for (k, v) in defs {
                defs_map.insert(k.clone(), v.clone());
            }
        }
        if let Some(defs) = root_obj.get("definitions").and_then(|d| d.as_object()) {
            for (k, v) in defs {
                defs_map.insert(k.clone(), v.clone());
            }
        }
    }

    // 2. Inline local references throughout the schema tree
    if !defs_map.is_empty() {
        let mut seen = HashSet::new();
        inline_local_references(&mut schema, &defs_map, &mut seen);
        if let Some(root_obj) = schema.as_object_mut() {
            root_obj.remove("$defs");
            root_obj.remove("definitions");
        }
    }

    // 3. Recursive tree cleaning
    fn clean_node(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                // Remove additionalProperties: true
                if let Some(add_prop) = map.get("additionalProperties") {
                    if add_prop == &serde_json::Value::Bool(true) {
                        map.remove("additionalProperties");
                    }
                }

                // Strip JS Number.MAX_SAFE_INTEGER noise (9007199254740991)
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

                // Simplify anyOf / oneOf unions
                simplify_union_key(map, "anyOf");
                simplify_union_key(map, "oneOf");

                // Flatten allOf if present
                flatten_all_of(map);

                // Filter HTTP protocol leaks that code generators mistakenly expose to LLM
                let prop_keys: Option<HashSet<String>> = if let Some(props) =
                    map.get_mut("properties").and_then(|p| p.as_object_mut())
                {
                    props.retain(|k, _| {
                        let lower = k.to_ascii_lowercase();
                        !LEAKED_HTTP_HEADERS.contains(&lower.as_str())
                    });

                    for child in props.values_mut() {
                        clean_node(child);
                    }
                    Some(props.keys().cloned().collect())
                } else {
                    None
                };

                // Clean required array: remove leaked headers AND properties that don't exist
                if let Some(prop_keys) = prop_keys {
                    if let Some(reqs) = map.get_mut("required").and_then(|r| r.as_array_mut()) {
                        reqs.retain(|item| {
                            if let Some(s) = item.as_str() {
                                let lower = s.to_ascii_lowercase();
                                !LEAKED_HTTP_HEADERS.contains(&lower.as_str())
                                    && prop_keys.contains(s)
                            } else {
                                true
                            }
                        });
                    }
                }

                if let Some(items) = map.get_mut("items") {
                    clean_node(items);
                }
                if let Some(any_of) = map.get_mut("anyOf").and_then(|a| a.as_array_mut()) {
                    for child in any_of {
                        clean_node(child);
                    }
                }
                if let Some(one_of) = map.get_mut("oneOf").and_then(|o| o.as_array_mut()) {
                    for child in one_of {
                        clean_node(child);
                    }
                }
                if let Some(all_of) = map.get_mut("allOf").and_then(|a| a.as_array_mut()) {
                    for child in all_of {
                        clean_node(child);
                    }
                }
            }
            serde_json::Value::Array(arr) => {
                for child in arr {
                    clean_node(child);
                }
            }
            _ => {}
        }
    }

    clean_node(&mut schema);
    schema
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sanitize_description() {
        let input = "  This is line 1.\n  This is line 2.\r\nLine 3 \t with  spaces.  ";
        let out = sanitize_description(input);
        assert_eq!(out, "This is line 1. This is line 2. Line 3 with spaces.");
    }

    #[test]
    fn test_truncate_description() {
        let short = "A short description";
        assert_eq!(truncate_description(short), short);

        let long = "a".repeat(3000);
        let truncated = truncate_description(&long);
        assert_eq!(truncated.chars().count(), MAX_MCP_DESCRIPTION_LENGTH);
        assert!(truncated.ends_with(TRUNCATION_SUFFIX));
    }

    #[test]
    fn test_ensure_object_schema() {
        let empty = json!({});
        let out = ensure_object_schema(empty);
        assert_eq!(out["type"], "object");

        let null = serde_json::Value::Null;
        let out = ensure_object_schema(null);
        assert_eq!(out["type"], "object");
    }

    #[test]
    fn test_inline_local_references() {
        let raw = json!({
            "type": "object",
            "properties": {
                "user": {
                    "$ref": "#/$defs/UserProfile"
                }
            },
            "$defs": {
                "UserProfile": {
                    "type": "object",
                    "properties": {
                        "username": { "type": "string" }
                    }
                }
            }
        });

        let cleaned = sanitize_mcp_schema(raw);
        assert!(!cleaned.as_object().unwrap().contains_key("$defs"));

        let user = cleaned["properties"]["user"].as_object().unwrap();
        assert_eq!(user.get("type").unwrap(), "object");
        assert_eq!(user["properties"]["username"]["type"], "string");
        assert!(!user.contains_key("$ref"));
    }

    #[test]
    fn test_simplify_anyof_with_null() {
        let raw = json!({
            "type": "object",
            "properties": {
                "optional_field": {
                    "anyOf": [
                        { "type": "string", "description": "some text" },
                        { "type": "null" }
                    ]
                }
            }
        });

        let cleaned = sanitize_mcp_schema(raw);
        let field = cleaned["properties"]["optional_field"].as_object().unwrap();
        assert_eq!(field.get("type").unwrap(), "string");
        assert_eq!(field.get("description").unwrap(), "some text");
        assert!(!field.contains_key("anyOf"));
    }

    #[test]
    fn test_flatten_all_of() {
        let raw = json!({
            "type": "object",
            "properties": {
                "base": { "type": "string" }
            },
            "allOf": [
                {
                    "properties": {
                        "extra": { "type": "integer" }
                    },
                    "required": ["extra"]
                }
            ]
        });

        let cleaned = sanitize_mcp_schema(raw);
        let root = cleaned.as_object().unwrap();
        assert!(!root.contains_key("allOf"));
        let props = root.get("properties").unwrap().as_object().unwrap();
        assert_eq!(props["base"]["type"], "string");
        assert_eq!(props["extra"]["type"], "integer");

        let reqs = root.get("required").unwrap().as_array().unwrap();
        assert!(reqs.contains(&json!("extra")));
    }

    #[test]
    fn test_clean_additional_properties_true() {
        let raw = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" }
            },
            "additionalProperties": true
        });

        let cleaned = sanitize_mcp_schema(raw);
        assert!(!cleaned.as_object().unwrap().contains_key("additionalProperties"));
    }

    #[test]
    fn test_sanitize_mcp_schema_preserves_array_items() {
        // Critical test for Gemini compatibility:
        // Properties with "items" must NOT have their type mutated away from array!
        let raw = json!({
            "type": "object",
            "properties": {
                "goggles": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Goggles URL list"
                },
                "count": {
                    "type": "integer",
                    "maximum": 9007199254740991i64,
                    "minimum": -9007199254740991i64
                },
                "accept": {
                    "type": "string"
                }
            },
            "required": ["goggles", "accept"]
        });

        let cleaned = sanitize_mcp_schema(raw);
        let props = cleaned.get("properties").unwrap().as_object().unwrap();

        // goggles retains type: "array" and items intact
        let goggles = props.get("goggles").unwrap().as_object().unwrap();
        assert_eq!(goggles.get("type").unwrap(), "array");
        assert_eq!(goggles.get("items").unwrap(), &json!({ "type": "string" }));

        // count strips MAX_SAFE_INTEGER
        let count = props.get("count").unwrap().as_object().unwrap();
        assert!(!count.contains_key("maximum"));
        assert!(!count.contains_key("minimum"));

        // accept is filtered out of properties and required
        assert!(!props.contains_key("accept"));
        let required = cleaned.get("required").unwrap().as_array().unwrap();
        assert_eq!(required, &vec![json!("goggles")]);
    }
}

