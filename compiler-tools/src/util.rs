/// Simple parse function for a string token with an arbitrary delimeter
pub fn parse_str<const DELIMITER: char>(input: &str) -> Option<(&str, &str)> {
    if !input.starts_with(DELIMITER) {
        return None;
    }
    let mut escaped = false;
    let mut iter = input.char_indices().skip(1);
    while let Some((i, c)) = iter.next() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == DELIMITER {
            if let Some((_, c)) = iter.next() {
                if c == DELIMITER {
                    continue;
                }
            }
            return Some((&input[..i + c.len_utf8()], &input[i + c.len_utf8()..]));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_string() {
        assert_eq!(parse_str::<'\''>("'hello'rest"), Some(("'hello'", "rest")));
    }

    #[test]
    fn empty_string() {
        assert_eq!(parse_str::<'\''>("''rest"), Some(("''", "rest")));
    }

    #[test]
    fn no_closing_delimiter() {
        assert_eq!(parse_str::<'\''>("'hello"), None);
    }

    #[test]
    fn does_not_start_with_delimiter() {
        assert_eq!(parse_str::<'\''>("hello'"), None);
        assert_eq!(parse_str::<'\''>(""), None);
    }

    #[test]
    fn doubled_delimiter_is_escape() {
        // A doubled delimiter is an escaped delimiter, so the whole literal is consumed.
        assert_eq!(parse_str::<'\''>("'a''b'"), Some(("'a''b'", "")));
        assert_eq!(parse_str::<'\''>("'a''b'after"), Some(("'a''b'", "after")));
    }

    #[test]
    fn backslash_escape() {
        // The backslash escapes the following delimiter; both stay in the matched slice.
        assert_eq!(parse_str::<'\''>("'a\\'b'"), Some(("'a\\'b'", "")));
    }

    #[test]
    fn unicode_content() {
        assert_eq!(parse_str::<'\''>("'héllo'x"), Some(("'héllo'", "x")));
    }

    #[test]
    fn alternate_delimiter() {
        assert_eq!(parse_str::<'"'>("\"hi\"!"), Some(("\"hi\"", "!")));
    }

    #[test]
    fn lone_delimiter() {
        assert_eq!(parse_str::<'\''>("'"), None);
    }
}
