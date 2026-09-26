//! Python `repr()` of JSON-decoded values, for messages that must match
//! the oracle's f-strings (`{x!r}`, `type(x).__name__`).

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

pub fn exponent(e: i32) -> String {
    format!("e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
}

/// `repr(float)`.
pub fn float(x: f64) -> String {
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

/// `repr(str)`.
pub fn string(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `repr()` of a JSON-decoded value (dict, list, str, int, float, bool, None).
pub fn value(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::String(s) => string(s),
        Value::Number(n) if n.is_f64() => n.as_f64().map_or_else(|| n.to_string(), float),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(value).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => {
            let items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", string(k), value(v)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// `type(v).__name__` of a JSON-decoded value.
pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::String(_) => "str",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_python() {
        assert_eq!(float(1e-5), "1e-05");
        assert_eq!(float(1e16), "1e+16");
        assert_eq!(float(1e15), "1000000000000000.0");
        assert_eq!(string("it's"), "\"it's\"");
        assert_eq!(
            value(&json!({"a": [1, 2.5, null, true], "b": "x"})),
            "{'a': [1, 2.5, None, True], 'b': 'x'}"
        );
        assert_eq!(type_name(&json!(null)), "NoneType");
    }
}
