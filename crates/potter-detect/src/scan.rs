//! Minimal text scanning for values in JS/PHP config files. These are heuristics over source
//! text, never evaluation (P9): a value that isn't a literal string is treated as unknown.

/// For each occurrence of `key`: skip a closing quote if present, whitespace, `sep`, whitespace,
/// then read a single- or double-quoted string. Returns the first occurrence that matches.
pub fn quoted_value_after(text: &str, key: &str, sep: char) -> Option<String> {
    let mut from = 0;
    while let Some(pos) = text[from..].find(key) {
        let start = from + pos + key.len();
        from = start;
        let rest = &text[start..];
        let rest = rest.strip_prefix(['\'', '"']).unwrap_or(rest).trim_start();
        let Some(rest) = rest.strip_prefix(sep) else { continue };
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '\'' || *c == '"') else {
            continue;
        };
        let body = &rest[1..];
        if let Some(end) = body.find(quote) {
            return Some(body[..end].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::quoted_value_after;

    #[test]
    fn finds_values() {
        assert_eq!(
            quoted_value_after("module.exports = { output: 'standalone' }", "output", ':').as_deref(),
            Some("standalone")
        );
        assert_eq!(
            quoted_value_after("const c: NextConfig = {\n  output: \"standalone\",\n}", "output", ':').as_deref(),
            Some("standalone")
        );
        assert_eq!(
            quoted_value_after("'server' => env('OCTANE_SERVER', 'swoole'),", "OCTANE_SERVER", ',').as_deref(),
            Some("swoole")
        );
        assert_eq!(
            quoted_value_after("build: { outDir: 'build' }", "outDir", ':').as_deref(),
            Some("build")
        );
        assert_eq!(quoted_value_after("output: process.env.X", "output", ':'), None);
        assert_eq!(quoted_value_after("nothing here", "output", ':'), None);
    }

    #[test]
    fn skips_non_matching_occurrences() {
        let text = "// output is configured below\nexport default { output: 'standalone' }";
        assert_eq!(quoted_value_after(text, "output", ':').as_deref(), Some("standalone"));
    }
}
