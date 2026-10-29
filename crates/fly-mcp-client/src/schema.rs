//! A small JSON Schema validator for tool arguments and structured results.
//!
//! Supported keywords: `type`, `properties`, `required`,
//! `additionalProperties`, `items`, `enum`, `const`, `minimum`, `maximum`,
//! `exclusiveMinimum`, `exclusiveMaximum`, `minLength`, `maxLength`,
//! `minItems`, `maxItems`, `anyOf`, `oneOf`, `allOf`. Other keywords
//! (`$ref`, `pattern`, `format`, …) are ignored, so a value they'd reject can
//! pass; results are still fenced and size-capped whatever this says.

use serde_json::Value;

const MAX_DEPTH: usize = 32;
const MAX_ERRORS: usize = 5;

/// Validate `value` against `schema`. `Err` lists up to five problems, each
/// with a JSON-pointer-like path.
pub fn validate(schema: &Value, value: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    check(schema, value, "$", 0, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        errors.truncate(MAX_ERRORS);
        Err(errors)
    }
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => {
            v.is_i64()
                || v.is_u64()
                || v.as_f64()
                    .is_some_and(|f| f.fract() == 0.0 && f.is_finite())
        }
        _ => true, // unknown type names: don't guess
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn check(schema: &Value, v: &Value, path: &str, depth: usize, errors: &mut Vec<String>) {
    if errors.len() >= MAX_ERRORS {
        return;
    }
    if depth > MAX_DEPTH {
        errors.push(format!("{path}: schema nested too deeply"));
        return;
    }
    let s = match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            errors.push(format!("{path}: no value is allowed here"));
            return;
        }
        Value::Object(s) => s,
        _ => return,
    };

    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(t) => type_matches(t, v),
            Value::Array(ts) => ts
                .iter()
                .filter_map(Value::as_str)
                .any(|t| type_matches(t, v)),
            _ => true,
        };
        if !ok {
            errors.push(format!("{path}: expected {t}, got {}", type_name(v)));
            return;
        }
    }
    if let Some(Value::Array(options)) = s.get("enum") {
        if !options.contains(v) {
            errors.push(format!("{path}: not one of the allowed values"));
        }
    }
    if let Some(c) = s.get("const") {
        if c != v {
            errors.push(format!("{path}: must be {c}"));
        }
    }

    match v {
        Value::Object(obj) => {
            if let Some(Value::Array(req)) = s.get("required") {
                for r in req.iter().filter_map(Value::as_str) {
                    if !obj.contains_key(r) {
                        errors.push(format!("{path}: missing required field {r:?}"));
                    }
                }
            }
            let props = s.get("properties").and_then(Value::as_object);
            for (k, child) in obj {
                let child_path = format!("{path}.{k}");
                match props.and_then(|p| p.get(k)) {
                    Some(ps) => check(ps, child, &child_path, depth + 1, errors),
                    None => match s.get("additionalProperties") {
                        Some(Value::Bool(false)) => {
                            errors.push(format!("{path}: unexpected field {k:?}"))
                        }
                        Some(extra @ Value::Object(_)) => {
                            check(extra, child, &child_path, depth + 1, errors)
                        }
                        _ => {}
                    },
                }
            }
        }
        Value::Array(items) => {
            if let Some(n) = s.get("minItems").and_then(Value::as_u64) {
                if (items.len() as u64) < n {
                    errors.push(format!("{path}: needs at least {n} items"));
                }
            }
            if let Some(n) = s.get("maxItems").and_then(Value::as_u64) {
                if (items.len() as u64) > n {
                    errors.push(format!("{path}: allows at most {n} items"));
                }
            }
            if let Some(item_schema) = s.get("items").filter(|i| i.is_object() || i.is_boolean()) {
                for (i, item) in items.iter().enumerate() {
                    check(
                        item_schema,
                        item,
                        &format!("{path}[{i}]"),
                        depth + 1,
                        errors,
                    );
                }
            }
        }
        Value::String(text) => {
            let n = text.chars().count() as u64;
            if let Some(min) = s.get("minLength").and_then(Value::as_u64) {
                if n < min {
                    errors.push(format!("{path}: shorter than {min} characters"));
                }
            }
            if let Some(max) = s.get("maxLength").and_then(Value::as_u64) {
                if n > max {
                    errors.push(format!("{path}: longer than {max} characters"));
                }
            }
        }
        Value::Number(num) => {
            let x = num.as_f64().unwrap_or(f64::NAN);
            let bound = |k: &str| s.get(k).and_then(Value::as_f64);
            if bound("minimum").is_some_and(|m| x < m)
                || bound("exclusiveMinimum").is_some_and(|m| x <= m)
            {
                errors.push(format!("{path}: too small"));
            }
            if bound("maximum").is_some_and(|m| x > m)
                || bound("exclusiveMaximum").is_some_and(|m| x >= m)
            {
                errors.push(format!("{path}: too large"));
            }
        }
        _ => {}
    }

    if let Some(Value::Array(all)) = s.get("allOf") {
        for sub in all {
            check(sub, v, path, depth + 1, errors);
        }
    }
    let passes = |sub: &Value| {
        let mut e = Vec::new();
        check(sub, v, path, depth + 1, &mut e);
        e.is_empty()
    };
    if let Some(Value::Array(any)) = s.get("anyOf") {
        if !any.is_empty() && !any.iter().any(passes) {
            errors.push(format!("{path}: matches none of the allowed shapes"));
        }
    }
    if let Some(Value::Array(one)) = s.get("oneOf") {
        if !one.is_empty() && one.iter().filter(|s| passes(s)).count() != 1 {
            errors.push(format!("{path}: must match exactly one allowed shape"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "minLength": 1, "maxLength": 20},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                "kind": {"enum": ["contact", "deal"]},
                "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 2}
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    #[test]
    fn accepts_valid_values() {
        assert!(validate(
            &schema(),
            &json!({"query": "acme", "limit": 5, "kind": "deal"})
        )
        .is_ok());
        assert!(validate(&schema(), &json!({"query": "a", "limit": 5.0})).is_ok());
    }

    #[test]
    fn reports_problems_with_paths() {
        let errs = validate(
            &schema(),
            &json!({"limit": 0, "kind": "email", "tags": ["a", 1, "c"], "extra": true}),
        )
        .unwrap_err();
        let all = errs.join("\n");
        assert!(all.contains("missing required field \"query\""), "{all}");
        assert!(all.contains("$.limit: too small"), "{all}");
        assert!(all.contains("$.kind: not one of"), "{all}");
        assert!(all.contains("$.tags: allows at most 2"), "{all}");
        assert!(errs.len() <= MAX_ERRORS);
    }

    #[test]
    fn combinators_and_types() {
        let s = json!({"anyOf": [{"type": "string"}, {"type": "integer"}]});
        assert!(validate(&s, &json!("x")).is_ok());
        assert!(validate(&s, &json!(true)).is_err());
        let s = json!({"oneOf": [{"type": "number"}, {"type": "integer"}]});
        assert!(validate(&s, &json!(1)).is_err(), "1 matches both");
        assert!(validate(&s, &json!(1.5)).is_ok());
        assert!(validate(&json!({"type": ["string", "null"]}), &json!(null)).is_ok());
        assert!(validate(&json!(false), &json!(1)).is_err());
    }

    #[test]
    fn deep_values_stop_at_the_limit() {
        let mut s = json!({"type": "object"});
        let mut v = json!({});
        for _ in 0..40 {
            s = json!({"type": "object", "properties": {"a": s}});
            v = json!({"a": v});
        }
        assert!(validate(&s, &v).is_err());
    }
}
