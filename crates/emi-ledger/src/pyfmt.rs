//! Python numeric and JSON formatting the calculator's outputs depend on:
//! `math.fsum`, `repr(float)`, `format(x, "g")`, and the canonical JSON
//! behind every contract digest.

use serde_json::Value;

/// `math.fsum`: exactly rounded sum (CPython's partials algorithm, including
/// its half-even correction across partials).
pub fn fsum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut partials: Vec<f64> = Vec::new();
    for value in values {
        let mut x = value;
        let mut i = 0;
        for j in 0..partials.len() {
            let mut y = partials[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let lo = y - (hi - x);
            if lo != 0.0 {
                partials[i] = lo;
                i += 1;
            }
            x = hi;
        }
        partials.truncate(i);
        if x != 0.0 {
            partials.push(x);
        }
    }
    let mut n = partials.len();
    if n == 0 {
        return 0.0;
    }
    n -= 1;
    let mut hi = partials[n];
    let mut lo = 0.0;
    while n > 0 {
        let x = hi;
        n -= 1;
        let y = partials[n];
        hi = x + y;
        let yr = hi - x;
        lo = y - yr;
        if lo != 0.0 {
            break;
        }
    }
    if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
        let y = lo * 2.0;
        let x = hi + y;
        if y == x - hi {
            hi = x;
        }
    }
    hi
}

/// Python `x ** 2` for a float: libm `pow(x, 2.0)`, which is not always
/// the correctly rounded `x * x` (about 0.1% of inputs differ in the last
/// place). The exponent is opaque so LLVM cannot fold the call into a
/// multiply; the host libm then decides, as it does for CPython.
pub fn square(x: f64) -> f64 {
    x.powf(std::hint::black_box(2.0))
}

use emi_ir::pyrepr::exponent;
pub use emi_ir::pyrepr::float as repr;

/// `format(x, "g")`: six significant digits, trailing zeros stripped.
pub fn g(x: f64) -> String {
    if !x.is_finite() {
        return repr(x);
    }
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let s = format!("{:.5e}", x);
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let strip = |m: &str| -> String {
        if m.contains('.') {
            m.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            m.to_owned()
        }
    };
    if (-4..6).contains(&exp) {
        let decimals = usize::try_from(5 - exp).unwrap_or(0);
        strip(&format!("{:.*}", decimals, x))
    } else {
        format!("{}{}", strip(mantissa), exponent(exp))
    }
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

/// `json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
/// allow_nan=False)`.
pub fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) if !n.is_f64() => out.push_str(&i.to_string()),
            (_, Some(u), _) if !n.is_f64() => out.push_str(&u.to_string()),
            (_, _, Some(f)) => out.push_str(&repr(f)),
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

    #[test]
    fn repr_matches_python() {
        for (x, want) in [
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (10.0, "10.0"),
            (0.1, "0.1"),
            (-2.5, "-2.5"),
            (64.758_241_758_241_77, "64.75824175824177"),
            (1.5e300, "1.5e+300"),
        ] {
            assert_eq!(repr(x), want);
        }
    }

    #[test]
    fn g_matches_python() {
        for (x, want) in [
            (64.758_241_758_241_76, "64.7582"),
            (10.0, "10"),
            (1_234_567.0, "1.23457e+06"),
            (0.000_12, "0.00012"),
            (0.000_012_3, "1.23e-05"),
            (-3.0, "-3"),
        ] {
            assert_eq!(g(x), want);
        }
    }

    #[test]
    fn fsum_is_exact() {
        assert_eq!(fsum([0.1; 10]), 1.0);
        assert_eq!(fsum([1e100, 1.0, -1e100, 1e-100]), 1.0);
        assert_eq!(fsum([]), 0.0);
    }
}
