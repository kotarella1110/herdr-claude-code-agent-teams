pub(crate) fn quote(word: &str) -> String {
    let safe = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "@%+:,./-_".contains(c));
    if safe { word.to_owned() } else { format!("'{}'", word.replace('\'', r#"'"'"'"#)) }
}

pub(crate) fn join(words: &[String]) -> String {
    words.iter().map(|word| quote(word)).collect::<Vec<_>>().join(" ")
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
}
