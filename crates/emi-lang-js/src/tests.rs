use super::*;

fn measured(file: &str, source: &str) -> FileMeasure {
    measure(file, source).unwrap_or_else(|| panic!("{file} did not parse"))
}

#[test]
fn sample_matches_oracle() {
    // Expected tables produced by the v0.5.1 oracle (tools/oracle_snippet.py).
    let src = include_str!("../tests/fixtures/sample.ts");
    let m = measured("sample.ts", src);
    let table: Vec<(&str, u32, u32, Option<u32>)> = m
        .functions
        .iter()
        .map(|f| (f.function.as_str(), f.line, f.sloc, f.cc))
        .collect();
    assert_eq!(
        table,
        vec![
            ("<anonymous>", 12, 1, Some(2)),
            ("<anonymous>", 21, 1, Some(1)),
            ("<anonymous>", 17, 4, Some(5)),
            ("onClick", 9, 5, Some(7)),
            ("<module/class blocks>", 1, 5, Some(1)),
        ]
    );
    let ids: Vec<(&str, u32)> = m.regions.iter().map(|r| (r.id.as_str(), r.sloc)).collect();
    assert_eq!(
        ids,
        vec![
            ("<anonymous>.onClick.f", 1),
            ("obj.key-a", 1),
            ("module.exports.handler", 4),
            ("<anonymous>.onClick", 5),
            ("<module>", 5),
        ]
    );
}

#[test]
fn syntax_errors_make_region_erosion_unknown() {
    let m = measured(
        "bad.js",
        "function ok() { return 1; }\nfunction bad() { return (; }\n",
    );
    assert!(!m.error_lines.is_empty());
    let bad = m
        .functions
        .iter()
        .find(|f| f.function == "bad")
        .map(|f| (f.cc, f.erosion_exact));
    assert_eq!(bad, Some((None, Some(false))));
    assert!(matches!(m.erosion, Evidence::Partial { .. }));
}
