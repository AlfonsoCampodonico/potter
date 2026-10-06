//! Escape characters that could forge terminal output (kiln T8): C0/C1 controls except newline
//! and tab, DEL, and bidirectional overrides.

pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let bad = (c.is_control() && c != '\n' && c != '\t')
            || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
        if bad {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn escapes_controls_and_bidi() {
        assert_eq!(sanitize("ok\n\tline"), "ok\n\tline");
        assert_eq!(sanitize("a\u{1b}[31mb"), "a\\u{1b}[31mb");
        assert_eq!(sanitize("x\u{202e}y\u{7f}"), "x\\u{202e}y\\u{7f}");
        assert_eq!(sanitize("\u{85}"), "\\u{85}");
    }
}
