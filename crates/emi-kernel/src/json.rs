//! The one JSON encoding EMI hashes and prints: canonical (sorted keys,
//! compact) for identities, pretty and one-line forms for output, and a
//! strict reader that refuses duplicate keys and non-finite numbers.

use serde_json::Value;

/// Shortest round-trip digits and decimal-point position:
/// value = 0.d1d2... × 10^decpt.
fn shortest(x: f64) -> (String, i32) {
    let s = format!("{:e}", x.abs());
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() {
        "0".to_owned()
    } else {
        digits.to_owned()
    };
    (digits, exp.parse::<i32>().unwrap_or(0) + 1)
}

fn exponent(e: i32) -> String {
    format!("e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
}

/// `repr(float)`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if x == 0.0 {
        return format!("{sign}0.0");
    }
    let (digits, decpt) = shortest(x);
    let n = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    let zeros = |k: i32| "0".repeat(usize::try_from(k).unwrap_or(0));
    if decpt <= -4 || decpt > 16 {
        let (first, rest) = digits.split_at(1);
        let dot = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        return format!("{sign}{first}{dot}{}", exponent(decpt - 1));
    }
    if decpt <= 0 {
        return format!("{sign}0.{}{digits}", zeros(-decpt));
    }
    if decpt >= n {
        return format!("{sign}{digits}{}.0", zeros(decpt - n));
    }
    let (int, frac) = digits.split_at(usize::try_from(decpt).unwrap_or(0));
    format!("{sign}{int}.{frac}")
}

fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A string as canonical JSON (quoted and escaped), for messages that name
/// a key or value exactly.
pub fn canonical_string(s: &str) -> String {
    let mut out = String::new();
    escape_into(&mut out, s);
    out
}

/// `json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
/// allow_nan=False)`.
pub fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) if !n.is_f64() => out.push_str(&i.to_string()),
            (_, Some(u), _) if !n.is_f64() => out.push_str(&u.to_string()),
            (_, _, Some(f)) => out.push_str(&float_repr(f)),
            _ => out.push_str(&n.to_string()),
        },
        Value::String(s) => escape_into(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                escape_into(out, k);
                out.push(':');
                canonical(&map[k], out);
            }
            out.push('}');
        }
    }
}

/// Contract digest: SHA-256 of `"<domain>:emi-contracts-0.1:" + canonical`.
pub fn digest(domain: &str, v: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut text = String::new();
    canonical(v, &mut text);
    let mut h = Sha256::new();
    h.update(format!("{domain}:emi-contracts-0.1:").as_bytes());
    h.update(text.as_bytes());
    hex::encode(h.finalize())
}

/// `json.dumps(v, indent=2)` (ASCII-escaped, insertion order): the
/// oracle CLI's output format, so reports diff byte for byte.
pub fn dumps_pretty(v: &Value) -> String {
    let mut out = String::new();
    pretty(v, 0, &mut out);
    out
}

fn escape_ascii_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// `json.dumps(v)`: one line, `", "`/`": "` separators, ASCII-escaped.
pub fn dumps_line(v: &Value) -> String {
    let mut out = String::new();
    line(v, &mut out);
    out
}

fn line(v: &Value, out: &mut String) {
    match v {
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                line(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                escape_ascii_into(out, k);
                out.push_str(": ");
                line(item, out);
            }
            out.push('}');
        }
        Value::String(s) => escape_ascii_into(out, s),
        scalar => canonical(scalar, out),
    }
}

fn pretty(v: &Value, depth: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match v {
        Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                out.push_str(if i == 0 { "\n" } else { ",\n" });
                out.push_str(&pad(depth + 1));
                pretty(item, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&pad(depth));
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push('{');
            for (i, (k, item)) in map.iter().enumerate() {
                out.push_str(if i == 0 { "\n" } else { ",\n" });
                out.push_str(&pad(depth + 1));
                escape_ascii_into(out, k);
                out.push_str(": ");
                pretty(item, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&pad(depth));
            out.push('}');
        }
        Value::Array(_) => out.push_str("[]"),
        Value::Object(_) => out.push_str("{}"),
        Value::String(s) => escape_ascii_into(out, s),
        scalar => canonical(scalar, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_print_as_python_repr() {
        for (x, want) in [
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (10.0, "10.0"),
            (0.1, "0.1"),
            (-2.5, "-2.5"),
            (1.5e300, "1.5e+300"),
        ] {
            assert_eq!(float_repr(x), want);
        }
    }

    #[test]
    fn canonical_sorts_keys_and_is_compact() {
        let mut out = String::new();
        canonical(&json!({"b": [1, 2.5], "a": "x\n"}), &mut out);
        assert_eq!(out, r#"{"a":"x\n","b":[1,2.5]}"#);
    }

    #[test]
    fn digest_depends_on_domain_and_content_not_key_order() {
        let a = digest("d", &json!({"x": 1, "y": 2}));
        assert_eq!(a, digest("d", &json!({"y": 2, "x": 1})));
        assert_ne!(a, digest("e", &json!({"x": 1, "y": 2})));
        assert_ne!(a, digest("d", &json!({"x": 1, "y": 3})));
    }
}
