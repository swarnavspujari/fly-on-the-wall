//! Canonical JSON and the hashes built on it.
//!
//! Object keys are sorted at every level, so a hash doesn't depend on how a
//! server (or serde_json's `preserve_order` feature) happens to order them.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Compact JSON with object keys sorted recursively.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// `sha256(domain || "\n" || canonical_json(v))` as lowercase hex. The domain
/// keeps hashes of different kinds of record from ever being comparable.
pub fn domain_hash(domain: &str, v: &Value) -> String {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update(b"\n");
    h.update(canonical_json(v).as_bytes());
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0xf) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_change_the_hash() {
        let a = json!({"b": 1, "a": {"y": [1, {"q": 1, "p": 2}], "x": null}});
        let b: Value =
            serde_json::from_str(r#"{"a":{"x":null,"y":[1,{"p":2,"q":1}]},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(domain_hash("d", &a), domain_hash("d", &b));
    }

    #[test]
    fn domains_and_values_separate_hashes() {
        let v = json!({"a": 1});
        assert_ne!(domain_hash("one", &v), domain_hash("two", &v));
        assert_ne!(domain_hash("one", &v), domain_hash("one", &json!({"a": 2})));
        assert_eq!(domain_hash("one", &v).len(), 64);
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(canonical_json(&json!({"k\"": "a\nb"})), r#"{"k\"":"a\nb"}"#);
    }
}
