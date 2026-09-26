//! `square` must reproduce CPython's `x ** 2` where it differs from `x * x`.
//! Cases recorded from CPython 3.14 on glibc (x, x**2 as float.hex).

fn parse_hex(s: &str) -> f64 {
    let (mantissa, exp) = s.split_once('p').unwrap_or((s, "0"));
    let mantissa = mantissa.trim_start_matches("0x");
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut value = u64::from_str_radix(int, 16).unwrap_or(0) as f64;
    for (i, c) in frac.chars().enumerate() {
        let digit = f64::from(c.to_digit(16).unwrap_or(0));
        value += digit * 16f64.powi(-(i32::try_from(i).unwrap_or(0) + 1));
    }
    value * 2f64.powi(exp.parse().unwrap_or(0))
}

#[test]
fn matches_cpython_pow() {
    let cases = include_str!("square_cases.txt");
    let mut differs_from_mul = 0;
    for line in cases.lines() {
        let (x, want) = line.split_once(' ').unwrap_or((line, line));
        let (x, want) = (parse_hex(x), parse_hex(want));
        assert_eq!(
            emi_ledger::pyfmt::square(x).to_bits(),
            want.to_bits(),
            "{x:e}"
        );
        differs_from_mul += usize::from(x * x != want);
    }
    assert!(differs_from_mul > 0);
}
