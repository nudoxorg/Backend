//! Exact POSIX shell operands for executable presentation affordances.
/// Quotes one complete operand. Only shell-inert ASCII bytes stay unquoted;
/// JSON, Unicode and metacharacters are protected as a single literal argv.
pub(crate) fn quote_argument(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn executable_affordance_arguments_survive_actual_posix_argv_parsing() {
        let values = [
            "",
            "plain/path",
            "{}",
            "é ' apostrophe",
            "$(printf BAD)",
            "`printf BAD`",
            "$HOME",
            "; printf BAD",
            "line\nbreak",
            "double\"quote",
        ];
        let script = format!(
            r#"set -- {}; printf '%s\000' "$@""#,
            values
                .iter()
                .map(|v| quote_argument(v))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let result = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .output()
            .expect("actual POSIX shell");
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        let args = result
            .stdout
            .split(|b| *b == 0)
            .take(values.len())
            .map(|v| std::str::from_utf8(v).expect("UTF-8 argv"))
            .collect::<Vec<_>>();
        assert_eq!(args, values);
        assert_eq!(result.stdout.last(), Some(&0));
    }
}
