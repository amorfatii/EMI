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
    let (mut ni, mut pi) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ni = 0usize;
    while ni < n.len() {
        match p.get(pi) {
            Some('*') => {
                while p.get(pi) == Some(&'*') {
                    pi += 1;
                }
                star_pi = Some(pi);
                star_ni = ni;
            }
            Some('?') => {
                ni += 1;
                pi += 1;
            }
            Some('[') => match class_end(&p[pi..]) {
                Some(end) if class_matches(&p[pi + 1..pi + end], n[ni]) => {
                    ni += 1;
                    pi += end + 1;
                }
                None if n[ni] == '[' => {
                    ni += 1;
                    pi += 1;
                }
                _ => {
                    if let Some(sp) = star_pi {
                        star_ni += 1;
                        ni = star_ni;
                        pi = sp;
                    } else {
                        return false;
                    }
                }
            },
            Some(&c) if n[ni] == c => {
                ni += 1;
                pi += 1;
            }
            _ => {
                if let Some(sp) = star_pi {
                    star_ni += 1;
                    ni = star_ni;
                    pi = sp;
                } else {
                    return false;
                }
            }
        }
    }
    while p.get(pi) == Some(&'*') {
        pi += 1;
    }
    pi == p.len()
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

    #[test]
    fn fnmatch_handles_many_stars_in_linear_time() {
        let name = "a".repeat(80);
        let pat = "*a*a*a*a*a*a*a*a*a*a*b";
        assert!(!fnmatch(&name, pat));
        assert!(fnmatch(&format!("{name}b"), pat));
    }
}
