pub(crate) fn quote(word: &str) -> String {
    let safe = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "@%+:,./-_".contains(c));
    if safe { word.to_owned() } else { format!("'{}'", word.replace('\'', r#"'"'"'"#)) }
}

pub(crate) fn join(words: &[String]) -> String {
    words.iter().map(|word| quote(word)).collect::<Vec<_>>().join(" ")
}

pub(crate) fn read_word(text: &str) -> Option<(String, usize)> {
    let mut word = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(index, c)) = chars.peek() {
        match c {
            '\'' => {
                chars.next();
                loop {
                    match chars.next()? {
                        (_, '\'') => break,
                        (_, inner) => word.push(inner),
                    }
                }
            }
            '"' => {
                chars.next();
                loop {
                    match chars.next()? {
                        (_, '"') => break,
                        (_, '$' | '`') => return None,
                        (_, '\\') => match chars.next()? {
                            (_, '\n') => {}
                            (_, escaped @ ('"' | '\\' | '$' | '`')) => word.push(escaped),
                            (_, other) => {
                                word.push('\\');
                                word.push(other);
                            }
                        },
                        (_, inner) => word.push(inner),
                    }
                }
            }
            '\\' => {
                chars.next();
                word.push(chars.next()?.1);
            }
            '$' | '`' | '*' | '?' | '[' => return None,
            '~' | '#' if index == 0 => return None,
            c if c.is_whitespace() || ";&|<>()".contains(c) => return (index > 0).then_some((word, index)),
            c => {
                chars.next();
                word.push(c);
            }
        }
    }
    (!text.is_empty()).then_some((word, text.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_leaves_safe_words_alone() {
        assert_eq!(quote("--agent-id"), "--agent-id");
        assert_eq!(quote("/tmp/a-b_c.sh"), "/tmp/a-b_c.sh");
    }

    #[test]
    fn quote_wraps_words_with_spaces_quotes_or_newlines() {
        assert_eq!(quote(""), "''");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r#"'it'"'"'s'"#);
        assert_eq!(quote("a\nb"), "'a\nb'");
    }

    #[test]
    fn quote_wraps_assignments_so_they_stay_arguments() {
        assert_eq!(quote("FOO=bar"), "'FOO=bar'");
    }

    #[test]
    fn join_quotes_each_word() {
        let words = ["claude".to_owned(), "--prompt".to_owned(), "say hi".to_owned()];

        assert_eq!(join(&words), "claude --prompt 'say hi'");
    }

    #[test]
    fn read_word_undoes_quote() {
        for word in ["/work", "a b", "it's", "/Users/me/Application Support/repo"] {
            let quoted = quote(word);
            assert_eq!(
                read_word(&format!("{quoted} && env")),
                Some((word.to_owned(), quoted.len())),
                "{word}"
            );
        }
    }

    #[test]
    fn read_word_reads_double_quotes_and_backslashes() {
        assert_eq!(read_word(r#""a \"b\"" rest"#), Some((r#"a "b""#.to_owned(), 9)));
        assert_eq!(read_word(r"a\ b;"), Some(("a b".to_owned(), 4)));
        assert_eq!(read_word(r#""a\$b\`c" "#), Some(("a$b`c".to_owned(), 9)));
        assert_eq!(read_word("/old~1 "), Some(("/old~1".to_owned(), 6)));
    }

    #[test]
    fn read_word_refuses_words_the_shell_would_expand() {
        for text in ["$HOME/x", "\"$HOME\"", "~/x", "#comment", "a*", "`pwd`", "'unclosed", "", " leading"] {
            assert_eq!(read_word(text), None, "{text}");
        }
    }
}
