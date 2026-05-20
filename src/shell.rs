//! Shared shell helpers. Currently exports POSIX single-quote escaping for
//! `cd ... && claude -r ...` resume strings (one in the Live tab `y`
//! handler, one in the session detail popup). Centralising the helper
//! keeps the `format!("cd {} && ...")` lint (#28) enforceable and
//! prevents the second copy from drifting if someone updates the
//! escaping policy.

/// POSIX-quote a cwd for `cd ...`, expanding a leading `~/` (or bare `~`)
/// to `$HOME` first. The Daily session detail popup feeds `project_name`
/// here, which is already display-formatted as `~/dev/foo`; naively
/// quoting that yields `cd '~/dev/foo'` and the shell treats `~` as a
/// literal directory name. If `$HOME` is unset (rare; not on a normal
/// login shell), fall back to plain quoting so the user at least sees
/// something pasteable rather than a broken command.
pub fn shell_quote_cwd(path: &str) -> String {
    shell_quote_cwd_with_home(path, std::env::var("HOME").ok().as_deref())
}

/// Testable core: expand a leading `~` against the given home (if any) and
/// POSIX-quote the result.
fn shell_quote_cwd_with_home(path: &str, home: Option<&str>) -> String {
    let expanded: String = if let Some(rest) = path.strip_prefix("~/") {
        match home {
            Some(h) => format!("{h}/{rest}"),
            None => path.to_string(),
        }
    } else if path == "~" {
        home.map_or_else(|| path.to_string(), str::to_string)
    } else {
        path.to_string()
    };
    posix_shell_quote(&expanded)
}

/// POSIX single-quote shell escape. Wraps the input in `'...'` and turns
/// any embedded `'` into `'\''`, which is the standard portable way to
/// embed arbitrary bytes in a sh / bash / zsh command line.
pub fn posix_shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::{posix_shell_quote, shell_quote_cwd_with_home};

    #[test]
    fn shell_quote_cwd_expands_tilde_slash() {
        assert_eq!(
            shell_quote_cwd_with_home("~/dev/myproject", Some("/Users/tester")),
            "'/Users/tester/dev/myproject'"
        );
    }

    #[test]
    fn shell_quote_cwd_expands_bare_tilde() {
        assert_eq!(
            shell_quote_cwd_with_home("~", Some("/Users/tester")),
            "'/Users/tester'"
        );
    }

    #[test]
    fn shell_quote_cwd_passes_absolute_paths_through() {
        assert_eq!(
            shell_quote_cwd_with_home("/var/log", Some("/Users/tester")),
            "'/var/log'"
        );
    }

    #[test]
    fn shell_quote_cwd_without_home_keeps_tilde() {
        // No $HOME — keep the literal `~/...` so the user at least sees a
        // pasteable command (broken, but visible) rather than a panic.
        assert_eq!(shell_quote_cwd_with_home("~/dev/foo", None), "'~/dev/foo'");
    }

    #[test]
    fn quotes_plain_path() {
        assert_eq!(posix_shell_quote("/Users/foo/bar"), "'/Users/foo/bar'");
    }

    #[test]
    fn quotes_path_with_space() {
        assert_eq!(posix_shell_quote("/My Docs/x"), "'/My Docs/x'");
    }

    #[test]
    fn neutralises_command_injection_attempts() {
        let payload = "/tmp; rm -rf /";
        let q = posix_shell_quote(payload);
        // The dangerous semicolon is now inside single quotes — `cd` will
        // simply receive the whole string as a (failing) directory name.
        assert_eq!(q, "'/tmp; rm -rf /'");
        assert!(q.starts_with('\'') && q.ends_with('\''));
    }

    #[test]
    fn escapes_embedded_single_quote() {
        // The classic escape: `'\''` ends the current quoted region,
        // inserts a literal `'`, and reopens.
        assert_eq!(posix_shell_quote("a'b"), "'a'\\''b'");
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(1024))]

        /// For ANY arbitrary input, the quoted form must be one shell
        /// argument — never a chain of commands. Test by simulating the
        /// shell-tokenisation rules: after our quoting, the only `'` chars
        /// should be the wrapping pair plus `'\''` patches; no unescaped
        /// `;` `|` `&` `$` `(` `>` `<` etc. should appear OUTSIDE quotes.
        #[test]
        fn quote_never_escapes_quoting_context(s in ".*") {
            let quoted = super::posix_shell_quote(&s);
            // Property: stripping the wrapping single quotes and the
            // documented `'\''` patches leaves a string that contains no
            // single quotes — i.e., every `'` in `quoted` is accounted for
            // by the wrapping scheme.
            proptest::prop_assert!(quoted.starts_with('\''));
            proptest::prop_assert!(quoted.ends_with('\''));
            // Walk the quoted string and check the only structural `'`s
            // are: opening quote (idx 0), closing quote (last idx), and
            // `'\''` triples in between.
            let bytes = quoted.as_bytes();
            let mut i = 1;
            let end = bytes.len() - 1;
            while i < end {
                if bytes[i] == b'\'' {
                    // Must be the start of `'\''` (3 bytes: ', \, ', ')
                    // — actually 4 bytes total. Verify the next 3.
                    proptest::prop_assert!(
                        i + 3 < bytes.len()
                            && bytes[i + 1] == b'\\'
                            && bytes[i + 2] == b'\''
                            && bytes[i + 3] == b'\'',
                        "found bare quote at index {i} in {quoted:?}"
                    );
                    i += 4;
                } else {
                    i += 1;
                }
            }
        }

        /// A POSIX-shell `printf %s` of the quoted form should reproduce
        /// the original input bytes exactly — i.e., the quote is faithful.
        /// We model `printf %s` here as: strip outer quotes, replace each
        /// `'\''` with a literal `'`, and verify the result equals input.
        #[test]
        fn quote_roundtrips_via_shell_interpretation(s in ".*") {
            let quoted = super::posix_shell_quote(&s);
            // Strip the outer single quotes
            let inner = &quoted[1..quoted.len() - 1];
            // Replace `'\''` (4 chars) with a single `'`
            let decoded = inner.replace("'\\''", "'");
            proptest::prop_assert_eq!(decoded, s);
        }
    }
}
