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
