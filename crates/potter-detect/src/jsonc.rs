//! JSON with comments and trailing commas (deno.jsonc) → strict JSON.

pub fn strip_jsonc(input: &str) -> String {
    strip_trailing_commas(&strip_comments(input))
}

fn strip_comments(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match (c, chars.get(i + 1)) {
            ('"', _) => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn strip_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut in_str = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|ch| !ch.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_jsonc;

    fn parse(s: &str) -> serde_json::Value {
        serde_json::from_str(&strip_jsonc(s)).unwrap()
    }

    #[test]
    fn comments_and_trailing_commas() {
        let v = parse("{\n // line\n \"a\": 1, /* block */ \"b\": [1, 2,],\n \"c\": {\"d\": \"x\",},\n}");
        assert_eq!(v, serde_json::json!({"a": 1, "b": [1, 2], "c": {"d": "x"}}));
    }

    #[test]
    fn strings_are_untouched() {
        let v = parse(r#"{"url": "http://x//y", "glob": "/* not a comment */", "q": "a\"//b", "t": ",}"}"#);
        assert_eq!(v["url"], "http://x//y");
        assert_eq!(v["glob"], "/* not a comment */");
        assert_eq!(v["q"], "a\"//b");
        assert_eq!(v["t"], ",}");
    }

    #[test]
    fn comma_then_comment_then_brace() {
        assert_eq!(parse("{\"a\": 1, // last\n}"), serde_json::json!({"a": 1}));
    }
}
