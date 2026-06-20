//! Shared shell helpers. Currently exports POSIX single-quote escaping for
//! `cd ... && claude -r ...` resume strings (one in the Live tab `y`
//! handler, one in the session detail popup). Centralising the helper
//! keeps the `format!("cd {} && ...")` lint (#28) enforceable and
//! prevents the second copy from drifting if someone updates the
//! escaping policy.

/// POSIX-quote a cwd for `cd ...`, expanding leading `~/` (or bare `~`)
/// via `$HOME` first. Without expansion `cd '~/dev/foo'` is literal.
/// `$HOME` absent → plain quote (pasteable, broken — still better than panic).
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

/// `cd ... && claude -r <UUID>` from a JSONL path under
/// `~/.claude/projects/<slug>/<uuid>.jsonl`. The cwd comes from the
/// JSONL's `cwd` field (authoritative); reversing the slug is lossy when
/// the real path contains `-`, so it's only a fallback. `None` for Cowork
/// audit logs and Codex sessions (neither uses `claude -r`).
pub fn resume_command_from_jsonl(jsonl_path: &std::path::Path) -> Option<String> {
    if crate::infrastructure::is_cowork_audit_path(jsonl_path)
        || crate::infrastructure::is_codex_path(jsonl_path)
    {
        return None;
    }
    let session_id = jsonl_path.file_stem().and_then(|s| s.to_str())?;
    let cwd =
        crate::infrastructure::live_sessions::read_cwd_from_jsonl(jsonl_path).or_else(|| {
            jsonl_path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|slug| slug.replace('-', "/"))
        })?;
    Some(format!(
        "cd {} && claude -r {}",
        shell_quote_cwd(&cwd),
        posix_shell_quote(session_id),
    ))
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
    use super::{posix_shell_quote, resume_command_from_jsonl, shell_quote_cwd_with_home};
    use std::path::PathBuf;

    #[test]
    fn resume_command_falls_back_to_slug_when_jsonl_absent() {
        // No file on disk → read_cwd_from_jsonl returns None → slug reversal.
        let jsonl =
            PathBuf::from("/Users/me/.claude/projects/-Users-me-work-project/0aef-1234.jsonl");
        let cmd = resume_command_from_jsonl(&jsonl).unwrap();
        assert!(cmd.starts_with("cd '/Users/me/work/project'"));
        assert!(cmd.ends_with("claude -r '0aef-1234'"));
    }

    #[test]
    fn resume_command_prefers_jsonl_cwd_over_lossy_slug() {
        // The slug `-tmp-...-multi-word-dir` would reverse to
        // `/tmp/.../multi/word/dir`; the real cwd in the JSONL keeps
        // the literal `-`. The command must `cd` to the authoritative path.
        let dir = std::env::temp_dir()
            .join(format!("ccsight_resume_{}", std::process::id()))
            .join("-Users-me-dev-multi-word-dir");
        std::fs::create_dir_all(&dir).unwrap();
        let jp = dir.join("abc-123.jsonl");
        std::fs::write(
            &jp,
            "{\"type\":\"summary\"}\n\
             {\"type\":\"user\",\"cwd\":\"/Users/me/dev/multi-word-dir\"}\n",
        )
        .unwrap();
        let cmd = resume_command_from_jsonl(&jp).unwrap();
        assert!(
            cmd.starts_with("cd '/Users/me/dev/multi-word-dir'"),
            "expected authoritative cwd, got: {cmd}"
        );
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn resume_command_returns_none_for_cowork_audit_path() {
        // Path inside the Cowork tree must be opted out — Cowork sessions
        // are re-opened from Claude Desktop, not the CLI.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let jsonl = PathBuf::from(format!(
            "{home}/Library/Application Support/Claude/local-agent-mode-sessions/a/b/c/audit.jsonl"
        ));
        // Skip the assertion if the test machine has no Cowork root (CI on
        // Linux); the predicate `is_cowork_audit_path` returns false then.
        if crate::infrastructure::is_cowork_audit_path(&jsonl) {
            assert!(resume_command_from_jsonl(&jsonl).is_none());
        }
    }

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
