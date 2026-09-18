//! Lightweight JSON-schema validation for tool arguments (required keys and
//! primitive types). Enough to reject malformed calls with a useful message.

use serde_json::Value;

pub fn validate(schema: &Value, args: &Value) -> Result<(), String> {
    validate_at(schema, args, "")
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn matches_type(expected: &str, v: &Value) -> bool {
    match expected {
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    }
}

fn validate_at(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(obj) = schema.as_object() else { return Ok(()) };
    let label = if path.is_empty() { "arguments".to_string() } else { path.to_string() };
    if let Some(any_of) = obj.get("anyOf").and_then(|a| a.as_array()) {
        if any_of.iter().any(|s| validate_at(s, value, path).is_ok()) {
            return Ok(());
        }
        return Err(format!("{label} does not match any allowed schema"));
    }
    if let Some(en) = obj.get("enum").and_then(|e| e.as_array()) {
        if !en.contains(value) {
            return Err(format!("{label} must be one of {}", serde_json::to_string(en).unwrap_or_default()));
        }
    }
    if let Some(c) = obj.get("const") {
        if c != value {
            return Err(format!("{label} must equal {c}"));
        }
    }
    match obj.get("type") {
        Some(Value::String(t)) => {
            if !matches_type(t, value) {
                return Err(format!("{label} must be of type {t}, got {}", type_name(value)));
            }
        }
        Some(Value::Array(ts)) => {
            if !ts.iter().any(|t| t.as_str().map(|t| matches_type(t, value)).unwrap_or(true)) {
                return Err(format!("{label} has invalid type {}", type_name(value)));
            }
        }
        _ => {}
    }
    if let (Some(props), Some(map)) = (obj.get("properties").and_then(|p| p.as_object()), value.as_object()) {
        if let Some(req) = obj.get("required").and_then(|r| r.as_array()) {
            for r in req.iter().filter_map(|r| r.as_str()) {
                if !map.contains_key(r) {
                    return Err(format!("{label} is missing required property '{r}'"));
                }
            }
        }
        for (k, sub) in props {
            if let Some(v) = map.get(k) {
                if v.is_null() && sub.get("type").and_then(|t| t.as_str()) != Some("null") {
                    continue; // treat null as absent for optional fields
                }
                let child = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                validate_at(sub, v, &child)?;
            }
        }
    }
    if let (Some(items), Some(arr)) = (obj.get("items"), value.as_array()) {
        for (i, v) in arr.iter().enumerate() {
            validate_at(items, v, &format!("{label}[{i}]"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_and_types() {
        let schema = json!({"type":"object","properties":{"path":{"type":"string"},"n":{"type":"number"}},"required":["path"]});
        assert!(validate(&schema, &json!({"path":"a"})).is_ok());
        assert!(validate(&schema, &json!({})).unwrap_err().contains("path"));
        assert!(validate(&schema, &json!({"path":"a","n":"x"})).unwrap_err().contains("number"));
        let en = json!({"type":"object","properties":{"action":{"type":"string","enum":["a","b"]}},"required":["action"]});
        assert!(validate(&en, &json!({"action":"c"})).is_err());
        assert!(validate(&en, &json!({"action":"a"})).is_ok());
    }
}
