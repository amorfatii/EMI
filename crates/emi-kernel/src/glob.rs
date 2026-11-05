//! Glob matching as `fnmatch.fnmatch` on posix: `*` spans `/`, `?` is one
//! character, `[...]` is a set. One implementation, used for test globs,
//! trusted-file lists and law directories alike.

/// `fnmatch.fnmatch` on posix: `*` spans `/`, `?` one char, `[...]` sets.
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    let n: Vec<char> = name.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    matches_at(&n, &p)
}

fn class_end(p: &[char]) -> Option<usize> {
    let mut j = 1;
    if p.get(j) == Some(&'!') {
        j += 1;
    }
    if p.get(j) == Some(&']') {
        j += 1;
    }
    while j < p.len() && p[j] != ']' {
        j += 1;
    }
    (j < p.len()).then_some(j)
}

fn class_matches(set: &[char], c: char) -> bool {
    let (negate, set) = match set.first() {
        Some('!') => (true, &set[1..]),
        _ => (false, set),
    };
    let mut hit = false;
    let mut i = 0;
    while i < set.len() {
        if i + 2 < set.len() && set[i + 1] == '-' {
            hit |= set[i] <= c && c <= set[i + 2];
            i += 3;
        } else {
            hit |= set[i] == c;
            i += 1;
        }
    }
    hit != negate
}

fn matches_at(n: &[char], p: &[char]) -> bool {
    match p.first() {
        None => n.is_empty(),
        Some('*') => (0..=n.len()).any(|k| matches_at(&n[k..], &p[1..])),
        Some('?') => !n.is_empty() && matches_at(&n[1..], &p[1..]),
        Some('[') => match class_end(p) {
            Some(end) => {
                !n.is_empty()
                    && class_matches(&p[1..end], n[0])
                    && matches_at(&n[1..], &p[end + 1..])
            }
            None => n.first() == Some(&'[') && matches_at(&n[1..], &p[1..]),
        },
        Some(&c) => n.first() == Some(&c) && matches_at(&n[1..], &p[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnmatch_matches_python() {
        assert!(fnmatch("a/b/c.test.js", "*.test.*"));
        assert!(fnmatch("pkg/x.py", "pkg/?.py"));
        assert!(fnmatch("a1", "a[0-9]"));
        assert!(!fnmatch("ab", "a[!b]"));
        assert!(fnmatch("a[", "a["));
        assert!(!fnmatch("x.py", "*.js"));
        assert!(fnmatch("tests/sub/test_x.py", "tests/*"));
    }
}
