//! Inline suppressions: `emi-ignore[rule-a, rule-b] until=2027-01-31: reason`
//! on the flagged line or the line above. A reason is mandatory; an
//! expired or reasonless suppression does not apply and is reported.

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Suppression {
    pub line: u32,
    /// The marker is on a comment-only line, so it covers the next line;
    /// a marker after code covers only its own line.
    pub own_line: bool,
    pub rules: Vec<String>,
    pub until: Option<String>,
    pub reason: String,
}

/// Parse every `emi-ignore[...]` marker in the source.
pub fn parse(source: &str) -> Vec<Suppression> {
    let mut out = Vec::new();
    for (i, text) in source.lines().enumerate() {
        let Some(start) = text.find("emi-ignore[") else {
            continue;
        };
        let before = text[..start].trim();
        let own_line = before
            .trim_start_matches(['/', '#', '*', '<', '!', '-', ' '])
            .is_empty();
        let rest = &text[start + "emi-ignore[".len()..];
        let Some(close) = rest.find(']') else {
            continue;
        };
        let rules = rest[..close]
            .split(',')
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
            .collect();
        let mut tail = rest[close + 1..].trim();
        let mut until = None;
        if let Some(after) = tail.strip_prefix("until=") {
            let date: String = after
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            tail = after[date.len()..].trim();
            until = Some(date);
        }
        let reason = tail
            .trim_start_matches([':', '-', ' '])
            .trim_end_matches("*/")
            .trim_end_matches("-->")
            .trim()
            .to_owned();
        out.push(Suppression {
            line: u32::try_from(i + 1).unwrap_or(u32::MAX),
            own_line,
            rules,
            until,
            reason,
        });
    }
    out
}

/// What happened to a hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Active,
    Suppressed(String),
}

/// Problems with the markers themselves, as `(line, message)`.
pub fn problems(suppressions: &[Suppression], today: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for s in suppressions {
        if s.rules.is_empty() {
            out.push((
                s.line,
                "emi-ignore names no rule; list the rule ids in brackets".to_owned(),
            ));
        } else if s.reason.is_empty() {
            out.push((
                s.line,
                format!(
                    "emi-ignore[{}] has no reason; suppressions must say why",
                    s.rules.join(", ")
                ),
            ));
        } else if s.until.as_deref().is_some_and(|u| u < today) {
            out.push((
                s.line,
                format!(
                    "emi-ignore[{}] expired on {}",
                    s.rules.join(", "),
                    s.until.as_deref().unwrap_or("")
                ),
            ));
        }
    }
    out
}

/// Whether a hit of `rule` starting at `line` is suppressed by a valid
/// marker on that line or the line above.
pub fn verdict(suppressions: &[Suppression], rule: &str, line: u32, today: &str) -> Verdict {
    let by_line: HashMap<u32, &Suppression> = suppressions.iter().map(|s| (s.line, s)).collect();
    for (l, above) in [(line, false), (line.saturating_sub(1), true)] {
        if let Some(s) = by_line.get(&l)
            && (!above || s.own_line)
            && s.rules.iter().any(|r| r == rule)
            && !s.reason.is_empty()
            && s.until.as_deref().is_none_or(|u| u >= today)
        {
            return Verdict::Suppressed(s.reason.clone());
        }
    }
    Verdict::Active
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reasons_and_expiry() {
        let src = "x = 1  # emi-ignore[a, b] until=2026-01-01: legacy API\n/* emi-ignore[c] */\n// emi-ignore[d] fixture needs it\n";
        let s = parse(src);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].rules, ["a", "b"]);
        assert_eq!(s[0].until.as_deref(), Some("2026-01-01"));
        assert_eq!(s[0].reason, "legacy API");
        assert!(s[1].reason.is_empty());
        assert_eq!(
            verdict(&s, "a", 1, "2025-12-31"),
            Verdict::Suppressed("legacy API".into())
        );
        assert_eq!(
            verdict(&s, "a", 1, "2026-02-01"),
            Verdict::Active,
            "expired"
        );
        assert_eq!(
            verdict(&s, "c", 3, "2026-02-01"),
            Verdict::Active,
            "no reason"
        );
        assert_eq!(
            verdict(&s, "d", 4, "2026-02-01"),
            Verdict::Suppressed("fixture needs it".into()),
            "line above"
        );
        let p = problems(&s, "2026-02-01");
        assert_eq!(p.len(), 2);
    }
}
