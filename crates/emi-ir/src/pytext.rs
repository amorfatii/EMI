//! Text rules that must match CPython exactly, because the v0.5 oracle
//! measured with them: `str.splitlines`, `str.isspace`/`str.strip`, and
//! universal-newline decoding as done by `Path.read_text()`.

/// `str.isspace()` for one character. Python counts U+001C..U+001F as
/// whitespace (bidi class B/S); Rust's `char::is_whitespace` does not.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str(UnicodeDecodeError)` for CPython's strict UTF-8 decoder.
pub fn utf8_error(bytes: &[u8], err: &std::str::Utf8Error) -> String {
    let start = err.valid_up_to();
    let (end, reason) = match err.error_len() {
        None => (bytes.len(), "unexpected end of data"),
        Some(n) if (0xc2..=0xf4).contains(&bytes[start]) => {
            (start + n, "invalid continuation byte")
        }
        Some(n) => (start + n, "invalid start byte"),
    };
    if end - start == 1 {
        format!(
            "'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            bytes[start]
        )
    } else {
        format!(
            "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
            end - 1
        )
    }
}

/// `str.strip()` with no arguments.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// True when `str.strip()` would return an empty string.
pub fn is_blank(s: &str) -> bool {
    s.chars().all(is_space)
}

fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// `str.splitlines()` (no keepends): splits on every Unicode line boundary
/// Python recognizes, treating `\r\n` as one break and dropping a trailing
/// empty segment.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !is_line_break(c) {
            continue;
        }
        out.push(&s[start..i]);
        let mut next = i + c.len_utf8();
        if c == '\r'
            && let Some(&(j, '\n')) = chars.peek()
        {
            chars.next();
            next = j + 1;
        }
        start = next;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Universal-newline translation as `open(..., newline=None).read()` does:
/// `\r\n` and lone `\r` both become `\n`.
pub fn universal_newlines(s: &str) -> String {
    if !s.contains('\r') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// `set(re.findall(r"[a-z]+", text.lower()))`: ASCII word runs of the
/// Unicode-lowercased text.
pub fn lower_ascii_words(text: &str) -> std::collections::BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_lowercase())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Lowercase hex SHA-256, as `hashlib.sha256(data).hexdigest()`.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_errors_match_cpython() {
        let cases: [(&[u8], &str); 9] = [
            (
                b"\xff",
                "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte",
            ),
            (
                b"ab\xe2\x82",
                "'utf-8' codec can't decode bytes in position 2-3: unexpected end of data",
            ),
            (
                b"\xe2(\xa1",
                "'utf-8' codec can't decode byte 0xe2 in position 0: invalid continuation byte",
            ),
            (
                b"\xf0\x9f\x98",
                "'utf-8' codec can't decode bytes in position 0-2: unexpected end of data",
            ),
            (
                b"\xc0\xaf",
                "'utf-8' codec can't decode byte 0xc0 in position 0: invalid start byte",
            ),
            (
                b"\xed\xa0\x80",
                "'utf-8' codec can't decode byte 0xed in position 0: invalid continuation byte",
            ),
            (
                b"\xe0\x80\x80",
                "'utf-8' codec can't decode byte 0xe0 in position 0: invalid continuation byte",
            ),
            (
                b"ok\xf0\x9f\x98x",
                "'utf-8' codec can't decode bytes in position 2-4: invalid continuation byte",
            ),
            (
                b"\xf5",
                "'utf-8' codec can't decode byte 0xf5 in position 0: invalid start byte",
            ),
        ];
        for (bytes, want) in cases {
            let err = std::str::from_utf8(bytes).err();
            assert_eq!(err.map(|e| utf8_error(bytes, &e)).as_deref(), Some(want));
        }
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(
            splitlines("a\nb\r\nc\rd\x0ce\u{2028}f\n"),
            ["a", "b", "c", "d", "e", "f"]
        );
        assert_eq!(splitlines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n"), [""]);
        assert_eq!(splitlines("x\r"), ["x"]);
    }

    #[test]
    fn strip_matches_python() {
        assert_eq!(strip("\x1c\t x \u{3000}"), "x");
        assert_eq!(strip("\u{feff}x"), "\u{feff}x");
        assert!(is_blank(" \x0b\x0c\u{85}"));
        assert!(!is_blank("\u{180e}"));
    }

    #[test]
    fn universal_newlines_translates_cr() {
        assert_eq!(universal_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn sha256_known_value() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
